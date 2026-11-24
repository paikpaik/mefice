/**
 * 쉬운 말 설명 (스펙 v0.2 4.6): 승인 요청을 개발을 모르는 사람이 이해할 수 있게 풀어 쓴다.
 * 명령 이름으로만 판별하는 간단한 규칙이며, 판별되지 않으면 일반 문구를 쓴다.
 */
import { createContext } from "react";
import type { Agent, PermissionMode } from "./api";

/** 개발자 모드 (원래 명령·세부 권한 표시) */
export const DevModeContext = createContext(false);

export type Risk = "normal" | "caution" | "danger";

export interface Plain {
  title: string;
  risk: Risk;
}

const RULES: { re: RegExp; title: string; risk: Risk }[] = [
  { re: /(^|[\s;&|])sudo\s/, title: "관리자 권한으로 명령을 실행하려고 합니다", risk: "danger" },
  { re: /(^|[\s;&|])(rm|rmdir|unlink)\s|git\s+(clean|reset\s+--hard)/, title: "파일을 삭제하려고 합니다", risk: "danger" },
  { re: /(npm|pnpm|yarn|bun)\s+(i|install|add)\b|pip3?\s+install|brew\s+install|gem\s+install|cargo\s+install/, title: "프로그램을 설치하려고 합니다", risk: "caution" },
  { re: /(^|[\s;&|])(curl|wget)\s/, title: "인터넷에서 내려받으려고 합니다", risk: "caution" },
  { re: /git\s+push/, title: "변경 내용을 원격 저장소에 올리려고 합니다", risk: "caution" },
  { re: /git\s+commit/, title: "변경 내용을 기록(커밋)하려고 합니다", risk: "normal" },
  { re: /(^|[\s;&|])(mv|cp)\s/, title: "파일을 옮기거나 복사하려고 합니다", risk: "normal" },
  { re: /(^|[\s;&|])(mkdir|touch)\s/, title: "새 파일이나 폴더를 만들려고 합니다", risk: "normal" },
  { re: /(^|[\s;&|])(python3?|node|npx|bash|sh|zsh)\s|(npm|pnpm|yarn)\s+(run|test|start)/, title: "프로그램을 실행하려고 합니다", risk: "normal" },
];

function baseName(p: unknown): string {
  return typeof p === "string" ? (p.split("/").pop() ?? p) : "";
}

export function explain(toolName: string, input: Record<string, unknown> | null | undefined): Plain {
  const i = input ?? {};
  switch (toolName) {
    case "Bash": {
      const cmd = String(i.command ?? "");
      const hit = RULES.find((r) => r.re.test(cmd));
      return hit ? { title: hit.title, risk: hit.risk } : { title: "명령을 실행하려고 합니다", risk: "normal" };
    }
    case "Write":
      return { title: `파일을 만들거나 덮어쓰려고 합니다: ${baseName(i.file_path)}`, risk: "normal" };
    case "Edit":
    case "MultiEdit":
      return { title: `파일을 고치려고 합니다: ${baseName(i.file_path)}`, risk: "normal" };
    case "WebFetch": {
      let host = "";
      try {
        host = new URL(String(i.url ?? "")).host;
      } catch {
        /* 주소가 아니면 비워 둔다 */
      }
      return { title: `웹 페이지를 읽으려고 합니다${host ? `: ${host}` : ""}`, risk: "caution" };
    }
    case "WebSearch":
      return { title: `웹을 검색하려고 합니다${i.query ? `: ${String(i.query)}` : ""}`, risk: "normal" };
    default:
      return { title: `${toolName} 기능을 쓰려고 합니다`, risk: "caution" };
  }
}

export const RISK_LABEL: Record<Risk, string | null> = { normal: null, caution: "주의", danger: "위험" };

/** 팀원 권한 3단계 (쉬운 말) */
export type Access = "read" | "edit" | "run";

export const ACCESS: Record<Access, { label: string; help: string; tools: string[]; mode: PermissionMode }> = {
  read: { label: "읽기만", help: "파일을 읽고 조사·검토만 합니다", tools: ["Read", "Glob", "Grep"], mode: "plan" },
  edit: {
    label: "문서 수정",
    help: "파일을 만들고 고칠 수 있습니다 (명령 실행은 불가)",
    tools: ["Read", "Glob", "Grep", "Edit", "Write"],
    mode: "acceptEdits",
  },
  run: {
    label: "명령 실행까지",
    help: "프로그램 실행·설치도 할 수 있습니다 (실행할 때마다 확인을 받습니다)",
    tools: ["Read", "Glob", "Grep", "Edit", "Write", "Bash"],
    mode: "acceptEdits",
  },
};

export function accessOf(a: Pick<Agent, "tools" | "permissionMode">): Access | null {
  for (const [k, v] of Object.entries(ACCESS) as [Access, (typeof ACCESS)[Access]][]) {
    if (v.mode === a.permissionMode && v.tools.length === a.tools.length && v.tools.every((t) => a.tools.includes(t))) return k;
  }
  return null; // 세부 설정으로 바꾼 경우
}
