//! PM: 실행 방식 판단 · 위임 · 결과 취합 (스펙 4.2, 5장).
//!
//! 모델 호출이 생기는 시점은 두 가지뿐이다.
//! 1) 사용자가 PM에게 메시지를 보낼 때
//! 2) PM이 위임한 작업이 끝났을 때 (완료·실패 이벤트)
//!
//! 진행 상황 확인이나 대기를 위한 PM 호출은 없다. 상태 변화는 실행 관리자가 로컬 이벤트로 처리한다.
//! PM에게는 전체 대화 대신 목표 단위 세션(이어서 실행)과 팀 요약, 작업자의 구조화된 보고만 전달한다.

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::agent::{self, Agent};
use crate::db::now_ms;
use crate::error::{AppError, AppResult};
use crate::project::Project;
use crate::runtime::{RunConfig, truncate_chars};
use crate::task::{self, Run, RunStatus, Task};

/// 작업자 최종 보고를 PM에게 넘길 때의 최대 길이 (긴 로그 재전달 방지)
const REPORT_MAX_CHARS: usize = 3000;

/// PM 응답 스키마. 런타임이 `--json-schema`로 검증한다.
/// 4단계는 단일 위임만 허용(tasks 최대 1개). 병렬 분할은 5단계(worktree 격리)에서 연다.
pub const PM_SCHEMA: &str = r#"{
  "type": "object",
  "properties": {
    "mode": {"type": "string", "enum": ["direct", "delegate", "ask", "hire", "report"]},
    "reply": {"type": "string", "description": "사용자에게 보여줄 짧은 한국어 메시지 (5줄 이내)"},
    "tasks": {
      "type": "array",
      "maxItems": 1,
      "items": {
        "type": "object",
        "properties": {
          "agentId": {"type": "integer"},
          "title": {"type": "string"},
          "goal": {"type": "string"},
          "scope": {"type": "string"},
          "inputs": {"type": "string"},
          "doneCriteria": {"type": "string"}
        },
        "required": ["agentId", "title", "goal", "scope", "doneCriteria"]
      }
    },
    "hire": {
      "type": "object",
      "properties": {
        "role": {"type": "string"},
        "reason": {"type": "string"},
        "tools": {"type": "array", "items": {"type": "string"}}
      },
      "required": ["role", "reason"]
    }
  },
  "required": ["mode", "reply"]
}"#;

const PM_SYSTEM: &str = "당신은 Mefice 개발팀의 PM입니다. 사용자의 목표를 이해하고, 가장 적은 비용으로 완료되도록 실행 방식을 정합니다.

## 실행 방식 (mode)
- direct: 질문 답변, 짧은 조사, 몇 줄 수정처럼 작은 일은 당신이 지금 직접 처리하고 결과를 reply에 적습니다. 위임보다 먼저 고려합니다.
- delegate: 팀원 한 명에게 맡깁니다. tasks에 정확히 1개를 넣습니다. 병렬 분할은 아직 지원하지 않습니다.
- ask: 목표나 완료 조건이 불분명해 진행할 수 없을 때만 질문합니다.
- hire: 맡길 만한 팀원이 없을 때 고용할 역할을 hire에 제안합니다. 고용은 사용자가 확정합니다.
- report: 팀원 작업 결과를 받았을 때 결과와 검증 내용을 사용자에게 요약합니다. 실패했다면 원인과 다음 조치를 제안하고, 다시 맡기려면 delegate를 씁니다.

