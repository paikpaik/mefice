//! 대화 (스펙 v0.2 A단계 1번).
//!
//! 공식 Claude Code 대화 UI(CLI·VS Code 확장)와 같은 경험을 목표로 한다.
//! - 대화 = Task(mode `chat`) + Run 여러 개. 기본 상대는 페르소나 없는 어시스턴트 (터미널과 같은 프롬프트)
//! - 대화 Run은 턴이 끝나도 프로세스를 유지한다 (`keep_alive`). 오래 쉬면 닫히고, 다음 메시지는 세션을 이어서 새 Run
//! - 작업 공간 격리 없이 업무 폴더에서 바로 작업한다 (터미널과 같은 동작)

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::agent::{self, PermissionMode};
use crate::error::{AppError, AppResult};
use crate::pm;
use crate::project::Project;
use crate::runtime::{ImageAttachment, RunConfig, validate_images};
use crate::task::{self, Approval, Run, RunStatus, StoredEvent, Task};

pub const MODE_CHAT: &str = "chat";

/// 대화별 설정
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ChatOptions {
    /// 절약 모드: 도구를 제한해 비용을 줄인다 (능력도 줄어듦). 기본은 표준(사용자 설정 그대로)
    pub economy: bool,
    /// 수정 전 확인(default) / 자동 수정(acceptEdits) / 계획(plan)
    pub permission_mode: PermissionMode,
    /// 비우면 런타임 기본 모델
    pub model: Option<String>,
}

impl Default for ChatOptions {
    fn default() -> Self {
        Self {
            economy: false,
            permission_mode: PermissionMode::Default,
            model: None,
        }
    }
}

pub fn options_of(t: &Task) -> ChatOptions {
    t.spec
        .as_ref()
        .and_then(|s| s.get("chat"))
        .and_then(|c| serde_json::from_value(c.clone()).ok())
        .unwrap_or_default()
}

fn config(project: &Project, opts: &ChatOptions, conn: &Connection) -> AppResult<(i64, RunConfig)> {
    let a = agent::ensure_assistant(conn)?;
    let mut c = pm::run_config(project, &a);
    c.full_settings = !opts.economy;
    c.keep_alive = true;
    c.stream_partial = true;
    c.permission_mode = opts.permission_mode;
    c.model = opts
        .model
        .clone()
        .filter(|m| !m.trim().is_empty())
        .or(c.model);
    Ok((a.id, c))
}

/// 새 대화를 시작한다 (첫 메시지로 Run을 큐에 넣는다)
pub fn start(
    conn: &Connection,
    project: &Project,
    text: &str,
    opts: &ChatOptions,
    images: &[ImageAttachment],
) -> AppResult<(Task, Run)> {
    validate_images(images)?;
    if text.trim().is_empty() && images.is_empty() {
        return Err(AppError::InvalidInput("메시지를 입력하세요".into()));
    }
    let (agent_id, cfg) = config(project, opts, conn)?;
    let title_src = if text.trim().is_empty() {
        "이미지 대화"
    } else {
        text
    };
    let t = task::create_task(conn, project.id, agent_id, title_src)?;
    conn.execute(
        "UPDATE tasks SET mode = ?1, spec = ?2 WHERE id = ?3",
        rusqlite::params![MODE_CHAT, json!({"chat": opts}).to_string(), t.id],
    )?;
    let t = task::get_task(conn, t.id)?;
    let run = task::create_run_with_role(
        conn,
        task::NewRun {
            task_id: t.id,
            agent_id,
            prompt: text,
            resume_session_id: None,
            config: &cfg,
            cwd: &project.path,
        },
        "worker",
    )?;
    task::set_run_attachments(conn, run.id, images)?;
    Ok((t, run))
}

/// 메시지를 어디로 보낼지
pub enum Delivery {
    /// 살아 있는 Run에 메시지로 전달
    SendTo(i64),
    /// 새 Run을 큐에 넣었다 (pump 필요)
    Queued,
}

/// 살아 있는 대화 Run (큐 대기 중이면 None이 아니라 에러로 알린다)
pub fn live_run(conn: &Connection, task_id: i64) -> AppResult<Option<Run>> {
    match task::latest_run(conn, task_id)? {
        Some(r) if r.status == RunStatus::Queued => Err(AppError::InvalidInput(
            "앞 메시지가 아직 시작되지 않았습니다. 잠시 후 보내세요".into(),
        )),
        Some(r) if !r.status.is_terminal() => Ok(Some(r)),
        _ => Ok(None),
    }
}

