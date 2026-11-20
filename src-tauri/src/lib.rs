mod commands;
mod db;
mod env_check;
mod error;
mod project;
mod settings;
mod sysproc;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // macOS: ~/Library/Application Support/dev.mefice.app/mefice.db
            let data_dir = app.path().app_data_dir()?;
            let db = db::Db::open(&data_dir.join("mefice.db"))?;
            app.manage(db);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_app_state,
            commands::open_project,
            commands::refresh_project,
            commands::update_project_instructions,
            commands::update_settings,
            commands::check_environment,
        ])
        .run(tauri::generate_context!())
        .expect("Mefice 실행 중 오류가 발생했습니다");
}

#[cfg(test)]
mod restore_tests {
    use crate::{db::Db, project, settings};

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
}
