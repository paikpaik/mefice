import { useState } from "react";
import type { StoredEvent, ToolDetail } from "../api";

type ToolUse = Extract<StoredEvent, { kind: "tool_use" }>;
type ToolResult = Extract<StoredEvent, { kind: "tool_result" }>;

const LABEL: Record<string, string> = {
  Read: "파일 읽기",
  Write: "파일 작성",
  Edit: "파일 수정",
  MultiEdit: "파일 수정",
  NotebookEdit: "노트북 수정",
  Bash: "명령 실행",
  Glob: "파일 찾기",
  Grep: "내용 검색",
  WebFetch: "웹 페이지 읽기",
  WebSearch: "웹 검색",
  Task: "하위 작업",
  Agent: "하위 작업",
  AskUserQuestion: "질문",
  ExitPlanMode: "계획 제출",
  TodoWrite: "할 일 목록",
};

function baseName(p?: string) {
  return p ? p.split("/").pop() : "";
}

/** 바뀐 줄 보기 (Edit: 이전 → 이후, Write: 새 내용) */
function DiffView({ d, name }: { d: ToolDetail; name: string }) {
  const lines: { kind: "add" | "del"; text: string }[] = [];
  if (name === "Edit" || name === "MultiEdit") {
    for (const l of (d.oldString ?? "").split("\n")) lines.push({ kind: "del", text: l });
    for (const l of (d.newString ?? "").split("\n")) lines.push({ kind: "add", text: l });
  } else {
    for (const l of (d.content ?? "").split("\n")) lines.push({ kind: "add", text: l });
  }
  const shown = lines.slice(0, 60);
  return (
    <pre className="tool-diff">
      {shown.map((l, i) => (
        <div key={i} className={l.kind === "add" ? "pl-add" : "pl-del"}>
          {l.kind === "add" ? "+ " : "- "}
          {l.text || " "}
        </div>
      ))}
      {lines.length > shown.length && <div className="muted">… {lines.length - shown.length}줄 더</div>}
    </pre>
  );
}

/** 도구 실행 카드: 한 줄 요약 + 펼치면 상세(바뀐 줄·명령·결과) */
export function ToolCard({ use, result }: { use: ToolUse; result?: ToolResult }) {
  const d = use.input ?? undefined;
  const isEdit = (use.name === "Edit" || use.name === "Write" || use.name === "MultiEdit") && d;
  const [open, setOpen] = useState(Boolean(isEdit));
  const status = !result ? "running" : result.isError ? "error" : "done";
  const target =
    use.name === "Bash"
      ? (d?.description ?? use.summary)
      : d?.filePath
        ? baseName(d.filePath)
        : use.summary;

  return (
    <div className={`tool-card ${status}`}>
      <button className="tool-head" onClick={() => setOpen(!open)}>
        <span className={`tool-dot ${status}`} />
        <span className="tool-name">{LABEL[use.name] ?? use.name}</span>
        <span className="tool-target mono">{target}</span>
        <span className="muted small">{open ? "접기" : "펼치기"}</span>
      </button>
      {open && (
        <div className="tool-body">
          {isEdit && <DiffView d={d!} name={use.name} />}
          {use.name === "Bash" && <pre className="tool-cmd mono">$ {d?.command ?? use.summary}</pre>}
          {!isEdit && use.name !== "Bash" && <div className="mono small muted">{use.summary}</div>}
          {result && (
            <details className="tool-result" open={result.isError}>
              <summary className="small muted">
                {result.isError ? "오류" : "결과"}
                {result.truncated && " (일부)"}
              </summary>
              <pre>{result.preview || "(출력 없음)"}</pre>
            </details>
          )}
        </div>
      )}
    </div>
  );
}
