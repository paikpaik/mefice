//! 프론트엔드에 노출되는 Tauri 명령 경계.
//! 스펙 10장: 임의 셸 실행은 노출하지 않고, 허용된 동작만 여기서 정의한다.

use std::sync::Arc;

use rusqlite::Connection;
use serde::Serialize;
use tauri::State;

use crate::agent::{self, Agent, AgentInput};
use crate::db::Db;
use crate::env_check::{self, EnvironmentReport};
use crate::error::{AppError, AppResult};
use crate::executor::Executor;
use crate::pm::{self, Message};
use crate::project::{self, Project, ProjectView};
use crate::runtime::RateLimitSnapshot;
use crate::settings::{self, AppSettings};
use crate::task::{self, Approval, Run, StoredEvent, Task, TaskSummary};
use crate::usage;
use crate::workspace;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppState {
    pub settings: AppSettings,
    pub project: Option<ProjectView>,
}

/// Git 검사 등 블로킹 작업을 비동기 런타임 밖에서 수행한다.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> AppResult<T> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))
}

async fn view(project: Project) -> AppResult<ProjectView> {
    blocking(move || project::to_view(project)).await
}

/// 앱 시작 시 호출: 저장된 설정과 마지막 프로젝트를 복원한다.
#[tauri::command]
pub async fn get_app_state(db: State<'_, Db>) -> AppResult<AppState> {
    let (settings, project) = {
        let conn = db.conn()?;
        let settings = settings::load(&conn)?;
        let project = match settings::current_project_id(&conn)? {
            Some(id) => project::get(&conn, id)?,
            None => None,
        };
        (settings, project)
    };
    let project = match project {
        Some(p) => Some(view(p).await?),
        None => None,
    };
    Ok(AppState { settings, project })
}

/// 폴더를 열어 현재 프로젝트로 지정한다. 폴더 내용은 읽기만 하고 수정하지 않는다.
#[tauri::command]
pub async fn open_project(db: State<'_, Db>, path: String) -> AppResult<ProjectView> {
    let path = project::normalize_path(&path)?;
    let project = {
        let conn = db.conn()?;
        let p = project::upsert(&conn, &path)?;
        settings::set_current_project_id(&conn, p.id)?;
        p
    };
    view(project).await
}

/// Git 상태 등을 다시 읽는다.
#[tauri::command]
pub async fn refresh_project(db: State<'_, Db>, id: i64) -> AppResult<ProjectView> {
    let project = {
        let conn = db.conn()?;
        project::get(&conn, id)?.ok_or_else(|| AppError::NotFound(format!("프로젝트 #{id}")))?
    };
    view(project).await
}

#[tauri::command]
pub async fn update_project_instructions(
    db: State<'_, Db>,
    id: i64,
    instructions: String,
) -> AppResult<Project> {
    let conn = db.conn()?;
    project::update_instructions(&conn, id, &instructions)
}

#[tauri::command]
pub async fn update_settings(db: State<'_, Db>, settings: AppSettings) -> AppResult<AppSettings> {
    let conn = db.conn()?;
    settings::save(&conn, &settings)?;
    settings::load(&conn)
}

#[tauri::command]
pub async fn check_environment() -> AppResult<EnvironmentReport> {
    blocking(env_check::check_environment).await
}

// ---------- 에이전트 ----------

#[tauri::command]
pub async fn list_agents(db: State<'_, Db>) -> AppResult<Vec<Agent>> {
    let conn = db.conn()?;
    // PM은 항상 팀에 있다 (모델 등 설정을 첫 대화 전에 바꿀 수 있도록). 생성·대기에 토큰은 들지 않는다.
    agent::ensure_pm(&conn)?;
    agent::assign_missing_desks(&conn)?;
    agent::list(&conn)
}

#[tauri::command]
pub async fn create_agent(db: State<'_, Db>, input: AgentInput) -> AppResult<Agent> {
    agent::create(&*db.conn()?, &input)
}

#[tauri::command]
pub async fn update_agent(db: State<'_, Db>, id: i64, input: AgentInput) -> AppResult<Agent> {
    agent::update(&*db.conn()?, id, &input)
}

#[tauri::command]
pub async fn archive_agent(db: State<'_, Db>, id: i64) -> AppResult<()> {
    agent::archive(&*db.conn()?, id)
}

// ---------- 작업·실행 (직접 실행 모드) ----------

fn current_project(conn: &Connection) -> AppResult<Project> {
    let id = settings::current_project_id(conn)?
        .ok_or_else(|| AppError::InvalidInput("먼저 프로젝트를 여세요".into()))?;
    project::get(conn, id)?.ok_or_else(|| AppError::NotFound(format!("프로젝트 #{id}")))
}

