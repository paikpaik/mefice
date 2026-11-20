import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api, EVENTS, type RateLimitSnapshot, type UsageRole, type UsageTotals } from "../api";

export const ROLE_LABEL: Record<UsageRole, string> = {
  pm: "PM",
  worker: "작업자",
  review: "검토",
};

export const fmtTokens = (n: number) =>
  n >= 1_000_000 ? `${(n / 1_000_000).toFixed(2)}M` : n >= 10_000 ? `${(n / 1000).toFixed(1)}k` : n.toLocaleString();

export const fmtCost = (c: number | null) => (c == null ? "미제공" : `$${c < 0.01 ? c.toFixed(4) : c.toFixed(2)}`);

const SOURCE_HELP = {
  reported: "런타임이 보고한 세션 누적값의 차이. 내부 보조 호출까지 포함됩니다.",
  partial: "턴 단위 사용량만 제공됨. 내부 보조 호출이 빠져 실제보다 작을 수 있고 비용은 미제공입니다.",
  unavailable: "런타임이 수치를 주지 않고 끝난 실행(강제 종료·실행 실패 등). 0으로 계산하지 않았습니다.",
};

/** 수치의 출처 표시 (스펙 5장: 정확한 수치가 없으면 미제공 또는 추정치로 표시) */
export function SourceBadges({ t }: { t: UsageTotals }) {
  return (
    <span className="row small">
      {t.reported > 0 && (
        <span className="badge ok" title={SOURCE_HELP.reported}>
          런타임 보고 {t.reported}
        </span>
      )}
      {t.partial > 0 && (
        <span className="badge warn" title={SOURCE_HELP.partial}>
          부분 {t.partial}
        </span>
      )}
      {t.unavailable > 0 && (
        <span className="badge danger" title={SOURCE_HELP.unavailable}>
          미제공 {t.unavailable}
        </span>
      )}
      {t.reported + t.partial + t.unavailable === 0 && <span className="badge">기록 없음</span>}
    </span>
  );
}

export function UsageNumbers({ t, compact = false }: { t: UsageTotals; compact?: boolean }) {
  const items: [string, string, string?][] = [
    ["입력", fmtTokens(t.inputTokens)],
    ["출력", fmtTokens(t.outputTokens)],
    ["캐시 읽기", fmtTokens(t.cacheReadTokens), "이전 맥락을 캐시에서 재사용한 입력 토큰 (일반 입력보다 저렴)"],
    ["캐시 생성", fmtTokens(t.cacheCreationTokens), "캐시에 새로 저장한 입력 토큰"],
    ["정가 환산", fmtCost(t.costUsd), "런타임이 API 정가로 계산한 금액. 구독 로그인 시 실제 청구액이 아닙니다."],
  ];
  return (
    <div className={compact ? "usage-inline" : "usage-grid"}>
      {items.map(([label, value, help]) => (
        <div key={label} className="usage-cell" title={help}>
          <span className="muted small">{label}</span>
          <span className="mono">{value}</span>
        </div>
      ))}
    </div>
  );
}

/** 구독 로그인 여부에 따라 비용 해석을 안내한다. */
export function CostNote({ authKinds }: { authKinds: string[] }) {
  if (authKinds.includes("subscription")) {
    return (
      <p className="hint">
        구독 로그인으로 실행되었습니다. "정가 환산"은 API 가격으로 계산한 참고값이며 실제 청구액이 아닙니다. 구독 한도
        소진은 아래 "구독 한도" 사용률로 확인하세요.
      </p>
    );
  }
  if (authKinds.includes("api_key")) {
    return <p className="hint">API 키로 실행되었습니다. "정가 환산"은 런타임이 계산한 API 요금 추정치입니다.</p>;
  }
  return null;
}

const WINDOW_LABEL: Record<string, string> = { five_hour: "5시간", seven_day: "7일" };

/** 계정 전체 구독 한도 사용률. 런타임 실행 중에만 갱신된다 (조회용 모델 호출 없음). */
export function RateLimitCard() {
  const [snap, setSnap] = useState<RateLimitSnapshot | null>(null);

  useEffect(() => {
    api.getRateLimit().then(setSnap).catch(() => {});
    const un = listen<RateLimitSnapshot>(EVENTS.rateLimitUpdated, ({ payload }) => setSnap(payload));
    return () => {
      un.then((f) => f());
    };
  }, []);

  return (
    <section className="card">
      <header className="card-header">
        <h2>구독 한도</h2>
        {snap && <span className="muted small">관측: {new Date(snap.observedAt).toLocaleString()}</span>}
      </header>
      {!snap ? (
        <p className="muted">아직 관측된 값이 없습니다. 에이전트가 한 번 실행되면 표시됩니다.</p>
      ) : (
        <>
          {snap.windows.map((w) => (
            <div key={w.name} className="meter-row">
              <span className="meter-label">{WINDOW_LABEL[w.name] ?? w.name}</span>
              <div className="meter">
                <div
                  className={`meter-fill ${w.utilization >= 0.8 ? "danger" : w.utilization >= 0.5 ? "warn" : ""}`}
                  style={{ width: `${Math.min(100, w.utilization * 100)}%` }}
                />
              </div>
              <span className="mono">{Math.round(w.utilization * 100)}%</span>
              {w.resetsAt && (
                <span className="muted small">초기화 {new Date(w.resetsAt * 1000).toLocaleString()}</span>
              )}
            </div>
          ))}
          <p className="hint">
            계정 전체 사용률입니다. 터미널·확장 등 Mefice 밖 사용도 포함되며, 마지막 실행 시점 값입니다.
          </p>
        </>
      )}
    </section>
  );
}
