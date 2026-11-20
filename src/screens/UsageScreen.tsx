import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api, errorMessage, EVENTS, type UsageGroup, type UsageRole, type UsageSummary } from "../api";
import { CostNote, RateLimitCard, ROLE_LABEL, SourceBadges, UsageNumbers } from "../components/Usage";

type Range = "today" | "7d" | "all";

function sinceFor(range: Range): number | null {
  if (range === "all") return null;
  const d = new Date();
  d.setHours(0, 0, 0, 0);
  if (range === "7d") d.setDate(d.getDate() - 6);
  return d.getTime();
}

/** 프로젝트 사용량 (스펙 5장: PM·작업자·검토 구분, 출처 표시) */
export function UsageScreen() {
  const [range, setRange] = useState<Range>("today");
  const [summary, setSummary] = useState<UsageSummary | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const load = () =>
      api
        .getUsageSummary(sinceFor(range))
        .then(setSummary)
        .catch((e) => setError(errorMessage(e)));
    load();
    const un = listen(EVENTS.usageUpdated, load);
    return () => {
      un.then((f) => f());
    };
  }, [range]);

  return (
    <div className="settings">
      <section className="card">
        <header className="card-header">
          <h2>사용량</h2>
          <div className="row">
            {(
              [
                ["today", "오늘"],
                ["7d", "최근 7일"],
                ["all", "전체"],
              ] as [Range, string][]
            ).map(([r, label]) => (
              <button key={r} className={range === r ? "primary" : ""} onClick={() => setRange(r)}>
                {label}
              </button>
            ))}
          </div>
        </header>
        {error && <p className="error">{error}</p>}
        {summary && (
          <>
            <div className="row">
              <span className="muted">실행 {summary.runCount}회</span>
              <SourceBadges t={summary.totals} />
            </div>
            <UsageNumbers t={summary.totals} />
            <CostNote authKinds={summary.authKinds} />
          </>
        )}
      </section>

      {summary && (
        <>
          <GroupTable
            title="역할별"
            hint="PM 관리로 생기는 추가 사용량을 작업자 사용량과 분리해 봅니다. (PM은 4단계부터 집계)"
            rows={summary.byRole}
            label={(g) => ROLE_LABEL[g.key as UsageRole] ?? g.label}
          />
          <GroupTable title="에이전트별" rows={summary.byAgent} label={(g) => g.label} />
        </>
      )}

      <RateLimitCard />
    </div>
  );
}

function GroupTable({
  title,
  hint,
  rows,
  label,
}: {
  title: string;
  hint?: string;
  rows: UsageGroup[];
  label: (g: UsageGroup) => string;
}) {
  return (
    <section className="card">
      <header className="card-header">
        <h2>{title}</h2>
      </header>
      {hint && <p className="hint">{hint}</p>}
      {rows.length === 0 ? (
        <p className="muted">기록이 없습니다.</p>
      ) : (
        <table className="table">
          <thead>
            <tr>
              <th>구분</th>
              <th>사용량</th>
              <th>출처</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((g) => (
              <tr key={g.key}>
                <td>{label(g)}</td>
                <td>
                  <UsageNumbers t={g.totals} compact />
                </td>
                <td>
                  <SourceBadges t={g.totals} />
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </section>
  );
}