fn summary(conn: &Connection, task: Task) -> AppResult<TaskSummary> {
    let agent_name = agent::get(conn, task.agent_id)?.name;
    let latest_run = task::latest_run(conn, task.id)?;
    let depends_on = task::dependencies(conn, task.id)?;
    Ok(TaskSummary {
        task,
        agent_name,
        latest_run,
        depends_on,
    })
}

/// 특정 에이전트에게 바로 작업을 맡긴다 (스펙 4.6 직접 실행 모드).
#[tauri::command]
pub async fn start_direct_task(
    db: State<'_, Db>,
    executor: State<'_, Arc<Executor>>,
    agent_id: i64,
    prompt: String,
    isolated: Option<bool>,
) -> AppResult<TaskSummary> {
    let result = {
        let conn = db.conn()?;
        let project = current_project(&conn)?;
        let agent = agent::get(&conn, agent_id)?;
        if agent.is_pm() {
            return Err(AppError::InvalidInput(
                "PM에게는 PM 채팅으로 요청하세요".into(),
            ));
        }
        let task = task::create_task(&conn, project.id, agent.id, &prompt)?;
        // 기본값: 파일을 바꿀 수 있는 에이전트이고 Git 격리가 가능하면 worktree에서 실행
        let want = isolated.unwrap_or(true) && agent.can_write();
        if want && crate::git::can_isolate(std::path::Path::new(&project.path)) {
            task::set_isolation(&conn, task.id, task::ISOLATION_WORKTREE)?;
        }
        let task = task::get_task(&conn, task.id)?;
        task::create_run_with_role(
            &conn,
            task::NewRun {
                task_id: task.id,
                agent_id: agent.id,
                prompt: &prompt,
                resume_session_id: None,
                config: &pm::run_config(&project, &agent),
                cwd: &project.path,
            },
            agent.usage_role(),
        )?;
        summary(&conn, task)?
    };
    executor.pump();
    Ok(result)
}

/// 같은 작업을 새 세션으로 다시 실행한다.
#[tauri::command]
pub async fn rerun_task(
    db: State<'_, Db>,
    executor: State<'_, Arc<Executor>>,
    task_id: i64,
) -> AppResult<Run> {
    let run = new_run_for_task(&db, task_id, None)?;
    executor.pump();
    Ok(run)
}

/// 끝난 작업의 세션을 이어서 추가 지시한다 (런타임 세션 재개).
#[tauri::command]
pub async fn continue_task(
    db: State<'_, Db>,
    executor: State<'_, Arc<Executor>>,
    task_id: i64,
    prompt: String,
) -> AppResult<Run> {
    let run = new_run_for_task(&db, task_id, Some(prompt))?;
    executor.pump();
    Ok(run)
}

fn new_run_for_task(db: &Db, task_id: i64, follow_up: Option<String>) -> AppResult<Run> {
    let conn = db.conn()?;
    let task = task::get_task(&conn, task_id)?;
    let project = project::get(&conn, task.project_id)?
        .ok_or_else(|| AppError::NotFound(format!("프로젝트 #{}", task.project_id)))?;
    let agent = agent::get(&conn, task.agent_id)?;
    let latest = task::latest_run(&conn, task_id)?;
    if latest.as_ref().is_some_and(|r| !r.status.is_terminal()) {
        return Err(AppError::InvalidInput(
            "이 작업은 아직 실행 중입니다".into(),
        ));
    }
    // 병합·폐기 후 다시 실행하면 새 결과가 생기므로 통합 상태를 되돌린다
    if task.integration != "none" {
        task::set_integration(&conn, task_id, "none")?;
    }
    let (prompt, resume) = match &follow_up {
        Some(p) => {
            if p.trim().is_empty() {
                return Err(AppError::InvalidInput("지시 내용을 입력하세요".into()));
            }
            let session = latest
                .as_ref()
                .and_then(|r| r.session_id.clone())
                .ok_or_else(|| {
                    AppError::InvalidInput("이어갈 세션이 없습니다. 재실행하세요".into())
                })?;
            (p.as_str(), Some(session))
        }
        None => (task.prompt.as_str(), None),
    };
    // PM 목표를 작업 화면에서 이어서·다시 실행해도 PM Run으로 기록된다 (결정 처리·사용량 구분)
    task::create_run_with_role(
        &conn,
        task::NewRun {
            task_id,
            agent_id: agent.id,
            prompt,
            resume_session_id: resume.as_deref(),
            config: &pm::run_config(&project, &agent),
            cwd: &project.path,
        },
        agent.usage_role(),
    )
}

