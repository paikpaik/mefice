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
  type PermissionMode,
  type Run,
  type RunActivity,
  type RunDelta,
  type StoredEvent,
  type Todo,
} from "../api";
import { Markdown } from "./Markdown";
import { ToolCard } from "./ToolCard";
import { RequestCard } from "./RequestCards";

const MODE_LABEL: Record<PermissionMode, string> = {
  default: "수정 전 확인",
  acceptEdits: "자동 수정",
  plan: "계획 먼저",
};

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
          <ChatView key={selected} chatId={selected} />
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
        onSend={async (text) => onStarted(await api.startChat(text, options))}
      />
    </div>
  );
}

function ChatView({ chatId }: { chatId: number }) {
  const [detail, setDetail] = useState<ChatDetail | null>(null);
  const [events, setEvents] = useState<StoredEvent[]>([]);
  const [stream, setStream] = useState("");
  const [activity, setActivity] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const endRef = useRef<HTMLDivElement>(null);
  const runIds = useRef<Set<number>>(new Set());

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
    return () => {
      [unEvent, unRun, unDelta, unAct].forEach((u) => u.then((f) => f()));
    };
  }, [chatId, load]);

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

      <Composer
        busy={busy}
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
        onSend={async (text) => {
          setError(null);
          await api.chatSend(chatId, text);
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
                <div className="msg msg-user">{e.text}</div>
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

function Composer({
  busy,
  options,
  onOptions,
  onSend,
  onStop,
}: {
  busy: boolean;
  options: ChatOptions;
  onOptions: (o: ChatOptions) => void;
  onSend: (text: string) => Promise<void>;
  onStop?: () => void;
}) {
  const [text, setText] = useState("");
  const [sending, setSending] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const send = async () => {
    const t = text.trim();
    if (!t || sending) return;
    setSending(true);
    setError(null);
    try {
      await onSend(t);
      setText("");
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setSending(false);
    }
  };

  // Esc: 응답 멈추기
  useEffect(() => {
    if (!busy || !onStop) return;
    const h = (e: KeyboardEvent) => {
      if (e.key === "Escape") onStop();
    };
    window.addEventListener("keydown", h);
    return () => window.removeEventListener("keydown", h);
  }, [busy, onStop]);

  return (
    <div className="composer chat-composer">
      <textarea
        rows={3}
        value={text}
        placeholder={busy ? "응답 중에도 이어서 보낼 수 있습니다 (Enter 전송, Shift+Enter 줄바꿈)" : "메시지 입력 (Enter 전송, Shift+Enter 줄바꿈)"}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          // 한글 조합 중 Enter는 글자 확정이므로 전송하지 않는다
          if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
            e.preventDefault();
            send();
          }
        }}
      />
      <div className="row composer-bar">
        <select
          value={options.permissionMode}
          onChange={(e) => onOptions({ ...options, permissionMode: e.target.value as PermissionMode })}
          title="수정 전 확인: 파일을 바꾸기 전에 묻습니다 · 자동 수정: 파일 수정은 묻지 않습니다 · 계획 먼저: 계획을 보여주고 승인받은 뒤 실행합니다"
        >
          {(Object.keys(MODE_LABEL) as PermissionMode[]).map((m) => (
            <option key={m} value={m}>
              {MODE_LABEL[m]}
            </option>
          ))}
        </select>
        <label className="row small" title="도구를 줄여 비용을 아낍니다 (약 1/3). 웹 조회·외부 연결 등은 쓸 수 없습니다. 다음 실행부터 적용됩니다.">
          <input type="checkbox" checked={options.economy} onChange={(e) => onOptions({ ...options, economy: e.target.checked })} />
          절약 모드
        </label>
        <span className="grow" />
        {error && <span className="error small">{error}</span>}
        {busy && onStop && (
          <button onClick={onStop} title="Esc">
            멈추기
          </button>
        )}
        <button className="primary" disabled={!text.trim() || sending} onClick={send}>
          보내기
        </button>
      </div>
    </div>
  );
}
