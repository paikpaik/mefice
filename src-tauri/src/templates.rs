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
    /// 소속 팀
    pub team: &'static str,
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
/// 문서 수정 (명령 실행 없음)
const DOCS: &[&str] = &["Read", "Glob", "Grep", "Edit", "Write"];

/// 팀 목록 (화면 묶음 순서)
pub const TEAMS: &[&str] = &["개발", "문서", "법률", "디자인", "자동화", "조사"];

pub const TEMPLATES: &[AgentTemplate] = &[
    AgentTemplate {
        id: "backend",
        team: "개발",
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
        team: "개발",
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
        team: "개발",
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
        team: "개발",
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
        team: "조사",
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
    AgentTemplate {
        id: "writer",
        team: "문서",
        role: "문서 작성",
        summary: "보고서·회의록·기획서 등 문서 작성과 정리",
        instructions: "결과물은 기본적으로 마크다운 파일로 만든다. 워드(docx)를 요청받으면 사용할 수 있는 변환 도구(pandoc 등)가 있을 때만 변환하고, 없으면 그 사실과 대안을 보고한다. 원본 자료의 사실을 바꾸지 않고, 추정한 내용은 추정이라고 표시한다.",
        tools: DOCS,
        permission_mode: PermissionMode::AcceptEdits,
        specialty_suggestions: &["보고서", "회의록", "기획서", "매뉴얼", "요약"],
        capabilities: &["문서 작성", "분석"],
        color: "#0e9aa7",
        avatar: "b",
    },
    AgentTemplate {
        id: "legal",
        team: "법률",
        role: "법률 검토",
        summary: "계약서·약관 검토 참고 자료 작성 (법률 자문 아님)",
        instructions: "법률 자문이 아니라 참고 자료로 작성한다. 근거가 되는 조항이나 출처를 밝히고, 확실하지 않은 부분은 불확실하다고 표시한다. 결과물 끝에 반드시 '이 문서는 참고용이며 법률 자문이 아닙니다. 중요한 결정 전에는 변호사 등 전문가의 검토가 필요합니다.'라고 적는다.",
        tools: DOCS,
        permission_mode: PermissionMode::AcceptEdits,
        specialty_suggestions: &["계약서 검토", "이용약관", "개인정보", "근로", "저작권"],
        capabilities: &["분석", "문서 작성", CAPABILITY_REVIEW],
        color: "#5a6270",
        avatar: "d",
    },
    AgentTemplate {
        id: "designer",
        team: "디자인",
        role: "디자인",
        summary: "화면 시안(HTML·SVG)과 디자인 제안",
        instructions: "이미지를 직접 생성할 수 없다. 시안은 HTML·CSS·SVG 파일로 만들고, 의도와 선택 이유를 짧게 설명한다. 기존 색상·글꼴·간격 규칙이 있으면 따른다.",
        tools: DOCS,
        permission_mode: PermissionMode::AcceptEdits,
        specialty_suggestions: &["UI 시안", "랜딩 페이지", "SVG 아이콘", "색상 팔레트"],
        capabilities: &["문서 작성", "코드 수정"],
        color: "#e0607e",
        avatar: "e",
    },
    AgentTemplate {
        id: "automation",
        team: "자동화",
        role: "자동화",
        summary: "반복 작업을 스크립트로 자동화 (파일 정리·엑셀/CSV 처리 등)",
        instructions: "반복 작업을 스크립트로 만든다. 실행 전에 무엇을 하는지 쉬운 말로 설명하고, 파일 삭제·덮어쓰기·외부 전송이 필요하면 먼저 확인을 받는다. 원본 파일은 바꾸지 말고 결과는 새 파일로 만든다.",
        tools: ALL,
        permission_mode: PermissionMode::AcceptEdits,
        specialty_suggestions: &["엑셀·CSV 처리", "파일 정리", "보고서 자동 생성", "Python"],
        capabilities: &["코드 수정", "테스트 실행"],
        color: "#b7791f",
        avatar: "f",
    },
];

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateCatalog {
    pub templates: &'static [AgentTemplate],
    pub teams: &'static [&'static str],
    pub capabilities: &'static [&'static str],
    pub desk_count: i64,
}

pub fn catalog() -> TemplateCatalog {
    TemplateCatalog {
        templates: TEMPLATES,
        teams: TEAMS,
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
            assert!(TEAMS.contains(&t.team), "{}", t.id);
        }
    }
}
