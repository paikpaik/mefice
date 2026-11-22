import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  ALL_TOOLS,
  api,
  errorMessage,
  EVENTS,
  isTerminal,
  type GoalDetail,
  type GoalSummary,
  type Message,
  type PmDecision,
  type PmMode,
  type Run,
  type TaskSummary,
  type UsageGroup,
} from "../api";
import { StatusBadge } from "../components/StatusBadge";
import { IntegrationBadge } from "../components/DiffPanel";
import { fmtCost, fmtTokens } from "../components/Usage";
import { inputFromTemplate, templateForRole } from "../office/hire";

const MODE_LABEL: Record<PmMode, string> = {
  direct: "직접 처리",
  delegate: "위임",
  ask: "질문",
  hire: "고용 제안",
  report: "결과 보고",
};

interface Props {
  onOpenTask: (taskId: number) => void;
  onGoTeam: () => void;
}

/** PM 채팅 (스펙 3장 흐름, 4.2). 목표 하나 = 스레드 하나 = PM 세션 하나. */
export function PmChat({ onOpenTask, onGoTeam }: Props) {
  const [goals, setGoals] = useState<GoalSummary[]>([]);
  const [selected, setSelected] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);

  const loadGoals = () => api.listGoals().then(setGoals).catch((e) => setError(errorMessage(e)));

  useEffect(() => {
    loadGoals();
    const unMsg = listen<Message>(EVENTS.pmMessage, loadGoals);
    const unRun = listen<Run>(EVENTS.runUpdated, loadGoals);
    return () => {
      unMsg.then((f) => f());
      unRun.then((f) => f());
    };
  }, []);

  return (
    <div className="pm">
      <aside className="goal-list">
        <button className={`goal-new ${selected === null ? "active" : ""}`} onClick={() => setSelected(null)}>
          + 새 목표
        </button>
        {goals.map((g) => (
          <div
            key={g.id}
            className={`goal-item ${selected === g.id ? "active" : ""}`}
            onClick={() => setSelected(g.id)}
          >
            <div className="task-title">{g.title}</div>
            <div className="row small">
              <GoalState goal={g} />
            </div>
          </div>
        ))}
      </aside>
      <section className="thread">
        {error && <p className="error">{error}</p>}
        {selected === null ? (
          <NewGoal onStarted={(id) => setSelected(id)} onGoTeam={onGoTeam} />
        ) : (
          <GoalThread key={selected} goalId={selected} onOpenTask={onOpenTask} />
        )}
      </section>
    </div>
  );
}

function GoalState({ goal }: { goal: GoalSummary }) {
  const pmActive = goal.latestRun && !isTerminal(goal.latestRun.status);
  if (pmActive) return <StatusBadge status={goal.latestRun!.status} />;
  if (goal.activeChildren > 0) return <span className="badge info">팀원 작업 중 {goal.activeChildren}</span>;
  const mode = (goal.lastMessage?.payload as PmDecision | null)?.mode;
  return <span className="muted">{mode ? MODE_LABEL[mode] : (goal.lastMessage?.author ?? "")}</span>;
}

function NewGoal({ onStarted, onGoTeam }: { onStarted: (id: number) => void; onGoTeam: () => void }) {
  return (
    <div className="new-goal">
      <div className="new-goal-intro">
        <h2>PM에게 목표 맡기기</h2>
        <p className="muted">
          PM이 직접 처리할지, 팀원에게 위임할지 판단합니다. 작은 일은 PM이 바로 처리해 관리 비용을 줄입니다.
        </p>
        <p className="hint">
          팀원 구성은 <button className="link" onClick={onGoTeam}>팀</button> 탭에서 관리합니다. 맞는 팀원이 없으면 PM이 고용을
          제안합니다.
        </p>
      </div>
      <Composer goalId={null} onSent={onStarted} placeholder="목표와 완료 조건을 적으세요. (⌘+Enter로 보내기)" />
    </div>
  );
}

