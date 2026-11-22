import { useEffect, useState } from "react";
import { ALL_TOOLS, api, errorMessage, type Agent, type AgentInput, type PermissionMode } from "../api";

const PERMISSION_LABEL: Record<PermissionMode, string> = {
  default: "수정·명령 실행 시 승인 (권장)",
  acceptEdits: "파일 편집 자동 허용, 명령은 승인",
  plan: "읽기 전용 (계획만)",
};

const EMPTY: AgentInput = {
  name: "",
  role: "",
  instructions: "",
  model: null,
  effort: null,
  permissionMode: "default",
  tools: [...ALL_TOOLS],
};

/** 2단계 최소 팀 관리. 역할 템플릿·외형 선택이 있는 고용 UI는 6단계에서 확장한다. */
export function Team() {
  const [agents, setAgents] = useState<Agent[]>([]);
  const [editing, setEditing] = useState<{ id: number | null; input: AgentInput } | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = () => api.listAgents().then(setAgents).catch((e) => setError(errorMessage(e)));
  useEffect(() => {
    load();
  }, []);

  async function save() {
    if (!editing) return;
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

  return (
    <div className="settings">
      <section className="card">
        <header className="card-header">
          <h2>팀</h2>
          <button className="primary" onClick={() => setEditing({ id: null, input: { ...EMPTY } })}>
            에이전트 추가
          </button>
        </header>
        <p className="muted">고용과 대기에는 토큰이 들지 않습니다. 설정 변경은 다음 실행부터 적용됩니다.</p>
        {agents.length === 0 ? (
          <p className="muted">아직 에이전트가 없습니다.</p>
        ) : (
          <table className="table">
            <thead>
              <tr>
                <th>이름</th>
                <th>역할</th>
                <th>모델</th>
                <th>권한</th>
                <th>도구</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {agents.map((a) => (
                <tr key={a.id}>
                  <td>
                    <span className="dot" style={{ background: a.color }} /> {a.name}
                    {a.kind === "pm" && <span className="badge info"> PM</span>}
                  </td>
                  <td>{a.role}</td>
                  <td className="mono">{a.model ?? "기본값"}</td>
                  <td>{PERMISSION_LABEL[a.permissionMode]}</td>
                  <td className="mono muted">{a.tools.join(", ")}</td>
                  <td className="row">
                    <button
                      onClick={() =>
                        setEditing({
                          id: a.id,
                          input: {
                            name: a.name,
                            role: a.role,
                            instructions: a.instructions,
                            model: a.model,
                            effort: a.effort,
                            permissionMode: a.permissionMode,
                            tools: a.tools,
                          },
                        })
                      }
                    >
                      수정
                    </button>
                    {a.kind !== "pm" && <button onClick={() => archive(a)}>내보내기</button>}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
        {error && <p className="error">{error}</p>}
      </section>

      {editing && (
        <AgentForm
          title={editing.id === null ? "새 에이전트" : "에이전트 수정"}
          input={editing.input}
          onChange={(input) => setEditing({ ...editing, input })}
          onSave={save}
          onCancel={() => setEditing(null)}
        />
      )}
    </div>
  );
}

function AgentForm({
  title,
  input,
  onChange,
  onSave,
  onCancel,
}: {
  title: string;
  input: AgentInput;
  onChange: (i: AgentInput) => void;
  onSave: () => void;
  onCancel: () => void;
}) {
  const set = (patch: Partial<AgentInput>) => onChange({ ...input, ...patch });
  const toggleTool = (t: string) =>
    set({ tools: input.tools.includes(t) ? input.tools.filter((x) => x !== t) : [...input.tools, t] });

  return (
    <section className="card">
      <header className="card-header">
        <h2>{title}</h2>
      </header>
      <div className="form-grid">
        <label className="field">
          <span>이름</span>
          <input value={input.name} onChange={(e) => set({ name: e.target.value })} placeholder="예: 민수" />
        </label>
        <label className="field">
          <span>역할</span>
          <input value={input.role} onChange={(e) => set({ role: e.target.value })} placeholder="예: 백엔드" />
        </label>
        <label className="field">
          <span>모델</span>
          <input
            value={input.model ?? ""}
            onChange={(e) => set({ model: e.target.value || null })}
            placeholder="비우면 런타임 기본값 (예: sonnet, opus, haiku)"
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
      <label className="field">
        <span>권한</span>
        <select
          value={input.permissionMode}
          onChange={(e) => set({ permissionMode: e.target.value as PermissionMode })}
        >
          {(Object.keys(PERMISSION_LABEL) as PermissionMode[]).map((m) => (
            <option key={m} value={m}>
              {PERMISSION_LABEL[m]}
            </option>
          ))}
        </select>
        <small className="muted">
          현재 단계에서는 프로젝트 폴더에서 직접 실행됩니다. 작업별 worktree 격리는 5단계에서 추가됩니다.
        </small>
      </label>
      <div className="field">
        <span>도구</span>
        <div className="row wrap">
          {ALL_TOOLS.map((t) => (
            <label key={t} className="chip">
              <input type="checkbox" checked={input.tools.includes(t)} onChange={() => toggleTool(t)} />
              <span className="mono">{t}</span>
            </label>
          ))}
        </div>
      </div>
      <label className="field">
        <span>역할 지침</span>
        <textarea
          rows={4}
          value={input.instructions}
          onChange={(e) => set({ instructions: e.target.value })}
          placeholder="예: 변경 후 관련 테스트를 실행하고 결과를 요약한다"
        />
      </label>
      <div className="row">
        <button className="primary" onClick={onSave}>
          저장
        </button>
        <button onClick={onCancel}>취소</button>
      </div>
    </section>
  );
}
