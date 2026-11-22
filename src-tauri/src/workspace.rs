//! 작업별 Git worktree (스펙 4.4).
//!
//! - 위치: 앱 데이터 폴더 `worktrees/project-<id>/task-<id>` (사용자 레포 안에 폴더를 만들지 않음)
//! - 브랜치: `mefice/task-<id>`. 기준은 사용자 현재 HEAD 또는 선행 작업 브랜치
//! - 미커밋 사용자 변경은 worktree에 포함되지 않는다 (커밋된 상태에서 시작)
//! - 실행이 끝나면 Mefice 브랜치에 스냅샷 커밋 → diff 확인 → 사용자가 병합 또는 폐기를 확정

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;

use crate::db::now_ms;
use crate::error::{AppError, AppResult};
use crate::git::{self, FileChange};
use crate::project;
use crate::task::{self, Task};

/// diff 본문 최대 크기 (UI 표시용)
const MAX_PATCH_BYTES: usize = 400_000;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub task_id: i64,
    pub path: String,
    pub branch: String,
    pub base_commit: String,
    pub base_ref: Option<String>,
    pub head_commit: Option<String>,
    pub merged_commit: Option<String>,
    pub created_at: i64,
    pub removed_at: Option<i64>,
}

fn from_row(r: &Row) -> rusqlite::Result<Workspace> {
    Ok(Workspace {
        task_id: r.get("task_id")?,
        path: r.get("path")?,
        branch: r.get("branch")?,
        base_commit: r.get("base_commit")?,
        base_ref: r.get("base_ref")?,
        head_commit: r.get("head_commit")?,
        merged_commit: r.get("merged_commit")?,
        created_at: r.get("created_at")?,
        removed_at: r.get("removed_at")?,
    })
}

pub fn get(conn: &Connection, task_id: i64) -> AppResult<Option<Workspace>> {
    Ok(conn
        .query_row(
            "SELECT * FROM workspaces WHERE task_id = ?1",
            [task_id],
            from_row,
        )
        .optional()?)
}

pub fn branch_name(task_id: i64) -> String {
    format!("mefice/task-{task_id}")
}

fn project_path(conn: &Connection, task: &Task) -> AppResult<PathBuf> {
    let p = project::get(conn, task.project_id)?
        .ok_or_else(|| AppError::NotFound(format!("프로젝트 #{}", task.project_id)))?;
    Ok(PathBuf::from(p.path))
}

/// Run 시작 전에 작업 공간을 준비하고 경로를 돌려준다. 같은 작업의 재실행·이어서 실행은 같은 경로를 쓴다
/// (런타임 세션이 작업 디렉터리에 묶이므로 경로가 바뀌면 이어서 실행이 불가능).
pub fn prepare(conn: &Connection, root: &Path, task: &Task) -> AppResult<PathBuf> {
    let repo = project_path(conn, task)?;
    if let Some(ws) = get(conn, task.id)? {
        let path = PathBuf::from(&ws.path);
        if ws.removed_at.is_none() && path.is_dir() {
            return Ok(path);
        }
        // 정리된 뒤 다시 실행하는 경우: 기록을 지우고 새로 만든다
        let _ = git::worktree_remove(&repo, &path);
        let _ = git::delete_branch(&repo, &ws.branch);
        conn.execute("DELETE FROM workspaces WHERE task_id = ?1", [task.id])?;
    }
    if !git::can_isolate(&repo) {
        return Err(AppError::InvalidInput(
            "커밋이 있는 Git 저장소가 아니어서 작업 공간을 만들 수 없습니다".into(),
        ));
    }

    // 기준: 첫 선행 작업 브랜치(있으면), 없으면 사용자 현재 HEAD
    let dep_workspaces: Vec<Workspace> = task::dependencies(conn, task.id)?
        .into_iter()
        .filter_map(|d| get(conn, d).ok().flatten())
        .filter(|w| w.head_commit.is_some())
        .collect();
    let (base_commit, base_ref) = match dep_workspaces.first() {
        Some(w) => (
            w.head_commit.clone().unwrap_or_default(),
            Some(w.branch.clone()),
        ),
        None => (git::head_commit(&repo)?, git::current_branch(&repo)),
    };

    let path = root
        .join(format!("project-{}", task.project_id))
        .join(format!("task-{}", task.id));
    let branch = branch_name(task.id);
    // 이전 기록 없이 남은 브랜치가 있으면 정리 (Mefice 소유 브랜치)
    let _ = git::worktree_remove(&repo, &path);
    let _ = git::delete_branch(&repo, &branch);
    git::worktree_add(&repo, &path, &branch, &base_commit)?;

    for w in dep_workspaces.iter().skip(1) {
        if let Some(head) = &w.head_commit
            && let Err(e) = git::merge_into_worktree(&path, head)
        {
            let _ = git::worktree_remove(&repo, &path);
            let _ = git::delete_branch(&repo, &branch);
            return Err(e);
        }
    }

    conn.execute(
        "INSERT INTO workspaces (task_id, path, branch, base_commit, base_ref, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            task.id,
            path.to_string_lossy(),
            branch,
            base_commit,
            base_ref,
            now_ms()
        ],
    )?;
    Ok(path)
}

