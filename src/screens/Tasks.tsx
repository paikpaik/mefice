import { useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  api,
  errorMessage,
  EVENTS,
  isTerminal,
  type Agent,
  type Approval,
  type Run,
  type StoredEvent,
  type TaskDetail,
  type RunStatus,
  type RunUsage,
  type TaskSummary,
  type Usage,
  type UsageTotals,
} from "../api";
import { StatusBadge } from "../components/StatusBadge";
import { SourceBadges, UsageNumbers } from "../components/Usage";
import { DiffPanel, IntegrationBadge } from "../components/DiffPanel";

interface Props {
  onGoTeam: () => void;
  /** 다른 화면에서 특정 작업을 열 때 */
  focusTaskId?: number | null;
  /** 커밋이 있는 Git 저장소인지 (작업 공간 격리 가능 여부) */
  canIsolate: boolean;
}

const MODE_LABEL: Record<string, string> = { pm: "PM 목표", delegated: "위임" };

/** 직접 실행 모드(스펙 4.6) + 작업 목록 + 작업 상세. */
export function Tasks({ onGoTeam, focusTaskId = null, canIsolate }: Props) {
  const [agents, setAgents] = useState<Agent[]>([]);
  const [tasks, setTasks] = useState<TaskSummary[]>([]);
  const [selected, setSelected] = useState<number | null>(focusTaskId);
  const [filter, setFilter] = useState("all");
  const [error, setError] = useState<string | null>(null);
  const active = FILTERS.find((f) => f.id === filter) ?? FILTERS[0];
  const visible = tasks.filter((t) => active.match(t.latestRun?.status));

  const loadTasks = () => api.listTasks().then(setTasks).catch((e) => setError(errorMessage(e)));

  useEffect(() => {
    // 직접 실행 대상에서 PM은 제외 (PM에게는 PM 채팅으로 요청)
    api
      .listAgents()
      .then((all) => setAgents(all.filter((a) => a.kind !== "pm")))
      .catch((e) => setError(errorMessage(e)));
    loadTasks();
    // Run 상태 변경 시 목록의 최신 실행 상태만 갱신. 모르는 작업이면(PM이 새로 위임) 목록을 다시 읽는다.
    const un = listen<Run>(EVENTS.runUpdated, ({ payload }) => {
      setTasks((prev) => {
        const idx = prev.findIndex((t) => t.id === payload.taskId);
        if (idx < 0) {
          loadTasks();
          return prev;
        }
        const next = [...prev];
        const cur = next[idx].latestRun;
        if (!cur || payload.id >= cur.id) next[idx] = { ...next[idx], latestRun: payload };
        return next;
      });
    });
    return () => {
      un.then((f) => f());
    };
  }, []);

  return (
    <div className="tasks">
      <NewTask
        agents={agents}
        canIsolate={canIsolate}
        onGoTeam={onGoTeam}
        onCreated={(t) => {
          setTasks((prev) => [t, ...prev]);
          setSelected(t.id);
        }}
      />
      {error && <p className="error">{error}</p>}
      <div className="row filters">
        {FILTERS.map((f) => {
          const count = tasks.filter((t) => f.match(t.latestRun?.status)).length;
          return (
            <button key={f.id} className={`chip ${filter === f.id ? "active" : ""}`} onClick={() => setFilter(f.id)}>
              {f.label} <span className="muted">{count}</span>
            </button>
          );
        })}
      </div>
      <div className="tasks-body">
        <ul className="task-list">
          {visible.length === 0 && <li className="muted empty">해당하는 작업이 없습니다.</li>}
          {visible.map((t) => (
            <li
              key={t.id}
              className={`task-item ${selected === t.id ? "active" : ""}`}
              onClick={() => setSelected(t.id)}
            >
              <div className="task-title">{t.title}</div>
              <div className="row small">
                <StatusBadge status={t.latestRun?.status} />
                {MODE_LABEL[t.mode] && <span className="badge">{MODE_LABEL[t.mode]}</span>}
                <IntegrationBadge task={t} />
                <span className="muted">{t.agentName}</span>
                {t.dependsOn.length > 0 && (
                  <span className="muted" title="선행 작업이 성공해야 시작합니다">
                    ← {t.dependsOn.map((d) => `#${d}`).join(", ")}
                  </span>
                )}
                {t.latestRun && t.latestRun.attempt > 1 && <span className="muted">#{t.latestRun.attempt}</span>}
              </div>
            </li>
          ))}
        </ul>
        <div className="task-detail">
          {selected ? (
            <TaskDetailPanel key={selected} taskId={selected} />
          ) : (
            <p className="muted center-text">작업을 선택하세요.</p>
          )}
        </div>
      </div>
    </div>
  );
}

function NewTask({
  agents,
  canIsolate,
  onGoTeam,
  onCreated,
}: {
  agents: Agent[];
  canIsolate: boolean;
  onGoTeam: () => void;
  onCreated: (t: TaskSummary) => void;
}) {
  const [agentId, setAgentId] = useState<number | null>(null);
  const [prompt, setPrompt] = useState("");
  const [isolated, setIsolated] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (agentId === null && agents.length > 0) setAgentId(agents[0].id);
  }, [agents, agentId]);

  if (agents.length === 0) {
    return (
      <section className="card row">
        <span className="muted">작업을 맡길 에이전트가 없습니다.</span>
        <button className="primary" onClick={onGoTeam}>
          팀에서 에이전트 추가
        </button>
      </section>
    );
  }

  async function submit() {
    if (agentId === null || !prompt.trim()) return;
    setBusy(true);
    setError(null);
    try {
      onCreated(await api.startDirectTask(agentId, prompt, isolated && canIsolate));
      setPrompt("");
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="card new-task">
      <div className="row">
        <span>직접 실행</span>
        <select value={agentId ?? ""} onChange={(e) => setAgentId(Number(e.target.value))}>
          {agents.map((a) => (
            <option key={a.id} value={a.id}>
              {a.name} · {a.role}
            </option>
          ))}
        </select>
        <small className="muted">PM을 거치지 않고 이 에이전트에게 바로 맡깁니다.</small>
      </div>
      <textarea
        rows={3}
        value={prompt}
        placeholder="할 일을 적으세요. (⌘+Enter로 실행)"
        onChange={(e) => setPrompt(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) submit();
        }}
      />
      <div className="row">
        <button className="primary" onClick={submit} disabled={busy || !prompt.trim()}>
          실행
        </button>
        <label
          className="row small"
          title={
            canIsolate
              ? "사용자 작업 트리와 분리된 git worktree에서 실행하고, 결과는 확인 후 병합합니다. 커밋하지 않은 내 변경은 포함되지 않습니다."
              : "커밋이 있는 Git 저장소에서만 쓸 수 있습니다."
          }
        >
          <input
            type="checkbox"
            checked={isolated && canIsolate}
            disabled={!canIsolate}
            onChange={(e) => setIsolated(e.target.checked)}
          />
          격리 작업 공간에서 실행
        </label>
        {error && <span className="error">{error}</span>}
      </div>
    </section>
  );
}

