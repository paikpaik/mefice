//! 대화 되돌리기 (스펙 v0.2 A단계 3번, 최소 안전망).
//!
//! 대화 응답 전후의 업무 폴더 상태를 앱 데이터 폴더의 숨긴 git 저장소에 기록한다.
//! `--git-dir`(기록 위치)과 `--work-tree`(사용자 폴더)를 분리하므로 사용자 폴더에는 아무것도 만들지 않는다.
//! 에이전트가 되살릴 수 없는 변경(삭제·덮어쓰기·바이너리 파일·명령으로 바뀐 파일)도 되돌릴 수 있다.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use rusqlite::{Connection, params};
use serde::Serialize;

use crate::db::now_ms;
use crate::error::{AppError, AppResult};
use crate::sysproc::{child_path, resolve_binary};

/// 기록하지 않는 폴더 (사용자 폴더의 .gitignore는 그대로 따른다)
const EXCLUDES: &[&str] = &[
    "node_modules/",
    "target/",
    "dist/",
    "build/",
    ".next/",
    ".venv/",
    "venv/",
    "__pycache__/",
    ".DS_Store",
];
/// 이벤트에 남기는 파일 이름 수 상한
const MAX_LISTED: usize = 50;

/// 같은 저장소에 여러 대화가 동시에 기록하지 않게 한다 (인덱스 충돌 방지)
static LOCK: Mutex<()> = Mutex::new(());

pub struct Shadow {
    git_dir: PathBuf,
    work_tree: PathBuf,
}

impl Shadow {
    pub fn new(root: &Path, project_id: i64, work_tree: &Path) -> Self {
        Self {
            git_dir: root.join(format!("project-{project_id}.git")),
            work_tree: work_tree.to_path_buf(),
        }
    }

    fn git(&self, args: &[&str]) -> AppResult<(i32, String, String)> {
        let bin = resolve_binary("git")
            .ok_or_else(|| AppError::Internal("git을 찾을 수 없습니다".into()))?;
        let out = Command::new(bin)
            .arg(format!("--git-dir={}", self.git_dir.display()))
            .arg(format!("--work-tree={}", self.work_tree.display()))
            .args([
                "-c",
                "user.name=Mefice",
                "-c",
                "user.email=mefice@localhost",
                "-c",
                "core.quotepath=false",
            ])
            .args(args)
            .current_dir(&self.work_tree)
            .env("PATH", child_path())
            .env("LC_ALL", "C")
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()?;
        Ok((
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ))
    }

    fn run(&self, args: &[&str]) -> AppResult<String> {
        let (code, out, err) = self.git(args)?;
        if code == 0 {
            Ok(out)
        } else {
            Err(AppError::Internal(format!(
                "기록 실패 (git {}): {err}",
                args.first().unwrap_or(&"")
            )))
        }
    }

    fn ensure(&self) -> AppResult<()> {
        if self.git_dir.join("HEAD").exists() {
            return Ok(());
        }
        std::fs::create_dir_all(&self.git_dir)?;
        self.run(&["init", "-q"])?;
        std::fs::create_dir_all(self.git_dir.join("info"))?;
        std::fs::write(
            self.git_dir.join("info/exclude"),
            EXCLUDES.join("\n") + "\n",
        )?;
        Ok(())
    }

    /// 현재 폴더 상태를 기록하고 커밋 id를 돌려준다
    pub fn snapshot(&self, label: &str) -> AppResult<String> {
        let _g = LOCK
            .lock()
            .map_err(|_| AppError::Internal("기록 잠금 오류".into()))?;
        self.ensure()?;
        self.run(&["add", "-A"])?;
        self.run(&["commit", "-q", "--allow-empty", "--no-verify", "-m", label])?;
        self.run(&["rev-parse", "HEAD"])
    }

    pub fn changed(&self, before: &str, after: &str) -> AppResult<Vec<String>> {
        let out = self.run(&["diff", "--name-only", "--no-renames", before, after])?;
        Ok(out
            .lines()
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect())
    }

