import { useEffect, useState } from "react";
import {
  ALL_TOOLS,
  api,
  errorMessage,
  type Agent,
  type AgentInput,
  type AgentTemplate,
  type PermissionMode,
  type TemplateCatalog,
} from "../api";
import { AVATARS, AVATAR_LABEL, Character } from "../office/Character";
import { COLORS, inputFromTemplate } from "../office/hire";

const PERMISSION_LABEL: Record<PermissionMode, string> = {
  default: "수정·명령 실행 시 승인",
  acceptEdits: "파일 편집 자동 허용, 명령은 승인",
  plan: "읽기 전용",
};

type Editing =
  | { step: "template" }
  | { step: "form"; id: number | null; isPm: boolean; input: AgentInput };

/** 고용·팀 관리 (스펙 4.3, 6장). 고용과 대기에는 토큰이 들지 않는다. */
export function Team() {
  const [agents, setAgents] = useState<Agent[]>([]);
  const [catalog, setCatalog] = useState<TemplateCatalog | null>(null);
  const [editing, setEditing] = useState<Editing | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = () => api.listAgents().then(setAgents).catch((e) => setError(errorMessage(e)));
  useEffect(() => {
    load();
    api.listTemplates().then(setCatalog).catch((e) => setError(errorMessage(e)));
  }, []);

  async function save() {
    if (editing?.step !== "form") return;
    setError(null);
    try {
      if (editing.id === null) await api.createAgent(editing.input);
      else await api.updateAgent(editing.id, editing.input);
      setEditing(null);
      load();
    } catch (e) {
      setError(errorMessage(e));
    }
  }

  async function archive(a: Agent) {
    if (!confirm(`${a.name}을(를) 팀에서 내보낼까요? 지난 작업 이력은 유지됩니다.`)) return;
    try {
      await api.archiveAgent(a.id);
      load();
    } catch (e) {
      setError(errorMessage(e));
    }
  }

  const edit = (a: Agent) =>
    setEditing({
      step: "form",
      id: a.id,
      isPm: a.kind === "pm",
      input: {
        name: a.name,
        role: a.role,
        instructions: a.instructions,
        model: a.model,
        effort: a.effort,
        permissionMode: a.permissionMode,
        tools: a.tools,
        color: a.color,
        profile: a.profile,
      },
    });

  return (
    <div className="settings">
      <section className="card">
        <header className="card-header">
          <h2>팀</h2>
          <button className="primary" onClick={() => setEditing({ step: "template" })}>
            고용하기
          </button>
        </header>
        <p className="muted small">
          고용과 대기에는 토큰이 들지 않습니다. 설정 변경은 실행 중인 작업에는 반영되지 않고 다음 실행부터 적용됩니다.
          역할 지침이 별도로 학습된 전문성을 보장하지는 않습니다.
        </p>
        <div className="agent-grid">
          {agents.map((a) => (
            <div key={a.id} className="agent-card">
              <svg viewBox="-24 -62 48 70" className="agent-avatar">
                <Character color={a.color} avatar={a.profile.avatar} pose="stand" badge={a.kind === "pm" ? "PM" : null} />
              </svg>
              <div className="agent-info">
                <div className="row">
                  <strong>{a.name}</strong>
                  <span className="muted">{a.role}</span>
                  {a.kind === "pm" && <span className="badge info">PM</span>}
                </div>
                {a.profile.specialties.length > 0 && (
                  <div className="row wrap small">
                    {a.profile.specialties.map((s) => (
                      <span key={s} className="chip small">
                        {s}
                      </span>
                    ))}
                  </div>
                )}
                <div className="small muted">
                  {a.model ?? "기본 모델"} · {PERMISSION_LABEL[a.permissionMode]}
                  {a.kind !== "pm" && a.profile.desk != null && ` · ${a.profile.desk + 1}번 자리`}
                </div>
                <div className="row">
                  <button onClick={() => edit(a)}>수정</button>
                  {a.kind !== "pm" && <button onClick={() => archive(a)}>내보내기</button>}
                </div>
              </div>
            </div>
          ))}
        </div>
        {error && <p className="error">{error}</p>}
      </section>

      {editing?.step === "template" && catalog && (
        <TemplatePicker
          templates={catalog.templates}
          onPick={(t) => setEditing({ step: "form", id: null, isPm: false, input: inputFromTemplate(t, agents) })}
          onCancel={() => setEditing(null)}
        />
      )}

      {editing?.step === "form" && catalog && (
        <AgentForm
          title={editing.id === null ? "고용" : editing.isPm ? "PM 설정" : "팀원 설정"}
          isPm={editing.isPm}
          input={editing.input}
          catalog={catalog}
          template={catalog.templates.find((t) => t.id === editing.input.profile.templateId) ?? null}
          takenDesks={agents.filter((a) => a.id !== editing.id && a.kind !== "pm").flatMap((a) => (a.profile.desk != null ? [a.profile.desk] : []))}
          onChange={(input) => setEditing({ ...editing, input })}
          onSave={save}
          onCancel={() => setEditing(null)}
          saveLabel={editing.id === null ? "고용 확정" : "저장"}
        />
      )}
    </div>
  );
}

