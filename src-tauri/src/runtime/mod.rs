//! 에이전트 런타임 어댑터 계층 (스펙 7장).
//!
//! 실행 관리자(executor)는 런타임별 출력 형식을 몰라야 한다. 각 어댑터는
//! 1) 실행 명령 구성, 2) 출력 한 줄 → 공통 이벤트 변환, 3) 입력 메시지 인코딩을 담당한다.
//!
//! MVP는 Claude Code CLI 하나만 연결한다.

pub mod claude_code;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent::PermissionMode;

/// Run 생성 시점에 고정되는 실행 설정 스냅샷 (스펙 8장 Run.설정 스냅샷).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunConfig {
    pub agent_name: String,
    pub agent_role: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub permission_mode: PermissionMode,
    pub tools: Vec<String>,
    /// 프로젝트 기본 지침 + 역할 지침. 런타임 시스템 프롬프트에 덧붙인다.
    pub system_append: String,
}

/// 사용량. 런타임이 제공한 값만 채우고, 없으면 None (추정하지 않음).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_creation_tokens: Option<u64>,
    /// 런타임이 계산한 API 정가 기준 금액. 구독 사용량과 같은 값이 아니다 (스펙 5장).
    pub cost_usd: Option<f64>,
}

/// 런타임 세션 전체의 누적 사용량 (내부 보조 호출 포함).
/// 턴별 `Usage`는 내부 호출을 빠뜨리므로, 집계는 이 누적값의 차이로 계산한다.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTotals {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cost_usd: Option<f64>,
    pub models: Vec<String>,
}

/// 구독 한도 사용률 창 (예: five_hour, seven_day). 계정 전체 값이며 Mefice 외 사용도 포함된다.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitWindow {
    pub name: String,
    /// 0.0 ~ 1.0
    pub utilization: f64,
    /// 유닉스 초
    pub resets_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitSnapshot {
    pub status: Option<String>,
    pub windows: Vec<RateLimitWindow>,
    /// 관측 시각 (epoch ms, 실행 관리자가 채움)
    #[serde(default)]
    pub observed_at: i64,
}

/// 저장·표시용 공통 실행 이벤트. `kind` 태그가 run_events.kind 컬럼이 된다.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum RunEvent {
    /// 런타임 세션 시작
    Started {
        session_id: String,
        model: Option<String>,
        /// subscription | api_key | unknown (2단계에서 저장된 이벤트에는 없음)
        #[serde(default)]
        auth: Option<String>,
    },
    /// 사용자가 보낸 메시지 (최초 지시, 추가 지시)
    UserMessage {
        text: String,
    },
    /// 에이전트의 텍스트 출력
    Text {
        text: String,
    },
    /// 도구 호출 (입력은 요약만 저장)
    ToolUse {
        tool_use_id: String,
        name: String,
        summary: String,
    },
    /// 도구 결과 (긴 출력은 잘라서 저장, 스펙 5장 "긴 로그 재전달" 방지)
    ToolResult {
        tool_use_id: String,
        is_error: bool,
        preview: String,
        truncated: bool,
    },
    /// 승인 요청 (approvals 테이블과 연결)
    ApprovalRequested {
        approval_id: i64,
        tool_name: String,
        summary: String,
    },
    ApprovalDecided {
        approval_id: i64,
        allowed: bool,
    },
    /// 권한 정책에 의해 자동 거부된 도구 호출
    PermissionDenied {
        tool_name: String,
        message: String,
    },
    /// 한 턴 완료
    TurnResult {
        is_error: bool,
        subtype: String,
        result: Option<String>,
        /// 이 턴의 사용량 (런타임 보고, 내부 보조 호출 제외)
        usage: Usage,
        duration_ms: Option<u64>,
        num_turns: Option<u64>,
        /// 세션 누적 사용량 (2단계에서 저장된 이벤트에는 없음)
        #[serde(default)]
        session_totals: Option<SessionTotals>,
    },
    /// 런타임 stderr (진단용, 일부만 저장)
    Stderr {
        text: String,
    },
    /// 실행 관리자 측 알림 (중단 요청, 종료 등)
    Notice {
        text: String,
    },
}

impl RunEvent {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Started { .. } => "started",
            Self::UserMessage { .. } => "user_message",
            Self::Text { .. } => "text",
            Self::ToolUse { .. } => "tool_use",
            Self::ToolResult { .. } => "tool_result",
            Self::ApprovalRequested { .. } => "approval_requested",
            Self::ApprovalDecided { .. } => "approval_decided",
            Self::PermissionDenied { .. } => "permission_denied",
            Self::TurnResult { .. } => "turn_result",
            Self::Stderr { .. } => "stderr",
            Self::Notice { .. } => "notice",
        }
    }
}

/// 어댑터가 출력 한 줄을 해석한 결과.
#[derive(Debug, Clone, PartialEq)]
pub enum Parsed {
    /// 저장·표시할 이벤트
    Event(RunEvent),
    /// 도구 실행 승인 요청 — 실행 관리자가 approvals에 기록하고 사용자 결정을 기다린다
    PermissionRequest {
        request_id: String,
        tool_name: String,
        input: Value,
        description: Option<String>,
    },
    /// 구독 한도 사용률 갱신 (Run 이벤트로 저장하지 않고 앱 전역 상태로 관리)
    RateLimit(RateLimitSnapshot),
}

/// 문자 수 기준으로 자른다 (UTF-8 경계 안전).
pub fn truncate_chars(s: &str, max: usize) -> (String, bool) {
    match s.char_indices().nth(max) {
        Some((idx, _)) => (format!("{}…", &s[..idx]), true),
        None => (s.to_string(), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 이벤트_직렬화는_kind는_snake_필드는_camel() {
        let v = serde_json::to_value(RunEvent::ToolResult {
            tool_use_id: "t".into(),
            is_error: false,
            preview: String::new(),
            truncated: false,
        })
        .unwrap();
        assert_eq!(v["kind"], "tool_result");
        assert_eq!(v["toolUseId"], "t");
        assert_eq!(v["isError"], false);
    }
}