    /// `after` 이후 사용자가 직접 바꾼 파일 (작업 트리 vs after)
    fn modified_since(&self, after: &str, files: &[String]) -> AppResult<Vec<String>> {
        let _g = LOCK
            .lock()
            .map_err(|_| AppError::Internal("기록 잠금 오류".into()))?;
        // 미추적 새 파일까지 비교하도록 인덱스를 갱신 (사용자 폴더는 바꾸지 않음)
        self.run(&["add", "-A"])?;
        let mut args = vec![
            "diff",
            "--cached",
            "--name-only",
            "--no-renames",
            after,
            "--",
        ];
        let refs: Vec<&str> = files.iter().map(String::as_str).collect();
        args.extend(refs);
        let out = self.run(&args)?;
        Ok(out
            .lines()
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect())
    }

    fn exists_in(&self, commit: &str, path: &str) -> bool {
        self.git(&["cat-file", "-e", &format!("{commit}:{path}")])
            .is_ok_and(|(c, _, _)| c == 0)
    }

    /// `before`→`after`에서 바뀐 파일을 `before` 상태로 돌린다. 그 뒤 사용자가 고친 파일은 건너뛴다.
    pub fn restore(&self, before: &str, after: &str) -> AppResult<RestoreResult> {
        let files = self.changed(before, after)?;
        let skipped = self.modified_since(after, &files)?;
        let mut restored = Vec::new();
        {
            let _g = LOCK
                .lock()
                .map_err(|_| AppError::Internal("기록 잠금 오류".into()))?;
            for f in files.iter().filter(|f| !skipped.contains(f)) {
                if self.exists_in(before, f) {
                    self.run(&["checkout", before, "--", f])?;
                } else {
                    // 응답에서 새로 만든 파일 → 지운다
                    let p = self.work_tree.join(f);
                    if p.exists() {
                        std::fs::remove_file(&p)?;
                    }
                }
                restored.push(f.clone());
            }
        }
        self.snapshot("되돌림")?;
        Ok(RestoreResult { restored, skipped })
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RestoreResult {
    pub restored: Vec<String>,
    /// 응답 뒤 사용자가 직접 고쳐서 건너뛴 파일
    pub skipped: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Checkpoint {
    pub id: i64,
    pub task_id: i64,
    pub run_id: i64,
    pub project_id: i64,
    pub before_commit: String,
    pub after_commit: String,
    pub files: Vec<String>,
    pub restored_at: Option<i64>,
}

pub fn insert(
    conn: &Connection,
    task_id: i64,
    run_id: i64,
    project_id: i64,
    before: &str,
    after: &str,
    files: &[String],
) -> AppResult<i64> {
    conn.execute(
        "INSERT INTO checkpoints (task_id, run_id, project_id, before_commit, after_commit, files, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![task_id, run_id, project_id, before, after, serde_json::to_string(files)?, now_ms()],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn get(conn: &Connection, id: i64) -> AppResult<Checkpoint> {
    conn.query_row(
        "SELECT id, task_id, run_id, project_id, before_commit, after_commit, files, restored_at FROM checkpoints WHERE id = ?1",
        [id],
        |r| {
            Ok(Checkpoint {
                id: r.get(0)?,
                task_id: r.get(1)?,
                run_id: r.get(2)?,
                project_id: r.get(3)?,
                before_commit: r.get(4)?,
                after_commit: r.get(5)?,
                files: serde_json::from_str(&r.get::<_, String>(6)?).unwrap_or_default(),
                restored_at: r.get(7)?,
            })
        },
    )
    .map_err(|_| AppError::NotFound(format!("되돌리기 기록 #{id}")))
}

pub fn mark_restored(conn: &Connection, id: i64) -> AppResult<()> {
    conn.execute(
        "UPDATE checkpoints SET restored_at = ?1 WHERE id = ?2",
        params![now_ms(), id],
    )?;
    Ok(())
}

pub fn listed(files: &[String]) -> Vec<String> {
    files.iter().take(MAX_LISTED).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, tempfile::TempDir, Shadow) {
        let root = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let s = Shadow::new(root.path(), 1, work.path());
        (root, work, s)
    }

    #[test]
    fn 사용자_폴더에_아무것도_만들지_않고_삭제_덮어쓰기_새파일을_되돌린다() {
        let (_r, work, s) = setup();
        let w = work.path();
        std::fs::write(w.join("report.md"), "원본\n").unwrap();
        std::fs::write(w.join("keep.bin"), [0u8, 1, 2, 255]).unwrap();
        std::fs::create_dir_all(w.join("node_modules/x")).unwrap();
        std::fs::write(w.join("node_modules/x/big.js"), "x").unwrap();
        let before = s.snapshot("before").unwrap();

        // 응답 중 변경: 덮어쓰기, 바이너리 삭제, 새 파일
        std::fs::write(w.join("report.md"), "에이전트가 덮어씀\n").unwrap();
        std::fs::remove_file(w.join("keep.bin")).unwrap();
        std::fs::write(w.join("new.txt"), "새 파일\n").unwrap();
        let after = s.snapshot("after").unwrap();
        let mut files = s.changed(&before, &after).unwrap();
        files.sort();
        assert_eq!(
            files,
            vec!["keep.bin", "new.txt", "report.md"],
            "node_modules는 기록 안 함"
        );

        let r = s.restore(&before, &after).unwrap();
        assert!(r.skipped.is_empty());
        assert_eq!(
            std::fs::read_to_string(w.join("report.md")).unwrap(),
            "원본\n"
        );
        assert_eq!(
            std::fs::read(w.join("keep.bin")).unwrap(),
            vec![0u8, 1, 2, 255],
            "삭제된 바이너리 복구"
        );
        assert!(!w.join("new.txt").exists(), "응답에서 만든 파일은 지움");
        assert!(!w.join(".git").exists(), "사용자 폴더에 .git을 만들지 않음");
        let names: Vec<String> = std::fs::read_dir(w)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert!(!names.iter().any(|n| n.contains("mefice")), "{names:?}");
    }

    #[test]
    fn 응답_뒤_사용자가_고친_파일은_건너뛴다() {
        let (_r, work, s) = setup();
        let w = work.path();
        std::fs::write(w.join("a.md"), "원본 a\n").unwrap();
        std::fs::write(w.join("b.md"), "원본 b\n").unwrap();
        let before = s.snapshot("before").unwrap();
        std::fs::write(w.join("a.md"), "에이전트 a\n").unwrap();
        std::fs::write(w.join("b.md"), "에이전트 b\n").unwrap();
        let after = s.snapshot("after").unwrap();
        std::fs::write(w.join("b.md"), "사용자가 나중에 고침\n").unwrap();

        let r = s.restore(&before, &after).unwrap();
        assert_eq!(r.restored, vec!["a.md"]);
        assert_eq!(r.skipped, vec!["b.md"]);
        assert_eq!(std::fs::read_to_string(w.join("a.md")).unwrap(), "원본 a\n");
        assert_eq!(
            std::fs::read_to_string(w.join("b.md")).unwrap(),
            "사용자가 나중에 고침\n",
            "사용자 변경 보존"
        );
    }

    #[test]
    fn 사용자_폴더가_git_저장소여도_그_저장소를_건드리지_않는다() {
        let repo = crate::git::test_repo::init();
        let root = tempfile::tempdir().unwrap();
        let s = Shadow::new(root.path(), 2, repo.path());
        let head = crate::git::head_commit(repo.path()).unwrap();
        let before = s.snapshot("before").unwrap();
        std::fs::write(repo.path().join("a.txt"), "변경\n").unwrap();
        let after = s.snapshot("after").unwrap();
        assert_eq!(s.changed(&before, &after).unwrap(), vec!["a.txt"]);
        s.restore(&before, &after).unwrap();
        assert_eq!(
            crate::git::head_commit(repo.path()).unwrap(),
            head,
            "사용자 저장소 이력 그대로"
        );
        assert!(
            crate::git::dirty_files(repo.path()).unwrap().is_empty(),
            "되돌린 뒤 사용자 저장소 깨끗"
        );
    }
}
