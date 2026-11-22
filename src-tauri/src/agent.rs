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
    /// pm | worker
    pub kind: String,
    pub profile: AgentProfile,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Agent {
    pub fn is_pm(&self) -> bool {
        self.kind == KIND_PM
    }

    /// 파일을 바꿀 수 있는 설정인지 (작업 공간 격리 대상 판단)
    pub fn can_write(&self) -> bool {
        self.permission_mode != PermissionMode::Plan
            && self
                .tools
                .iter()
                .any(|t| matches!(t.as_str(), "Edit" | "Write" | "Bash"))
    }
}

pub const KIND_PM: &str = "pm";

/// 사무실 책상 수 (PM 자리는 별도)
pub const DESK_COUNT: i64 = 10;

/// 실행 제한 (스펙 4.3). 넘으면 실행을 멈춘다.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Limits {
    /// 실행 시간 (분). 실행 관리자가 타이머로 중단
    pub max_minutes: Option<u32>,
    /// 사용량 상한 (API 정가 환산 USD). 런타임 `--max-budget-usd`
    pub max_budget_usd: Option<f64>,
    /// 에이전트 턴 수 (도구 호출 반복 횟수 상한). 런타임 `--max-turns`
    pub max_turns: Option<u32>,
}

/// 고용 프로필: 외형·자리·전문 분야·수행 능력·작업 범위·제한
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentProfile {
    pub template_id: Option<String>,
    /// 캐릭터 형태 (프론트엔드 스프라이트 변형 id)
    pub avatar: String,
    /// 책상 번호 (0..DESK_COUNT). None이면 고용 시 빈자리 자동 배정
    pub desk: Option<i64>,
    pub specialties: Vec<String>,
    pub capabilities: Vec<String>,
    /// 작업 범위 (프로젝트 기준 폴더). 지침으로 전달하며 권한으로 강제하지는 않는다
    pub scope_paths: Vec<String>,
    pub limits: Limits,
}

pub const CAPABILITY_REVIEW: &str = "코드 리뷰";

impl Agent {
    /// 사용량 구분 역할 (스펙 5장: PM / 작업자 / 선택적 검토)
    pub fn usage_role(&self) -> &'static str {
        if self.is_pm() {
            "pm"
        } else if self
            .profile
            .capabilities
            .iter()
            .any(|c| c == CAPABILITY_REVIEW)
        {
            "review"
        } else {
            "worker"
        }
    }
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
    #[serde(default)]
    pub profile: AgentProfile,
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

const COLS: &str = "id, name, role, instructions, model, effort, permission_mode, tools, color, kind, profile, created_at, updated_at";

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
        kind: r.get("kind")?,
        profile: serde_json::from_str(&r.get::<_, String>("profile")?).unwrap_or_default(),
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

/// 위임 가능한 팀원 (PM 제외)
pub fn list_workers(conn: &Connection) -> AppResult<Vec<Agent>> {
    Ok(list(conn)?.into_iter().filter(|a| !a.is_pm()).collect())
}

/// PM 에이전트를 돌려준다. 없으면 기본 설정으로 만든다 (고용·대기에는 토큰이 들지 않음).
pub fn ensure_pm(conn: &Connection) -> AppResult<Agent> {
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM agents WHERE kind = 'pm' AND archived_at IS NULL ORDER BY id LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = existing {
        return get(conn, id);
    }
    let now = now_ms();
    conn.execute(
        "INSERT INTO agents (name, role, instructions, permission_mode, tools, color, kind, created_at, updated_at)
         VALUES ('PM', 'PM', '', 'default', ?1, '#8a5cf6', 'pm', ?2, ?2)",
        params![serde_json::to_string(&default_tools())?, now],
    )?;
    get(conn, conn.last_insert_rowid())
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

fn clean_list(v: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for x in v.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        if !out.iter().any(|o| o == x) {
            out.push(x.to_string());
        }
    }
    out
}

/// 사용 중인 책상 (보관되지 않은 작업자 기준)
fn taken_desks(conn: &Connection, exclude: Option<i64>) -> AppResult<Vec<i64>> {
    Ok(list(conn)?
        .into_iter()
        .filter(|a| Some(a.id) != exclude && !a.is_pm())
        .filter_map(|a| a.profile.desk)
        .collect())
}

/// 프로필 정리·검증. 자리는 겹치지 않게 하고, 비어 있으면 빈자리를 자동 배정한다.
fn prepare_profile(
    conn: &Connection,
    exclude: Option<i64>,
    p: &AgentProfile,
) -> AppResult<AgentProfile> {
    let mut p = p.clone();
    p.specialties = clean_list(&p.specialties);
    p.capabilities = clean_list(&p.capabilities);
    p.scope_paths = clean_list(&p.scope_paths);
    if p.avatar.trim().is_empty() {
        p.avatar = "a".into();
    }
    let l = &p.limits;
    if l.max_minutes == Some(0)
        || l.max_turns == Some(0)
        || l.max_budget_usd.is_some_and(|b| b <= 0.0)
    {
        return Err(AppError::InvalidInput(
            "제한 값은 0보다 커야 합니다 (없으면 비워 두세요)".into(),
        ));
    }
    let taken = taken_desks(conn, exclude)?;
    match p.desk {
        Some(d) if !(0..DESK_COUNT).contains(&d) => {
            return Err(AppError::InvalidInput(format!(
                "자리 번호는 1~{DESK_COUNT} 사이여야 합니다"
            )));
        }
        Some(d) if taken.contains(&d) => {
            return Err(AppError::InvalidInput(format!(
                "{}번 자리는 이미 사용 중입니다",
                d + 1
            )));
        }
        Some(_) => {}
        None => p.desk = (0..DESK_COUNT).find(|d| !taken.contains(d)),
    }
    Ok(p)
}

