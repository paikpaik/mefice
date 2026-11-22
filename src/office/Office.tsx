import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api, errorMessage, EVENTS, type OfficeAgent } from "../api";
import { Character, CharacterBubble, type BubbleKind, type Pose } from "./Character";
import {
  DESK_H,
  DESK_W,
  LOUNGE,
  PM_DESK,
  PM_LOCATION,
  VIEW,
  deskLocation,
  deskPos,
  route,
  type Location,
  type Point,
} from "./layout";
import { AgentPanel } from "./AgentPanel";

/** 캐릭터 행동 모드 (스펙 4.5 상태 → 시각 표현) */
type Mode = "idle" | "wait" | "work" | "ask" | "error" | "done";

const DONE_SHOW_MS = 8_000; // 완료 알림 후 휴식
const ERROR_SHOW_MS = 10 * 60_000; // 오류 표시 (클릭으로 확인 가능)
const SPEED = 90; // 논리 좌표/초
const DISMISSED_KEY = "mefice.office.dismissedErrors";

const BUBBLE: Record<Mode, BubbleKind> = {
  idle: null,
  wait: "wait",
  work: null,
  ask: "question",
  error: "error",
  done: "done",
};

const MODE_LABEL: Record<Mode, string> = {
  idle: "쉬는 중",
  wait: "실행 대기",
  work: "작업 중",
  ask: "승인·입력 필요",
  error: "오류",
  done: "완료",
};

interface Sim {
  x: number;
  y: number;
  loc: Location | null;
  target: Location | null;
  queue: Point[];
  dwellUntil: number;
  facing: 1 | -1;
  /** React에 마지막으로 반영한 자세·방향 (바뀔 때만 다시 그리기 위해) */
  shownPose: Pose;
  shownFacing: 1 | -1;
}

function loadDismissed(): Set<number> {
  try {
    return new Set(JSON.parse(localStorage.getItem(DISMISSED_KEY) ?? "[]"));
  } catch {
    return new Set();
  }
}

function saveDismissed(s: Set<number>) {
  try {
    localStorage.setItem(DISMISSED_KEY, JSON.stringify([...s].slice(-200)));
  } catch {
    /* 저장 실패는 무시 (표시 편의 기능) */
  }
}

function homeOf(a: OfficeAgent): Location {
  if (a.agent.kind === "pm") return PM_LOCATION;
  return a.agent.profile.desk != null ? deskLocation(a.agent.profile.desk) : LOUNGE[a.agent.id % LOUNGE.length];
}

function modeOf(a: OfficeAgent, now: number, dismissed: Set<number>): Mode {
  switch (a.state) {
    case "working":
      return "work";
    case "needs_input":
      return "ask";
    case "queued":
      return "wait";
    case "error":
      return a.last && !dismissed.has(a.last.runId) && now - (a.last.endedAt ?? 0) < ERROR_SHOW_MS ? "error" : "idle";
    case "done":
      return a.last && now - (a.last.endedAt ?? 0) < DONE_SHOW_MS ? "done" : "idle";
    default:
      return "idle";
  }
}

interface Props {
  onOpenTask: (taskId: number) => void;
  onGoTeam: () => void;
}

