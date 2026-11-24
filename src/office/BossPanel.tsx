import { useState } from "react";
import { api, errorMessage, type Boss, type PendingRequest } from "../api";
import { AVATARS, AVATAR_LABEL, Character } from "./Character";
import { COLORS } from "./hire";

const REQUEST_LABEL: Record<string, string> = {
  AskUserQuestion: "질문에 답해 주세요",
  ExitPlanMode: "계획을 확인해 주세요",
};

interface Props {
  boss: Boss;
  pending: PendingRequest[];
  onBossChange: (b: Boss) => void;
  onOpen: (r: PendingRequest) => void;
  onClose: () => void;
}

/** 사장 패널: 답을 기다리는 요청 목록 + 사장 꾸미기 (A단계 7번, 최소 범위) */
export function BossPanel({ boss, pending, onBossChange, onOpen, onClose }: Props) {
  const [draft, setDraft] = useState<Boss>(boss);
  const [editing, setEditing] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function save() {
    try {
      onBossChange(await api.setBoss(draft));
      setEditing(false);
      setError(null);
    } catch (e) {
      setError(errorMessage(e));
    }
  }

  return (
    <aside className="agent-panel">
      <header className="card-header">
        <svg width="44" height="56" viewBox="-22 -56 44 60">
          <Character color={boss.color} avatar={boss.avatar} badge="사장" />
        </svg>
        <div className="grow">
          <strong>{boss.name}</strong>
          <div className="small muted">나 (사장)</div>
        </div>
        <button onClick={onClose} aria-label="닫기">
          ×
        </button>
      </header>

      <h3>답을 기다리는 요청 {pending.length}건</h3>
      {pending.length === 0 ? (
        <p className="small muted">지금은 기다리는 요청이 없습니다.</p>
      ) : (
        <ul className="boss-requests">
          {pending.map((r) => (
            <li key={r.approvalId}>
              <button className="boss-request" onClick={() => onOpen(r)}>
                <strong>{REQUEST_LABEL[r.toolName] ?? "허락이 필요해요"}</strong>
                <span className="small muted">
                  {r.agentName} · {r.taskTitle}
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}

      {editing ? (
        <div className="boss-edit">
          <label className="field">
            <span>이름</span>
            <input value={draft.name} maxLength={20} onChange={(e) => setDraft({ ...draft, name: e.target.value })} />
          </label>
          <label className="field">
            <span>외형</span>
            <select value={draft.avatar} onChange={(e) => setDraft({ ...draft, avatar: e.target.value })}>
              {AVATARS.map((a) => (
                <option key={a} value={a}>
                  {AVATAR_LABEL[a]}
                </option>
              ))}
            </select>
          </label>
          <div className="field">
            <span>옷 색</span>
            <div className="row wrap">
              {[...COLORS, "#c0392b"].map((c) => (
                <button
                  key={c}
                  className={`swatch ${draft.color === c ? "active" : ""}`}
                  style={{ background: c }}
                  aria-label={c}
                  onClick={() => setDraft({ ...draft, color: c })}
                />
              ))}
            </div>
          </div>
          <div className="row">
            <button className="primary" onClick={save}>
              저장
            </button>
            <button
              onClick={() => {
                setDraft(boss);
                setEditing(false);
              }}
            >
              취소
            </button>
          </div>
        </div>
      ) : (
        <button onClick={() => setEditing(true)}>꾸미기</button>
      )}
      {error && <p className="error">{error}</p>}
    </aside>
  );
}
