use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};

/// 동시 실행 상한의 허용 범위. 스펙 4.4: 초기값 2, PM 호출도 포함.
pub const MIN_CONCURRENCY: u32 = 1;
pub const MAX_CONCURRENCY: u32 = 8;

const KEY_APP_SETTINGS: &str = "app_settings";
const KEY_CURRENT_PROJECT: &str = "current_project_id";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppSettings {
    /// PM 호출을 포함한 모델 실행 동시 상한
    pub max_concurrency: u32,
    /// 창 닫기 시 트레이에 남아 작업을 계속할지 여부 (스펙 10장 기본 제안)
    pub close_to_tray: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            max_concurrency: 2,
            close_to_tray: true,
        }
    }
}

impl AppSettings {
    pub fn validate(&self) -> AppResult<()> {
        if !(MIN_CONCURRENCY..=MAX_CONCURRENCY).contains(&self.max_concurrency) {
            return Err(AppError::InvalidInput(format!(
                "동시 실행 상한은 {MIN_CONCURRENCY}~{MAX_CONCURRENCY} 사이여야 합니다"
            )));
        }
        Ok(())
    }
}

fn get_raw(conn: &Connection, key: &str) -> AppResult<Option<String>> {
    Ok(conn
        .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
            r.get(0)
        })
        .optional()?)
}

fn set_raw(conn: &Connection, key: &str, value: &str) -> AppResult<()> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

/// 저장된 설정을 읽는다. 없거나 일부 필드가 빠져 있으면 기본값으로 채운다.
pub fn load(conn: &Connection) -> AppResult<AppSettings> {
    match get_raw(conn, KEY_APP_SETTINGS)? {
        Some(json) => Ok(serde_json::from_str(&json).unwrap_or_default()),
        None => Ok(AppSettings::default()),
    }
}

pub fn save(conn: &Connection, settings: &AppSettings) -> AppResult<()> {
    settings.validate()?;
    set_raw(conn, KEY_APP_SETTINGS, &serde_json::to_string(settings)?)
}

pub fn current_project_id(conn: &Connection) -> AppResult<Option<i64>> {
    Ok(get_raw(conn, KEY_CURRENT_PROJECT)?.and_then(|v| v.parse().ok()))
}

pub fn set_current_project_id(conn: &Connection, id: i64) -> AppResult<()> {
    set_raw(conn, KEY_CURRENT_PROJECT, &id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    #[test]
    fn 설정이_없으면_기본값() {
        let db = Db::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        assert_eq!(load(&conn).unwrap(), AppSettings::default());
        assert_eq!(current_project_id(&conn).unwrap(), None);
    }

    #[test]
    fn 설정_저장후_복원() {
        let db = Db::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let s = AppSettings {
            max_concurrency: 3,
            close_to_tray: false,
        };
        save(&conn, &s).unwrap();
        assert_eq!(load(&conn).unwrap(), s);
    }

    #[test]
    fn 범위_밖_동시실행수는_거부() {
        let db = Db::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let s = AppSettings {
            max_concurrency: 0,
            ..Default::default()
        };
        assert!(save(&conn, &s).is_err());
    }
}