#[tauri::command]
pub async fn list_tasks(db: State<'_, Db>) -> AppResult<Vec<TaskSummary>> {
    let conn = db.conn()?;
    let project = current_project(&conn)?;
    task::list_tasks(&conn, project.id)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskDetail {
    #[serde(flatten)]
    pub summary: TaskSummary,
    pub runs: Vec<Run>,
}

#[tauri::command]
pub async fn get_task_detail(db: State<'_, Db>, task_id: i64) -> AppResult<TaskDetail> {
    let conn = db.conn()?;
    let task = task::get_task(&conn, task_id)?;
    let runs = task::list_runs(&conn, task_id)?;
    Ok(TaskDetail {
        summary: summary(&conn, task)?,
        runs,
    })
}

#[tauri::command]
pub async fn get_run_events(db: State<'_, Db>, run_id: i64) -> AppResult<Vec<StoredEvent>> {
    task::list_events(&*db.conn()?, run_id)
}

#[tauri::command]
pub async fn get_pending_approvals(db: State<'_, Db>, run_id: i64) -> AppResult<Vec<Approval>> {
    task::pending_approvals(&*db.conn()?, run_id)
}

/// 실행 중인 Run에 추가 지시를 보낸다.
#[tauri::command]
pub async fn send_run_message(
    executor: State<'_, Arc<Executor>>,
    run_id: i64,
    text: String,
) -> AppResult<()> {
    executor.send_message(run_id, text).await
}

#[tauri::command]
pub async fn cancel_run(executor: State<'_, Arc<Executor>>, run_id: i64) -> AppResult<()> {
    executor.cancel(run_id)
}

#[tauri::command]
pub async fn respond_approval(
    executor: State<'_, Arc<Executor>>,
    approval_id: i64,
    allow: bool,
) -> AppResult<()> {
    executor.respond_approval(approval_id, allow)
}

// ---------- 사용량 (3단계) ----------

/// 현재 프로젝트 사용량 요약. `since_ms`(epoch ms) 이후만 집계, 없으면 전체 기간.
#[tauri::command]
pub async fn get_usage_summary(
    db: State<'_, Db>,
    since_ms: Option<i64>,
) -> AppResult<usage::UsageSummary> {
    let conn = db.conn()?;
    let project = current_project(&conn)?;
    usage::summary(&conn, project.id, since_ms)
}

#[tauri::command]
pub async fn get_task_usage(db: State<'_, Db>, task_id: i64) -> AppResult<Vec<usage::RunUsage>> {
    usage::task_usage(&*db.conn()?, task_id)
}

/// 마지막으로 관측한 구독 한도 사용률 (계정 전체, Mefice 외 사용 포함)
#[tauri::command]
pub async fn get_rate_limit(db: State<'_, Db>) -> AppResult<Option<RateLimitSnapshot>> {
    usage::load_rate_limit(&*db.conn()?)
}

// ---------- PM (4단계) ----------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalSummary {
    #[serde(flatten)]
    pub summary: TaskSummary,
    pub last_message: Option<Message>,
    /// 진행 중인 위임 작업 수
    pub active_children: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalDetail {
    #[serde(flatten)]
    pub summary: TaskSummary,
    pub messages: Vec<Message>,
    pub children: Vec<TaskSummary>,
    /// 역할별 사용량 (PM 추가 사용량 확인용, 스펙 13장)
    pub usage_by_role: Vec<usage::Grouped>,
}

fn goal_summary(conn: &Connection, goal: Task) -> AppResult<GoalSummary> {
    let children = task::list_children(conn, goal.id)?;
    let mut active_children = 0;
    for c in &children {
        if task::latest_run(conn, c.id)?.is_some_and(|r| !r.status.is_terminal()) {
            active_children += 1;
        }
    }
    let last_message = pm::last_message(conn, goal.id)?;
    Ok(GoalSummary {
        summary: summary(conn, goal)?,
        last_message,
        active_children,
    })
}

#[tauri::command]
pub async fn list_goals(db: State<'_, Db>) -> AppResult<Vec<GoalSummary>> {
    let conn = db.conn()?;
    let project = current_project(&conn)?;
    pm::list_goals(&conn, project.id)?
        .into_iter()
        .map(|g| goal_summary(&conn, g))
        .collect()
}

#[tauri::command]
pub async fn get_goal(db: State<'_, Db>, goal_id: i64) -> AppResult<GoalDetail> {
    let conn = db.conn()?;
    let goal = task::get_task(&conn, goal_id)?;
    if goal.mode != task::MODE_PM {
        return Err(AppError::InvalidInput("PM 목표가 아닙니다".into()));
    }
    let children = task::list_children(&conn, goal_id)?
        .into_iter()
        .map(|c| summary(&conn, c))
        .collect::<AppResult<_>>()?;
    Ok(GoalDetail {
        messages: pm::list_messages(&conn, goal_id)?,
        usage_by_role: usage::goal_usage(&conn, goal_id)?,
        summary: summary(&conn, goal)?,
        children,
    })
}

/// PM에게 메시지를 보낸다. `goal_id`가 없으면 새 목표를 시작한다.
#[tauri::command]
pub async fn pm_send(
    db: State<'_, Db>,
    executor: State<'_, Arc<Executor>>,
    goal_id: Option<i64>,
    text: String,
) -> AppResult<i64> {
    if text.trim().is_empty() {
        return Err(AppError::InvalidInput("메시지를 입력하세요".into()));
    }
    let (goal_id, delivery, msg) = {
        let conn = db.conn()?;
        let project = current_project(&conn)?;
        match goal_id {
            None => {
                let (goal, msg, _run) = pm::start_goal(&conn, &project, &text)?;
                (goal.id, pm::Delivery::Queued, msg)
            }
            Some(id) => {
                let goal = task::get_task(&conn, id)?;
                let prompt = pm::user_followup_prompt(&conn, &text)?;
                let delivery = pm::deliver_to_pm(&conn, &project, &goal, prompt)?;
                let msg = pm::add_message(&conn, &goal, "user", &text, None, None)?;
                (goal.id, delivery, msg)
            }
        }
    };
    executor.emit_message(&msg);
    match delivery {
        pm::Delivery::Queued => executor.pump(),
        pm::Delivery::SendTo { run_id, prompt } => executor.send_message(run_id, prompt).await?,
    }
    Ok(goal_id)
}

// ---------- 작업 공간 · 통합 (5단계) ----------

/// git 등 블로킹 작업을 DB와 함께 별도 스레드에서 실행
async fn with_db_blocking<T: Send + 'static>(
    app: tauri::AppHandle,
    f: impl FnOnce(&Connection) -> AppResult<T> + Send + 'static,
) -> AppResult<T> {
    use tauri::Manager;
    blocking(move || {
        let db = app.state::<Db>();
        let conn = db.inner().conn()?;
        f(&conn)
    })
    .await?
}

