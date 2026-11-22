//! Task(목표) / Run(실행 시도) / RunEvent / Approval 저장소 (스펙 8·9장).

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::db::now_ms;
use crate::error::{AppError, AppResult};
use crate::runtime::{RunConfig, RunEvent};

/// 스펙 9장 실행 상태
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Queued,
    Running,
    WaitingForInput,
    WaitingForApproval,
    Cancelling,
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
}

impl RunStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::WaitingForInput => "waiting_for_input",
            Self::WaitingForApproval => "waiting_for_approval",
            Self::Cancelling => "cancelling",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }

    fn from_db(s: &str) -> Self {
        match s {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "waiting_for_input" => Self::WaitingForInput,
            "waiting_for_approval" => Self::WaitingForApproval,
            "cancelling" => Self::Cancelling,
            "succeeded" => Self::Succeeded,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => Self::Interrupted,
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::Interrupted
        )
    }
}

/// 프로세스가 살아 있어야 하는 상태 (동시 실행 상한 계산 대상)
const ACTIVE_STATUSES: &str = "'running','waiting_for_input','waiting_for_approval','cancelling'";
const NON_TERMINAL_STATUSES: &str =
    "'queued','running','waiting_for_input','waiting_for_approval','cancelling'";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: i64,
    pub project_id: i64,
    pub agent_id: i64,
    /// PM 목표(mode=pm)에서 위임된 작업이면 그 목표 Task id
    pub parent_id: Option<i64>,
    /// direct(직접 실행) | pm(PM 목표) | delegated(PM이 위임)
    pub mode: String,
    /// 위임 명세 (목표·범위·입력·완료 조건)
    pub spec: Option<Value>,
    pub title: String,
    pub prompt: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    pub id: i64,
    pub task_id: i64,
    pub agent_id: i64,
    pub attempt: i64,
    /// 사용량 구분: pm | worker | review
    pub role: String,
    /// subscription | api_key (런타임 시작 후 채워짐)
    pub auth_kind: Option<String>,
    pub status: RunStatus,
    pub prompt: String,
    pub resume_session_id: Option<String>,
    pub session_id: Option<String>,
    pub config: RunConfig,
    pub cwd: String,
    pub exit_code: Option<i64>,
    pub error: Option<String>,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskSummary {
    #[serde(flatten)]
    pub task: Task,
    pub agent_name: String,
    pub latest_run: Option<Run>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredEvent {
    pub id: i64,
    pub run_id: i64,
    pub seq: i64,
    pub created_at: i64,
    #[serde(flatten)]
    pub event: RunEvent,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Approval {
    pub id: i64,
    pub run_id: i64,
    pub request_id: String,
    pub tool_name: String,
    pub tool_input: Value,
    pub description: Option<String>,
    pub status: String,
    pub created_at: i64,
    pub decided_at: Option<i64>,
}

// ---------- Task ----------

fn task_from_row(r: &Row) -> rusqlite::Result<Task> {
    Ok(Task {
        id: r.get("id")?,
        project_id: r.get("project_id")?,
        agent_id: r.get("agent_id")?,
        parent_id: r.get("parent_id")?,
        mode: r.get("mode")?,
        spec: r
            .get::<_, Option<String>>("spec")?
            .and_then(|s| serde_json::from_str(&s).ok()),
        title: r.get("title")?,
        prompt: r.get("prompt")?,
        created_at: r.get("created_at")?,
    })
}

/// 작업 제목은 지시의 첫 줄을 잘라 쓴다 (제목 생성을 위해 모델을 호출하지 않음).
pub fn title_from_prompt(prompt: &str) -> String {
    let first = prompt
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    crate::runtime::truncate_chars(first, 60).0
}

pub fn create_task(
    conn: &Connection,
    project_id: i64,
    agent_id: i64,
    prompt: &str,
) -> AppResult<Task> {
    if prompt.trim().is_empty() {
        return Err(AppError::InvalidInput("작업 내용을 입력하세요".into()));
    }
    insert_task(
        conn,
        project_id,
        agent_id,
        None,
        "direct",
        &title_from_prompt(prompt),
        prompt,
        None,
    )
}

pub const MODE_PM: &str = "pm";
pub const MODE_DELEGATED: &str = "delegated";

/// PM 목표 Task (대화 스레드 단위)
pub fn create_goal(
    conn: &Connection,
    project_id: i64,
    pm_agent_id: i64,
    text: &str,
) -> AppResult<Task> {
    if text.trim().is_empty() {
        return Err(AppError::InvalidInput("메시지를 입력하세요".into()));
    }
    insert_task(
        conn,
        project_id,
        pm_agent_id,
        None,
        MODE_PM,
        &title_from_prompt(text),
        text,
        None,
    )
}

/// PM이 위임한 작업
pub fn create_delegated(
    conn: &Connection,
    goal: &Task,
    agent_id: i64,
    title: &str,
    prompt: &str,
    spec: &Value,
) -> AppResult<Task> {
    let title = if title.trim().is_empty() {
        title_from_prompt(prompt)
    } else {
        crate::runtime::truncate_chars(title.trim(), 60).0
    };
    insert_task(
        conn,
        goal.project_id,
        agent_id,
        Some(goal.id),
        MODE_DELEGATED,
        &title,
        prompt,
        Some(spec),
    )
}

#[allow(clippy::too_many_arguments)]
fn insert_task(
    conn: &Connection,
    project_id: i64,
    agent_id: i64,
    parent_id: Option<i64>,
    mode: &str,
    title: &str,
    prompt: &str,
    spec: Option<&Value>,
) -> AppResult<Task> {
    conn.execute(
        "INSERT INTO tasks (project_id, agent_id, parent_id, mode, title, prompt, spec, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            project_id,
            agent_id,
            parent_id,
            mode,
            title,
            prompt,
            spec.map(|v| v.to_string()),
            now_ms()
        ],
    )?;
    get_task(conn, conn.last_insert_rowid())
}

pub fn get_task(conn: &Connection, id: i64) -> AppResult<Task> {
    conn.query_row("SELECT * FROM tasks WHERE id = ?1", [id], task_from_row)
        .optional()?
        .ok_or_else(|| AppError::NotFound(format!("작업 #{id}")))
}

pub fn list_tasks(conn: &Connection, project_id: i64) -> AppResult<Vec<TaskSummary>> {
    let mut stmt = conn.prepare(
        "SELECT t.*, a.name AS agent_name FROM tasks t JOIN agents a ON a.id = t.agent_id
         WHERE t.project_id = ?1 ORDER BY t.id DESC LIMIT 200",
    )?;
    let rows: Vec<(Task, String)> = stmt
        .query_map([project_id], |r| {
            Ok((task_from_row(r)?, r.get("agent_name")?))
        })?
        .collect::<Result<_, _>>()?;
    rows.into_iter()
        .map(|(task, agent_name)| {
            let latest_run = latest_run(conn, task.id)?;
            Ok(TaskSummary {
                task,
                agent_name,
                latest_run,
            })
        })
        .collect()
}

// ---------- Run ----------

fn run_from_row(r: &Row) -> rusqlite::Result<Run> {
    let status: String = r.get("status")?;
    let config: String = r.get("config_snapshot")?;
    Ok(Run {
        id: r.get("id")?,
        task_id: r.get("task_id")?,
        agent_id: r.get("agent_id")?,
        attempt: r.get("attempt")?,
        role: r.get("role")?,
        auth_kind: r.get("auth_kind")?,
        status: RunStatus::from_db(&status),
        prompt: r.get("prompt")?,
        resume_session_id: r.get("resume_session_id")?,
        session_id: r.get("session_id")?,
        config: serde_json::from_str(&config).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        cwd: r.get("cwd")?,
        exit_code: r.get("exit_code")?,
        error: r.get("error")?,
        created_at: r.get("created_at")?,
        started_at: r.get("started_at")?,
        ended_at: r.get("ended_at")?,
    })
}

pub struct NewRun<'a> {
    pub task_id: i64,
    pub agent_id: i64,
    pub prompt: &'a str,
    pub resume_session_id: Option<&'a str>,
    pub config: &'a RunConfig,
    pub cwd: &'a str,
}