/// 프로세스가 닫힌 대화를 같은 세션으로 이어서 새 Run을 만든다
pub fn resume_run(
    conn: &Connection,
    project: &Project,
    t: &Task,
    text: &str,
    images: &[ImageAttachment],
) -> AppResult<Run> {
    let (agent_id, cfg) = config(project, &options_of(t), conn)?;
    let session = task::latest_run(conn, t.id)?.and_then(|r| r.session_id);
    let run = task::create_run_with_role(
        conn,
        task::NewRun {
            task_id: t.id,
            agent_id,
            prompt: text,
            resume_session_id: session.as_deref(),
            config: &cfg,
            cwd: &project.path,
        },
        "worker",
    )?;
    task::set_run_attachments(conn, run.id, images)?;
    Ok(run)
}

pub fn deliver(
    conn: &Connection,
    project: &Project,
    task_id: i64,
    text: &str,
    images: &[ImageAttachment],
) -> AppResult<Delivery> {
    validate_images(images)?;
    if text.trim().is_empty() && images.is_empty() {
        return Err(AppError::InvalidInput("메시지를 입력하세요".into()));
    }
    let t = task::get_task(conn, task_id)?;
    if t.mode != MODE_CHAT {
        return Err(AppError::InvalidInput("대화가 아닙니다".into()));
    }
    match live_run(conn, task_id)? {
        Some(r) => Ok(Delivery::SendTo(r.id)),
        None => {
            resume_run(conn, project, &t, text, images)?;
            Ok(Delivery::Queued)
        }
    }
}

