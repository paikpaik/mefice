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
/// 한 결정에서 최대 `MAX_PLAN_TASKS`개 작업. 독립 작업은 병렬, `dependsOn`은 순서, `paths`는 겹침 감지에 쓴다.
pub const PM_SCHEMA: &str = r#"{
  "type": "object",
  "properties": {
    "mode": {"type": "string", "enum": ["direct", "delegate", "ask", "hire", "report"]},
    "reply": {"type": "string", "description": "사용자에게 보여줄 짧은 한국어 메시지 (5줄 이내)"},
    "tasks": {
      "type": "array",
      "maxItems": 5,
      "items": {
        "type": "object",
        "properties": {
          "key": {"type": "string", "description": "이 계획 안에서 작업을 가리키는 짧은 이름 (예: api, ui)"},
          "agentId": {"type": "integer"},
          "title": {"type": "string"},
          "goal": {"type": "string"},
          "scope": {"type": "string"},
          "paths": {"type": "array", "items": {"type": "string"}, "description": "수정할 파일·폴더 경로 (프로젝트 기준 상대 경로)"},
          "dependsOn": {"type": "array", "items": {"type": "string"}, "description": "먼저 끝나야 하는 작업의 key"},
          "inputs": {"type": "string"},
          "doneCriteria": {"type": "string"}
        },
        "required": ["key", "agentId", "title", "goal", "scope", "paths", "doneCriteria"]
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
- delegate: 팀원에게 맡깁니다. 보통 tasks 1개입니다. 서로 독립적이고 각각 충분히 큰 작업일 때만 여러 개(최대 5개)로 나눕니다. 나누면 PM 취합 비용과 통합 비용이 늘어납니다.
- ask: 목표나 완료 조건이 불분명해 진행할 수 없을 때만 질문합니다.
- hire: 맡길 만한 팀원이 없을 때 고용할 역할을 hire에 제안합니다. 고용은 사용자가 확정합니다.
- report: 팀원 작업 결과를 받았을 때 결과와 검증 내용을 사용자에게 요약합니다. 실패했다면 원인과 다음 조치를 제안하고, 다시 맡기려면 delegate를 씁니다.

## 규칙
- 탐색은 실행 방식을 정하는 데 필요한 만큼만 합니다. 구현을 위한 세부 탐색은 담당자에게 맡깁니다.
- 위임할 때 담당자는 이 대화를 볼 수 없습니다. goal(목표), scope(수정해도 되는 폴더·파일), inputs(필요한 맥락), doneCriteria(검증 가능한 완료 조건)를 구체적으로 씁니다.
- 각 작업의 paths에 수정할 경로를 적습니다. 수정 범위가 겹치는 작업은 나누지 말고 하나로 합치거나 dependsOn으로 순서를 정합니다. 앞 작업의 결과가 필요한 작업도 dependsOn을 씁니다.
- 위임 작업은 각자 격리된 작업 공간(git worktree)에서 실행되고, 결과는 사용자가 확인한 뒤 병합합니다. 담당자에게 커밋·브랜치 작업을 시키지 않습니다.
- 팀원 목록에 있는 agentId만 사용합니다.
- reply는 짧게 씁니다. 결과를 반복하거나 계획을 길게 설명하지 않습니다.";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DelegateSpec {
    #[serde(default)]
    pub key: String,
    pub agent_id: i64,
    /// 수정할 경로 (겹침 감지용)
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
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
    } else if agent.kind == agent::KIND_ASSISTANT {
        // 대화: 터미널과 같은 프롬프트 (페르소나 없음). 프로젝트 지침만 덧붙인다.
        String::new()
    } else {
        format!(
            "당신은 Mefice 개발팀의 '{}' 역할 에이전트 '{}'입니다.",
            agent.role, agent.name
        )
    };
    let pf = &agent.profile;
    if !pf.specialties.is_empty() {
        sys.push_str(&format!("\n전문 분야: {}", pf.specialties.join(", ")));
    }
    if !pf.capabilities.is_empty() {
        sys.push_str(&format!("\n수행 능력: {}", pf.capabilities.join(", ")));
    }
    if !pf.scope_paths.is_empty() {
        sys.push_str(&format!(
            "\n작업 범위: {} (이 범위 밖은 수정하지 마세요)",
            pf.scope_paths.join(", ")
        ));
    }
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
        limits: agent.profile.limits.clone(),
        full_settings: false,
        keep_alive: false,
        stream_partial: false,
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
            let mut extra = String::new();
            if !a.profile.specialties.is_empty() {
                extra.push_str(&format!(" · 전문 {}", a.profile.specialties.join("/")));
            }
            if !a.profile.scope_paths.is_empty() {
                extra.push_str(&format!(" · 범위 {}", a.profile.scope_paths.join(",")));
            }
            if !a.instructions.trim().is_empty() {
                extra.push_str(&format!(
                    " · {}",
                    truncate_chars(a.instructions.trim(), 80)
                        .0
                        .replace('\n', " ")
                ));
            }
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
        "# 작업 지시 (PM 배정)\n\n## 목표\n{}\n\n## 범위\n{}\n\n## 수정 경로\n{}\n\n## 입력·맥락\n{}\n\n## 완료 조건\n{}\n\n\
         범위 밖 파일은 수정하지 마세요. 진행할 수 없으면 추측하지 말고 이유를 보고하세요.\n\
         끝나면 마지막 메시지를 다음 형식으로 끝내세요.\n\n\
         ## 작업 보고\n- 결과: 완료 | 부분 완료 | 실패\n- 요약:\n- 변경 파일:\n- 검증: (실행한 명령과 결과)",
        spec.goal.trim(),
        or_none(&spec.scope),
        or_none(&spec.paths.join(", ")),
        or_none(&spec.inputs),
        or_none(&spec.done_criteria),
    )
}