/// 자리가 없는 작업자(6단계 이전 고용 등)에게 빈자리를 배정한다. 자리가 모자라면 그대로 둔다.
pub fn assign_missing_desks(conn: &Connection) -> AppResult<()> {
    for a in list(conn)?
        .into_iter()
        .filter(|a| !a.is_pm() && a.profile.desk.is_none())
    {
        let taken = taken_desks(conn, Some(a.id))?;
        let Some(desk) = (0..DESK_COUNT).find(|d| !taken.contains(d)) else {
            break;
        };
        let mut p = a.profile.clone();
        p.desk = Some(desk);
        conn.execute(
            "UPDATE agents SET profile = ?1 WHERE id = ?2",
            params![serde_json::to_string(&p)?, a.id],
        )?;
    }
    Ok(())
}

pub fn create(conn: &Connection, input: &AgentInput) -> AppResult<Agent> {
    input.validate()?;
    let profile = prepare_profile(conn, None, &input.profile)?;
    let now = now_ms();
    conn.execute(
        "INSERT INTO agents (name, role, instructions, model, effort, permission_mode, tools, color, profile, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?10, ?9, ?9)",
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
            serde_json::to_string(&profile)?,
        ],
    )?;
    get(conn, conn.last_insert_rowid())
}

/// 설정 변경은 실행 중인 Run에 반영되지 않는다 (Run이 생성 시점 스냅샷을 쓰기 때문, 스펙 4.3).
pub fn update(conn: &Connection, id: i64, input: &AgentInput) -> AppResult<Agent> {
    input.validate()?;
    let mut profile = prepare_profile(conn, Some(id), &input.profile)?;
    if get(conn, id)?.is_pm() {
        profile.desk = None; // PM은 전용 자리
    }
    let changed = conn.execute(
        "UPDATE agents SET name=?1, role=?2, instructions=?3, model=?4, effort=?5, permission_mode=?6,
                tools=?7, color=COALESCE(?8, color), updated_at=?9, profile=?11
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
            serde_json::to_string(&profile)?,
        ],
    )?;
    if changed == 0 {
        return Err(AppError::NotFound(format!("에이전트 #{id}")));
    }
    get(conn, id)
}

/// 해고는 보관 처리한다 (과거 작업·실행 이력이 에이전트를 참조하므로 삭제하지 않음).
pub fn archive(conn: &Connection, id: i64) -> AppResult<()> {
    if get(conn, id)?.is_pm() {
        return Err(AppError::InvalidInput("PM은 내보낼 수 없습니다".into()));
    }
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
            profile: Default::default(),
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
    fn pm은_하나만_생성되고_위임대상과_보관에서_제외() {
        let db = Db::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let pm = ensure_pm(&conn).unwrap();
        assert!(pm.is_pm());
        assert_eq!(
            ensure_pm(&conn).unwrap().id,
            pm.id,
            "두 번째 호출은 기존 PM 반환"
        );
        let w = create(&conn, &input()).unwrap();
        let workers: Vec<i64> = list_workers(&conn).unwrap().iter().map(|a| a.id).collect();
        assert_eq!(workers, vec![w.id]);
        assert!(archive(&conn, pm.id).is_err());
    }

    #[test]
    fn 자리는_자동_배정되고_겹치지_않는다() {
        let db = Db::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        ensure_pm(&conn).unwrap();
        let a = create(&conn, &input()).unwrap();
        let b = create(&conn, &input()).unwrap();
        assert_eq!((a.profile.desk, b.profile.desk), (Some(0), Some(1)));
        assert_eq!(
            ensure_pm(&conn).unwrap().profile.desk,
            None,
            "PM은 책상 배정 대상 아님"
        );

        let mut i = input();
        i.profile.desk = Some(1);
        assert!(create(&conn, &i).is_err(), "사용 중인 자리");
        i.profile.desk = Some(DESK_COUNT);
        assert!(create(&conn, &i).is_err(), "범위 밖");
        // 내보내면 자리가 비고, 자기 자리로 수정하는 건 허용
        archive(&conn, a.id).unwrap();
        assert_eq!(create(&conn, &input()).unwrap().profile.desk, Some(0));
        let mut same = input();
        same.profile.desk = Some(1);
        assert!(update(&conn, b.id, &same).is_ok());
    }

    #[test]
    fn 자리_없는_기존_작업자에게_빈자리_배정() {
        let db = Db::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let a = create(&conn, &input()).unwrap(); // 0번
        conn.execute("UPDATE agents SET profile = '{}' WHERE id = ?1", [a.id])
            .unwrap(); // 6단계 이전 상태
        let b = create(&conn, &input()).unwrap(); // 빈 0번을 받음
        assert_eq!(b.profile.desk, Some(0));
        assign_missing_desks(&conn).unwrap();
        assert_eq!(get(&conn, a.id).unwrap().profile.desk, Some(1));
    }

    #[test]
    fn 프로필_정리와_검토_역할() {
        let db = Db::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let mut i = input();
        i.profile.specialties = vec![" Rust ".into(), "Rust".into(), "".into()];
        i.profile.capabilities = vec![CAPABILITY_REVIEW.into()];
        let a = create(&conn, &i).unwrap();
        assert_eq!(a.profile.specialties, vec!["Rust"]);
        assert_eq!(a.usage_role(), "review");
        i.profile.limits.max_turns = Some(0);
        assert!(create(&conn, &i).is_err());
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