function GoalThread({ goalId, onOpenTask }: { goalId: number; onOpenTask: (id: number) => void }) {
  const [detail, setDetail] = useState<GoalDetail | null>(null);
  const [error, setError] = useState<string | null>(null);
  const endRef = useRef<HTMLDivElement>(null);

  const load = () =>
    api
      .getGoal(goalId)
      .then(setDetail)
      .catch((e) => setError(errorMessage(e)));

  useEffect(() => {
    load();
    const unMsg = listen<Message>(EVENTS.pmMessage, ({ payload }) => {
      if (payload.goalTaskId === goalId) load();
    });
    // 목표 또는 위임 작업의 상태·사용량 변화
    const unRun = listen<Run>(EVENTS.runUpdated, load);
    const unUsage = listen(EVENTS.usageUpdated, load);
    return () => {
      unMsg.then((f) => f());
      unRun.then((f) => f());
      unUsage.then((f) => f());
    };
  }, [goalId]);

  useEffect(() => endRef.current?.scrollIntoView({ block: "end" }), [detail?.messages.length]);

  if (!detail) return <p className="muted">{error ?? "불러오는 중…"}</p>;
  const pmRun = detail.latestRun;
  const pmBusy = pmRun && !isTerminal(pmRun.status);

  return (
    <div className="goal-thread">
      <header className="detail-header">
        <div>
          <h2>{detail.title}</h2>
          <GoalUsage groups={detail.usageByRole} />
        </div>
        <button onClick={() => onOpenTask(detail.id)}>PM 실행 기록</button>
      </header>

      <div className="messages">
        {detail.messages.map((m) => (
          <MessageRow key={m.id} m={m} children={detail.children} onOpenTask={onOpenTask} />
        ))}
        {pmBusy && (
          <div className="msg msg-system">
            {pmRun.status === "waiting_for_approval" ? (
              <>
                PM이 도구 실행 승인을 기다립니다.{" "}
                <button className="link" onClick={() => onOpenTask(detail.id)}>
                  승인하러 가기
                </button>
              </>
            ) : pmRun.status === "queued" ? (
              "PM 실행 대기 중 (동시 실행 상한)"
            ) : (
              "PM이 응답 중…"
            )}
          </div>
        )}
        <div ref={endRef} />
      </div>

      {detail.children.length > 0 && (
        <div className="children">
          {detail.children.map((c) => (
            <ChildCard key={c.id} task={c} onOpen={() => onOpenTask(c.id)} />
          ))}
        </div>
      )}

      {error && <p className="error">{error}</p>}
      <Composer
        goalId={goalId}
        onSent={() => load()}
        placeholder={pmBusy ? "추가 지시 (진행 중인 PM에게 전달)" : "답변이나 추가 지시 (같은 PM 세션에서 이어짐)"}
      />
    </div>
  );
}

function MessageRow({
  m,
  children,
  onOpenTask,
}: {
  m: Message;
  children: TaskSummary[];
  onOpenTask: (id: number) => void;
}) {
  if (m.author === "user") return <div className="msg msg-user">{m.text}</div>;
  if (m.author === "system") {
    const taskId = typeof m.payload?.taskId === "number" ? (m.payload.taskId as number) : null;
    return (
      <div className="msg msg-system">
        {m.text}
        {taskId && children.some((c) => c.id === taskId) && (
          <>
            {" "}
            <button className="link" onClick={() => onOpenTask(taskId)}>
              작업 보기
            </button>
          </>
        )}
      </div>
    );
  }
  const d = m.payload as PmDecision | null;
  return (
    <div className="msg msg-pm">
      <div className="row small">
        <strong>PM</strong>
        {d && <span className={`badge ${d.mode === "report" ? "ok" : d.mode === "ask" ? "warn" : "info"}`}>{MODE_LABEL[d.mode]}</span>}
      </div>
      <div className="msg-text">{m.text}</div>
      {d?.mode === "delegate" &&
        d.tasks.map((t, i) => (
          <details key={i} className="spec">
            <summary className="small muted">위임 명세: {t.title || t.goal}</summary>
            <dl className="kv small">
              <dt>목표</dt>
              <dd>{t.goal}</dd>
              <dt>범위</dt>
              <dd>{t.scope || "-"}</dd>
              <dt>입력</dt>
              <dd>{t.inputs || "-"}</dd>
              <dt>완료 조건</dt>
              <dd>{t.doneCriteria || "-"}</dd>
            </dl>
          </details>
        ))}
      {d?.mode === "hire" && d.hire && <HireCard hire={d.hire} />}
    </div>
  );
}