fn workspace_files(conn: &Connection, task: &Task) -> Option<Vec<String>> {
    let ws = crate::workspace::get(conn, task.id).ok()??;
    let head = ws.head_commit.as_deref()?;
    let repo = crate::project::get(conn, task.project_id).ok()??.path;
    crate::git::changed_files(std::path::Path::new(&repo), &ws.base_commit, head)
        .ok()
        .map(|fs| {
            fs.into_iter()
                .map(|f| format!("{} {}", f.status, f.path))
                .collect()
        })
}

/// 작업자 결과를 PM에게 넘길 보고문. 최종 보고 + 변경 파일만 담는다.
/// 변경 파일은 worktree가 있으면 git 기준(명령으로 바꾼 파일 포함), 없으면 도구 기록 기준.
fn compose_report_limited(
    conn: &Connection,
    task: &Task,
    run: &Run,
    max_chars: usize,
) -> AppResult<String> {
    let agent = agent::get(conn, task.agent_id)?;
    let status = match run.status {
        RunStatus::Succeeded => "완료",
        RunStatus::Failed => "실패",
        _ => run.status.as_str(),
    };
    let (source, files) = match workspace_files(conn, task) {
        Some(f) => ("git", f),
        None => ("도구 기록", task::changed_files(conn, run.id)?),
    };
    let files = if files.is_empty() {
        "(없음)".to_string()
    } else {
        files.join(", ")
    };
    let result = task::last_result_text(conn, run.id)?.unwrap_or_else(|| "(보고 없음)".into());
    let (result, truncated) = truncate_chars(&result, max_chars);
    let mut out = format!(
        "작업 #{} '{}' · 담당 {} · 상태 {}\n변경 파일({source}): {}\n\n담당자 최종 보고{}:\n{}",
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
        } else {
            create_plan(conn, &goal, run, &decision.tasks, &mut fx)?;
        }
    }
    Ok(fx)
}

/// 한 결정에서 만들 수 있는 최대 작업 수 (스키마 maxItems와 같음)
pub const MAX_PLAN_TASKS: usize = 5;

