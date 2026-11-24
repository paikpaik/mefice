import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  api,
  defaultChatOptions,
  errorMessage,
  EVENTS,
  isTerminal,
  type ChatDetail,
  type ChatOptions,
  type ChatSummary,
  type Run,
  type RunActivity,
  type RunDelta,
  type StoredEvent,
  type Todo,
} from "../api";
import { Markdown } from "./Markdown";
import { ToolCard } from "./ToolCard";
import { RequestCard } from "./RequestCards";
import { Composer } from "./Composer";
import { fmtCost, fmtTokens } from "../components/Usage";

function activityLabel(a: string | null): string | null {
  if (!a || a === "idle") return null;
  if (a === "thinking") return "생각 중…";
  if (a === "writing") return "답변 쓰는 중…";
  if (a.startsWith("tool:")) return "도구 사용 중…";
  return "응답 기다리는 중…";
}

/** 대화 화면 (v0.2 A단계 1번): 공식 Claude Code 대화 UI와 같은 기능을 Mefice 디자인으로 */
export function ChatScreen() {
  const [chats, setChats] = useState<ChatSummary[]>([]);
  const [selected, setSelected] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);

  const loadChats = useCallback(() => api.listChats().then(setChats).catch((e) => setError(errorMessage(e))), []);
  useEffect(() => {
    loadChats();
    const un = listen<Run>(EVENTS.runUpdated, loadChats);
    return () => {
      un.then((f) => f());
    };
  }, [loadChats]);

  return (
    <div className="chat">
      <aside className="chat-list">
        <button className={`goal-new ${selected === null ? "active" : ""}`} onClick={() => setSelected(null)}>
          + 새 대화
        </button>
        {chats.map((c) => (
          <div key={c.id} className={`goal-item ${selected === c.id ? "active" : ""}`} onClick={() => setSelected(c.id)}>
            <div className="task-title">{c.title}</div>
            <div className="small muted">
              {c.status === "running" || c.status === "waiting_for_approval" ? "응답 중" : new Date(c.updatedAt).toLocaleString()}
            </div>
          </div>
        ))}
      </aside>
      <section className="chat-main">
        {error && <p className="error">{error}</p>}
        {selected === null ? (
          <NewChat onStarted={(id) => { setSelected(id); loadChats(); }} />
        ) : (
          <ChatView key={selected} chatId={selected} onNewChat={() => setSelected(null)} />
        )}
      </section>
    </div>
  );
}

function NewChat({ onStarted }: { onStarted: (id: number) => void }) {
  const [options, setOptions] = useState<ChatOptions>(defaultChatOptions);
  return (
    <div className="chat-view">
      <div className="chat-empty">
        <h2>무엇을 도와드릴까요?</h2>
        <p className="muted">간단한 일은 여기서 바로 맡기세요. 큰 일은 PM 탭에서 팀에게 맡길 수 있습니다.</p>
      </div>
      <Composer
        busy={false}
        options={options}
        onOptions={setOptions}
        onSend={async (text, images) => onStarted(await api.startChat(text, options, images))}
      />
    </div>
  );
}