/// 변경 내역과 병합 가능 여부 (충돌·미커밋 겹침 사전 확인 포함, 작업 트리 변경 없음)
#[tauri::command]
pub async fn get_task_diff(app: tauri::AppHandle, task_id: i64) -> AppResult<workspace::TaskDiff> {
    with_db_blocking(app, move |c| workspace::diff(c, task_id)).await
}

/// 사용자 현재 브랜치에 병합 (사용자 확정 시에만 호출)
#[tauri::command]
pub async fn merge_task(app: tauri::AppHandle, task_id: i64) -> AppResult<String> {
    with_db_blocking(app, move |c| workspace::merge(c, task_id)).await
}

/// 작업 결과를 버리고 작업 공간을 정리
#[tauri::command]
pub async fn discard_task_workspace(app: tauri::AppHandle, task_id: i64) -> AppResult<()> {
    with_db_blocking(app, move |c| workspace::discard(c, task_id)).await
}

// ---------- 고용 · 사무실 (6단계) ----------

#[tauri::command]
pub async fn list_templates() -> AppResult<crate::templates::TemplateCatalog> {
    Ok(crate::templates::catalog())
}

/// 사무실 캐릭터 상태 (DB 조회만, 모델 호출 없음)
#[tauri::command]
pub async fn get_office_state(db: State<'_, Db>) -> AppResult<Vec<crate::office::OfficeAgent>> {
    let conn = db.conn()?;
    agent::ensure_pm(&conn)?;
    agent::assign_missing_desks(&conn)?;
    crate::office::office_state(&conn)
}

#[tauri::command]
pub async fn get_agent_overview(
    db: State<'_, Db>,
    agent_id: i64,
) -> AppResult<crate::office::AgentOverview> {
    crate::office::agent_overview(&*db.conn()?, agent_id)
}

// ---------- 앱 수명주기 (7단계) ----------

/// 종료 확인 창용: 진행 중인 실행 목록
#[tauri::command]
pub async fn get_active_runs(db: State<'_, Db>) -> AppResult<Vec<crate::lifecycle::ActiveRunInfo>> {
    crate::lifecycle::active_runs(&*db.conn()?)
}

/// "작업 중단 후 종료": 중단 요청 → 최대 8초 대기 → 남은 프로세스 정리 → 종료
#[tauri::command]
pub async fn confirm_quit(app: tauri::AppHandle) -> AppResult<()> {
    crate::lifecycle::confirm_quit(app).await;
    Ok(())
}

/// "백그라운드에서 계속": 창만 숨기고 작업은 계속 (트레이에서 다시 열 수 있음)
#[tauri::command]
pub async fn keep_running_in_background(app: tauri::AppHandle) -> AppResult<()> {
    crate::lifecycle::hide_main(&app);
    Ok(())
}