function TemplatePicker({
  templates,
  onPick,
  onCancel,
}: {
  templates: AgentTemplate[];
  onPick: (t: AgentTemplate | null) => void;
  onCancel: () => void;
}) {
  return (
    <section className="card">
      <header className="card-header">
        <h2>역할 선택</h2>
        <button onClick={onCancel}>취소</button>
      </header>
      <div className="template-grid">
        {templates.map((t) => (
          <button key={t.id} className="template-card" onClick={() => onPick(t)}>
            <svg viewBox="-24 -62 48 70" className="agent-avatar">
              <Character color={t.color} avatar={t.avatar} pose="stand" />
            </svg>
            <strong>{t.role}</strong>
            <span className="small muted">{t.summary}</span>
            <span className="small muted">{PERMISSION_LABEL[t.permissionMode]}</span>
          </button>
        ))}
        <button className="template-card" onClick={() => onPick(null)}>
          <strong>직접 구성</strong>
          <span className="small muted">빈 양식에서 시작</span>
        </button>
      </div>
    </section>
  );
}

function numOrNull(v: string): number | null {
  const n = Number(v);
  return v.trim() === "" || !Number.isFinite(n) ? null : n;
}

function AgentForm({
  title,
  isPm,
  input,
  catalog,
  template,
  takenDesks,
  onChange,
  onSave,
  onCancel,
  saveLabel,
}: {
  title: string;
  isPm: boolean;
  input: AgentInput;
  catalog: TemplateCatalog;
  template: AgentTemplate | null;
  takenDesks: number[];
  onChange: (i: AgentInput) => void;
  onSave: () => void;
  onCancel: () => void;
  saveLabel: string;
}) {
  const [specialty, setSpecialty] = useState("");
  const set = (patch: Partial<AgentInput>) => onChange({ ...input, ...patch });
  const p = input.profile;
  const setP = (patch: Partial<AgentInput["profile"]>) => set({ profile: { ...p, ...patch } });
  const toggle = (list: string[], v: string) => (list.includes(v) ? list.filter((x) => x !== v) : [...list, v]);
  const suggestions = [...new Set([...(template?.specialtySuggestions ?? []), ...p.specialties])];

  return (
    <section className="card hire-form">
      <header className="card-header">
        <h2>{title}</h2>
      </header>

      <div className="hire-layout">
        <div className="hire-preview">
          <svg viewBox="-30 -70 60 80" className="agent-avatar large">
            <Character color={input.color ?? COLORS[0]} avatar={p.avatar} pose="stand" badge={isPm ? "PM" : null} />
          </svg>
          <div className="row wrap avatar-picker">
            {AVATARS.map((av) => (
              <button
                key={av}
                className={`avatar-opt ${p.avatar === av ? "active" : ""}`}
                title={AVATAR_LABEL[av]}
                onClick={() => setP({ avatar: av })}
              >
                <svg viewBox="-16 -52 32 38">
                  <Character color={input.color ?? COLORS[0]} avatar={av} pose="stand" />
                </svg>
              </button>
            ))}
          </div>
          <div className="row wrap">
            {COLORS.map((c) => (
              <button
                key={c}
                className={`swatch ${input.color === c ? "active" : ""}`}
                style={{ background: c }}
                aria-label={c}
                onClick={() => set({ color: c })}
              />
            ))}
          </div>
        </div>

        <div className="hire-fields">
          <div className="form-grid">
            <label className="field">
              <span>이름</span>
              <input value={input.name} onChange={(e) => set({ name: e.target.value })} />
            </label>
            <label className="field">
              <span>역할</span>
              <input value={input.role} disabled={isPm} onChange={(e) => set({ role: e.target.value })} />
            </label>
            {!isPm && (
              <label className="field">
                <span>자리</span>
                <select
                  value={p.desk ?? ""}
                  onChange={(e) => setP({ desk: e.target.value === "" ? null : Number(e.target.value) })}
                >
                  <option value="">자동 배정</option>
                  {Array.from({ length: catalog.deskCount }, (_, i) => (
                    <option key={i} value={i} disabled={takenDesks.includes(i)}>
                      {i + 1}번{takenDesks.includes(i) ? " (사용 중)" : ""}
                    </option>
                  ))}
                </select>
              </label>
            )}
          </div>

          <div className="field">
            <span>전문 분야</span>
            <div className="row wrap">
              {suggestions.map((s) => (
                <button
                  key={s}
                  className={`chip ${p.specialties.includes(s) ? "active" : ""}`}
                  onClick={() => setP({ specialties: toggle(p.specialties, s) })}
                >
                  {s}
                </button>
              ))}
              <input
                className="chip-input"
                placeholder="직접 추가 (Enter)"
                value={specialty}
                onChange={(e) => setSpecialty(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && specialty.trim()) {
                    setP({ specialties: [...p.specialties, specialty.trim()] });
                    setSpecialty("");
                  }
                }}
              />
            </div>
          </div>

          <div className="field">
            <span>수행 능력</span>
            <div className="row wrap">
              {catalog.capabilities.map((c) => (
                <label key={c} className="chip">
                  <input
                    type="checkbox"
                    checked={p.capabilities.includes(c)}
                    onChange={() => setP({ capabilities: toggle(p.capabilities, c) })}
                  />
                  {c}
                </label>
              ))}
            </div>
            <small className="muted">"코드 리뷰"가 있으면 사용량을 검토로 따로 집계합니다.</small>
          </div>

          {!isPm && (
            <label className="field">
              <span>작업 범위</span>
              <input
                placeholder="예: src/api, src/db (쉼표로 구분, 비우면 프로젝트 전체)"
                value={p.scopePaths.join(", ")}
                onChange={(e) => setP({ scopePaths: e.target.value.split(",").map((s) => s.trim()).filter(Boolean) })}
              />
              <small className="muted">지침으로 전달됩니다. 파일 접근 권한을 막는 기능은 아닙니다.</small>
            </label>
          )}

          <div className="field">
            <span>도구 권한</span>
            <div className="row wrap">
              {ALL_TOOLS.map((t) => (
                <label key={t} className="chip">
                  <input type="checkbox" checked={input.tools.includes(t)} onChange={() => set({ tools: toggle(input.tools, t) })} />
                  <span className="mono">{t}</span>
                </label>
              ))}
            </div>
            <select value={input.permissionMode} onChange={(e) => set({ permissionMode: e.target.value as PermissionMode })}>
              {(Object.keys(PERMISSION_LABEL) as PermissionMode[]).map((m) => (
                <option key={m} value={m}>
                  {PERMISSION_LABEL[m]}
                </option>
              ))}
            </select>
            <small className="muted">수정 작업은 격리된 작업 공간(git worktree)에서 실행되고, 결과는 직접 확인한 뒤 병합합니다.</small>
          </div>

          <div className="form-grid">
            <label className="field">
              <span>런타임</span>
              <input value="Claude Code CLI" disabled />
            </label>
            <label className="field">
              <span>모델</span>
              <input
                value={input.model ?? ""}
                placeholder="비우면 기본값 (예: sonnet, opus, haiku)"
                onChange={(e) => set({ model: e.target.value || null })}
              />
            </label>
            <label className="field">
              <span>추론 수준</span>
              <select value={input.effort ?? ""} onChange={(e) => set({ effort: e.target.value || null })}>
                <option value="">기본값</option>
                <option value="low">low</option>
                <option value="medium">medium</option>
                <option value="high">high</option>
              </select>
            </label>
          </div>

          <div className="form-grid three">
            <label className="field">
              <span>시간 제한 (분)</span>
              <input
                type="number"
                min={1}
                value={p.limits.maxMinutes ?? ""}
                placeholder="없음"
                onChange={(e) => setP({ limits: { ...p.limits, maxMinutes: numOrNull(e.target.value) } })}
              />
            </label>
            <label className="field">
              <span>사용량 상한 ($, 정가 환산)</span>
              <input
                type="number"
                min={0.01}
                step={0.1}
                value={p.limits.maxBudgetUsd ?? ""}
                placeholder="없음"
                onChange={(e) => setP({ limits: { ...p.limits, maxBudgetUsd: numOrNull(e.target.value) } })}
              />
            </label>
            <label className="field">
              <span>턴 수 제한</span>
              <input
                type="number"
                min={1}
                value={p.limits.maxTurns ?? ""}
                placeholder="없음"
                onChange={(e) => setP({ limits: { ...p.limits, maxTurns: numOrNull(e.target.value) } })}
              />
            </label>
          </div>

          <label className="field">
            <span>역할 지침</span>
            <textarea rows={3} value={input.instructions} onChange={(e) => set({ instructions: e.target.value })} />
          </label>

          <div className="row">
            <button className="primary" onClick={onSave}>
              {saveLabel}
            </button>
            <button onClick={onCancel}>취소</button>
          </div>
        </div>
      </div>
    </section>
  );
}