function ChatView({ chatId, onNewChat }: { chatId: number; onNewChat: () => void }) {
  const [detail, setDetail] = useState<ChatDetail | null>(null);
  const [events, setEvents] = useState<StoredEvent[]>([]);
  const [stream, setStream] = useState("");
  const [activity, setActivity] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [cost, setCost] = useState<number | null>(null);
  const endRef = useRef<HTMLDivElement>(null);
  const runIds = useRef<Set<number>>(new Set());

  // 이 대화의 비용 (런타임이 보고한 API 정가 환산값의 합)
  const loadCost = useCallback(
    () =>
      api
        .getTaskUsage(chatId)
        .then((rows) => {
          const vals = rows.map((r) => r.totals.costUsd).filter((c): c is number => c != null);
          setCost(vals.length ? vals.reduce((a, b) => a + b, 0) : null);
        })
        .catch(() => {}),
    [chatId],
  );

  const load = useCallback(
    () =>
      api
        .getChat(chatId)
        .then((d) => {
          setDetail(d);
          setEvents(d.events);
          runIds.current = new Set(d.events.map((e) => e.runId).concat(d.run ? [d.run.id] : []));
        })
        .catch((e) => setError(errorMessage(e))),
    [chatId],
  );

  useEffect(() => {
    load();
    const unEvent = listen<StoredEvent>(EVENTS.runEvent, ({ payload }) => {
      if (!runIds.current.has(payload.runId)) return;
      setEvents((prev) => (prev.some((e) => e.runId === payload.runId && e.seq === payload.seq) ? prev : [...prev, payload]));
      // 완성된 메시지가 오면 스트리밍 버퍼를 비운다
      if (payload.kind === "text" || payload.kind === "turn_result" || payload.kind === "tool_use") setStream("");
      if (payload.kind.startsWith("approval")) load();
    });
    const unRun = listen<Run>(EVENTS.runUpdated, ({ payload }) => {
      if (payload.taskId !== chatId) return;
      runIds.current.add(payload.id);
      setDetail((d) => (d ? { ...d, run: payload } : d));
      if (payload.status === "waiting_for_approval" || isTerminal(payload.status)) load();
    });
    const unDelta = listen<RunDelta>(EVENTS.runDelta, ({ payload }) => {
      if (payload.taskId === chatId) setStream((s) => s + payload.text);
    });
    const unAct = listen<RunActivity>(EVENTS.runActivity, ({ payload }) => {
      if (payload.taskId === chatId) setActivity(payload.activity);
    });
    loadCost();
    const unUsage = listen<number>(EVENTS.usageUpdated, ({ payload }) => {
      if (payload === chatId) loadCost();
    });
    return () => {
      [unEvent, unRun, unDelta, unAct, unUsage].forEach((u) => u.then((f) => f()));
    };
  }, [chatId, load, loadCost]);

  useEffect(() => endRef.current?.scrollIntoView({ block: "end" }), [events.length, stream, detail?.pending.length]);

  const run = detail?.run;
  const busy = !!run && ["queued", "running", "waiting_for_approval", "cancelling"].includes(run.status);

  // 최신 할 일 목록 (에이전트가 매번 전체 목록을 보낸다)
  const todos: Todo[] | null = useMemo(() => {
    for (let i = events.length - 1; i >= 0; i--) {
      const e = events[i];
      if (e.kind === "tool_use" && e.name === "TodoWrite" && e.input?.todos) return e.input.todos;
    }
    return null;
  }, [events]);

  // 현재 컨텍스트: 마지막 턴의 직전 호출 크기, 그 뒤에 압축했다면 압축 후 크기
  const context = useMemo(() => {
    let window: number | null = null;
    for (const e of events) if (e.kind === "turn_result" && e.contextWindow) window = e.contextWindow;
    for (let i = events.length - 1; i >= 0; i--) {
      const e = events[i];
      if (e.kind === "compacted" && e.postTokens != null) return { used: e.postTokens, window };
      if (e.kind === "turn_result" && e.contextTokens) return { used: e.contextTokens, window };
    }
    return null;
  }, [events]);

  if (!detail) return <p className="muted">{error ?? "불러오는 중…"}</p>;

  return (
    <div className="chat-view">
      <div className="messages chat-messages">
        <MessageList events={events} />
        {stream && (
          <div className="msg-assistant streaming">
            <Markdown text={stream} />
          </div>
        )}
        {busy && !stream && activityLabel(activity) && <div className="activity small muted">{activityLabel(activity)}</div>}
        {detail.pending.map((ap) => (
          <RequestCard key={ap.id} ap={ap} onError={setError} />
        ))}
        {run?.status === "failed" && run.error && <p className="error small">{run.error}</p>}
        <div ref={endRef} />
      </div>

      {todos && todos.some((t) => t.status !== "completed") && <TodoPanel todos={todos} />}
      {error && <p className="error small">{error}</p>}

      <div className="chat-meter small muted">
        {context && (
          <span title="모델이 한 번에 보는 대화 분량입니다. 가득 차면 /compact로 압축하세요.">
            컨텍스트 {fmtTokens(context.used)}
            {context.window ? ` / ${fmtTokens(context.window)} (${Math.max(1, Math.round((context.used / context.window) * 100))}%)` : ""}
          </span>
        )}
        {cost != null && (
          <span title="런타임이 보고한 API 정가 환산값입니다. 구독 로그인이라면 실제 청구액이 아닙니다.">
            이 대화 {fmtCost(cost)} (정가 환산)
          </span>
        )}
      </div>

      <Composer
        busy={busy}
        onNewChat={onNewChat}
        options={detail.options}
        onOptions={async (o) => {
          setDetail({ ...detail, options: o });
          try {
            await api.chatSetOptions(chatId, o);
          } catch (e) {
            setError(errorMessage(e));
          }
        }}
        onStop={() => api.chatInterrupt(chatId).catch((e) => setError(errorMessage(e)))}
        onSend={async (text, images) => {
          setError(null);
          await api.chatSend(chatId, text, images);
        }}
      />
    </div>
  );
}