/// 실행 종료 후 worktree 변경을 Mefice 브랜치에 고정한다.
pub fn snapshot(conn: &Connection, task: &Task, attempt: i64) -> AppResult<Option<String>> {
    let Some(ws) = get(conn, task.id)? else {
        return Ok(None);
    };
    let path = PathBuf::from(&ws.path);
    if ws.removed_at.is_some() || !path.is_dir() {
        return Ok(None);
    }
    let head = git::snapshot_commit(
        &path,
        &format!("Mefice 작업 #{} {} (실행 #{attempt})", task.id, task.title),
    )?;
    conn.execute(
        "UPDATE workspaces SET head_commit = ?1 WHERE task_id = ?2",
        params![head, task.id],
    )?;
    Ok(Some(head))
}

/// 앱 시작 시: 정리되지 않은 작업 공간의 남은 변경을 고정한다 (강제 종료로 스냅샷을 못 한 경우).
/// 실패한 작업 공간은 건너뛴다 (사용자가 폴더를 지운 경우 등).
pub fn snapshot_all(conn: &Connection) -> AppResult<usize> {
    let ids: Vec<i64> = conn
        .prepare("SELECT task_id FROM workspaces WHERE removed_at IS NULL")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    let mut n = 0;
    for id in ids {
        let Ok(t) = task::get_task(conn, id) else {
            continue;
        };
        let attempt = task::latest_run(conn, id)?.map(|r| r.attempt).unwrap_or(0);
        if snapshot(conn, &t, attempt).is_ok() {
            n += 1;
        }
    }
    Ok(n)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskDiff {
    pub available: bool,
    /// available이 false인 이유
    pub reason: Option<String>,
    pub integration: String,
    pub workspace: Option<Workspace>,
    pub files: Vec<FileChange>,
    pub patch: String,
    pub truncated: bool,
    /// 사용자 현재 HEAD와 병합 시 충돌할 파일
    pub conflicts: Vec<String>,
    /// 사용자 미커밋 변경과 겹치는 파일
    pub dirty_overlap: Vec<String>,
    pub can_merge: bool,
}

impl TaskDiff {
    fn unavailable(reason: &str, integration: &str, workspace: Option<Workspace>) -> Self {
        Self {
            available: false,
            reason: Some(reason.into()),
            integration: integration.into(),
            workspace,
            files: vec![],
            patch: String::new(),
            truncated: false,
            conflicts: vec![],
            dirty_overlap: vec![],
            can_merge: false,
        }
    }
}

fn run_active(conn: &Connection, task_id: i64) -> AppResult<bool> {
    Ok(task::latest_run(conn, task_id)?.is_some_and(|r| !r.status.is_terminal()))
}

/// 변경 내역과 병합 가능 여부 (사전 확인 포함, 작업 트리를 바꾸지 않음)
pub fn diff(conn: &Connection, task_id: i64) -> AppResult<TaskDiff> {
    let task = task::get_task(conn, task_id)?;
    let Some(ws) = get(conn, task_id)? else {
        return Ok(TaskDiff::unavailable(
            "격리된 작업 공간에서 실행되지 않은 작업입니다",
            &task.integration,
            None,
        ));
    };
    if ws.removed_at.is_some() {
        let reason = match task.integration.as_str() {
            "merged" => "병합 완료 — 작업 공간을 정리했습니다",
            "discarded" => "폐기됨 — 작업 공간을 정리했습니다",
            _ => "작업 공간이 정리되었습니다",
        };
        return Ok(TaskDiff::unavailable(reason, &task.integration, Some(ws)));
    }
    if run_active(conn, task_id)? {
        return Ok(TaskDiff::unavailable(
            "실행이 끝나면 변경 내역이 표시됩니다",
            &task.integration,
            Some(ws),
        ));
    }
    let Some(head) = ws.head_commit.clone() else {
        return Ok(TaskDiff::unavailable(
            "아직 기록된 변경이 없습니다",
            &task.integration,
            Some(ws),
        ));
    };

    let repo = project_path(conn, &task)?;
    let files = git::changed_files(&repo, &ws.base_commit, &head)?;
    let (patch, truncated) = git::diff(&repo, &ws.base_commit, &head, MAX_PATCH_BYTES)?;
    let conflicts = git::merge_conflicts(&repo, "HEAD", &head)?;
    let dirty = git::dirty_files(&repo)?;
    let dirty_overlap: Vec<String> = files
        .iter()
        .map(|f| f.path.clone())
        .filter(|p| dirty.contains(p))
        .collect();
    let can_merge = !files.is_empty()
        && conflicts.is_empty()
        && dirty_overlap.is_empty()
        && task.integration == "none";

    Ok(TaskDiff {
        available: true,
        reason: if files.is_empty() {
            Some("변경된 파일이 없습니다".into())
        } else {
            None
        },
        integration: task.integration,
        workspace: Some(ws),
        files,
        patch,
        truncated,
        conflicts,
        dirty_overlap,
        can_merge,
    })
}

fn cleanup(conn: &Connection, repo: &Path, ws: &Workspace) -> AppResult<()> {
    git::worktree_remove(repo, Path::new(&ws.path))?;
    let _ = git::delete_branch(repo, &ws.branch);
    conn.execute(
        "UPDATE workspaces SET removed_at = ?1 WHERE task_id = ?2",
        params![now_ms(), ws.task_id],
    )?;
    Ok(())
}

/// 사용자 현재 브랜치에 병합한다 (사용자 확정 시에만 호출). 사전 확인을 다시 수행한다.
pub fn merge(conn: &Connection, task_id: i64) -> AppResult<String> {
    let d = diff(conn, task_id)?;
    if !d.can_merge {
        let why = if !d.conflicts.is_empty() {
            format!("충돌이 예상됩니다: {}", d.conflicts.join(", "))
        } else if !d.dirty_overlap.is_empty() {
            format!(
                "커밋하지 않은 내 변경과 겹치는 파일이 있습니다: {}",
                d.dirty_overlap.join(", ")
            )
        } else {
            d.reason
                .unwrap_or_else(|| "병합할 수 없는 상태입니다".into())
        };
        return Err(AppError::InvalidInput(why));
    }
    let task = task::get_task(conn, task_id)?;
    let ws = d.workspace.expect("available이면 workspace 있음");
    let repo = project_path(conn, &task)?;
    let head = ws.head_commit.clone().expect("available이면 head 있음");
    let commit = git::merge_branch(
        &repo,
        &head,
        &format!("Mefice 작업 #{} 병합: {}", task.id, task.title),
    )?;
    task::set_integration(conn, task_id, "merged")?;
    conn.execute(
        "UPDATE workspaces SET merged_commit = ?1 WHERE task_id = ?2",
        params![commit, task_id],
    )?;
    cleanup(conn, &repo, &ws)?;
    Ok(commit)
}

/// 작업 결과를 버리고 작업 공간을 정리한다 (사용자 확정 시에만 호출).
pub fn discard(conn: &Connection, task_id: i64) -> AppResult<()> {
    if run_active(conn, task_id)? {
        return Err(AppError::InvalidInput(
            "실행 중인 작업은 폐기할 수 없습니다. 먼저 중단하세요".into(),
        ));
    }
    let task = task::get_task(conn, task_id)?;
    let ws = get(conn, task_id)?
        .filter(|w| w.removed_at.is_none())
        .ok_or_else(|| AppError::InvalidInput("정리할 작업 공간이 없습니다".into()))?;
    let repo = project_path(conn, &task)?;
    cleanup(conn, &repo, &ws)?;
    task::set_integration(conn, task_id, "discarded")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{self, AgentInput, PermissionMode};
    use crate::db::Db;
    use crate::task::RunStatus;

    struct Env {
        db: Db,
        repo: tempfile::TempDir,
        root: tempfile::TempDir,
        project_id: i64,
        agent_id: i64,
    }

    fn env() -> Env {
        let db = Db::open_in_memory().unwrap();
        let repo = git::test_repo::init();
        let root = tempfile::tempdir().unwrap();
        let (project_id, agent_id) = {
            let conn = db.conn().unwrap();
            let p = project::upsert(
                &conn,
                &project::normalize_path(repo.path().to_str().unwrap()).unwrap(),
            )
            .unwrap();
            let a = agent::create(
                &conn,
                &AgentInput {
                    name: "a".into(),
                    role: "r".into(),
                    instructions: String::new(),
                    model: None,
                    effort: None,
                    permission_mode: PermissionMode::Default,
                    tools: agent::default_tools(),
                    color: None,
                    profile: Default::default(),
                },
            )
            .unwrap();
            (p.id, a.id)
        };
        Env {
            db,
            repo,
            root,
            project_id,
            agent_id,
        }
    }

    /// 작업 + 성공한 Run 하나를 만들고 worktree에서 파일을 바꾼 뒤 스냅샷
    fn done_task(e: &Env, conn: &Connection, file: &str, content: &str) -> Task {
        let t = task::create_task(conn, e.project_id, e.agent_id, "x").unwrap();
        task::set_isolation(conn, t.id, task::ISOLATION_WORKTREE).unwrap();
        let path = prepare(conn, e.root.path(), &t).unwrap();
        std::fs::write(path.join(file), content).unwrap();
        let cfg = crate::pm::run_config(
            &project::get(conn, e.project_id).unwrap().unwrap(),
            &agent::get(conn, e.agent_id).unwrap(),
        );
        let r = task::create_run(
            conn,
            task::NewRun {
                task_id: t.id,
                agent_id: e.agent_id,
                prompt: "x",
                resume_session_id: None,
                config: &cfg,
                cwd: path.to_str().unwrap(),
            },
        )
        .unwrap();
        task::finish_run(conn, r.id, RunStatus::Succeeded, Some(0), None).unwrap();
        snapshot(conn, &t, 1).unwrap();
        task::get_task(conn, t.id).unwrap()
    }

    #[test]
    fn 준비는_사용자_폴더_밖에_만들고_재사용한다() {
        let e = env();
        let conn = e.db.conn().unwrap();
        let t = task::create_task(&conn, e.project_id, e.agent_id, "x").unwrap();
        let p1 = prepare(&conn, e.root.path(), &t).unwrap();
        assert!(p1.starts_with(e.root.path()));
        assert!(!p1.starts_with(e.repo.path()));
        assert_eq!(
            prepare(&conn, e.root.path(), &t).unwrap(),
            p1,
            "같은 작업은 같은 경로"
        );
        assert!(
            git::dirty_files(e.repo.path()).unwrap().is_empty(),
            "사용자 작업 트리는 그대로"
        );
    }

    #[test]
    fn 병렬_작업은_격리되고_충돌은_병합_전에_알린다() {
        let e = env();
        let conn = e.db.conn().unwrap();
        let t1 = done_task(&e, &conn, "a.txt", "one\nAAA\n");
        let t2 = done_task(&e, &conn, "a.txt", "one\nBBB\n");
        let t3 = done_task(&e, &conn, "c.txt", "new\n");

        let d1 = diff(&conn, t1.id).unwrap();
        assert!(d1.can_merge, "{d1:?}");
        assert_eq!(d1.files[0].path, "a.txt");
        merge(&conn, t1.id).unwrap();
        assert_eq!(
            std::fs::read_to_string(e.repo.path().join("a.txt")).unwrap(),
            "one\nAAA\n"
        );
        assert_eq!(task::get_task(&conn, t1.id).unwrap().integration, "merged");
        assert!(!diff(&conn, t1.id).unwrap().available);

        // t2는 t1 병합 후 같은 줄을 바꿔서 충돌 → 작업 트리 변경 없이 거부
        let d2 = diff(&conn, t2.id).unwrap();
        assert_eq!(d2.conflicts, vec!["a.txt"]);
        assert!(merge(&conn, t2.id).is_err());
        assert_eq!(
            std::fs::read_to_string(e.repo.path().join("a.txt")).unwrap(),
            "one\nAAA\n"
        );

        // 사용자가 c.txt를 미커밋 상태로 갖고 있으면 t3 병합 거부 (덮어쓰기 방지)
        std::fs::write(e.repo.path().join("c.txt"), "사용자 작업\n").unwrap();
        let d3 = diff(&conn, t3.id).unwrap();
        assert_eq!(d3.dirty_overlap, vec!["c.txt"]);
        assert!(merge(&conn, t3.id).is_err());
        assert_eq!(
            std::fs::read_to_string(e.repo.path().join("c.txt")).unwrap(),
            "사용자 작업\n"
        );

        discard(&conn, t2.id).unwrap();
        assert_eq!(
            task::get_task(&conn, t2.id).unwrap().integration,
            "discarded"
        );
        assert!(!Path::new(&get(&conn, t2.id).unwrap().unwrap().path).exists());
    }

    #[test]
    fn 강제종료_후_시작시_남은_변경을_고정한다() {
        let e = env();
        let conn = e.db.conn().unwrap();
        let t = task::create_task(&conn, e.project_id, e.agent_id, "x").unwrap();
        let path = prepare(&conn, e.root.path(), &t).unwrap();
        std::fs::write(path.join("b.txt"), "중간 결과\n").unwrap(); // 스냅샷 전에 앱이 죽음
        assert!(get(&conn, t.id).unwrap().unwrap().head_commit.is_none());
        assert_eq!(snapshot_all(&conn).unwrap(), 1);
        let d = diff(&conn, t.id).unwrap();
        assert_eq!(
            d.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
            vec!["b.txt"]
        );
    }

    #[test]
    fn 의존_작업은_선행_작업_결과_위에서_시작한다() {
        let e = env();
        let conn = e.db.conn().unwrap();
        let t1 = done_task(&e, &conn, "a.txt", "one\nFROM_T1\n");
        let t2 = task::create_task(&conn, e.project_id, e.agent_id, "y").unwrap();
        task::add_dependency(&conn, t2.id, t1.id, task::DEP_PLANNED).unwrap();
        let p2 = prepare(&conn, e.root.path(), &t2).unwrap();
        assert_eq!(
            std::fs::read_to_string(p2.join("a.txt")).unwrap(),
            "one\nFROM_T1\n"
        );
        assert_eq!(
            get(&conn, t2.id).unwrap().unwrap().base_ref.as_deref(),
            Some("mefice/task-1")
        );
    }
}
