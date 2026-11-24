import { useContext, useState } from "react";
import { DevModeContext, explain, RISK_LABEL } from "../plain";
import { api, errorMessage, type Approval, type AskQuestion, type Decision } from "../api";
import { Markdown } from "./Markdown";

function useDecide(onError: (m: string) => void) {
  const [busy, setBusy] = useState(false);
  const decide = async (id: number, d: Decision, answers?: Record<string, string>, message?: string) => {
    setBusy(true);
    try {
      await api.decideApproval(id, d, answers, message);
    } catch (e) {
      onError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };
  return { busy, decide };
}

/** 권한 요청: 쉬운 말 설명 + 위험도, 원래 명령은 '자세히'(개발자 모드면 펼침). 이번만 / 이 대화 동안 / 거부 */
export function PermissionCard({ ap, onError }: { ap: Approval; onError: (m: string) => void }) {
  const { busy, decide } = useDecide(onError);
  const dev = useContext(DevModeContext);
  const [reason, setReason] = useState("");
  const [denying, setDenying] = useState(false);
  const input = (ap.toolInput ?? {}) as Record<string, unknown>;
  const plain = explain(ap.toolName, input);
  const detail =
    ap.toolName === "Bash"
      ? String(input.command ?? "")
      : String(input.file_path ?? input.url ?? input.query ?? JSON.stringify(input));
  const sessionLabel = ap.toolName === "Bash" ? "이 명령은 이 대화 동안 허용" : "이 대화 동안 파일 수정 허용";
  const risk = RISK_LABEL[plain.risk];

  return (
    <div className={`req-card perm risk-${plain.risk}`}>
      <div className="row">
        {risk && <span className={`badge ${plain.risk === "danger" ? "danger" : "warn"}`}>{risk}</span>}
        <div className="req-title">{plain.title}</div>
      </div>
      {typeof input.description === "string" && <div className="small muted">{input.description}</div>}
      <details open={dev}>
        <summary className="small muted">자세히</summary>
        <pre className="mono req-detail">{detail}</pre>
      </details>
      {denying ? (
        <div className="row">
          <input
            autoFocus
            className="grow"
            placeholder="거부 이유나 다르게 할 방법 (선택)"
            value={reason}
            onChange={(e) => setReason(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.nativeEvent.isComposing) decide(ap.id, "deny", undefined, reason);
            }}
          />
          <button disabled={busy} onClick={() => decide(ap.id, "deny", undefined, reason)}>
            거부 보내기
          </button>
        </div>
      ) : (
        <div className="row wrap">
          <button className="primary" disabled={busy} onClick={() => decide(ap.id, "allow")}>
            이번만 허용
          </button>
          <button disabled={busy} onClick={() => decide(ap.id, "allow_session")} title="이 대화가 끝나면 사라집니다. 폴더에 설정을 저장하지 않습니다.">
            {sessionLabel}
          </button>
          <button disabled={busy} onClick={() => setDenying(true)}>
            거부
          </button>
        </div>
      )}
    </div>
  );
}

/** 에이전트의 질문: 선택지 버튼(여러 개 선택 지원) + 직접 입력 */
export function QuestionCard({ ap, onError }: { ap: Approval; onError: (m: string) => void }) {
  const { busy, decide } = useDecide(onError);
  const questions = ((ap.toolInput as { questions?: AskQuestion[] })?.questions ?? []) as AskQuestion[];
  const [picked, setPicked] = useState<Record<string, string[]>>({});
  const [other, setOther] = useState<Record<string, string>>({});

  const toggle = (q: AskQuestion, label: string) =>
    setPicked((p) => {
      const cur = p[q.question] ?? [];
      const next = q.multiSelect ? (cur.includes(label) ? cur.filter((x) => x !== label) : [...cur, label]) : [label];
      return { ...p, [q.question]: next };
    });

  const answers = () => {
    const out: Record<string, string> = {};
    for (const q of questions) {
      const vals = [...(picked[q.question] ?? [])];
      if (other[q.question]?.trim()) vals.push(other[q.question].trim());
      out[q.question] = vals.join(", ");
    }
    return out;
  };
  const ready = questions.every((q) => (picked[q.question]?.length ?? 0) > 0 || other[q.question]?.trim());

  return (
    <div className="req-card question">
      {questions.map((q) => (
        <div key={q.question} className="q-block">
          {q.header && <span className="badge info">{q.header}</span>}
          <div className="req-title">{q.question}</div>
          <div className="q-options">
            {q.options.map((o) => (
              <button
                key={o.label}
                className={`q-opt ${(picked[q.question] ?? []).includes(o.label) ? "active" : ""}`}
                onClick={() => toggle(q, o.label)}
              >
                <strong>{o.label}</strong>
                {o.description && <span className="small muted">{o.description}</span>}
              </button>
            ))}
          </div>
          <input
            placeholder="직접 입력"
            value={other[q.question] ?? ""}
            onChange={(e) => setOther({ ...other, [q.question]: e.target.value })}
          />
        </div>
      ))}
      <div className="row">
        <button className="primary" disabled={busy || !ready} onClick={() => decide(ap.id, "allow", answers())}>
          답하기
        </button>
        <button disabled={busy} onClick={() => decide(ap.id, "deny", undefined, "사용자가 답하지 않고 넘겼습니다.")}>
          넘기기
        </button>
      </div>
    </div>
  );
}

/** 계획 모드의 계획 승인: 진행 / 고쳐 달라고 하기 */
export function PlanCard({ ap, onError }: { ap: Approval; onError: (m: string) => void }) {
  const { busy, decide } = useDecide(onError);
  const [feedback, setFeedback] = useState("");
  const plan = String((ap.toolInput as { plan?: string })?.plan ?? "");
  return (
    <div className="req-card plan">
      <div className="req-title">이 계획대로 진행할까요?</div>
      <div className="plan-body">
        <Markdown text={plan} />
      </div>
      <textarea rows={2} placeholder="고칠 점이 있으면 적어 주세요" value={feedback} onChange={(e) => setFeedback(e.target.value)} />
      <div className="row">
        <button className="primary" disabled={busy} onClick={() => decide(ap.id, "allow")}>
          진행
        </button>
        <button disabled={busy || !feedback.trim()} onClick={() => decide(ap.id, "deny", undefined, feedback)}>
          고쳐 주세요
        </button>
      </div>
    </div>
  );
}

export function RequestCard({ ap, onError }: { ap: Approval; onError: (m: string) => void }) {
  if (ap.toolName === "AskUserQuestion") return <QuestionCard ap={ap} onError={onError} />;
  if (ap.toolName === "ExitPlanMode") return <PlanCard ap={ap} onError={onError} />;
  return <PermissionCard ap={ap} onError={onError} />;
}
