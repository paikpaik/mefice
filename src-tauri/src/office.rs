//! 사무실 화면용 상태 (스펙 4.5).
//!
//! 실행 상태 → 캐릭터 표현에 필요한 사실만 DB에서 계산한다. 이동 경로·휴식·표정은 프론트엔드
//! 로컬 로직이 정하며, 어떤 경우에도 모델을 호출하지 않는다.

use rusqlite::{Connection, params};
use serde::Serialize;

use crate::agent::{self, Agent};
use crate::error::AppResult;
use crate::task::{self, RunStatus, TaskSummary};
use crate::usage::{self, UsageTotals};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRef {
    pub run_id: i64,
    pub task_id: i64,
    pub task_title: String,
    pub status: RunStatus,
    pub ended_at: Option<i64>,
}

/// 캐릭터 상태. 시간에 따른 표현(완료 알림 후 휴식 등)은 프론트엔드가 `last.ended_at`으로 정한다.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OfficeState {
    /// 작업 없음 → 산책·휴식·자리 이동
    Idle,
    /// 실행 대기 → 대기 또는 휴식
    Queued,
    /// 작업 중 → 자리에서 작업
    Working,
    /// 사용자 입력·승인 필요 → 질문·승인 아이콘
    NeedsInput,
    /// 마지막 실행이 실패·중단 확인 → 오류 표시
    Error,
    /// 마지막 실행 완료 → 완료 알림 후 휴식
    Done,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OfficeAgent {
    pub agent: Agent,
    pub state: OfficeState,
    /// 진행 중인 실행 (같은 에이전트가 여러 작업을 동시에 맡을 수 있음)
    pub active: Vec<RunRef>,
    /// 마지막으로 끝난 실행
    pub last: Option<RunRef>,
}

fn run_refs(conn: &Connection, sql: &str, agent_id: i64) -> AppResult<Vec<RunRef>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params![agent_id], |r| {
        Ok(RunRef {
            run_id: r.get(0)?,
            task_id: r.get(1)?,
            task_title: r.get(2)?,
            status: RunStatus::from_db(&r.get::<_, String>(3)?),
            ended_at: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn state_of(active: &[RunRef], last: Option<&RunRef>) -> OfficeState {
    use RunStatus::*;
    if active
        .iter()
        .any(|r| matches!(r.status, WaitingForApproval | WaitingForInput))
    {
        OfficeState::NeedsInput
    } else if active
        .iter()
        .any(|r| matches!(r.status, Running | Cancelling))
    {
        OfficeState::Working
    } else if !active.is_empty() {
        OfficeState::Queued
    } else {
        match last.map(|r| r.status) {
            Some(Failed | Interrupted) => OfficeState::Error,
            Some(Succeeded) => OfficeState::Done,
            _ => OfficeState::Idle,
        }
    }
}

pub fn office_state(conn: &Connection) -> AppResult<Vec<OfficeAgent>> {
    let mut out = Vec::new();
    // 대화용 어시스턴트는 사무실 캐릭터가 아니다
    for a in agent::list(conn)?
        .into_iter()
        .filter(|a| a.kind != agent::KIND_ASSISTANT)
    {
        let active = run_refs(
            conn,
            "SELECT r.id, r.task_id, t.title, r.status, r.ended_at FROM runs r JOIN tasks t ON t.id = r.task_id
             WHERE r.agent_id = ?1 AND r.status IN ('queued','running','waiting_for_input','waiting_for_approval','cancelling')
             ORDER BY r.id",
            a.id,
        )?;
        let last = run_refs(
            conn,
            "SELECT r.id, r.task_id, t.title, r.status, r.ended_at FROM runs r JOIN tasks t ON t.id = r.task_id
             WHERE r.agent_id = ?1 AND r.ended_at IS NOT NULL ORDER BY r.ended_at DESC LIMIT 1",
            a.id,
        )?
        .pop();
        let state = state_of(&active, last.as_ref());
        out.push(OfficeAgent {
            agent: a,
            state,
            active,
            last,
        });
    }
    Ok(out)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentOverview {
    pub agent: Agent,
    /// 최근 맡은 작업 (최신순)
    pub recent: Vec<TaskSummary>,
    pub usage: UsageTotals,
}

pub fn agent_overview(conn: &Connection, agent_id: i64) -> AppResult<AgentOverview> {
    let agent = agent::get(conn, agent_id)?;
    let ids: Vec<i64> = conn
        .prepare("SELECT id FROM tasks WHERE agent_id = ?1 ORDER BY id DESC LIMIT 10")?
        .query_map([agent_id], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    let mut recent = Vec::new();
    for id in ids {
        let t = task::get_task(conn, id)?;
        recent.push(TaskSummary {
            latest_run: task::latest_run(conn, t.id)?,
            depends_on: task::dependencies(conn, t.id)?,
            agent_name: agent.name.clone(),
            task: t,
        });
    }
    Ok(AgentOverview {
        usage: usage::agent_usage(conn, agent_id)?,
        agent,
        recent,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(status: RunStatus) -> RunRef {
        RunRef {
            run_id: 1,
            task_id: 1,
            task_title: "t".into(),
            status,
            ended_at: Some(1),
        }
    }

    #[test]
    fn 실행_상태를_캐릭터_상태로_매핑한다() {
        use RunStatus::*;
        assert_eq!(state_of(&[], None), OfficeState::Idle);
        assert_eq!(state_of(&[r(Queued)], None), OfficeState::Queued);
        assert_eq!(
            state_of(&[r(Queued), r(Running)], None),
            OfficeState::Working
        );
        assert_eq!(
            state_of(&[r(Running), r(WaitingForApproval)], None),
            OfficeState::NeedsInput,
            "입력 필요가 가장 우선"
        );
        assert_eq!(state_of(&[], Some(&r(Failed))), OfficeState::Error);
        assert_eq!(state_of(&[], Some(&r(Interrupted))), OfficeState::Error);
        assert_eq!(state_of(&[], Some(&r(Succeeded))), OfficeState::Done);
        assert_eq!(
            state_of(&[], Some(&r(Cancelled))),
            OfficeState::Idle,
            "사용자 중단은 오류가 아님"
        );
    }
}