## 규칙
- 탐색은 실행 방식을 정하는 데 필요한 만큼만 합니다. 구현을 위한 세부 탐색은 담당자에게 맡깁니다.
- 위임할 때 담당자는 이 대화를 볼 수 없습니다. goal(목표), scope(수정해도 되는 폴더·파일), inputs(필요한 맥락), doneCriteria(검증 가능한 완료 조건)를 구체적으로 씁니다.
- 팀원 목록에 있는 agentId만 사용합니다.
- reply는 짧게 씁니다. 결과를 반복하거나 계획을 길게 설명하지 않습니다.";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DelegateSpec {
    pub agent_id: i64,
    #[serde(default)]
    pub title: String,
    pub goal: String,
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub inputs: String,
    #[serde(default)]
    pub done_criteria: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HireSuggestion {
    pub role: String,
    pub reason: String,
    #[serde(default)]
    pub tools: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Decision {
    pub mode: String,
    pub reply: String,
    #[serde(default)]
    pub tasks: Vec<DelegateSpec>,
    #[serde(default)]
    pub hire: Option<HireSuggestion>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: i64,
    pub project_id: i64,
    pub goal_task_id: i64,
    /// user | pm | system
    pub author: String,
    pub text: String,
    pub payload: Option<Value>,
    pub run_id: Option<i64>,
    pub created_at: i64,
}

// ---------- 실행 설정 ----------

/// Run 생성 시점의 설정 스냅샷. 이후 에이전트·지침 변경은 다음 실행부터 반영된다 (스펙 4.3).
pub fn run_config(project: &Project, agent: &Agent) -> RunConfig {
    let mut sys = if agent.is_pm() {
        PM_SYSTEM.to_string()
    } else {
        format!(
            "당신은 Mefice 개발팀의 '{}' 역할 에이전트 '{}'입니다.",
            agent.role, agent.name
        )
    };
    if !agent.instructions.trim().is_empty() {
        sys.push_str("\n\n## 역할 지침\n");
        sys.push_str(agent.instructions.trim());
    }
    if !project.instructions.trim().is_empty() {
        sys.push_str("\n\n## 프로젝트 지침\n");
        sys.push_str(project.instructions.trim());
    }
    RunConfig {
        agent_name: agent.name.clone(),
        agent_role: agent.role.clone(),
        model: agent.model.clone(),
        effort: agent.effort.clone(),
        permission_mode: agent.permission_mode,
        tools: agent.tools.clone(),
        system_append: sys,
        json_schema: agent.is_pm().then(|| PM_SCHEMA.to_string()),
    }
}

// ---------- 프롬프트 구성 (모두 로컬 문자열 조합, 모델 호출 없음) ----------

fn permission_label(a: &Agent) -> &'static str {
    match a.permission_mode {
        agent::PermissionMode::Default => "수정 시 승인",
        agent::PermissionMode::AcceptEdits => "편집 자동",
        agent::PermissionMode::Plan => "읽기 전용",
    }
}

