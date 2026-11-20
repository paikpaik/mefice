//! 사용량 기록·집계 (스펙 5장).
//!
//! - 런타임이 주는 값만 기록한다. 없으면 `unavailable`로 남기고 추정하지 않는다.
//! - Claude Code의 `modelUsage`·`total_cost_usd`는 세션 누적값이다 (`--resume` 후에도 이어짐).
//!   그래서 같은 세션의 직전 기록과의 차이를 그 턴의 사용량으로 본다. 이어서 실행해도 이중 집계되지 않는다.
//! - 비용은 런타임이 계산한 API 정가 환산값이다. 구독 로그인 사용 시 실제 청구액이 아니다.

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::db::now_ms;
use crate::error::AppResult;
use crate::runtime::{RateLimitSnapshot, SessionTotals, Usage};

pub const SOURCE_REPORTED: &str = "reported";
pub const SOURCE_PARTIAL: &str = "partial";
pub const SOURCE_UNAVAILABLE: &str = "unavailable";

const KEY_RATE_LIMIT: &str = "rate_limit_snapshot";

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageTotals {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    /// 비용 값이 하나도 없으면 None ("미제공"과 "0원"을 구분)
    pub cost_usd: Option<f64>,
    /// 출처별 기록 수
    pub reported: i64,
    pub partial: i64,
    pub unavailable: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunUsage {
    pub run_id: i64,
    pub attempt: i64,
    pub role: String,
    pub totals: UsageTotals,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Grouped {
    pub key: String,
    pub label: String,
    pub totals: UsageTotals,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSummary {
    pub totals: UsageTotals,
    pub by_role: Vec<Grouped>,
    pub by_agent: Vec<Grouped>,
    pub run_count: i64,
    /// 이 기간 실행에서 관측된 인증 방식 (subscription / api_key)
    pub auth_kinds: Vec<String>,
}

fn sub(cur: u64, base: Option<i64>) -> i64 {
    let cur = cur as i64;
    match base {
        // 누적값이 줄었다면 세션이 바뀐 것 → 현재 누적값 전체를 이 턴의 값으로 본다
        Some(b) if b <= cur => cur - b,
        _ => cur,
    }
}

/// 한 턴의 사용량을 기록한다.
pub fn record_turn(
    conn: &Connection,
    run_id: i64,
    session_id: Option<&str>,
    role: &str,
    turn: &Usage,
    totals: Option<&SessionTotals>,
) -> AppResult<()> {
    let now = now_ms();
    let Some(t) = totals else {
        // 누적값이 없으면 턴 사용량만 기록 (내부 보조 호출은 빠짐)
        let g = |v: Option<u64>| v.map(|x| x as i64);
        conn.execute(
            "INSERT INTO usage_records (run_id, session_id, role, source, input_tokens, output_tokens,
                 cache_read_tokens, cache_creation_tokens, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                run_id,
                session_id,
                role,
                SOURCE_PARTIAL,
                g(turn.input_tokens),
                g(turn.output_tokens),
                g(turn.cache_read_tokens),
                g(turn.cache_creation_tokens),
                now
            ],
        )?;
        return Ok(());
    };

    // 같은 세션의 직전 누적값 (다른 Run에서 기록된 것 포함 → 이어서 실행 시 이중 집계 방지)
    type Base = (
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<f64>,
    );
    let base: Option<Base> = match session_id {
        Some(sid) => conn
            .query_row(
                "SELECT cum_input_tokens, cum_output_tokens, cum_cache_read_tokens, cum_cache_creation_tokens, cum_cost_usd
                 FROM usage_records WHERE session_id = ?1 AND source = 'reported' ORDER BY id DESC LIMIT 1",
                [sid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?,
        None => None,
    };
    let (bi, bo, br, bc, bcost) = base.unwrap_or((None, None, None, None, None));
    let cost = t.cost_usd.map(|c| match bcost {
        Some(b) if b <= c => c - b,
        _ => c,
    });

    conn.execute(
        "INSERT INTO usage_records (run_id, session_id, role, source,
             input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens, cost_usd,
             cum_input_tokens, cum_output_tokens, cum_cache_read_tokens, cum_cache_creation_tokens, cum_cost_usd,
             models, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        params![
            run_id,
            session_id,
            role,
            SOURCE_REPORTED,
            sub(t.input_tokens, bi),
            sub(t.output_tokens, bo),
            sub(t.cache_read_tokens, br),
            sub(t.cache_creation_tokens, bc),
            cost,
            t.input_tokens as i64,
            t.output_tokens as i64,
            t.cache_read_tokens as i64,
            t.cache_creation_tokens as i64,
            t.cost_usd,
            t.models.join(","),
            now
        ],
    )?;
    Ok(())
}

/// 결과 없이 끝난 Run(강제 종료, 실행 실패 등)은 "미제공"으로 남긴다.
pub fn mark_unavailable_if_empty(conn: &Connection, run_id: i64, role: &str) -> AppResult<()> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM usage_records WHERE run_id = ?1)",
        [run_id],
        |r| r.get(0),
    )?;
    if !exists {
        conn.execute(
            "INSERT INTO usage_records (run_id, role, source, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![run_id, role, SOURCE_UNAVAILABLE, now_ms()],
        )?;
    }
    Ok(())
}

/// 앱 시작 시: 종료 처리를 못 하고 끝난 Run(앱 강제 종료 → interrupted 등)에 "미제공" 기록을 채운다.
pub fn backfill_unavailable(conn: &Connection) -> AppResult<usize> {
    Ok(conn.execute(
        "INSERT INTO usage_records (run_id, role, source, created_at)
         SELECT r.id, r.role, 'unavailable', ?1 FROM runs r
         WHERE r.status IN ('succeeded','failed','cancelled','interrupted')
           AND NOT EXISTS (SELECT 1 FROM usage_records u WHERE u.run_id = r.id)",
        [now_ms()],
    )?)
}

const TOTALS_SELECT: &str = "
    COALESCE(SUM(u.input_tokens), 0), COALESCE(SUM(u.output_tokens), 0),
    COALESCE(SUM(u.cache_read_tokens), 0), COALESCE(SUM(u.cache_creation_tokens), 0),
    SUM(u.cost_usd),
    COALESCE(SUM(u.source = 'reported'), 0), COALESCE(SUM(u.source = 'partial'), 0),
    COALESCE(SUM(u.source = 'unavailable'), 0)";

fn totals_from_row(r: &rusqlite::Row, offset: usize) -> rusqlite::Result<UsageTotals> {
    Ok(UsageTotals {
        input_tokens: r.get(offset)?,
        output_tokens: r.get(offset + 1)?,
        cache_read_tokens: r.get(offset + 2)?,
        cache_creation_tokens: r.get(offset + 3)?,
        cost_usd: r.get(offset + 4)?,
        reported: r.get(offset + 5)?,
        partial: r.get(offset + 6)?,
        unavailable: r.get(offset + 7)?,
    })
}

/// 작업의 실행별 사용량
pub fn task_usage(conn: &Connection, task_id: i64) -> AppResult<Vec<RunUsage>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT r.id, r.attempt, r.role, {TOTALS_SELECT}
         FROM runs r LEFT JOIN usage_records u ON u.run_id = r.id
         WHERE r.task_id = ?1 GROUP BY r.id ORDER BY r.id"
    ))?;
    let rows = stmt.query_map([task_id], |r| {
        Ok(RunUsage {
            run_id: r.get(0)?,
            attempt: r.get(1)?,
            role: r.get(2)?,
            totals: totals_from_row(r, 3)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// 프로젝트 사용량 요약. `since_ms`가 있으면 그 이후 기록만.
pub fn summary(
    conn: &Connection,
    project_id: i64,
    since_ms: Option<i64>,
) -> AppResult<UsageSummary> {
    let since = since_ms.unwrap_or(0);
    let from =
        "FROM usage_records u JOIN runs r ON r.id = u.run_id JOIN tasks t ON t.id = r.task_id
                JOIN agents a ON a.id = r.agent_id
                WHERE t.project_id = ?1 AND u.created_at >= ?2";

    let totals = conn.query_row(
        &format!("SELECT {TOTALS_SELECT} {from}"),
        params![project_id, since],
        |r| totals_from_row(r, 0),
    )?;

    let grouped = |key_col: &str, label_col: &str| -> AppResult<Vec<Grouped>> {
        let mut stmt = conn.prepare(&format!(
            "SELECT {key_col}, {label_col}, {TOTALS_SELECT} {from}
             GROUP BY {key_col} ORDER BY SUM(u.cost_usd) DESC NULLS LAST, {key_col}"
        ))?;
        let rows = stmt.query_map(params![project_id, since], |r| {
            Ok(Grouped {
                key: r.get::<_, rusqlite::types::Value>(0).map(|v| match v {
                    rusqlite::types::Value::Integer(i) => i.to_string(),
                    rusqlite::types::Value::Text(s) => s,
                    _ => String::new(),
                })?,
                label: r.get(1)?,
                totals: totals_from_row(r, 2)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    };
    let by_role = grouped("u.role", "u.role")?;
    let by_agent = grouped("a.id", "a.name")?;

    let run_count = conn.query_row(
        &format!("SELECT COUNT(DISTINCT u.run_id) {from}"),
        params![project_id, since],
        |r| r.get(0),
    )?;
    let mut stmt = conn.prepare(
        "SELECT DISTINCT r.auth_kind FROM runs r JOIN tasks t ON t.id = r.task_id
         WHERE t.project_id = ?1 AND r.created_at >= ?2 AND r.auth_kind IS NOT NULL",
    )?;
    let auth_kinds = stmt
        .query_map(params![project_id, since], |r| r.get(0))?
        .collect::<Result<_, _>>()?;

    Ok(UsageSummary {
        totals,
        by_role,
        by_agent,
        run_count,
        auth_kinds,
    })
}

// ---------- 구독 한도 사용률 (계정 전체) ----------

pub fn save_rate_limit(conn: &Connection, snapshot: &RateLimitSnapshot) -> AppResult<()> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![KEY_RATE_LIMIT, serde_json::to_string(snapshot)?],
    )?;
    Ok(())
}

pub fn load_rate_limit(conn: &Connection) -> AppResult<Option<RateLimitSnapshot>> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            [KEY_RATE_LIMIT],
            |r| r.get(0),
        )
        .optional()?;
    Ok(raw.and_then(|s| serde_json::from_str(&s).ok()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{self, AgentInput, PermissionMode};
    use crate::db::Db;
    use crate::runtime::RunConfig;
    use crate::task;

    fn totals(i: u64, o: u64, cost: f64) -> SessionTotals {
        SessionTotals {
            input_tokens: i,
            output_tokens: o,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            cost_usd: Some(cost),
            models: vec!["m".into()],
        }
    }

    /// 프로젝트·에이전트·작업 하나와 Run 두 개(두 번째는 이어서 실행)를 만든다.
    fn setup(conn: &Connection) -> (i64, i64, i64, i64) {
        let dir = tempfile::tempdir().unwrap();
        let p = crate::project::upsert(conn, dir.path()).unwrap();
        let a = agent::create(
            conn,
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
        let t = task::create_task(conn, p.id, a.id, "x").unwrap();
        let mk = |resume| {
            task::create_run(
                conn,
                task::NewRun {
                    task_id: t.id,
                    agent_id: a.id,
                    prompt: "x",
                    resume_session_id: resume,
                    config: &cfg,
                    cwd: "/tmp",
                },
            )
            .unwrap()
            .id
        };
        let r1 = mk(None);
        let r2 = mk(Some("s1"));
        (p.id, t.id, r1, r2)
    }

    #[test]
    fn 누적값_차이로_기록하고_이어서_실행해도_이중집계_안함() {
        let db = Db::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let (pid, tid, r1, r2) = setup(&conn);
        let turn = Usage::default();

        // Run1: 두 턴 (누적 100→150 입력, 비용 0.01→0.015)
        record_turn(
            &conn,
            r1,
            Some("s1"),
            "worker",
            &turn,
            Some(&totals(100, 10, 0.01)),
        )
        .unwrap();
        record_turn(
            &conn,
            r1,
            Some("s1"),
            "worker",
            &turn,
            Some(&totals(150, 12, 0.015)),
        )
        .unwrap();
        // Run2: 같은 세션을 이어서 실행 (누적 200)
        record_turn(
            &conn,
            r2,
            Some("s1"),
            "worker",
            &turn,
            Some(&totals(200, 20, 0.02)),
        )
        .unwrap();

        let per_run = task_usage(&conn, tid).unwrap();
        assert_eq!(per_run[0].totals.input_tokens, 150);
        assert_eq!(
            per_run[1].totals.input_tokens, 50,
            "재개 Run은 이전 누적과의 차이만"
        );
        assert!((per_run[1].totals.cost_usd.unwrap() - 0.005).abs() < 1e-9);

        let s = summary(&conn, pid, None).unwrap();
        assert_eq!(s.totals.input_tokens, 200, "합계 = 세션 최종 누적값");
        assert!((s.totals.cost_usd.unwrap() - 0.02).abs() < 1e-9);
        assert_eq!(s.totals.reported, 3);
        assert_eq!(s.run_count, 2);
        assert_eq!(s.by_role[0].key, "worker");
    }

    #[test]
    fn 결과_없이_끝난_run은_미제공으로_구분() {
        let db = Db::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let (pid, _, r1, r2) = setup(&conn);
        record_turn(
            &conn,
            r1,
            Some("s1"),
            "worker",
            &Usage::default(),
            Some(&totals(10, 1, 0.001)),
        )
        .unwrap();
        mark_unavailable_if_empty(&conn, r1, "worker").unwrap(); // 이미 기록 있음 → 무시
        mark_unavailable_if_empty(&conn, r2, "worker").unwrap();

        let s = summary(&conn, pid, None).unwrap();
        assert_eq!((s.totals.reported, s.totals.unavailable), (1, 1));
        assert_eq!(
            s.totals.input_tokens, 10,
            "미제공은 0으로 더하지 않고 개수만 센다"
        );
    }

    #[test]
    fn 누적값이_없으면_턴_사용량을_부분_기록() {
        let db = Db::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let (pid, _, r1, _) = setup(&conn);
        let turn = Usage {
            input_tokens: Some(7),
            output_tokens: Some(3),
            ..Default::default()
        };
        record_turn(&conn, r1, Some("s1"), "worker", &turn, None).unwrap();
        let s = summary(&conn, pid, None).unwrap();
        assert_eq!((s.totals.partial, s.totals.input_tokens), (1, 7));
        assert_eq!(s.totals.cost_usd, None, "비용 미제공은 0이 아니라 None");
    }

    #[test]
    fn 기간_필터() {
        let db = Db::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let (pid, _, r1, _) = setup(&conn);
        record_turn(
            &conn,
            r1,
            Some("s1"),
            "worker",
            &Usage::default(),
            Some(&totals(10, 1, 0.001)),
        )
        .unwrap();
        let future = now_ms() + 60_000;
        assert_eq!(
            summary(&conn, pid, Some(future)).unwrap().totals.reported,
            0
        );
    }
}
