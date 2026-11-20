mod agent;
mod commands;
mod db;
mod env_check;
mod error;
mod executor;
mod project;
mod runtime;
mod settings;
mod sysproc;
mod task;
mod usage;

use tauri::Manager;

/// 앱 시작 시 복구 (스펙 9장).
/// - 이전 실행에서 끝나지 않은 Run은 실제 상태를 확인할 수 없으므로 interrupted로 표시한다 (자동 재실행 안 함).
/// - 종료 처리를 못 한 Run의 사용량은 "미제공"으로 남긴다.
fn recover_on_startup(conn: &rusqlite::Connection) -> error::AppResult<()> {
    task::interrupt_unfinished_runs(conn)?;
    usage::backfill_unavailable(conn)?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // macOS: ~/Library/Application Support/dev.mefice.app/mefice.db
            let data_dir = app.path().app_data_dir()?;
            let db = db::Db::open(&data_dir.join("mefice.db"))?;
            recover_on_startup(&*db.conn()?)?;
            app.manage(db);
            app.manage(executor::Executor::new(app.handle().clone()));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_app_state,
            commands::open_project,
            commands::refresh_project,
            commands::update_project_instructions,
            commands::update_settings,
            commands::check_environment,
            commands::list_agents,
            commands::create_agent,
            commands::update_agent,
            commands::archive_agent,
            commands::start_direct_task,
            commands::rerun_task,
            commands::continue_task,
            commands::list_tasks,
            commands::get_task_detail,
            commands::get_run_events,
            commands::get_pending_approvals,
            commands::send_run_message,
            commands::cancel_run,
            commands::respond_approval,
            commands::get_usage_summary,
            commands::get_task_usage,
            commands::get_rate_limit,
        ])
        .run(tauri::generate_context!())
        .expect("Mefice 실행 중 오류가 발생했습니다");
}

#[cfg(test)]
mod restore_tests {
    use super::recover_on_startup;
    use crate::{db::Db, project, settings, task, usage};

    /// 1단계 완료 기준: 앱 재실행(= DB 파일을 닫고 다시 열기) 후 설정·프로젝트·지침이 복원된다.
    #[test]
    fn 재실행후_설정과_프로젝트_복원() {
        let data = tempfile::tempdir().unwrap();
        let proj_dir = tempfile::tempdir().unwrap();
        let db_path = data.path().join("nested/mefice.db");

        let project_id = {
            let db = Db::open(&db_path).unwrap();
            let conn = db.conn().unwrap();
            let path = project::normalize_path(proj_dir.path().to_str().unwrap()).unwrap();
            let p = project::upsert(&conn, &path).unwrap();
            project::update_instructions(&conn, p.id, "pnpm 사용").unwrap();
            settings::set_current_project_id(&conn, p.id).unwrap();
            settings::save(
                &conn,
                &settings::AppSettings {
                    max_concurrency: 4,
                    close_to_tray: false,
                },
            )
            .unwrap();
            p.id
        }; // 여기서 연결이 닫힘 = 앱 종료

        let db = Db::open(&db_path).unwrap();
        let conn = db.conn().unwrap();
        let s = settings::load(&conn).unwrap();
        assert_eq!(s.max_concurrency, 4);
        assert!(!s.close_to_tray);
        assert_eq!(
            settings::current_project_id(&conn).unwrap(),
            Some(project_id)
        );
        let p = project::get(&conn, project_id).unwrap().unwrap();
        assert_eq!(p.instructions, "pnpm 사용");
    }

    /// 3단계 완료 기준: 앱이 실행 도중 강제 종료되어도, 새로 열면 이력·사용량을 확인할 수 있고
    /// 확인되지 않는 실행은 진행 중으로 표시되지 않는다.
    #[test]
    fn 강제종료후_재실행시_이력과_사용량_복원() {
        use crate::agent::{self, AgentInput, PermissionMode};
        use crate::runtime::{RunConfig, RunEvent, SessionTotals, Usage};
        use crate::task::RunStatus;

        let data = tempfile::tempdir().unwrap();
        let proj_dir = tempfile::tempdir().unwrap();
        let db_path = data.path().join("mefice.db");

        let (done_run, crashed_run, project_id) = {
            let db = Db::open(&db_path).unwrap();
            let conn = db.conn().unwrap();
            let path = project::normalize_path(proj_dir.path().to_str().unwrap()).unwrap();
            let p = project::upsert(&conn, &path).unwrap();
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
                },
            )
            .unwrap();
            let cfg = RunConfig {
                agent_name: "a".into(),
                agent_role: "r".into(),
                model: None,
                effort: None,
                permission_mode: PermissionMode::Default,
                tools: vec![],
                system_append: String::new(),
            };
            let mk = |prompt: &str| {
                let t = task::create_task(&conn, p.id, a.id, prompt).unwrap();
                task::create_run(
                    &conn,
                    task::NewRun {
                        task_id: t.id,
                        agent_id: a.id,
                        prompt,
                        resume_session_id: None,
                        config: &cfg,
                        cwd: &p.path,
                    },
                )
                .unwrap()
                .id
            };
            // 정상 완료된 Run
            let r1 = mk("끝난 작업");
            task::mark_run_started(&conn, r1, "s1").unwrap();
            task::append_event(
                &conn,
                r1,
                &RunEvent::Text {
                    text: "결과".into(),
                },
            )
            .unwrap();
            let totals = SessionTotals {
                input_tokens: 10,
                output_tokens: 5,
                cost_usd: Some(0.01),
                ..Default::default()
            };
            usage::record_turn(
                &conn,
                r1,
                Some("s1"),
                "worker",
                &Usage::default(),
                Some(&totals),
            )
            .unwrap();
            task::finish_run(&conn, r1, RunStatus::Succeeded, Some(0), None).unwrap();
            // 실행 도중 앱이 죽은 Run (running 상태 그대로, 사용량 없음)
            let r2 = mk("도중에 죽은 작업");
            task::mark_run_started(&conn, r2, "s2").unwrap();
            task::append_event(
                &conn,
                r2,
                &RunEvent::Text {
                    text: "작업 중".into(),
                },
            )
            .unwrap();
            (r1, r2, p.id)
        }; // 강제 종료

        let db = Db::open(&db_path).unwrap();
        let conn = db.conn().unwrap();
        recover_on_startup(&conn).unwrap();

        assert_eq!(
            task::get_run(&conn, done_run).unwrap().status,
            RunStatus::Succeeded
        );
        assert_eq!(
            task::get_run(&conn, crashed_run).unwrap().status,
            RunStatus::Interrupted
        );
        assert_eq!(task::list_events(&conn, done_run).unwrap().len(), 1);
        assert_eq!(task::list_events(&conn, crashed_run).unwrap().len(), 1);
        assert_eq!(task::list_tasks(&conn, project_id).unwrap().len(), 2);

        let s = usage::summary(&conn, project_id, None).unwrap();
        assert_eq!((s.totals.reported, s.totals.unavailable), (1, 1));
        assert_eq!(s.totals.input_tokens, 10);

        // 복구를 다시 돌려도 중복 기록되지 않는다
        recover_on_startup(&conn).unwrap();
        assert_eq!(
            usage::summary(&conn, project_id, None)
                .unwrap()
                .totals
                .unavailable,
            1
        );
    }
}