/** 고용 제안. MVP에서는 사용자가 확정한다 (스펙 3장 5번). */
function HireCard({ hire }: { hire: NonNullable<PmDecision["hire"]> }) {
  const [state, setState] = useState<"idle" | "busy" | "done">("idle");
  const [error, setError] = useState<string | null>(null);

  async function hireNow() {
    setState("busy");
    setError(null);
    const tools = hire.tools.filter((t) => (ALL_TOOLS as readonly string[]).includes(t));
    try {
      // PM이 제안한 역할에 가까운 템플릿의 기본값(지침·권한·외형)으로 고용하고, 이름·자리는 로컬에서 정한다
      const [{ templates }, agents] = await Promise.all([api.listTemplates(), api.listAgents()]);
      const input = inputFromTemplate(templateForRole(hire.role, templates), agents);
      await api.createAgent({
        ...input,
        role: hire.role,
        tools: tools.length ? tools : input.tools,
      });
      setState("done");
    } catch (e) {
      setError(errorMessage(e));
      setState("idle");
    }
  }

  return (
    <div className="hire">
      <div>
        <strong>고용 제안: {hire.role}</strong>
      </div>
      <div className="small muted">{hire.reason}</div>
      {hire.tools.length > 0 && <div className="small mono muted">도구: {hire.tools.join(", ")}</div>}
      <div className="row">
        {state === "done" ? (
          <span className="badge ok">고용함 — PM에게 진행하라고 답하세요</span>
        ) : (
          <button className="primary" disabled={state === "busy"} onClick={hireNow}>
            고용하기
          </button>
        )}
        {error && <span className="error">{error}</span>}
      </div>
    </div>
  );
}

function ChildCard({ task, onOpen }: { task: TaskSummary; onOpen: () => void }) {
  return (
    <div className="child" onClick={onOpen}>
      <span className="muted small">#{task.id}</span>
      <span className="task-title">
        {task.title}
        {task.dependsOn.length > 0 && (
          <span className="muted small"> ← {task.dependsOn.map((d) => `#${d}`).join(", ")}</span>
        )}
      </span>
      <span className="muted small">{task.agentName}</span>
      <span className="row">
        <StatusBadge status={task.latestRun?.status} />
        <IntegrationBadge task={task} />
      </span>
    </div>
  );
}

/** PM 추가 사용량을 작업자 사용량과 나란히 보여준다 (스펙 13장). */
function GoalUsage({ groups }: { groups: UsageGroup[] }) {
  if (groups.length === 0) return null;
  const pm = groups.find((g) => g.key === "pm")?.totals;
  const work = groups
    .filter((g) => g.key !== "pm")
    .reduce((acc, g) => acc + (g.totals.costUsd ?? 0), 0);
  const ratio = pm?.costUsd != null && work > 0 ? Math.round((pm.costUsd / work) * 100) : null;
  return (
    <div className="row small muted goal-usage">
      {groups.map((g) => (
        <span key={g.key} title={`입력 ${g.totals.inputTokens} / 출력 ${g.totals.outputTokens} / 캐시 읽기 ${g.totals.cacheReadTokens}`}>
          {g.key === "pm" ? "PM" : g.key === "worker" ? "작업자" : "검토"} {fmtCost(g.totals.costUsd)} ·{" "}
          {fmtTokens(g.totals.outputTokens)} 출력
        </span>
      ))}
      {ratio != null && (
        <span title="작업자 비용 대비 PM 관리 비용 (API 정가 환산 기준)">PM 추가 사용량 {ratio}%</span>
      )}
    </div>
  );
}

function Composer({
  goalId,
  onSent,
  placeholder,
}: {
  goalId: number | null;
  onSent: (goalId: number) => void;
  placeholder: string;
}) {
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function send() {
    if (!text.trim() || busy) return;
    setBusy(true);
    setError(null);
    try {
      const id = await api.pmSend(goalId, text);
      setText("");
      onSent(id);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="composer">
      <textarea
        rows={3}
        value={text}
        placeholder={placeholder}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) send();
        }}
      />
      <div className="row">
        <button className="primary" disabled={busy || !text.trim()} onClick={send}>
          보내기
        </button>
        {error && <span className="error">{error}</span>}
      </div>
    </div>
  );
}
