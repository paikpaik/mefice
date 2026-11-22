import { ALL_TOOLS, emptyProfile, type Agent, type AgentInput, type AgentTemplate } from "../api";

export const COLORS = ["#3a6df0", "#e0607e", "#2f8a4c", "#8a5cf6", "#b7791f", "#0e9aa7", "#d9534f", "#5a6270"];

const NAMES = ["민수", "지은", "서준", "하윤", "도윤", "서연", "예준", "지호", "수아", "하준", "유나", "시우"];

/** 팀에서 아직 쓰지 않은 이름 (모델 호출 없이 로컬에서 고름) */
export function pickName(agents: Agent[]): string {
  const used = new Set(agents.map((a) => a.name));
  return NAMES.find((n) => !used.has(n)) ?? `팀원 ${agents.length + 1}`;
}

/** 템플릿 기본값으로 고용 양식을 채운다 */
export function inputFromTemplate(t: AgentTemplate | null, agents: Agent[]): AgentInput {
  const profile = emptyProfile();
  if (!t) {
    return {
      name: pickName(agents),
      role: "",
      instructions: "",
      model: null,
      effort: null,
      permissionMode: "default",
      tools: [...ALL_TOOLS],
      color: COLORS[agents.length % COLORS.length],
      profile,
    };
  }
  return {
    name: pickName(agents),
    role: t.role,
    instructions: t.instructions,
    model: null,
    effort: null,
    permissionMode: t.permissionMode,
    tools: [...t.tools],
    color: t.color,
    profile: { ...profile, templateId: t.id, avatar: t.avatar, capabilities: [...t.capabilities] },
  };
}

/** PM이 제안한 역할 이름에 가장 가까운 템플릿 */
export function templateForRole(role: string, templates: AgentTemplate[]): AgentTemplate | null {
  const r = role.toLowerCase();
  const keys: Record<string, string[]> = {
    backend: ["백엔드", "backend", "서버", "api", "db", "데이터베이스"],
    frontend: ["프론트", "frontend", "ui", "화면", "react", "웹"],
    qa: ["qa", "테스트", "test", "품질"],
    reviewer: ["리뷰", "review", "검토"],
    researcher: ["조사", "research", "분석", "리서치"],
  };
  const hit = Object.entries(keys).find(([, ks]) => ks.some((k) => r.includes(k)));
  return templates.find((t) => t.id === hit?.[0]) ?? null;
}