/// 대화 설정을 저장한다. 살아 있는 Run이 있으면 그 id를 돌려준다 (모드·모델 즉시 전환용).
/// 절약 모드 변경은 다음 Run부터 적용된다 (실행 중인 프로세스의 도구 구성은 바꿀 수 없음).
pub fn set_options(conn: &Connection, task_id: i64, opts: &ChatOptions) -> AppResult<Option<i64>> {
    conn.execute(
        "UPDATE tasks SET spec = ?1 WHERE id = ?2 AND mode = ?3",
        rusqlite::params![json!({"chat": opts}).to_string(), task_id, MODE_CHAT],
    )?;
    Ok(task::latest_run(conn, task_id)?
        .filter(|r| !r.status.is_terminal() && r.status != RunStatus::Queued)
        .map(|r| r.id))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSummary {
    pub id: i64,
    pub title: String,
    pub created_at: i64,
    pub status: Option<RunStatus>,
    /// 마지막 메시지 시각 (목록 정렬용)
    pub updated_at: i64,
}

pub fn list(conn: &Connection, project_id: i64) -> AppResult<Vec<ChatSummary>> {
    let mut stmt = conn.prepare(
        "SELECT t.id, t.title, t.created_at,
                (SELECT status FROM runs WHERE task_id = t.id ORDER BY id DESC LIMIT 1),
                COALESCE((SELECT MAX(e.created_at) FROM run_events e JOIN runs r ON r.id = e.run_id WHERE r.task_id = t.id), t.created_at)
         FROM tasks t WHERE t.project_id = ?1 AND t.mode = 'chat'
         ORDER BY 5 DESC LIMIT 200",
    )?;
    let rows = stmt.query_map([project_id], |r| {
        Ok(ChatSummary {
            id: r.get(0)?,
            title: r.get(1)?,
            created_at: r.get(2)?,
            status: r
                .get::<_, Option<String>>(3)?
                .map(|s| RunStatus::from_db(&s)),
            updated_at: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatDetail {
    pub id: i64,
    pub title: String,
    pub options: ChatOptions,
    /// 가장 최근 Run (상태·세션 표시)
    pub run: Option<Run>,
    /// 모든 Run의 이벤트를 시간순으로 (Run id, seq 순)
    pub events: Vec<StoredEvent>,
    /// 답을 기다리는 승인·질문·계획
    pub pending: Vec<Approval>,
}

pub fn detail(conn: &Connection, task_id: i64) -> AppResult<ChatDetail> {
    let t = task::get_task(conn, task_id)?;
    if t.mode != MODE_CHAT {
        return Err(AppError::InvalidInput("대화가 아닙니다".into()));
    }
    let runs = task::list_runs(conn, task_id)?;
    let mut events = Vec::new();
    for r in &runs {
        events.extend(task::list_events(conn, r.id)?);
    }
    let run = runs.last().cloned();
    let pending = match &run {
        Some(r) => task::pending_approvals(conn, r.id)?,
        None => vec![],
    };
    Ok(ChatDetail {
        id: t.id,
        title: t.title.clone(),
        options: options_of(&t),
        run,
        events,
        pending,
    })
}

// ---------- @파일 자동완성 ----------

const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    ".next",
    ".venv",
    "venv",
    "__pycache__",
    ".idea",
];
const MAX_WALK: usize = 20_000;

/// 업무 폴더의 파일 목록 (상대 경로). Git 저장소면 .gitignore를 따른다.
pub fn project_files(root: &std::path::Path) -> Vec<String> {
    if let Ok(out) = crate::git::run(
        root,
        &["ls-files", "--cached", "--others", "--exclude-standard"],
    ) {
        return out.lines().map(str::to_string).collect();
    }
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let path = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            match e.file_type() {
                Ok(ft) if ft.is_dir() => {
                    if !SKIP_DIRS.contains(&name.as_str()) {
                        stack.push(path);
                    }
                }
                Ok(ft) if ft.is_file() => {
                    if let Ok(rel) = path.strip_prefix(root) {
                        out.push(rel.to_string_lossy().to_string());
                    }
                }
                _ => {}
            }
            if out.len() >= MAX_WALK {
                return out;
            }
        }
    }
    out
}

/// 검색어로 고른다: 파일 이름이 검색어로 시작 > 파일 이름에 포함 > 경로에 포함, 짧은 경로 우선
pub fn search_files(files: &[String], query: &str, limit: usize) -> Vec<String> {
    let q = query.to_lowercase();
    let mut scored: Vec<(u8, usize, &String)> = files
        .iter()
        .filter_map(|f| {
            let lower = f.to_lowercase();
            let name = lower.rsplit('/').next().unwrap_or(&lower);
            let rank = if q.is_empty() || name.starts_with(&q) {
                0
            } else if name.contains(&q) {
                1
            } else if lower.contains(&q) {
                2
            } else {
                return None;
            };
            Some((rank, f.len(), f))
        })
        .collect();
    scored.sort();
    scored
        .into_iter()
        .take(limit)
        .map(|(_, _, f)| f.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    fn project(conn: &Connection) -> (Project, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let p = crate::project::upsert(conn, dir.path()).unwrap();
        (p, dir)
    }

    #[test]
    fn 대화_설정은_표준_모드_프로세스_유지_스트리밍() {
        let db = Db::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let (p, _d) = project(&conn);
        let opts = ChatOptions {
            permission_mode: PermissionMode::Plan,
            model: Some("haiku".into()),
            ..Default::default()
        };
        let (t, r) = start(&conn, &p, "안녕\n두 번째 줄", &opts, &[]).unwrap();
        assert_eq!(t.mode, MODE_CHAT);
        assert_eq!(t.title, "안녕");
        assert_eq!(options_of(&t), opts);
        assert!(r.config.full_settings && r.config.keep_alive && r.config.stream_partial);
        assert_eq!(r.config.permission_mode, PermissionMode::Plan);
        assert_eq!(r.config.model.as_deref(), Some("haiku"));
        assert!(
            r.config.system_append.trim().is_empty(),
            "어시스턴트는 페르소나 없음 (터미널과 같은 프롬프트)"
        );
        assert_eq!(t.isolation, task::ISOLATION_NONE);
        // 어시스턴트는 위임 대상이 아니다
        assert!(agent::list_workers(&conn).unwrap().is_empty());
    }

    #[test]
    fn 보내기_살아있으면_전달_닫혔으면_세션_이어서_새_run() {
        let db = Db::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let (p, _d) = project(&conn);
        let img = ImageAttachment {
            media_type: "image/png".into(),
            data: "AAAA".into(),
        };
        let (t, r1) = start(
            &conn,
            &p,
            "",
            &ChatOptions::default(),
            std::slice::from_ref(&img),
        )
        .unwrap();
        assert_eq!(t.title, "이미지 대화", "이미지만 보내도 대화 시작");
        assert_eq!(
            task::take_run_attachments(&conn, r1.id).unwrap(),
            vec![img.clone()]
        );
        assert!(
            task::take_run_attachments(&conn, r1.id).unwrap().is_empty(),
            "꺼낸 뒤에는 비움"
        );
        assert!(
            deliver(&conn, &p, t.id, "x", &[]).is_err(),
            "큐 대기 중에는 거부"
        );
        task::mark_run_started(&conn, r1.id, "sess-1").unwrap();
        task::set_run_status(&conn, r1.id, RunStatus::WaitingForInput).unwrap();
        assert!(
            matches!(deliver(&conn, &p, t.id, "두 번째", &[]).unwrap(), Delivery::SendTo(id) if id == r1.id)
        );
        task::finish_run(&conn, r1.id, RunStatus::Succeeded, Some(0), None).unwrap();
        assert!(matches!(
            deliver(&conn, &p, t.id, "세 번째", std::slice::from_ref(&img)).unwrap(),
            Delivery::Queued
        ));
        let r2 = task::latest_run(&conn, t.id).unwrap().unwrap();
        assert_eq!(r2.resume_session_id.as_deref(), Some("sess-1"));
        assert_eq!(r2.prompt, "세 번째");
        assert_eq!(task::take_run_attachments(&conn, r2.id).unwrap().len(), 1);
        // 설정 변경은 저장되고, 다음 Run에 반영
        let econ = ChatOptions {
            economy: true,
            ..Default::default()
        };
        assert_eq!(
            set_options(&conn, t.id, &econ).unwrap(),
            None,
            "큐 대기 Run은 즉시 전환 대상 아님"
        );
        assert!(options_of(&task::get_task(&conn, t.id).unwrap()).economy);
        let d = detail(&conn, t.id).unwrap();
        assert_eq!(d.run.unwrap().id, r2.id);
        assert_eq!(list(&conn, p.id).unwrap().len(), 1);
    }

    #[test]
    fn 파일_목록은_gitignore와_무거운_폴더를_건너뛴다() {
        let repo = crate::git::test_repo::init();
        std::fs::write(repo.path().join(".gitignore"), "secret.env\n").unwrap();
        std::fs::write(repo.path().join("secret.env"), "x").unwrap();
        std::fs::create_dir_all(repo.path().join("docs")).unwrap();
        std::fs::write(repo.path().join("docs/report.md"), "x").unwrap();
        let files = project_files(repo.path());
        assert!(
            files.contains(&"docs/report.md".to_string()),
            "미추적 새 파일 포함"
        );
        assert!(!files.iter().any(|f| f == "secret.env"), ".gitignore 반영");

        let plain = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(plain.path().join("node_modules/x")).unwrap();
        std::fs::write(plain.path().join("node_modules/x/a.js"), "x").unwrap();
        std::fs::write(plain.path().join("회의록.md"), "x").unwrap();
        assert_eq!(project_files(plain.path()), vec!["회의록.md".to_string()]);

        let list: Vec<String> = [
            "src/report.ts",
            "docs/report.md",
            "README.md",
            "a/b/reporter.rs",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            search_files(&list, "rep", 10),
            // 둘 다 이름이 검색어로 시작 → 짧은 경로 우선, 경로에만 포함된 것은 뒤
            vec!["src/report.ts", "docs/report.md", "a/b/reporter.rs"]
        );
        assert_eq!(search_files(&list, "", 2).len(), 2);
    }

    #[test]
    fn 입력_대기_대화는_동시_실행_상한에_세지_않는다() {
        let db = Db::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let (p, _d) = project(&conn);
        let (_, r) = start(&conn, &p, "x", &ChatOptions::default(), &[]).unwrap();
        task::mark_run_started(&conn, r.id, "s").unwrap();
        assert_eq!(task::count_active_runs(&conn).unwrap(), 1);
        task::set_run_status(&conn, r.id, RunStatus::WaitingForInput).unwrap();
        assert_eq!(task::count_active_runs(&conn).unwrap(), 0);
    }
}