/// 팀 요약 (한 줄에 한 명). 팀이 바뀔 수 있으므로 PM 메시지마다 붙인다.
pub fn team_summary(conn: &Connection) -> AppResult<String> {
    let workers = agent::list_workers(conn)?;
    if workers.is_empty() {
        return Ok("(팀원 없음 — 직접 처리하거나 hire로 고용을 제안하세요)".into());
    }
    Ok(workers
        .iter()
        .map(|a| {
            let extra = if a.instructions.trim().is_empty() {
                String::new()
            } else {
                format!(
                    " · {}",
                    truncate_chars(a.instructions.trim(), 80)
                        .0
                        .replace('\n', " ")
                )
            };
            format!(
                "- agentId {}: {} ({}) · 도구 {} · {}{}",
                a.id,
                a.name,
                a.role,
                a.tools.join(","),
                permission_label(a),
                extra
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

pub fn compose_pm_prompt(team: &str, heading: &str, body: &str) -> String {
    format!("## 팀원\n{team}\n\n## {heading}\n{body}")
}

pub fn compose_worker_prompt(spec: &DelegateSpec) -> String {
    let or_none = |s: &str| {
        if s.trim().is_empty() {
            "(지정 없음)".to_string()
        } else {
            s.trim().to_string()
        }
    };
    format!(
        "# 작업 지시 (PM 배정)\n\n## 목표\n{}\n\n## 범위\n{}\n\n## 입력·맥락\n{}\n\n## 완료 조건\n{}\n\n\
         범위 밖 파일은 수정하지 마세요. 진행할 수 없으면 추측하지 말고 이유를 보고하세요.\n\
         끝나면 마지막 메시지를 다음 형식으로 끝내세요.\n\n\
         ## 작업 보고\n- 결과: 완료 | 부분 완료 | 실패\n- 요약:\n- 변경 파일:\n- 검증: (실행한 명령과 결과)",
        spec.goal.trim(),
        or_none(&spec.scope),
        or_none(&spec.inputs),
        or_none(&spec.done_criteria),
    )
}

/// 작업자 결과를 PM에게 넘길 보고문. 최종 보고 + 도구 기록 기반 변경 파일만 담는다.
pub fn compose_report(conn: &Connection, task: &Task, run: &Run) -> AppResult<String> {
    let agent = agent::get(conn, task.agent_id)?;
    let status = match run.status {
        RunStatus::Succeeded => "완료",
        RunStatus::Failed => "실패",
        _ => run.status.as_str(),
    };
    let files = task::changed_files(conn, run.id)?;
    let files = if files.is_empty() {
        "(없음)".to_string()
    } else {
        files.join(", ")
    };
    let result = task::last_result_text(conn, run.id)?.unwrap_or_else(|| "(보고 없음)".into());
    let (result, truncated) = truncate_chars(&result, REPORT_MAX_CHARS);
    let mut out = format!(
        "작업 #{} '{}' · 담당 {} · 상태 {}\n변경 파일(도구 기록): {}\n\n담당자 최종 보고{}:\n{}",
        task.id,
        task.title,
        agent.name,
        status,
        files,
        if truncated { " (일부)" } else { "" },
        result
    );
    if let Some(err) = &run.error {
        out.push_str(&format!("\n\n실행 오류: {}", truncate_chars(err, 500).0));
    }
    Ok(out)
}

// ---------- 메시지 ----------

fn message_from_row(r: &Row) -> rusqlite::Result<Message> {
    Ok(Message {
        id: r.get("id")?,
        project_id: r.get("project_id")?,
        goal_task_id: r.get("goal_task_id")?,
        author: r.get("author")?,
        text: r.get("text")?,
        payload: r
            .get::<_, Option<String>>("payload")?
            .and_then(|s| serde_json::from_str(&s).ok()),
        run_id: r.get("run_id")?,
        created_at: r.get("created_at")?,
    })
}

pub fn add_message(
    conn: &Connection,
    goal: &Task,
    author: &str,
    text: &str,
    payload: Option<&Value>,
    run_id: Option<i64>,
) -> AppResult<Message> {
    conn.execute(
        "INSERT INTO messages (project_id, goal_task_id, author, text, payload, run_id, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            goal.project_id,
            goal.id,
            author,
            text,
            payload.map(|v| v.to_string()),
            run_id,
            now_ms()
        ],
    )?;
    Ok(conn.query_row(
        "SELECT * FROM messages WHERE id = ?1",
        [conn.last_insert_rowid()],
        message_from_row,
    )?)
}

pub fn list_messages(conn: &Connection, goal_id: i64) -> AppResult<Vec<Message>> {
    let mut stmt = conn.prepare("SELECT * FROM messages WHERE goal_task_id = ?1 ORDER BY id")?;
    let rows = stmt.query_map([goal_id], message_from_row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn last_message(conn: &Connection, goal_id: i64) -> AppResult<Option<Message>> {
    Ok(conn
        .query_row(
            "SELECT * FROM messages WHERE goal_task_id = ?1 ORDER BY id DESC LIMIT 1",
            [goal_id],
            message_from_row,
        )
        .optional()?)
}

pub fn list_goals(conn: &Connection, project_id: i64) -> AppResult<Vec<Task>> {
    let ids: Vec<i64> = conn
        .prepare(
            "SELECT id FROM tasks WHERE project_id = ?1 AND mode = 'pm' ORDER BY id DESC LIMIT 100",
        )?
        .query_map([project_id], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    ids.into_iter().map(|id| task::get_task(conn, id)).collect()
}

// ---------- PM 실행 시작 ----------

/// 사용자 메시지를 PM에게 전달하는 방법
pub enum Delivery {
    /// 새 PM Run을 큐에 넣었다 (pump 필요)
    Queued,
    /// 이미 실행 중인 PM Run에 메시지로 전달해야 한다
    SendTo { run_id: i64, prompt: String },
}

fn active_run(conn: &Connection, task_id: i64) -> AppResult<Option<Run>> {
    Ok(task::latest_run(conn, task_id)?.filter(|r| !r.status.is_terminal()))
}

/// 새 목표를 만들고 PM을 실행한다.
pub fn start_goal(
    conn: &Connection,
    project: &Project,
    text: &str,
) -> AppResult<(Task, Message, Run)> {
    let pm = agent::ensure_pm(conn)?;
    let goal = task::create_goal(conn, project.id, pm.id, text)?;
    let msg = add_message(conn, &goal, "user", text, None, None)?;
    let prompt = compose_pm_prompt(&team_summary(conn)?, "사용자 요청", text);
    let run = task::create_run_with_role(
        conn,
        task::NewRun {
            task_id: goal.id,
            agent_id: pm.id,
            prompt: &prompt,
            resume_session_id: None,
            config: &run_config(project, &pm),
            cwd: &project.path,
        },
        "pm",
    )?;
    Ok((goal, msg, run))
}

/// 목표 스레드에 PM 메시지를 전달한다. 같은 목표의 PM 세션을 이어서 쓴다.
pub fn deliver_to_pm(
    conn: &Connection,
    project: &Project,
    goal: &Task,
    prompt: String,
) -> AppResult<Delivery> {
    if let Some(r) = active_run(conn, goal.id)? {
        if r.status == RunStatus::Queued {
            return Err(AppError::InvalidInput(
                "PM이 아직 이전 메시지를 시작하지 않았습니다. 잠시 후 다시 보내세요".into(),
            ));
        }
        return Ok(Delivery::SendTo {
            run_id: r.id,
            prompt,
        });
    }
    let pm = agent::ensure_pm(conn)?;
    let resume = task::latest_run(conn, goal.id)?.and_then(|r| r.session_id);
    task::create_run_with_role(
        conn,
        task::NewRun {
            task_id: goal.id,
            agent_id: pm.id,
            prompt: &prompt,
            resume_session_id: resume.as_deref(),
            config: &run_config(project, &pm),
            cwd: &project.path,
        },
        "pm",
    )?;
    Ok(Delivery::Queued)
}

pub fn user_followup_prompt(conn: &Connection, text: &str) -> AppResult<String> {
    Ok(compose_pm_prompt(
        &team_summary(conn)?,
        "사용자 메시지",
        text,
    ))
}

pub fn report_prompt(conn: &Connection, report: &str) -> AppResult<String> {
    Ok(compose_pm_prompt(
        &team_summary(conn)?,
        "작업 결과 보고",
        &format!("{report}\n\nmode=report로 사용자에게 결과와 검증 내용을 요약하세요."),
    ))
}

// ---------- 이벤트 반응 ----------

/// PM 턴 결과 처리 결과
#[derive(Default)]
pub struct TurnEffects {
    pub messages: Vec<Message>,
    /// 새로 큐에 넣은 Run이 있으면 true (pump 필요)
    pub queued: bool,
}

/// PM의 한 턴이 끝났을 때: 결정을 메시지로 남기고 위임을 실행한다.
pub fn on_pm_turn(
    conn: &Connection,
    run: &Run,
    structured: Option<&Value>,
    result_text: Option<&str>,
    is_error: bool,
) -> AppResult<TurnEffects> {
    let goal = task::get_task(conn, run.task_id)?;
    let mut fx = TurnEffects::default();

    let decision = structured.and_then(|v| serde_json::from_value::<Decision>(v.clone()).ok());
    let Some(decision) = decision.filter(|_| !is_error) else {
        let text = result_text
            .filter(|t| !t.trim().is_empty())
            .unwrap_or("PM 응답을 해석하지 못했습니다.");
        fx.messages
            .push(add_message(conn, &goal, "pm", text, None, Some(run.id))?);
        if is_error {
            fx.messages.push(add_message(
                conn,
                &goal,
                "system",
                "PM 실행 중 오류가 발생했습니다.",
                None,
                Some(run.id),
            )?);
        }
        return Ok(fx);
    };

    let payload = serde_json::to_value(&decision)?;
    fx.messages.push(add_message(
        conn,
        &goal,
        "pm",
        &decision.reply,
        Some(&payload),
        Some(run.id),
    )?);

    if decision.mode == "delegate" {
        if decision.tasks.is_empty() {
            fx.messages.push(add_message(
                conn,
                &goal,
                "system",
                "PM이 위임을 선택했지만 작업 명세가 없습니다.",
                None,
                Some(run.id),
            )?);
        }
        let project = crate::project::get(conn, goal.project_id)?
            .ok_or_else(|| AppError::NotFound(format!("프로젝트 #{}", goal.project_id)))?;
        let workers = agent::list_workers(conn)?;
        for spec in decision.tasks.iter().take(1) {
            let Some(worker) = workers.iter().find(|a| a.id == spec.agent_id) else {
                fx.messages.push(add_message(
                    conn,
                    &goal,
                    "system",
                    &format!("배정 실패: 팀원 #{}을(를) 찾을 수 없습니다.", spec.agent_id),
                    None,
                    Some(run.id),
                )?);
                continue;
            };
            let prompt = compose_worker_prompt(spec);
            let title = if spec.title.trim().is_empty() {
                &spec.goal
            } else {
                &spec.title
            };
            let child = task::create_delegated(
                conn,
                &goal,
                worker.id,
                title,
                &prompt,
                &serde_json::to_value(spec)?,
            )?;
            task::create_run(
                conn,
                task::NewRun {
                    task_id: child.id,
                    agent_id: worker.id,
                    prompt: &prompt,
                    resume_session_id: None,
                    config: &run_config(&project, worker),
                    cwd: &project.path,
                },
            )?;
            fx.queued = true;
            fx.messages.push(add_message(
                conn,
                &goal,
                "system",
                &format!("작업 배정: {} → {}", child.title, worker.name),
                Some(&json!({"taskId": child.id, "agentId": worker.id})),
                Some(run.id),
            )?);
        }
    }
    Ok(fx)
}

/// 실행이 끝났을 때 PM 쪽 반응
#[derive(Default)]
pub struct FinishEffects {
    pub messages: Vec<Message>,
    /// PM에게 보고를 전달해야 하면 (목표 Task, 프롬프트)
    pub report: Option<(Task, String)>,
}

pub fn on_run_finished(conn: &Connection, run: &Run) -> AppResult<FinishEffects> {
    let task = task::get_task(conn, run.task_id)?;
    let mut fx = FinishEffects::default();

    match task.mode.as_str() {
        task::MODE_PM => {
            let note = match run.status {
                RunStatus::Failed => Some(format!(
                    "PM 실행 실패: {}",
                    run.error.as_deref().unwrap_or("원인 미상")
                )),
                RunStatus::Cancelled => Some("PM 응답을 중단했습니다.".into()),
                RunStatus::Interrupted => Some("앱 종료로 PM 응답이 중단되었습니다.".into()),
                _ => None,
            };
            if let Some(n) = note {
                fx.messages
                    .push(add_message(conn, &task, "system", &n, None, Some(run.id))?);
            }
        }
        task::MODE_DELEGATED => {
            let Some(goal_id) = task.parent_id else {
                return Ok(fx);
            };
            let goal = task::get_task(conn, goal_id)?;
            let payload = json!({"taskId": task.id, "status": run.status.as_str()});
            match run.status {
                RunStatus::Succeeded | RunStatus::Failed => {
                    let label = if run.status == RunStatus::Succeeded {
                        "완료"
                    } else {
                        "실패"
                    };
                    fx.messages.push(add_message(
                        conn,
                        &goal,
                        "system",
                        &format!("작업 {label}: {} — PM이 결과를 취합합니다", task.title),
                        Some(&payload),
                        Some(run.id),
                    )?);
                    let report = compose_report(conn, &task, run)?;
                    fx.report = Some((goal, report_prompt(conn, &report)?));
                }
                // 사용자가 중단했거나 앱이 종료된 경우 PM을 부르지 않는다 (사용자가 이미 알고 있음)
                _ => {
                    fx.messages.push(add_message(
                        conn,
                        &goal,
                        "system",
                        &format!("작업 중단됨: {} (PM 호출 없음)", task.title),
                        Some(&payload),
                        Some(run.id),
                    )?);
                }
            }
        }
        _ => {}
    }
    Ok(fx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentInput, PermissionMode};
    use crate::db::Db;
    use crate::runtime::RunEvent;

    fn worker(conn: &Connection) -> Agent {
        agent::create(
            conn,
            &AgentInput {
                name: "민수".into(),
                role: "백엔드".into(),
                instructions: "테스트 먼저".into(),
                model: None,
                effort: None,
                permission_mode: PermissionMode::AcceptEdits,
                tools: vec!["Read".into(), "Edit".into()],
                color: None,
            },
        )
        .unwrap()
    }

    fn project(conn: &Connection) -> (Project, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let p = crate::project::upsert(conn, dir.path()).unwrap();
        (p, dir)
    }

    #[test]
    fn pm_설정은_스키마와_pm지침을_포함하고_작업자는_제외() {
        let db = Db::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let (p, _d) = project(&conn);
        let pm = agent::ensure_pm(&conn).unwrap();
        let w = worker(&conn);
        let pc = run_config(&p, &pm);
        assert!(pc.json_schema.is_some());
        assert!(pc.system_append.contains("mode"));
        let wc = run_config(&p, &w);
        assert!(wc.json_schema.is_none());
        assert!(wc.system_append.contains("테스트 먼저"));
    }

    #[test]
    fn 팀요약에_pm은_빠진다() {
        let db = Db::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        agent::ensure_pm(&conn).unwrap();
        assert!(team_summary(&conn).unwrap().contains("팀원 없음"));
        let w = worker(&conn);
        let s = team_summary(&conn).unwrap();
        assert!(s.contains(&format!("agentId {}", w.id)) && s.contains("편집 자동"));
        assert!(!s.contains("PM ("));
    }

    #[test]
    fn 위임_결정이면_작업과_작업자_run을_만든다() {
        let db = Db::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let (p, _d) = project(&conn);
        let w = worker(&conn);
        let (goal, _, pm_run) = start_goal(&conn, &p, "로그인 API에 레이트 리밋 추가").unwrap();
        assert_eq!(pm_run.role, "pm");
        assert!(pm_run.prompt.contains("로그인 API") && pm_run.prompt.contains("민수"));

        let decision = json!({"mode":"delegate","reply":"민수에게 맡깁니다","tasks":[
            {"agentId": w.id, "title":"레이트 리밋","goal":"분당 10회 제한","scope":"src/auth","doneCriteria":"테스트 통과"}]});
        let fx = on_pm_turn(&conn, &pm_run, Some(&decision), None, false).unwrap();
        assert!(fx.queued);
        assert_eq!(fx.messages.len(), 2, "PM 답변 + 배정 알림");

        let children = task::list_children(&conn, goal.id).unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].mode, task::MODE_DELEGATED);
        assert_eq!(children[0].spec.as_ref().unwrap()["scope"], "src/auth");
        let wr = task::latest_run(&conn, children[0].id).unwrap().unwrap();
        assert_eq!((wr.role.as_str(), wr.agent_id), ("worker", w.id));
        assert!(wr.prompt.contains("## 완료 조건\n테스트 통과"));
        assert!(wr.config.json_schema.is_none());
    }

    #[test]
    fn 없는_팀원이나_pm에게는_위임하지_않는다() {
        let db = Db::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let (p, _d) = project(&conn);
        let (goal, _, pm_run) = start_goal(&conn, &p, "x").unwrap();
        let pm_id = agent::ensure_pm(&conn).unwrap().id;
        for id in [9999, pm_id] {
            let d = json!({"mode":"delegate","reply":"r","tasks":[{"agentId": id,"title":"t","goal":"g","scope":"s","doneCriteria":"d"}]});
            let fx = on_pm_turn(&conn, &pm_run, Some(&d), None, false).unwrap();
            assert!(!fx.queued);
            assert!(fx.messages.iter().any(|m| m.text.contains("배정 실패")));
        }
        assert!(task::list_children(&conn, goal.id).unwrap().is_empty());
    }

    #[test]
    fn 직접처리와_해석불가_응답() {
        let db = Db::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let (p, _d) = project(&conn);
        let (goal, _, pm_run) = start_goal(&conn, &p, "x").unwrap();
        let fx = on_pm_turn(
            &conn,
            &pm_run,
            Some(&json!({"mode":"direct","reply":"답: 42"})),
            None,
            false,
        )
        .unwrap();
        assert!(!fx.queued);
        assert_eq!(fx.messages[0].payload.as_ref().unwrap()["mode"], "direct");
        let fx = on_pm_turn(&conn, &pm_run, None, Some("그냥 텍스트"), false).unwrap();
        assert_eq!(fx.messages[0].text, "그냥 텍스트");
        assert_eq!(
            list_messages(&conn, goal.id).unwrap().len(),
            3,
            "사용자 1 + PM 2"
        );
    }

    #[test]
    fn 위임작업_완료시에만_pm에게_보고하고_중단은_보고하지_않는다() {
        let db = Db::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let (p, _d) = project(&conn);
        let w = worker(&conn);
        let (goal, _, pm_run) = start_goal(&conn, &p, "x").unwrap();
        let d = json!({"mode":"delegate","reply":"r","tasks":[{"agentId": w.id,"title":"t","goal":"g","scope":"s","doneCriteria":"d"}]});
        on_pm_turn(&conn, &pm_run, Some(&d), None, false).unwrap();
        let child = task::list_children(&conn, goal.id).unwrap().remove(0);
        let wr = task::latest_run(&conn, child.id).unwrap().unwrap();

        // 작업자 실행 기록: 파일 수정 + 최종 보고
        task::append_event(
            &conn,
            wr.id,
            &RunEvent::ToolUse {
                tool_use_id: "1".into(),
                name: "Edit".into(),
                summary: "/p/a.rs".into(),
            },
        )
        .unwrap();
        task::append_event(
            &conn,
            wr.id,
            &RunEvent::ToolUse {
                tool_use_id: "2".into(),
                name: "Edit".into(),
                summary: "/p/a.rs".into(),
            },
        )
        .unwrap();
        task::append_event(
            &conn,
            wr.id,
            &RunEvent::ToolUse {
                tool_use_id: "3".into(),
                name: "Read".into(),
                summary: "/p/b.rs".into(),
            },
        )
        .unwrap();
        task::append_event(
            &conn,
            wr.id,
            &RunEvent::TurnResult {
                is_error: false,
                subtype: "success".into(),
                result: Some("## 작업 보고\n- 결과: 완료".into()),
                usage: Default::default(),
                duration_ms: None,
                num_turns: None,
                session_totals: None,
                structured: None,
            },
        )
        .unwrap();

        task::finish_run(&conn, wr.id, RunStatus::Succeeded, Some(0), None).unwrap();
        let fx = on_run_finished(&conn, &task::get_run(&conn, wr.id).unwrap()).unwrap();
        let (g, prompt) = fx.report.expect("완료 시 보고");
        assert_eq!(g.id, goal.id);
        assert!(
            prompt.contains("변경 파일(도구 기록): /p/a.rs\n"),
            "중복 제거, 읽기는 제외: {prompt}"
        );
        assert!(prompt.contains("- 결과: 완료") && prompt.contains("mode=report"));

        // 재실행 후 사용자가 중단 → PM 호출 없음
        let wr2 = task::create_run(
            &conn,
            task::NewRun {
                task_id: child.id,
                agent_id: w.id,
                prompt: "x",
                resume_session_id: None,
                config: &wr.config,
                cwd: &p.path,
            },
        )
        .unwrap();
        task::finish_run(&conn, wr2.id, RunStatus::Cancelled, None, None).unwrap();
        let fx = on_run_finished(&conn, &task::get_run(&conn, wr2.id).unwrap()).unwrap();
        assert!(fx.report.is_none());
        assert!(fx.messages[0].text.contains("PM 호출 없음"));
    }

    #[test]
    fn 실행중인_pm에는_메시지로_전달하고_끝났으면_세션을_이어서_새_run() {
        let db = Db::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let (p, _d) = project(&conn);
        let (goal, _, r1) = start_goal(&conn, &p, "x").unwrap();
        // 큐 대기 중이면 거부
        assert!(deliver_to_pm(&conn, &p, &goal, "y".into()).is_err());
        task::mark_run_started(&conn, r1.id, "sess-1").unwrap();
        assert!(
            matches!(deliver_to_pm(&conn, &p, &goal, "y".into()).unwrap(), Delivery::SendTo { run_id, .. } if run_id == r1.id)
        );
        task::finish_run(&conn, r1.id, RunStatus::Succeeded, Some(0), None).unwrap();
        match deliver_to_pm(&conn, &p, &goal, "y".into()).unwrap() {
            Delivery::Queued => {
                let r2 = task::latest_run(&conn, goal.id).unwrap().unwrap();
                assert_eq!(r2.resume_session_id.as_deref(), Some("sess-1"));
                assert_eq!(r2.role, "pm");
            }
            _ => panic!("새 Run이어야 함"),
        }
    }
}