/// 새 실행 시도를 queued 상태로 만든다. 재실행·이어서 실행도 항상 새 Run이다 (스펙 8장).
pub fn create_run(conn: &Connection, n: NewRun) -> AppResult<Run> {
    create_run_with_role(conn, n, "worker")
}

/// 사용량 구분 역할(pm | worker | review)을 지정해 Run을 만든다.
pub fn create_run_with_role(conn: &Connection, n: NewRun, role: &str) -> AppResult<Run> {
    let attempt: i64 = conn.query_row(
        "SELECT COALESCE(MAX(attempt), 0) + 1 FROM runs WHERE task_id = ?1",
        [n.task_id],
        |r| r.get(0),
    )?;
    conn.execute(
        "INSERT INTO runs (task_id, agent_id, attempt, status, prompt, resume_session_id, config_snapshot, cwd, created_at, role)
         VALUES (?1, ?2, ?3, 'queued', ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            n.task_id,
            n.agent_id,
            attempt,
            n.prompt,
            n.resume_session_id,
            serde_json::to_string(n.config)?,
            n.cwd,
            now_ms(),
            role
        ],
    )?;
    get_run(conn, conn.last_insert_rowid())
}

pub fn get_run(conn: &Connection, id: i64) -> AppResult<Run> {
    conn.query_row("SELECT * FROM runs WHERE id = ?1", [id], run_from_row)
        .optional()?
        .ok_or_else(|| AppError::NotFound(format!("실행 #{id}")))
}