/** 이벤트를 대화 형태로: 사용자 말풍선, 에이전트 마크다운, 도구 카드(결과를 짝지어) */
function MessageList({ events }: { events: StoredEvent[] }) {
  const results = new Map<string, Extract<StoredEvent, { kind: "tool_result" }>>();
  for (const e of events) if (e.kind === "tool_result") results.set(e.toolUseId, e);

  return (
    <>
      {events.map((e) => {
        const key = `${e.runId}-${e.seq}`;
        switch (e.kind) {
          case "user_message":
            return (
              <div key={key} className="msg-user-row">
                <div className="msg msg-user">
                  {e.images ? <div className="img-chip">이미지 {e.images}장</div> : null}
                  {e.text}
                </div>
              </div>
            );
          case "text":
            return (
              <div key={key} className="msg-assistant">
                <Markdown text={e.text} />
              </div>
            );
          case "tool_use":
            // 할 일 목록은 아래 패널로, 질문·계획은 요청 카드로 보여준다
            if (e.name === "TodoWrite" || e.name === "AskUserQuestion" || e.name === "ExitPlanMode") return null;
            return <ToolCard key={key} use={e} result={results.get(e.toolUseId)} />;
          case "permission_denied":
            return (
              <div key={key} className="msg-system">
                허용되지 않은 도구: {e.toolName}
              </div>
            );
          case "compacted":
            return (
              <div key={key} className="msg-system">
                대화를 압축했습니다
                {e.preTokens != null && e.postTokens != null
                  ? ` (${e.preTokens.toLocaleString()} → ${e.postTokens.toLocaleString()} 토큰)`
                  : ""}
              </div>
            );
          case "notice":
            return (
              <div key={key} className="msg-system">
                {e.text}
              </div>
            );
          case "turn_result":
            return e.isError ? (
              <div key={key} className="msg-system error-text">
                응답 중 문제가 생겼습니다 ({e.subtype})
              </div>
            ) : null;
          default:
            return null;
        }
      })}
    </>
  );
}

function TodoPanel({ todos }: { todos: Todo[] }) {
  const done = todos.filter((t) => t.status === "completed").length;
  return (
    <div className="todo-panel">
      <div className="small muted">
        할 일 {done}/{todos.length}
      </div>
      <ul className="plain">
        {todos.map((t, i) => (
          <li key={i} className={`todo ${t.status}`}>
            <span className="todo-mark">{t.status === "completed" ? "✓" : t.status === "in_progress" ? "▸" : "○"}</span>
            {t.status === "in_progress" ? t.activeForm : t.content}
          </li>
        ))}
      </ul>
    </div>
  );
}
