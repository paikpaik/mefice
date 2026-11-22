//! 역할 템플릿 (스펙 4.3, 8장 AgentTemplate). 고용 화면의 출발점이다.
//! 역할 지침은 별도로 학습된 모델이나 전문성을 보장하지 않는다.

use serde::Serialize;

use crate::agent::{CAPABILITY_REVIEW, PermissionMode};

/// 고용 화면에서 고를 수 있는 수행 능력
pub const CAPABILITIES: &[&str] = &[
    "코드 수정",
    "테스트 실행",
    "문서 작성",
    "분석",
    CAPABILITY_REVIEW,
];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTemplate {
    pub id: &'static str,
    pub role: &'static str,
    pub summary: &'static str,
    pub instructions: &'static str,
    pub tools: &'static [&'static str],
    pub permission_mode: PermissionMode,
    pub specialty_suggestions: &'static [&'static str],
    pub capabilities: &'static [&'static str],
    pub color: &'static str,
    pub avatar: &'static str,
}

const ALL: &[&str] = &["Read", "Glob", "Grep", "Edit", "Write", "Bash"];
const READ_ONLY: &[&str] = &["Read", "Glob", "Grep"];

pub const TEMPLATES: &[AgentTemplate] = &[
    AgentTemplate {
        id: "backend",
        role: "백엔드",
        summary: "API·데이터베이스·서버 로직 구현과 테스트",
        instructions: "변경 전 관련 코드와 테스트를 먼저 읽고, 기존 패턴을 따른다. 변경 후 관련 테스트를 실행하고 결과를 보고한다.",
        tools: ALL,
        permission_mode: PermissionMode::AcceptEdits,
        specialty_suggestions: &[
            "Node.js",
            "TypeScript",
            "MySQL",
            "PostgreSQL",
            "Python",
            "Go",
            "Rust",
        ],
        capabilities: &["코드 수정", "테스트 실행", "분석"],
        color: "#3a6df0",
        avatar: "a",
    },
    AgentTemplate {
        id: "frontend",
        role: "프론트엔드",
        summary: "화면·컴포넌트·상태 관리 구현",
        instructions: "기존 컴포넌트와 스타일 규칙을 재사용한다. 타입 검사와 빌드가 통과하는지 확인하고 보고한다.",
        tools: ALL,
        permission_mode: PermissionMode::AcceptEdits,
        specialty_suggestions: &["React", "TypeScript", "CSS", "Vue", "Next.js"],
        capabilities: &["코드 수정", "테스트 실행"],
        color: "#e0607e",
        avatar: "b",
    },
    AgentTemplate {
        id: "qa",
        role: "QA",
        summary: "테스트 작성·실행과 버그 재현",
        instructions: "재현 절차와 기대 결과를 먼저 정리한다. 테스트를 추가하거나 실행하고, 실패하면 원인 후보와 로그 요약을 보고한다. 제품 코드는 필요한 경우에만 최소한으로 고친다.",
        tools: ALL,
        permission_mode: PermissionMode::AcceptEdits,
        specialty_suggestions: &["Jest", "Vitest", "Playwright", "pytest", "통합 테스트"],
        capabilities: &["테스트 실행", "코드 수정", "분석"],
        color: "#2f8a4c",
        avatar: "c",
    },
    AgentTemplate {
        id: "reviewer",
        role: "리뷰어",
        summary: "변경 검토 (읽기 전용)",
        instructions: "정확성 버그, 보안 문제, 놓친 경계 조건을 우선 찾는다. 확실한 문제와 추측을 구분하고, 파일:줄 위치와 함께 보고한다. 파일은 수정하지 않는다.",
        tools: READ_ONLY,
        permission_mode: PermissionMode::Plan,
        specialty_suggestions: &["보안", "성능", "TypeScript", "Rust"],
        capabilities: &[CAPABILITY_REVIEW, "분석"],
        color: "#8a5cf6",
        avatar: "d",
    },
    AgentTemplate {
        id: "researcher",
        role: "조사",
        summary: "코드베이스 조사·구조 파악 (읽기 전용)",
        instructions: "질문에 답하는 데 필요한 만큼만 탐색한다. 근거가 된 파일 경로를 함께 적고, 확인하지 못한 부분은 그렇다고 밝힌다.",
        tools: READ_ONLY,
        permission_mode: PermissionMode::Plan,
        specialty_suggestions: &["아키텍처", "의존성 분석", "문서화"],
        capabilities: &["분석", "문서 작성"],
        color: "#b7791f",
        avatar: "e",
    },
];

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateCatalog {
    pub templates: &'static [AgentTemplate],
    pub capabilities: &'static [&'static str],
    pub desk_count: i64,
}

pub fn catalog() -> TemplateCatalog {
    TemplateCatalog {
        templates: TEMPLATES,
        capabilities: CAPABILITIES,
        desk_count: crate::agent::DESK_COUNT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::ALLOWED_TOOLS;

    #[test]
    fn 템플릿은_허용된_도구만_쓰고_읽기전용은_쓰기도구가_없다() {
        for t in TEMPLATES {
            assert!(
                t.tools.iter().all(|x| ALLOWED_TOOLS.contains(x)),
                "{}",
                t.id
            );
            if t.permission_mode == PermissionMode::Plan {
                assert!(
                    !t.tools
                        .iter()
                        .any(|x| matches!(*x, "Edit" | "Write" | "Bash")),
                    "{}",
                    t.id
                );
            }
            assert!(
                t.capabilities.iter().all(|c| CAPABILITIES.contains(c)),
                "{}",
                t.id
            );
        }
    }
}