/** 사무실 (스펙 4.5). 대기 행동은 모두 로컬 애니메이션이며 모델을 호출하지 않는다. */
export function Office({ onOpenTask, onGoTeam }: Props) {
  const [agents, setAgents] = useState<OfficeAgent[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<number | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const [dismissed, setDismissed] = useState<Set<number>>(loadDismissed);
  const [poses, setPoses] = useState<Record<number, { pose: Pose; facing: 1 | -1 }>>({});

  const nodes = useRef(new Map<number, SVGGElement>());
  const sims = useRef(new Map<number, Sim>());
  const agentsRef = useRef<OfficeAgent[]>([]);
  const modesRef = useRef(new Map<number, Mode>());

  const load = useCallback(
    () =>
      api
        .getOfficeState()
        .then((a) => {
          setAgents(a);
          setError(null);
        })
        .catch((e) => setError(errorMessage(e))),
    [],
  );

  // 실행 상태가 바뀔 때만 다시 읽는다 (폴링 없음). 여러 이벤트가 몰리면 한 번으로 묶는다.
  useEffect(() => {
    load();
    let timer: number | undefined;
    const debounced = () => {
      window.clearTimeout(timer);
      timer = window.setTimeout(load, 300);
    };
    const un = listen(EVENTS.runUpdated, debounced);
    // 완료 알림·오류 표시 시간 경과 판단용 로컬 시계
    const tick = window.setInterval(() => setNow(Date.now()), 2000);
    return () => {
      un.then((f) => f());
      window.clearTimeout(timer);
      window.clearInterval(tick);
    };
  }, [load]);

  const modes = useMemo(() => {
    const m = new Map<number, Mode>();
    for (const a of agents) m.set(a.agent.id, modeOf(a, now, dismissed));
    return m;
  }, [agents, now, dismissed]);

  agentsRef.current = agents;
  modesRef.current = modes;

  // 이동 엔진: requestAnimationFrame으로 위치만 직접 갱신. 창이 숨겨지면 멈춘다.
  useEffect(() => {
    const reduced = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false;
    let raf = 0;
    let last = performance.now();

    const frame = (t: number) => {
      const dt = Math.min(0.1, (t - last) / 1000);
      last = t;
      const clock = Date.now();
      const changed: Record<number, { pose: Pose; facing: 1 | -1 }> = {};

      for (const a of agentsRef.current) {
        const id = a.agent.id;
        const home = homeOf(a);
        const mode = modesRef.current.get(id) ?? "idle";
        let s = sims.current.get(id);
        if (!s) {
          s = {
            x: home.x,
            y: home.y,
            loc: home,
            target: home,
            queue: [],
            dwellUntil: clock + Math.random() * 5000,
            facing: 1,
            shownPose: "sit",
            shownFacing: 1,
          };
          sims.current.set(id, s);
        }

        // 목적지 결정: 할 일이 있으면 자리로, 없으면 쉬는 곳을 돌아다닌다
        if (s.queue.length === 0) {
          if (mode !== "idle") {
            if (s.loc?.id !== home.id) {
              s.queue = route(s.loc, home);
              s.target = home;
            }
          } else if (clock > s.dwellUntil) {
            const options = [home, ...LOUNGE].filter((l) => l.id !== s!.loc?.id);
            const next = options[Math.floor(Math.random() * options.length)];
            s.queue = route(s.loc, next);
            s.target = next;
          }
        }

        // 이동
        let moving = false;
        if (s.queue.length > 0) {
          const p = s.queue[0];
          const dx = p.x - s.x;
          const dy = p.y - s.y;
          const dist = Math.hypot(dx, dy);
          const step = reduced ? Infinity : SPEED * dt;
          if (Math.abs(dx) > 0.5) {
            const f: 1 | -1 = dx > 0 ? 1 : -1;
            if (f !== s.facing) s.facing = f;
          }
          if (dist <= step) {
            s.x = p.x;
            s.y = p.y;
            s.queue.shift();
            if (s.queue.length === 0) {
              s.loc = s.target;
              s.dwellUntil = clock + 4000 + Math.random() * 8000;
            }
          } else {
            s.x += (dx / dist) * step;
            s.y += (dy / dist) * step;
          }
          moving = s.queue.length > 0;
        }

        const pose: Pose = moving ? "walk" : s.loc?.sit ? (mode === "work" && s.loc.id === home.id ? "type" : "sit") : "stand";
        if (pose !== s.shownPose || s.facing !== s.shownFacing) {
          s.shownPose = pose;
          s.shownFacing = s.facing;
          changed[id] = { pose, facing: s.facing };
        }
        nodes.current.get(id)?.setAttribute("transform", `translate(${s.x.toFixed(1)},${s.y.toFixed(1)})`);
      }

      if (Object.keys(changed).length) setPoses((prev) => ({ ...prev, ...changed }));
      raf = requestAnimationFrame(frame);
    };

    const start = () => {
      if (raf || document.hidden) return;
      last = performance.now();
      raf = requestAnimationFrame(frame);
    };
    const stop = () => {
      cancelAnimationFrame(raf);
      raf = 0;
    };
    const onVisibility = () => (document.hidden ? stop() : start());
    document.addEventListener("visibilitychange", onVisibility);
    start();
    return () => {
      stop();
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, []);

  const dismissError = (runId: number) => {
    const next = new Set(dismissed);
    next.add(runId);
    setDismissed(next);
    saveDismissed(next);
  };

  const workers = agents.filter((a) => a.agent.kind !== "pm");
  const counts = useMemo(() => {
    const c: Partial<Record<Mode, number>> = {};
    for (const m of modes.values()) c[m] = (c[m] ?? 0) + 1;
    return c;
  }, [modes]);
  const selectedAgent = agents.find((a) => a.agent.id === selected) ?? null;

  return (
    <div className="office">
      <div className="office-bar row small">
        <span>팀원 {workers.length}명</span>
        {(["work", "ask", "wait", "error", "done"] as Mode[]).map(
          (m) =>
            counts[m] ? (
              <span key={m} className={`badge ${m === "ask" || m === "error" ? "warn" : m === "work" ? "info" : ""}`}>
                {MODE_LABEL[m]} {counts[m]}
              </span>
            ) : null,
        )}
        <span className="muted">대기 중인 팀원은 모델을 호출하지 않습니다.</span>
        {workers.length === 0 && (
          <button className="primary" onClick={onGoTeam}>
            팀원 고용하기
          </button>
        )}
        {error && <span className="error">{error}</span>}
      </div>

      <div className="office-stage">
        <svg viewBox={`0 0 ${VIEW.width} ${VIEW.height}`} className="office-svg" role="img" aria-label="사무실">
          <Floor agents={agents} />
          {agents.map((a) => {
            const mode = modes.get(a.agent.id) ?? "idle";
            const p = poses[a.agent.id] ?? { pose: "sit" as Pose, facing: 1 as const };
            const s = sims.current.get(a.agent.id);
            const init = s ?? homeOf(a);
            const task = a.active[0]?.taskTitle ?? a.last?.taskTitle;
            return (
              <g
                key={a.agent.id}
                ref={(el) => {
                  if (el) nodes.current.set(a.agent.id, el);
                  else nodes.current.delete(a.agent.id);
                }}
                transform={`translate(${init.x},${init.y})`}
                className={`char-wrap ${selected === a.agent.id ? "selected" : ""}`}
                onClick={() => setSelected(a.agent.id)}
              >
                <title>
                  {a.agent.name} ({a.agent.role}) · {MODE_LABEL[mode]}
                  {task ? ` · ${task}` : ""}
                </title>
                <g transform={`scale(${p.facing},1)`}>
                  <Character
                    color={a.agent.color}
                    avatar={a.agent.profile.avatar}
                    pose={p.pose}
                    badge={a.agent.kind === "pm" ? "PM" : null}
                  />
                </g>
                {/* 말풍선·이름은 좌우 반전 영향을 받지 않게 바깥에 */}
                <CharacterBubble kind={BUBBLE[mode]} />
                <text y="16" textAnchor="middle" className="char-name">
                  {a.agent.name}
                  {a.active.length > 1 ? ` ×${a.active.length}` : ""}
                </text>
              </g>
            );
          })}
        </svg>

        {selectedAgent && (
          <AgentPanel
            key={selectedAgent.agent.id}
            office={selectedAgent}
            modeLabel={MODE_LABEL[modes.get(selectedAgent.agent.id) ?? "idle"]}
            showDismiss={modes.get(selectedAgent.agent.id) === "error"}
            onDismiss={() => selectedAgent.last && dismissError(selectedAgent.last.runId)}
            onClose={() => setSelected(null)}
            onOpenTask={onOpenTask}
          />
        )}
      </div>
    </div>
  );
}

/** 바닥·가구 (정적) */
function Floor({ agents }: { agents: OfficeAgent[] }) {
  const deskOwner = new Map<number, OfficeAgent>();
  for (const a of agents) if (a.agent.kind !== "pm" && a.agent.profile.desk != null) deskOwner.set(a.agent.profile.desk, a);
  return (
    <g className="floor">
      <rect x="0" y="0" width={VIEW.width} height={VIEW.height} className="floor-bg" />
      <rect x="0" y="470" width={VIEW.width} height={170} className="lounge-bg" />
      <text x="16" y="494" className="zone-label">
        휴게 공간
      </text>
      {/* 책상 */}
      {Array.from({ length: 10 }, (_, i) => {
        const d = deskPos(i);
        const owner = deskOwner.get(i);
        return (
          <g key={i} transform={`translate(${d.x},${d.y})`} className={owner ? "desk" : "desk empty"}>
            <rect x={-DESK_W / 2} y={-DESK_H / 2} width={DESK_W} height={DESK_H} rx="4" className="desk-top" />
            <rect x="-16" y="-34" width="32" height="20" rx="2" className="monitor" />
            <rect x="-3" y="-14" width="6" height="4" className="monitor" />
            <text x={DESK_W / 2 - 6} y={DESK_H / 2 - 5} textAnchor="end" className="desk-no">
              {i + 1}
            </text>
            {owner && <rect x={-DESK_W / 2} y={DESK_H / 2 - 3} width={DESK_W} height="3" fill={owner.agent.color} opacity="0.8" />}
          </g>
        );
      })}
      {/* PM 자리 */}
      <g transform={`translate(${PM_DESK.x},${PM_DESK.y})`} className="desk pm-desk">
        <rect x="-60" y={-DESK_H / 2} width="120" height={DESK_H} rx="4" className="desk-top" />
        <rect x="-16" y="-34" width="32" height="20" rx="2" className="monitor" />
        <text x="54" y={DESK_H / 2 - 5} textAnchor="end" className="desk-no">
          PM
        </text>
      </g>
      {/* 휴게 공간 가구 */}
      <rect x="62" y="586" width="186" height="26" rx="8" className="sofa" />
      <rect x="62" y="566" width="186" height="14" rx="6" className="sofa back" />
      <g transform="translate(420,515)">
        <rect x="-16" y="-30" width="32" height="34" rx="4" className="machine" />
        <circle cx="0" cy="-16" r="5" className="machine-light" />
      </g>
      <g transform="translate(600,520)">
        <rect x="-40" y="-40" width="80" height="44" rx="3" className="shelf" />
        {[-30, -18, -6, 6, 18].map((x, i) => (
          <rect key={x} x={x} y="-36" width="9" height="16" className={`book book-${i}`} />
        ))}
      </g>
      <g transform="translate(770,560)">
        <rect x="-10" y="-6" width="20" height="16" rx="3" className="pot" />
        <circle cx="0" cy="-16" r="14" className="leaf" />
        <circle cx="-9" cy="-24" r="9" className="leaf" />
        <circle cx="9" cy="-24" r="9" className="leaf" />
      </g>
      <g transform="translate(940,520)">
        <rect x="-12" y="-36" width="24" height="40" rx="4" className="machine" />
        <rect x="-9" y="-48" width="18" height="14" rx="4" className="water" />
      </g>
    </g>
  );
}
