//! 앱 수명주기 (스펙 10장).
//!
//! - 창 닫기: 설정이 켜져 있으면 트레이에 남아 작업을 계속한다 (기본값).
//! - 앱 종료: 진행 중인 작업이 있으면 목록과 중단 영향을 보여주고 사용자가 종료 방식을 고른다.
//! - 실제 종료 직전: 남은 런타임 프로세스 그룹을 정리한다 (고아 방지).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{App, AppHandle, Emitter, Manager, RunEvent, Window, WindowEvent};

use crate::db::Db;
use crate::error::AppResult;
use crate::executor::Executor;
use crate::{settings, task};

pub const EVENT_QUIT_REQUESTED: &str = "quit-requested";

/// 확인을 거친 종료만 통과시키는 문
#[derive(Default)]
pub struct QuitGate(AtomicBool);

/// 종료 확인 창에 보여줄 진행 중 실행
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveRunInfo {
    pub run_id: i64,
    pub task_id: i64,
    pub task_title: String,
    pub agent_name: String,
    pub status: task::RunStatus,
    /// 격리 작업 공간이면 중단해도 사용자 폴더에 부분 변경이 남지 않는다
    pub isolated: bool,
}

pub fn active_runs(conn: &rusqlite::Connection) -> AppResult<Vec<ActiveRunInfo>> {
    let mut stmt = conn.prepare(
        "SELECT r.id, r.task_id, t.title, a.name, r.status, t.isolation
         FROM runs r JOIN tasks t ON t.id = r.task_id JOIN agents a ON a.id = r.agent_id
         WHERE r.status IN ('queued','running','waiting_for_approval','cancelling')
         ORDER BY r.id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(ActiveRunInfo {
            run_id: r.get(0)?,
            task_id: r.get(1)?,
            task_title: r.get(2)?,
            agent_name: r.get(3)?,
            status: task::RunStatus::from_db(&r.get::<_, String>(4)?),
            isolated: r.get::<_, String>(5)? == task::ISOLATION_WORKTREE,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

pub fn hide_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.hide();
    }
}

/// 종료 요청: 진행 중인 작업이 없으면 바로 종료, 있으면 창을 띄워 확인을 받는다.
pub fn request_quit(app: &AppHandle) {
    let runs = app
        .state::<Db>()
        .conn()
        .and_then(|c| active_runs(&c))
        .unwrap_or_default();
    if runs.is_empty() {
        allow_and_exit(app);
    } else {
        show_main(app);
        let _ = app.emit(EVENT_QUIT_REQUESTED, &runs);
    }
}

fn allow_and_exit(app: &AppHandle) {
    app.state::<QuitGate>().0.store(true, Ordering::SeqCst);
    app.exit(0);
}

/// 사용자가 "작업 중단 후 종료"를 고른 경우: 중단을 요청하고 잠시 기다린 뒤 종료한다.
pub async fn confirm_quit(app: AppHandle) {
    let exec = app.state::<Arc<Executor>>().inner().clone();
    exec.cancel_all();
    let deadline = Instant::now() + Duration::from_secs(8);
    while exec.active_count() > 0 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    exec.kill_remaining();
    allow_and_exit(&app);
}

pub fn setup_tray(app: &App) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Mefice 열기", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "종료…", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;
    let mut tray = TrayIconBuilder::with_id("main")
        .tooltip("Mefice (미피스)")
        .menu(&menu)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main(app),
            "quit" => request_quit(app),
            _ => {}
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

/// 창 닫기: 트레이 설정이면 숨기고, 아니면 종료 요청으로 처리
pub fn on_window_event(window: &Window, event: &WindowEvent) {
    if let WindowEvent::CloseRequested { api, .. } = event {
        let app = window.app_handle();
        let to_tray = app
            .state::<Db>()
            .conn()
            .and_then(|c| settings::load(&c))
            .map(|s| s.close_to_tray)
            .unwrap_or(true);
        api.prevent_close();
        if to_tray {
            let _ = window.hide();
        } else {
            request_quit(app);
        }
    }
}

pub fn on_run_event(app: &AppHandle, event: RunEvent) {
    match event {
        // Cmd+Q 등: 확인을 거치지 않은 종료는 막고 확인 흐름으로 보낸다
        RunEvent::ExitRequested { api, .. }
            if !app.state::<QuitGate>().0.load(Ordering::SeqCst) =>
        {
            api.prevent_exit();
            request_quit(app);
        }
        RunEvent::Exit => {
            if let Some(exec) = app.try_state::<Arc<Executor>>() {
                exec.kill_remaining();
            }
        }
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => show_main(app),
        _ => {}
    }
}
