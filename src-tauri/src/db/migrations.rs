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
    // v3: 사용량 (3단계: 이벤트·이력·사용량)
    r#"
    ALTER TABLE runs ADD COLUMN role TEXT NOT NULL DEFAULT 'worker';
    ALTER TABLE runs ADD COLUMN auth_kind TEXT;

    -- 턴마다 1행. source: reported(세션 누적 차이, 내부 호출 포함)
    --                    partial(턴 사용량만, 내부 호출 제외)
    --                    unavailable(런타임이 수치를 주지 않음)
    CREATE TABLE usage_records (
        id                        INTEGER PRIMARY KEY AUTOINCREMENT,
        run_id                    INTEGER NOT NULL REFERENCES runs(id),
        session_id                TEXT,
        role                      TEXT    NOT NULL,
        source                    TEXT    NOT NULL,
        input_tokens              INTEGER,
        output_tokens             INTEGER,
        cache_read_tokens         INTEGER,
        cache_creation_tokens     INTEGER,
        cost_usd                  REAL,
        cum_input_tokens          INTEGER,
        cum_output_tokens         INTEGER,
        cum_cache_read_tokens     INTEGER,
        cum_cache_creation_tokens INTEGER,
        cum_cost_usd              REAL,
        models                    TEXT,
        created_at                INTEGER NOT NULL
    );
    CREATE INDEX idx_usage_run ON usage_records(run_id);
    CREATE INDEX idx_usage_session ON usage_records(session_id, id);
    CREATE INDEX idx_usage_created ON usage_records(created_at);
    "#,
    // v4: PM과 단일 위임 (4단계)
    r#"
    -- pm | worker. PM은 프로젝트당 1명, 위임 대상에서 제외
    ALTER TABLE agents ADD COLUMN kind TEXT NOT NULL DEFAULT 'worker';
    -- 위임 작업 명세 (목표·범위·입력·완료 조건 JSON)
    ALTER TABLE tasks ADD COLUMN spec TEXT;
    CREATE INDEX idx_tasks_parent ON tasks(parent_id);

    -- PM 대화. 목표(goal) = mode 'pm'인 Task 하나
    CREATE TABLE messages (
        id           INTEGER PRIMARY KEY AUTOINCREMENT,
        project_id   INTEGER NOT NULL REFERENCES projects(id),
        goal_task_id INTEGER NOT NULL REFERENCES tasks(id),
        author       TEXT    NOT NULL,   -- user | pm | system
        text         TEXT    NOT NULL,
        payload      TEXT,               -- PM 결정 JSON 등
        run_id       INTEGER REFERENCES runs(id),
        created_at   INTEGER NOT NULL
    );
    CREATE INDEX idx_messages_goal ON messages(goal_task_id, id);
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