pub fn latest_run(conn: &Connection, task_id: i64) -> AppResult<Option<Run>> {
    Ok(conn
        .query_row(
            "SELECT * FROM runs WHERE task_id = ?1 ORDER BY id DESC LIMIT 1",
            [task_id],
            run_from_row,
        )
        .optional()?)
}

/// 목표에서 위임된 작업들
pub fn list_children(conn: &Connection, parent_id: i64) -> AppResult<Vec<Task>> {
    let mut stmt = conn.prepare("SELECT * FROM tasks WHERE parent_id = ?1 ORDER BY id")?;
    let rows = stmt.query_map([parent_id], task_from_row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// 실행의 마지막 턴 결과 텍스트 (작업자 보고 취합용)
pub fn last_result_text(conn: &Connection, run_id: i64) -> AppResult<Option<String>> {
    Ok(list_events(conn, run_id)?
        .into_iter()
        .rev()
        .find_map(|e| match e.event {
            RunEvent::TurnResult { result, .. } => result,
            _ => None,
        }))
}

/// 도구 기록에서 수정한 파일 경로를 뽑는다 (모델에게 묻지 않고 로컬에서 계산).
pub fn changed_files(conn: &Connection, run_id: i64) -> AppResult<Vec<String>> {
    let mut files: Vec<String> = Vec::new();
    for e in list_events(conn, run_id)? {
        if let RunEvent::ToolUse { name, summary, .. } = e.event
            && (name == "Edit" || name == "Write")
            && !files.contains(&summary)
        {
            files.push(summary);
        }
    }
    Ok(files)
}

pub fn list_runs(conn: &Connection, task_id: i64) -> AppResult<Vec<Run>> {
    let mut stmt = conn.prepare("SELECT * FROM runs WHERE task_id = ?1 ORDER BY id")?;
    let rows = stmt.query_map([task_id], run_from_row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn count_active_runs(conn: &Connection) -> AppResult<i64> {
    Ok(conn.query_row(
        &format!("SELECT COUNT(*) FROM runs WHERE status IN ({ACTIVE_STATUSES})"),
        [],
        |r| r.get(0),
    )?)
}

pub fn queued_run_ids(conn: &Connection, limit: i64) -> AppResult<Vec<i64>> {
    let mut stmt =
        conn.prepare("SELECT id FROM runs WHERE status = 'queued' ORDER BY id LIMIT ?1")?;
    let rows = stmt.query_map([limit], |r| r.get(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn set_run_status(conn: &Connection, id: i64, status: RunStatus) -> AppResult<()> {
    conn.execute(
        "UPDATE runs SET status = ?1 WHERE id = ?2",
        params![status.as_str(), id],
    )?;
    Ok(())
}

pub fn mark_run_started(conn: &Connection, id: i64, session_id: &str) -> AppResult<()> {
    conn.execute(
        "UPDATE runs SET status = 'running', session_id = ?1, started_at = ?2 WHERE id = ?3",
        params![session_id, now_ms(), id],
    )?;
    Ok(())
}

pub fn set_run_pid(conn: &Connection, id: i64, pid: Option<u32>) -> AppResult<()> {
    conn.execute("UPDATE runs SET pid = ?1 WHERE id = ?2", params![pid, id])?;
    Ok(())
}

pub fn set_run_auth(conn: &Connection, id: i64, auth_kind: &str) -> AppResult<()> {
    conn.execute(
        "UPDATE runs SET auth_kind = ?1 WHERE id = ?2",
        params![auth_kind, id],
    )?;
    Ok(())
}

pub fn set_run_session(conn: &Connection, id: i64, session_id: &str) -> AppResult<()> {
    conn.execute(
        "UPDATE runs SET session_id = ?1 WHERE id = ?2",
        params![session_id, id],
    )?;
    Ok(())
}

pub fn finish_run(
    conn: &Connection,
    id: i64,
    status: RunStatus,
    exit_code: Option<i32>,
    error: Option<&str>,
) -> AppResult<()> {
    conn.execute(
        "UPDATE runs SET status = ?1, exit_code = ?2, error = ?3, ended_at = ?4, pid = NULL WHERE id = ?5",
        params![status.as_str(), exit_code, error, now_ms(), id],
    )?;
    conn.execute(
        "UPDATE approvals SET status = 'expired', decided_at = ?1 WHERE run_id = ?2 AND status = 'pending'",
        params![now_ms(), id],
    )?;
    Ok(())
}

/// 앱 시작 시: 이전 프로세스는 앱과 함께 종료되었으므로 끝나지 않은 Run을 interrupted로 표시한다.
/// 확인되지 않는 실행을 진행 중으로 표시하지 않고, 자동 재실행도 하지 않는다 (스펙 4.4, 9장).
pub fn interrupt_unfinished_runs(conn: &Connection) -> AppResult<usize> {
    let now = now_ms();
    let n = conn.execute(
        &format!(
            "UPDATE runs SET status = 'interrupted', ended_at = ?1, pid = NULL,
                error = COALESCE(error, '앱이 종료되어 실행 상태를 확인할 수 없습니다')
             WHERE status IN ({NON_TERMINAL_STATUSES})"
        ),
        [now],
    )?;
    conn.execute(
        "UPDATE approvals SET status = 'expired', decided_at = ?1 WHERE status = 'pending'",
        [now],
    )?;
    Ok(n)
}

// ---------- RunEvent ----------

/// 이벤트를 다음 순번으로 저장한다. 연결 Mutex 안에서 실행되므로 순번이 겹치지 않는다.
pub fn append_event(conn: &Connection, run_id: i64, event: &RunEvent) -> AppResult<StoredEvent> {
    let now = now_ms();
    let seq: i64 = conn.query_row(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM run_events WHERE run_id = ?1",
        [run_id],
        |r| r.get(0),
    )?;
    conn.execute(
        "INSERT INTO run_events (run_id, seq, kind, payload, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![run_id, seq, event.kind(), serde_json::to_string(event)?, now],
    )?;
    Ok(StoredEvent {
        id: conn.last_insert_rowid(),
        run_id,
        seq,
        created_at: now,
        event: event.clone(),
    })
}

pub fn list_events(conn: &Connection, run_id: i64) -> AppResult<Vec<StoredEvent>> {
    let mut stmt = conn.prepare(
        "SELECT id, run_id, seq, payload, created_at FROM run_events WHERE run_id = ?1 ORDER BY seq",
    )?;
    let rows = stmt.query_map([run_id], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, i64>(4)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, run_id, seq, payload, created_at) = row?;
        // 알 수 없는 이벤트(향후 버전에서 추가된 kind 등)는 건너뛴다
        if let Ok(event) = serde_json::from_str::<RunEvent>(&payload) {
            out.push(StoredEvent {
                id,
                run_id,
                seq,
                created_at,
                event,
            });
        }
    }
    Ok(out)
}

// ---------- Approval ----------

fn approval_from_row(r: &Row) -> rusqlite::Result<Approval> {
    let input: String = r.get("tool_input")?;
    Ok(Approval {
        id: r.get("id")?,
        run_id: r.get("run_id")?,
        request_id: r.get("request_id")?,
        tool_name: r.get("tool_name")?,
        tool_input: serde_json::from_str(&input).unwrap_or(Value::Null),
        description: r.get("description")?,
        status: r.get("status")?,
        created_at: r.get("created_at")?,
        decided_at: r.get("decided_at")?,
    })
}

pub fn create_approval(
    conn: &Connection,
    run_id: i64,
    request_id: &str,
    tool_name: &str,
    input: &Value,
    description: Option<&str>,
) -> AppResult<Approval> {
    conn.execute(
        "INSERT INTO approvals (run_id, request_id, tool_name, tool_input, description, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            run_id,
            request_id,
            tool_name,
            input.to_string(),
            description,
            now_ms()
        ],
    )?;
    get_approval(conn, conn.last_insert_rowid())
}

pub fn get_approval(conn: &Connection, id: i64) -> AppResult<Approval> {
    conn.query_row(
        "SELECT * FROM approvals WHERE id = ?1",
        [id],
        approval_from_row,
    )
    .optional()?
    .ok_or_else(|| AppError::NotFound(format!("승인 요청 #{id}")))
}

pub fn pending_approvals(conn: &Connection, run_id: i64) -> AppResult<Vec<Approval>> {
    let mut stmt = conn
        .prepare("SELECT * FROM approvals WHERE run_id = ?1 AND status = 'pending' ORDER BY id")?;
    let rows = stmt.query_map([run_id], approval_from_row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// pending인 승인만 결정한다. 이미 결정·만료된 요청이면 false.
pub fn decide_approval(conn: &Connection, id: i64, allowed: bool) -> AppResult<bool> {
    let n = conn.execute(
        "UPDATE approvals SET status = ?1, decided_at = ?2 WHERE id = ?3 AND status = 'pending'",
        params![if allowed { "allowed" } else { "denied" }, now_ms(), id],
    )?;
    Ok(n == 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{self, AgentInput, PermissionMode};
    use crate::db::Db;

    fn setup(conn: &Connection) -> (i64, i64, RunConfig) {
        let dir = tempfile::tempdir().unwrap();
        let p = crate::project::upsert(conn, dir.path()).unwrap();
        let a = agent::create(
            conn,
            &AgentInput {
                name: "a".into(),
                role: "r".into(),
                instructions: String::new(),
                model: None,
                effort: None,
                permission_mode: PermissionMode::Default,
                tools: agent::default_tools(),
                color: None,
            },
        )
        .unwrap();
        let cfg = RunConfig {
            agent_name: "a".into(),
            agent_role: "r".into(),
            model: None,
            effort: None,
            permission_mode: PermissionMode::Default,
            tools: vec!["Read".into()],
            system_append: String::new(),
            json_schema: None,
        };
        (p.id, a.id, cfg)
    }

    #[test]
    fn 재실행은_새_run과_attempt_증가() {
        let db = Db::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let (pid, aid, cfg) = setup(&conn);
        let t = create_task(&conn, pid, aid, "첫 줄 제목\n본문").unwrap();
        assert_eq!(t.title, "첫 줄 제목");
        let new = |p| NewRun {
            task_id: t.id,
            agent_id: aid,
            prompt: p,
            resume_session_id: None,
            config: &cfg,
            cwd: "/tmp",
        };
        let r1 = create_run(&conn, new("a")).unwrap();
        let r2 = create_run(&conn, new("b")).unwrap();
        assert_eq!((r1.attempt, r2.attempt), (1, 2));
        assert_eq!(latest_run(&conn, t.id).unwrap().unwrap().id, r2.id);
        assert_eq!(queued_run_ids(&conn, 10).unwrap(), vec![r1.id, r2.id]);
    }

    #[test]
    fn 이벤트_순번과_재시작_복구() {
        let db = Db::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let (pid, aid, cfg) = setup(&conn);
        let t = create_task(&conn, pid, aid, "x").unwrap();
        let r = create_run(
            &conn,
            NewRun {
                task_id: t.id,
                agent_id: aid,
                prompt: "x",
                resume_session_id: None,
                config: &cfg,
                cwd: "/tmp",
            },
        )
        .unwrap();
        mark_run_started(&conn, r.id, "s1").unwrap();
        let e1 = append_event(&conn, r.id, &RunEvent::Text { text: "a".into() }).unwrap();
        let e2 = append_event(&conn, r.id, &RunEvent::Text { text: "b".into() }).unwrap();
        assert_eq!((e1.seq, e2.seq), (1, 2));

        let ap =
            create_approval(&conn, r.id, "req", "Write", &serde_json::json!({}), None).unwrap();
        assert_eq!(count_active_runs(&conn).unwrap(), 1);

        // 앱 재시작 시뮬레이션
        assert_eq!(interrupt_unfinished_runs(&conn).unwrap(), 1);
        assert_eq!(get_run(&conn, r.id).unwrap().status, RunStatus::Interrupted);
        assert_eq!(get_approval(&conn, ap.id).unwrap().status, "expired");
        assert!(
            !decide_approval(&conn, ap.id, true).unwrap(),
            "만료된 승인은 결정 불가"
        );
        assert_eq!(list_events(&conn, r.id).unwrap().len(), 2);
    }
}
