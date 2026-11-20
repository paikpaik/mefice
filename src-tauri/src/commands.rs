//! 프론트엔드에 노출되는 Tauri 명령 경계.
//! 스펙 10장: 임의 셸 실행은 노출하지 않고, 허용된 동작만 여기서 정의한다.

use serde::Serialize;
use tauri::State;

use crate::db::Db;
use crate::env_check::{self, EnvironmentReport};
use crate::error::{AppError, AppResult};
use crate::project::{self, Project, ProjectView};
use crate::settings::{self, AppSettings};

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
