use rusqlite::Connection;

use crate::error::AppResult;

/// 순차 마이그레이션 목록. 인덱스+1 이 `PRAGMA user_version` 값이 된다.
/// 이미 배포된 항목은 절대 수정하지 말고 새 항목을 뒤에 추가한다.
const MIGRATIONS: &[&str] = &[
    // v1: 앱 설정, 프로젝트
    r#"
    CREATE TABLE settings (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );

    CREATE TABLE projects (
        id             INTEGER PRIMARY KEY AUTOINCREMENT,
        path           TEXT    NOT NULL UNIQUE,
        name           TEXT    NOT NULL,
        instructions   TEXT    NOT NULL DEFAULT '',
        created_at     INTEGER NOT NULL,
        updated_at     INTEGER NOT NULL,
        last_opened_at INTEGER NOT NULL
    );
    "#,
];

pub fn migrate(conn: &mut Connection) -> AppResult<()> {
    let current: usize =
        conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))? as usize;

    for (idx, sql) in MIGRATIONS.iter().enumerate().skip(current) {
        let version = idx + 1;
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", version as i64)?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 마이그레이션은_멱등이다() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        migrate(&mut conn).unwrap();
        let v: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(v as usize, MIGRATIONS.len());
    }
}
