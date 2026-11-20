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
use crate::project::{self, Project, ProjectView};
use crate::runtime::RunConfig;
use crate::settings::{self, AppSettings};
use crate::task::{self, Approval, Run, StoredEvent, Task, TaskSummary};

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
    agent::list(&*db.conn()?)
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

/// Run 생성 시점의 설정 스냅샷. 이후 에이전트·지침 변경은 다음 실행부터 반영된다 (스펙 4.3).
pub(crate) fn snapshot(project: &Project, agent: &Agent) -> RunConfig {
    let mut sys = format!(
        "당신은 Mefice 개발팀의 '{}' 역할 에이전트 '{}'입니다.",
        agent.role, agent.name
    );
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
    }
}

fn summary(conn: &Connection, task: Task) -> AppResult<TaskSummary> {
    let agent_name = agent::get(conn, task.agent_id)?.name;
    let latest_run = task::latest_run(conn, task.id)?;
    Ok(TaskSummary {
        task,
        agent_name,
        latest_run,
    })
}

/// 특정 에이전트에게 바로 작업을 맡긴다 (스펙 4.6 직접 실행 모드).
#[tauri::command]
pub async fn start_direct_task(
    db: State<'_, Db>,
    executor: State<'_, Arc<Executor>>,
    agent_id: i64,
    prompt: String,
) -> AppResult<TaskSummary> {
    let result = {
        let conn = db.conn()?;
        let project = current_project(&conn)?;
        let agent = agent::get(&conn, agent_id)?;
        let task = task::create_task(&conn, project.id, agent.id, &prompt)?;
        task::create_run(
            &conn,
            task::NewRun {
                task_id: task.id,
                agent_id: agent.id,
                prompt: &prompt,
                resume_session_id: None,
                config: &snapshot(&project, &agent),
                cwd: &project.path,
            },
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
    task::create_run(
        &conn,
        task::NewRun {
            task_id,
            agent_id: agent.id,
            prompt,
            resume_session_id: resume.as_deref(),
            config: &snapshot(&project, &agent),
            cwd: &project.path,
        },
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