fn normalize_path(p: &str) -> String {
    let t = p.trim().trim_start_matches("./").trim_end_matches('/');
    if t == "." {
        String::new()
    } else {
        t.to_string()
    }
}

/// 두 경로 목록이 겹치는지. 빈 경로나 "."는 저장소 전체로 본다. 목록이 비면 알 수 없음 → false.
pub fn paths_overlap(a: &[String], b: &[String]) -> bool {
    let norm = |v: &[String]| v.iter().map(|p| normalize_path(p)).collect::<Vec<_>>();
    let (a, b) = (norm(a), norm(b));
    a.iter().any(|x| {
        b.iter().any(|y| {
            x.is_empty()
                || y.is_empty()
                || x == y
                || x.starts_with(&format!("{y}/"))
                || y.starts_with(&format!("{x}/"))
        })
    })
}

struct Planned {
    task: Task,
    spec: DelegateSpec,
    writes: bool,
    isolated: bool,
}

/// PM 계획을 작업·의존성·Run으로 만든다 (스펙 4.4).
/// - 독립 작업은 병렬, dependsOn은 순서
/// - 수정 경로가 겹치는데 순서가 없으면 Mefice가 순차로 바꾼다
/// - Git 격리를 못 쓰면 쓰기 작업은 모두 순차 실행
fn create_plan(
    conn: &Connection,
    goal: &Task,
    run: &Run,
    specs: &[DelegateSpec],
    fx: &mut TurnEffects,
) -> AppResult<()> {
    let project = crate::project::get(conn, goal.project_id)?
        .ok_or_else(|| AppError::NotFound(format!("프로젝트 #{}", goal.project_id)))?;
    let can_isolate = crate::git::can_isolate(std::path::Path::new(&project.path));
    let workers = agent::list_workers(conn)?;
    let mut notes: Vec<String> = Vec::new();
    let mut planned: Vec<Planned> = Vec::new();

    for (i, spec) in specs.iter().take(MAX_PLAN_TASKS).enumerate() {
        let Some(worker) = workers.iter().find(|a| a.id == spec.agent_id) else {
            notes.push(format!(
                "배정 실패: 팀원 #{}을(를) 찾을 수 없습니다.",
                spec.agent_id
            ));
            continue;
        };
        let mut spec = spec.clone();
        if spec.key.trim().is_empty() {
            spec.key = format!("t{}", i + 1);
        }
        let writes = worker.can_write();
        let isolated = writes && can_isolate;
        let prompt = compose_worker_prompt(&spec);
        let title = if spec.title.trim().is_empty() {
            spec.goal.clone()
        } else {
            spec.title.clone()
        };
        let child = task::create_delegated(
            conn,
            goal,
            worker.id,
            &title,
            &prompt,
            &serde_json::to_value(&spec)?,
        )?;
        task::set_plan_run(conn, child.id, run.id)?;
        task::set_isolation(
            conn,
            child.id,
            if isolated {
                task::ISOLATION_WORKTREE
            } else {
                task::ISOLATION_NONE
            },
        )?;
        let child = task::get_task(conn, child.id)?;
        planned.push(Planned {
            task: child,
            spec,
            writes,
            isolated,
        });
    }

    // PM이 지정한 순서
    for p in &planned {
        for key in &p.spec.depends_on {
            match planned.iter().find(|q| &q.spec.key == key) {
                Some(q) if task::depends_transitively(conn, q.task.id, p.task.id)? => {
                    notes.push(format!(
                        "순환 의존 무시: {} → {}",
                        p.task.title, q.task.title
                    ));
                }
                Some(q) => task::add_dependency(conn, p.task.id, q.task.id, task::DEP_PLANNED)?,
                None => notes.push(format!(
                    "알 수 없는 선행 작업 '{key}' 무시 ({})",
                    p.task.title
                )),
            }
        }
    }

    // 순서 없이 수정 경로가 겹치면 순차로 (뒤 작업이 앞 작업 결과 위에서 시작)
    for j in 0..planned.len() {
        for i in 0..j {
            let (a, b) = (&planned[i], &planned[j]);
            if !(a.writes && b.writes) || !paths_overlap(&a.spec.paths, &b.spec.paths) {
                continue;
            }
            if task::depends_transitively(conn, a.task.id, b.task.id)?
                || task::depends_transitively(conn, b.task.id, a.task.id)?
            {
                continue;
            }
            task::add_dependency(conn, b.task.id, a.task.id, task::DEP_OVERLAP)?;
            notes.push(format!(
                "수정 경로가 겹쳐 순차 실행: {} → {}",
                a.task.title, b.task.title
            ));
        }
    }

    // 격리 불가: 같은 폴더를 동시에 고치지 않도록 쓰기 작업을 한 줄로 세운다
    let unisolated: Vec<&Planned> = planned.iter().filter(|p| p.writes && !p.isolated).collect();
    if unisolated.len() > 1 {
        for w in unisolated.windows(2) {
            let (a, b) = (w[0], w[1]);
            if !task::depends_transitively(conn, b.task.id, a.task.id)?
                && !task::depends_transitively(conn, a.task.id, b.task.id)?
            {
                task::add_dependency(conn, b.task.id, a.task.id, task::DEP_NO_ISOLATION)?;
            }
        }
        notes.push("Git 작업 공간 격리를 쓸 수 없어 수정 작업을 순서대로 실행합니다.".into());
    }

    for p in &planned {
        let worker = workers
            .iter()
            .find(|a| a.id == p.task.agent_id)
            .expect("검증된 팀원");
        task::create_run_with_role(
            conn,
            task::NewRun {
                task_id: p.task.id,
                agent_id: worker.id,
                prompt: &p.task.prompt,
                resume_session_id: None,
                config: &run_config(&project, worker),
                cwd: &project.path,
            },
            worker.usage_role(),
        )?;
        fx.queued = true;
        let deps = task::dependencies(conn, p.task.id)?;
        let after = if deps.is_empty() {
            String::new()
        } else {
            format!(
                " (선행: {})",
                deps.iter()
                    .map(|d| format!("#{d}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let iso = if p.isolated {
            " · 격리 작업 공간"
        } else {
            ""
        };
        fx.messages.push(add_message(
            conn,
            goal,
            "system",
            &format!(
                "작업 배정: #{} {} → {}{after}{iso}",
                p.task.id, p.task.title, worker.name
            ),
            Some(&json!({"taskId": p.task.id, "agentId": worker.id, "dependsOn": deps})),
            Some(run.id),
        )?);
    }
    for n in notes {
        fx.messages
            .push(add_message(conn, goal, "system", &n, None, Some(run.id))?);
    }
    Ok(())
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
            let label = match run.status {
                RunStatus::Succeeded => "완료",
                RunStatus::Failed => "실패",
                RunStatus::Cancelled if run.error.is_some() => "취소(선행 작업 실패)",
                RunStatus::Cancelled => "중단됨",
                _ => "중단 확인",
            };

            // 같은 계획의 작업이 모두 끝났을 때만 PM을 한 번 부른다
            let siblings = match task.plan_run_id {
                Some(p) => task::plan_tasks(conn, p)?,
                None => vec![task.clone()],
            };
            let mut finished: Vec<(Task, Run)> = Vec::new();
            let mut remaining = 0;
            for t in &siblings {
                match task::latest_run(conn, t.id)? {
                    Some(r) if r.status.is_terminal() => finished.push((t.clone(), r)),
                    Some(_) => remaining += 1,
                    None => {}
                }
            }
            let any_result = finished
                .iter()
                .any(|(_, r)| matches!(r.status, RunStatus::Succeeded | RunStatus::Failed));

            let text = if remaining > 0 {
                format!("작업 {label}: {} (남은 작업 {remaining}개)", task.title)
            } else if any_result {
                format!("작업 {label}: {} — PM이 결과를 취합합니다", task.title)
            } else {
                // 사용자가 모두 중단했거나 앱이 종료된 경우 PM을 부르지 않는다 (사용자가 이미 알고 있음)
                format!("작업 {label}: {} (PM 호출 없음)", task.title)
            };
            fx.messages.push(add_message(
                conn,
                &goal,
                "system",
                &text,
                Some(&payload),
                Some(run.id),
            )?);

            if remaining == 0 && any_result {
                let per_task = (REPORT_MAX_CHARS / finished.len().max(1)).max(1200);
                let parts = finished
                    .iter()
                    .map(|(t, r)| compose_report_limited(conn, t, r, per_task))
                    .collect::<AppResult<Vec<_>>>()?;
                let header = if parts.len() > 1 {
                    format!("계획의 작업 {}개가 모두 끝났습니다.\n\n", parts.len())
                } else {
                    String::new()
                };
                let report = format!(
                    "{header}{}\n\n격리 작업 공간의 결과는 사용자가 변경 내역을 확인한 뒤 병합합니다.",
                    parts.join("\n\n---\n\n")
                );
                fx.report = Some((goal, report_prompt(conn, &report)?));
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
                profile: Default::default(),
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
                input: None,
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
                input: None,
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
                input: None,
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
                context_tokens: None,
                context_window: None,
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

    fn git_project(conn: &Connection) -> (Project, tempfile::TempDir) {
        let repo = crate::git::test_repo::init();
        let p = crate::project::upsert(
            conn,
            &crate::project::normalize_path(repo.path().to_str().unwrap()).unwrap(),
        )
        .unwrap();
        (p, repo)
    }

    fn plan_spec(key: &str, agent: i64, paths: &[&str], deps: &[&str]) -> Value {
        json!({"key": key, "agentId": agent, "title": key, "goal": key, "scope": "s",
               "paths": paths, "dependsOn": deps, "doneCriteria": "d"})
    }

    #[test]
    fn 계획_병렬_순서_겹침_격리() {
        let db = Db::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let (p, _repo) = git_project(&conn);
        let w = worker(&conn);
        let (goal, _, pm_run) = start_goal(&conn, &p, "x").unwrap();
        let d = json!({"mode": "delegate", "reply": "r", "tasks": [
            plan_spec("api", w.id, &["src/api"], &[]),
            plan_spec("ui", w.id, &["src/ui"], &[]),
            plan_spec("api2", w.id, &["src/api/auth.rs"], &[]),   // api와 겹침 → 자동 순차
            plan_spec("docs", w.id, &["docs"], &["api", "ui"]),   // PM 지정 순서
        ]});
        let fx = on_pm_turn(&conn, &pm_run, Some(&d), None, false).unwrap();
        assert!(fx.queued);
        let kids = task::list_children(&conn, goal.id).unwrap();
        assert_eq!(kids.len(), 4);
        let id = |k: &str| kids.iter().find(|t| t.title == k).unwrap().id;
        for k in &kids {
            assert_eq!(
                k.isolation,
                task::ISOLATION_WORKTREE,
                "git 저장소 + 쓰기 가능 → 격리"
            );
            assert_eq!(k.plan_run_id, Some(pm_run.id));
        }
        assert!(task::dependencies(&conn, id("api")).unwrap().is_empty());
        assert!(
            task::dependencies(&conn, id("ui")).unwrap().is_empty(),
            "독립 작업은 병렬"
        );
        assert_eq!(
            task::dependencies(&conn, id("api2")).unwrap(),
            vec![id("api")]
        );
        assert_eq!(
            task::dependencies(&conn, id("docs")).unwrap(),
            vec![id("api"), id("ui")]
        );
        assert!(
            fx.messages
                .iter()
                .any(|m| m.text.contains("수정 경로가 겹쳐 순차 실행"))
        );

        // 스케줄링: 선행 작업이 끝나기 전에는 대기, 실패하면 차단
        assert_eq!(
            task::readiness(&conn, id("api")).unwrap(),
            task::Readiness::Ready
        );
        assert_eq!(
            task::readiness(&conn, id("docs")).unwrap(),
            task::Readiness::Waiting
        );
        let api_run = task::latest_run(&conn, id("api")).unwrap().unwrap();
        task::finish_run(&conn, api_run.id, RunStatus::Failed, Some(1), Some("x")).unwrap();
        assert_eq!(
            task::readiness(&conn, id("api2")).unwrap(),
            task::Readiness::Blocked {
                dependency: id("api")
            }
        );
    }

    #[test]
    fn git이_없으면_쓰기_작업을_순서대로() {
        let db = Db::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let (p, _d) = project(&conn); // git 저장소 아님
        let w = worker(&conn);
        let (goal, _, pm_run) = start_goal(&conn, &p, "x").unwrap();
        let d = json!({"mode": "delegate", "reply": "r", "tasks": [
            plan_spec("a", w.id, &["a"], &[]), plan_spec("b", w.id, &["b"], &[]), plan_spec("c", w.id, &["c"], &[])]});
        let fx = on_pm_turn(&conn, &pm_run, Some(&d), None, false).unwrap();
        let kids = task::list_children(&conn, goal.id).unwrap();
        assert!(kids.iter().all(|k| k.isolation == task::ISOLATION_NONE));
        assert_eq!(
            task::dependencies(&conn, kids[1].id).unwrap(),
            vec![kids[0].id]
        );
        assert_eq!(
            task::dependencies(&conn, kids[2].id).unwrap(),
            vec![kids[1].id]
        );
        assert!(
            fx.messages
                .iter()
                .any(|m| m.text.contains("격리를 쓸 수 없어"))
        );
    }

    #[test]
    fn 경로_겹침_판정() {
        let v = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(paths_overlap(&v(&["src/api"]), &v(&["./src/api/x.rs"])));
        assert!(paths_overlap(&v(&["src/"]), &v(&["src/ui"])));
        assert!(
            paths_overlap(&v(&["."]), &v(&["docs"])),
            "저장소 전체는 모두와 겹침"
        );
        assert!(
            !paths_overlap(&v(&["src/api"]), &v(&["src/apiv2"])),
            "접두 문자열만 같은 건 다른 경로"
        );
        assert!(
            !paths_overlap(&v(&[]), &v(&["src"])),
            "경로 미지정은 판단 불가"
        );
    }

    #[test]
    fn 계획의_작업이_모두_끝나야_한_번_보고한다() {
        let db = Db::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let (p, _repo) = git_project(&conn);
        let w = worker(&conn);
        let (goal, _, pm_run) = start_goal(&conn, &p, "x").unwrap();
        let d = json!({"mode": "delegate", "reply": "r", "tasks": [
            plan_spec("a", w.id, &["a"], &[]), plan_spec("b", w.id, &["b"], &[])]});
        on_pm_turn(&conn, &pm_run, Some(&d), None, false).unwrap();
        let kids = task::list_children(&conn, goal.id).unwrap();
        let ra = task::latest_run(&conn, kids[0].id).unwrap().unwrap();
        let rb = task::latest_run(&conn, kids[1].id).unwrap().unwrap();

        task::finish_run(&conn, ra.id, RunStatus::Succeeded, Some(0), None).unwrap();
        let fx = on_run_finished(&conn, &task::get_run(&conn, ra.id).unwrap()).unwrap();
        assert!(fx.report.is_none(), "b가 남아 있으면 보고하지 않음");
        assert!(fx.messages[0].text.contains("남은 작업 1개"));

        task::finish_run(
            &conn,
            rb.id,
            RunStatus::Failed,
            Some(1),
            Some("테스트 실패"),
        )
        .unwrap();
        let fx = on_run_finished(&conn, &task::get_run(&conn, rb.id).unwrap()).unwrap();
        let (_, prompt) = fx.report.expect("마지막 작업에서 한 번 보고");
        assert!(prompt.contains("작업 2개가 모두 끝났습니다"));
        assert!(
            prompt.contains(&format!("작업 #{}", kids[0].id))
                && prompt.contains("실행 오류: 테스트 실패")
        );
    }
}