function TaskDetailPanel({ taskId }: { taskId: number }) {
  const [detail, setDetail] = useState<TaskDetail | null>(null);
  const [runId, setRunId] = useState<number | null>(null);
  const [events, setEvents] = useState<StoredEvent[]>([]);
  const [approvals, setApprovals] = useState<Approval[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [view, setView] = useState<"log" | "changes">("log");
  const runIdRef = useRef<number | null>(null);
  runIdRef.current = runId;

  const loadDetail = (selectLatest: boolean) =>
    api
      .getTaskDetail(taskId)
      .then((d) => {
        setDetail(d);
        if (selectLatest && d.runs.length) setRunId(d.runs[d.runs.length - 1].id);
      })
      .catch((e) => setError(errorMessage(e)));

  const loadApprovals = (id: number) => api.getPendingApprovals(id).then(setApprovals).catch(() => {});

  useEffect(() => {
    loadDetail(true);
    const unUpdated = listen<Run>(EVENTS.runUpdated, ({ payload }) => {
      if (payload.taskId !== taskId) return;
      setDetail((d) => {
        if (!d) return d;
        const exists = d.runs.some((r) => r.id === payload.id);
        const runs = exists ? d.runs.map((r) => (r.id === payload.id ? payload : r)) : [...d.runs, payload];
        return { ...d, runs, latestRun: runs[runs.length - 1] };
      });
      if (payload.id === runIdRef.current) loadApprovals(payload.id);
    });
    const unEvent = listen<StoredEvent>(EVENTS.runEvent, ({ payload }) => {
      if (payload.runId !== runIdRef.current) return;
      setEvents((prev) => (prev.some((e) => e.seq === payload.seq) ? prev : [...prev, payload]));
      if (payload.kind === "approval_requested" || payload.kind === "approval_decided") loadApprovals(payload.runId);
    });
    return () => {
      unUpdated.then((f) => f());
      unEvent.then((f) => f());
    };
  }, [taskId]);

  // 실행 선택 시 이벤트 이력 로드 (실시간 이벤트와 seq로 병합)
  useEffect(() => {
    if (runId === null) return;
    setEvents([]);
    api
      .getRunEvents(runId)
      .then((loaded) =>
        setEvents((live) => {
          const seen = new Set(loaded.map((e) => e.seq));
          return [...loaded, ...live.filter((e) => e.runId === runId && !seen.has(e.seq))];
        }),
      )
      .catch((e) => setError(errorMessage(e)));
    loadApprovals(runId);
  }, [runId]);

  const run = useMemo(() => detail?.runs.find((r) => r.id === runId) ?? null, [detail, runId]);

  if (!detail) return <p className="muted">불러오는 중…</p>;

  return (
    <div className="detail">
      <header className="detail-header">
        <div>
          <h2>{detail.title}</h2>
          <div className="row small muted">
            <span>{detail.agentName}</span>
            {run && <StatusBadge status={run.status} />}
            <IntegrationBadge task={detail} />
            {run?.config.model && <span className="mono">{run.config.model}</span>}
            {detail.isolation === "worktree" && <span title={run?.cwd}>격리 작업 공간</span>}
          </div>
        </div>
        {detail.runs.length > 1 && (
          <select value={runId ?? ""} onChange={(e) => setRunId(Number(e.target.value))}>
            {detail.runs.map((r) => (
              <option key={r.id} value={r.id}>
                실행 #{r.attempt} {r.resumeSessionId ? "(이어서)" : ""}
              </option>
            ))}
          </select>
        )}
      </header>

      {detail.isolation === "worktree" && (
        <div className="row tabs">
          <button className={`chip ${view === "log" ? "active" : ""}`} onClick={() => setView("log")}>
            실행 기록
          </button>
          <button className={`chip ${view === "changes" ? "active" : ""}`} onClick={() => setView("changes")}>
            변경 내역
          </button>
        </div>
      )}

      {view === "changes" && detail.isolation === "worktree" ? (
        <div className="timeline">
          <DiffPanel taskId={taskId} onChanged={() => loadDetail(false)} />
        </div>
      ) : (
        <Timeline events={events} />
      )}

      {runId !== null && <RunUsagePanel taskId={taskId} runId={runId} />}

      {approvals.map((a) => (
        <ApprovalCard key={a.id} approval={a} onError={setError} />
      ))}

      {run?.error && <p className="error pre">{run.error}</p>}
      {error && <p className="error">{error}</p>}

      {run && (
        <RunActions
          run={run}
          taskId={taskId}
          onError={setError}
          onNewRun={(r) => {
            setDetail((d) => (d ? { ...d, runs: [...d.runs.filter((x) => x.id !== r.id), r] } : d));
            setRunId(r.id);
          }}
        />
      )}
    </div>
  );
}

function Timeline({ events }: { events: StoredEvent[] }) {
  const endRef = useRef<HTMLDivElement>(null);
  useEffect(() => endRef.current?.scrollIntoView({ block: "end" }), [events.length]);

  return (
    <div className="timeline">
      {events.length === 0 && <p className="muted">아직 기록이 없습니다.</p>}
      {events.map((e) => (
        <EventRow key={e.seq} e={e} />
      ))}
      <div ref={endRef} />
    </div>
  );
}

function EventRow({ e }: { e: StoredEvent }) {
  switch (e.kind) {
    case "user_message":
      return <div className="ev ev-user">{e.text}</div>;
    case "text":
      return <div className="ev ev-text">{e.text}</div>;
    case "tool_use":
      return (
        <div className="ev ev-tool">
          <span className="mono tool-name">{e.name}</span> <span className="mono muted">{e.summary}</span>
        </div>
      );
    case "tool_result":
      return (
        <details className={`ev ev-result ${e.isError ? "is-error" : ""}`}>
          <summary className="muted">
            {e.isError ? "도구 오류" : "도구 결과"}
            {e.truncated && " (일부만 저장됨)"}
          </summary>
          <pre>{e.preview}</pre>
        </details>
      );
    case "approval_requested":
      return (
        <div className="ev ev-notice">
          승인 요청: <span className="mono">{e.toolName}</span> <span className="mono muted">{e.summary}</span>
        </div>
      );
    case "approval_decided":
      return <div className="ev ev-notice">{e.allowed ? "승인함" : "거부함"}</div>;
    case "permission_denied":
      return (
        <div className="ev ev-notice warn-text">
          권한 정책으로 거부: <span className="mono">{e.toolName}</span>
        </div>
      );
    case "turn_result":
      return (
        <div className={`ev ev-result-summary ${e.isError ? "is-error" : ""}`}>
          <span>{e.isError ? `턴 오류 (${e.subtype})` : "턴 완료"}</span>
          {e.durationMs != null && <span className="muted"> · {(e.durationMs / 1000).toFixed(1)}초</span>}
          <UsageLine u={e.usage} />
        </div>
      );
    case "stderr":
      return <div className="ev mono muted small">{e.text}</div>;
    case "notice":
      return <div className="ev ev-notice">{e.text}</div>;
    case "started":
      return <div className="ev muted small">세션 시작 · {e.model ?? "기본 모델"}</div>;
  }
}

/** 턴 단위 토큰 (런타임 보고, 내부 보조 호출 제외). 비용은 실행 사용량 패널에서 본다. */
function UsageLine({ u }: { u: Usage }) {
  const n = (v: number | null) => (v == null ? "-" : v.toLocaleString());
  return (
    <span className="muted small usage" title="이 턴의 모델 호출 토큰입니다. 내부 보조 호출은 실행 사용량에 합산됩니다.">
      {" "}
      · 입력 {n(u.inputTokens)} / 출력 {n(u.outputTokens)} / 캐시 읽기 {n(u.cacheReadTokens)} / 캐시 생성{" "}
      {n(u.cacheCreationTokens)}
    </span>
  );
}

/** 선택한 실행과 작업 전체의 사용량 (출처 포함) */
function RunUsagePanel({ taskId, runId }: { taskId: number; runId: number }) {
  const [rows, setRows] = useState<RunUsage[]>([]);

  useEffect(() => {
    const load = () => api.getTaskUsage(taskId).then(setRows).catch(() => {});
    load();
    const un = listen<number>(EVENTS.usageUpdated, ({ payload }) => {
      if (payload === taskId) load();
    });
    return () => {
      un.then((f) => f());
    };
  }, [taskId]);

  const cur = rows.find((r) => r.runId === runId);
  if (!cur) return null;
  const total = rows.reduce<UsageTotals>(
    (acc, r) => ({
      inputTokens: acc.inputTokens + r.totals.inputTokens,
      outputTokens: acc.outputTokens + r.totals.outputTokens,
      cacheReadTokens: acc.cacheReadTokens + r.totals.cacheReadTokens,
      cacheCreationTokens: acc.cacheCreationTokens + r.totals.cacheCreationTokens,
      costUsd:
        acc.costUsd == null && r.totals.costUsd == null ? null : (acc.costUsd ?? 0) + (r.totals.costUsd ?? 0),
      reported: acc.reported + r.totals.reported,
      partial: acc.partial + r.totals.partial,
      unavailable: acc.unavailable + r.totals.unavailable,
    }),
    { inputTokens: 0, outputTokens: 0, cacheReadTokens: 0, cacheCreationTokens: 0, costUsd: null, reported: 0, partial: 0, unavailable: 0 },
  );

  return (
    <details className="run-usage">
      <summary className="row small">
        <span>실행 #{cur.attempt} 사용량</span>
        <UsageNumbers t={cur.totals} compact />
        <SourceBadges t={cur.totals} />
      </summary>
      {rows.length > 1 && (
        <div className="row small">
          <span className="muted">작업 전체 ({rows.length}회 실행)</span>
          <UsageNumbers t={total} compact />
        </div>
      )}
    </details>
  );
}

const FILTERS: { id: string; label: string; match: (s: RunStatus | undefined) => boolean }[] = [
  { id: "all", label: "전체", match: () => true },
  { id: "active", label: "진행", match: (s) => s === "running" || s === "cancelling" },
  { id: "waiting", label: "입력·승인 대기", match: (s) => s === "waiting_for_input" || s === "waiting_for_approval" },
  { id: "queued", label: "대기", match: (s) => s === "queued" },
  { id: "done", label: "완료", match: (s) => s === "succeeded" },
  { id: "failed", label: "실패·중단", match: (s) => s === "failed" || s === "cancelled" || s === "interrupted" },
];

function ApprovalCard({ approval, onError }: { approval: Approval; onError: (m: string) => void }) {
  const [busy, setBusy] = useState(false);
  async function decide(allow: boolean) {
    setBusy(true);
    try {
      await api.respondApproval(approval.id, allow);
    } catch (e) {
      onError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <div className="approval">
      <div>
        <strong>승인 필요</strong> · <span className="mono">{approval.toolName}</span>
        {approval.description && <span className="muted"> — {approval.description}</span>}
      </div>
      <pre className="mono">{JSON.stringify(approval.toolInput, null, 2)}</pre>
      <div className="row">
        <button className="primary" disabled={busy} onClick={() => decide(true)}>
          허용
        </button>
        <button disabled={busy} onClick={() => decide(false)}>
          거부
        </button>
      </div>
    </div>
  );
}

function RunActions({
  run,
  taskId,
  onError,
  onNewRun,
}: {
  run: Run;
  taskId: number;
  onError: (m: string | null) => void;
  onNewRun: (r: Run) => void;
}) {
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const active = !isTerminal(run.status);

  async function act(fn: () => Promise<unknown>) {
    setBusy(true);
    onError(null);
    try {
      await fn();
    } catch (e) {
      onError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  }

  const send = () =>
    act(async () => {
      if (!text.trim()) return;
      if (active) await api.sendRunMessage(run.id, text);
      else onNewRun(await api.continueTask(taskId, text));
      setText("");
    });

  return (
    <div className="run-actions">
      <textarea
        rows={2}
        value={text}
        disabled={run.status === "cancelling" || run.status === "queued"}
        placeholder={
          active ? "추가 지시 (현재 실행에 전달)" : run.sessionId ? "이어서 지시 (같은 세션 재개)" : "세션이 없어 이어갈 수 없습니다"
        }
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) send();
        }}
      />
      <div className="row">
        <button className="primary" disabled={busy || !text.trim() || (!active && !run.sessionId)} onClick={send}>
          {active ? "추가 지시" : "이어서 실행"}
        </button>
        {active ? (
          <button disabled={busy || run.status === "cancelling"} onClick={() => act(() => api.cancelRun(run.id))}>
            중단
          </button>
        ) : (
          <button disabled={busy} onClick={() => act(async () => onNewRun(await api.rerunTask(taskId)))}>
            처음부터 재실행
          </button>
        )}
      </div>
    </div>
  );
}
