//! 고용된 에이전트(설정). 6단계에서 역할 템플릿·고용 UI가 이 위에 올라간다.

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use crate::db::now_ms;
use crate::error::{AppError, AppResult};

/// 런타임 권한 모드. 프로젝트 폴더에서 직접 실행되므로 모든 권한을 우회하는 모드는 제공하지 않는다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionMode {
    /// 파일 수정·명령 실행 시 사용자 승인 (기본)
    Default,
    /// 파일 편집은 자동 허용, 그 외 명령은 승인
    AcceptEdits,
    /// 읽기 전용 계획 모드
    Plan,
}

impl PermissionMode {
    pub fn as_cli(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::AcceptEdits => "acceptEdits",
            Self::Plan => "plan",
        }
    }

    fn from_db(s: &str) -> Self {
        match s {
            "acceptEdits" => Self::AcceptEdits,
            "plan" => Self::Plan,
            _ => Self::Default,
        }
    }
}

/// 에이전트가 쓸 수 있는 런타임 내장 도구 (스펙 4.3 "도구 권한").
/// 도구 목록을 명시하면 사용자 전역 MCP·도구 정의가 프롬프트에 실리지 않아 기본 토큰이 크게 준다.
pub const ALLOWED_TOOLS: &[&str] = &["Read", "Glob", "Grep", "Edit", "Write", "Bash"];

pub fn default_tools() -> Vec<String> {
    ALLOWED_TOOLS.iter().map(|s| s.to_string()).collect()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Agent {
    pub id: i64,
    pub name: String,
    pub role: String,
    pub instructions: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub permission_mode: PermissionMode,
    pub tools: Vec<String>,
    pub color: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentInput {
    pub name: String,
    pub role: String,
    #[serde(default)]
    pub instructions: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub permission_mode: PermissionMode,
    pub tools: Vec<String>,
    pub color: Option<String>,
}

impl AgentInput {
    fn validate(&self) -> AppResult<()> {
        if self.name.trim().is_empty() || self.role.trim().is_empty() {
            return Err(AppError::InvalidInput("이름과 역할은 필수입니다".into()));
        }
        if self.tools.is_empty() {
            return Err(AppError::InvalidInput(
                "도구를 하나 이상 선택해야 합니다".into(),
            ));
        }
        if let Some(bad) = self
            .tools
            .iter()
            .find(|t| !ALLOWED_TOOLS.contains(&t.as_str()))
        {
            return Err(AppError::InvalidInput(format!("지원하지 않는 도구: {bad}")));
        }
        Ok(())
    }
}

fn blank_to_none(v: &Option<String>) -> Option<String> {
    v.as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

const COLS: &str = "id, name, role, instructions, model, effort, permission_mode, tools, color, created_at, updated_at";

fn from_row(r: &Row) -> rusqlite::Result<Agent> {
    let tools: String = r.get("tools")?;
    let mode: String = r.get("permission_mode")?;
    Ok(Agent {
        id: r.get("id")?,
        name: r.get("name")?,
        role: r.get("role")?,
        instructions: r.get("instructions")?,
        model: r.get("model")?,
        effort: r.get("effort")?,
        permission_mode: PermissionMode::from_db(&mode),
        tools: serde_json::from_str(&tools).unwrap_or_else(|_| default_tools()),
        color: r.get("color")?,
        created_at: r.get("created_at")?,
        updated_at: r.get("updated_at")?,
    })
}

pub fn list(conn: &Connection) -> AppResult<Vec<Agent>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM agents WHERE archived_at IS NULL ORDER BY id"
    ))?;
    let rows = stmt.query_map([], from_row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn get(conn: &Connection, id: i64) -> AppResult<Agent> {
    conn.query_row(
        &format!("SELECT {COLS} FROM agents WHERE id = ?1"),
        [id],
        from_row,
    )
    .optional()?
    .ok_or_else(|| AppError::NotFound(format!("에이전트 #{id}")))
}

pub fn create(conn: &Connection, input: &AgentInput) -> AppResult<Agent> {
    input.validate()?;
    let now = now_ms();
    conn.execute(
        "INSERT INTO agents (name, role, instructions, model, effort, permission_mode, tools, color, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
        params![
            input.name.trim(),
            input.role.trim(),
            input.instructions,
            blank_to_none(&input.model),
            blank_to_none(&input.effort),
            input.permission_mode.as_cli(),
            serde_json::to_string(&input.tools)?,
            input.color.clone().unwrap_or_else(|| "#3a6df0".into()),
            now,
        ],
    )?;
    get(conn, conn.last_insert_rowid())
}

/// 설정 변경은 실행 중인 Run에 반영되지 않는다 (Run이 생성 시점 스냅샷을 쓰기 때문, 스펙 4.3).
pub fn update(conn: &Connection, id: i64, input: &AgentInput) -> AppResult<Agent> {
    input.validate()?;
    let changed = conn.execute(
        "UPDATE agents SET name=?1, role=?2, instructions=?3, model=?4, effort=?5, permission_mode=?6,
                tools=?7, color=COALESCE(?8, color), updated_at=?9
         WHERE id=?10 AND archived_at IS NULL",
        params![
            input.name.trim(),
            input.role.trim(),
            input.instructions,
            blank_to_none(&input.model),
            blank_to_none(&input.effort),
            input.permission_mode.as_cli(),
            serde_json::to_string(&input.tools)?,
            input.color,
            now_ms(),
            id,
        ],
    )?;
    if changed == 0 {
        return Err(AppError::NotFound(format!("에이전트 #{id}")));
    }
    get(conn, id)
}

/// 해고는 보관 처리한다 (과거 작업·실행 이력이 에이전트를 참조하므로 삭제하지 않음).
pub fn archive(conn: &Connection, id: i64) -> AppResult<()> {
    conn.execute(
        "UPDATE agents SET archived_at = ?1 WHERE id = ?2",
        params![now_ms(), id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    fn input() -> AgentInput {
        AgentInput {
            name: "민수".into(),
            role: "백엔드".into(),
            instructions: String::new(),
            model: Some("  ".into()),
            effort: None,
            permission_mode: PermissionMode::Default,
            tools: default_tools(),
            color: None,
        }
    }

    #[test]
    fn 에이전트_생성_수정_보관() {
        let db = Db::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let a = create(&conn, &input()).unwrap();
        assert_eq!(a.model, None, "공백 모델은 런타임 기본값(None)으로 저장");
        assert_eq!(a.tools.len(), ALLOWED_TOOLS.len());

        let mut i = input();
        i.permission_mode = PermissionMode::Plan;
        assert_eq!(
            update(&conn, a.id, &i).unwrap().permission_mode,
            PermissionMode::Plan
        );

        archive(&conn, a.id).unwrap();
        assert!(list(&conn).unwrap().is_empty());
        assert!(get(&conn, a.id).is_ok(), "보관해도 이력 조회용으로 남는다");
    }

    #[test]
    fn 허용되지_않은_도구는_거부() {
        let db = Db::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let mut i = input();
        i.tools = vec!["WebFetch".into()];
        assert!(create(&conn, &i).is_err());
    }
}
