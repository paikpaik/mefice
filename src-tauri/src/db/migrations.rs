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
    // v2: 에이전트, 작업, 실행, 실행 이벤트, 승인 (2단계: 런타임 어댑터와 직접 실행)
    r#"
    CREATE TABLE agents (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        name            TEXT    NOT NULL,
        role            TEXT    NOT NULL,
        instructions    TEXT    NOT NULL DEFAULT '',
        model           TEXT,
        effort          TEXT,
        permission_mode TEXT    NOT NULL DEFAULT 'default',
        tools           TEXT    NOT NULL DEFAULT '[]',
        color           TEXT    NOT NULL DEFAULT '#3a6df0',
        created_at      INTEGER NOT NULL,
        updated_at      INTEGER NOT NULL,
        archived_at     INTEGER
    );

    CREATE TABLE tasks (
        id         INTEGER PRIMARY KEY AUTOINCREMENT,
        project_id INTEGER NOT NULL REFERENCES projects(id),
        agent_id   INTEGER NOT NULL REFERENCES agents(id),
        parent_id  INTEGER REFERENCES tasks(id),
        mode       TEXT    NOT NULL DEFAULT 'direct',
        title      TEXT    NOT NULL,
        prompt     TEXT    NOT NULL,
        created_at INTEGER NOT NULL
    );
    CREATE INDEX idx_tasks_project ON tasks(project_id, id);

    CREATE TABLE runs (
        id                INTEGER PRIMARY KEY AUTOINCREMENT,
        task_id           INTEGER NOT NULL REFERENCES tasks(id),
        agent_id          INTEGER NOT NULL REFERENCES agents(id),
        attempt           INTEGER NOT NULL,
        status            TEXT    NOT NULL,
        prompt            TEXT    NOT NULL,
        resume_session_id TEXT,
        session_id        TEXT,
        config_snapshot   TEXT    NOT NULL,
        cwd               TEXT    NOT NULL,
        pid               INTEGER,
        exit_code         INTEGER,
        error             TEXT,
        created_at        INTEGER NOT NULL,
        started_at        INTEGER,
        ended_at          INTEGER
    );
    CREATE INDEX idx_runs_task ON runs(task_id, id);
    CREATE INDEX idx_runs_status ON runs(status);

    CREATE TABLE run_events (
        id         INTEGER PRIMARY KEY AUTOINCREMENT,
        run_id     INTEGER NOT NULL REFERENCES runs(id),
        seq        INTEGER NOT NULL,
        kind       TEXT    NOT NULL,
        payload    TEXT    NOT NULL,
        created_at INTEGER NOT NULL,
        UNIQUE (run_id, seq)
    );

    CREATE TABLE approvals (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        run_id      INTEGER NOT NULL REFERENCES runs(id),
        request_id  TEXT    NOT NULL,
        tool_name   TEXT    NOT NULL,
        tool_input  TEXT    NOT NULL,
        description TEXT,
        status      TEXT    NOT NULL DEFAULT 'pending',
        created_at  INTEGER NOT NULL,
        decided_at  INTEGER
    );
    CREATE INDEX idx_approvals_run ON approvals(run_id, status);
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
