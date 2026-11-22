import { useEffect, useState } from "react";
import { api, errorMessage, type AgentOverview, type FileChange, type OfficeAgent } from "../api";
import { StatusBadge } from "../components/StatusBadge";
import { IntegrationBadge } from "../components/DiffPanel";
import { SourceBadges, UsageNumbers } from "../components/Usage";

interface Props {
  office: OfficeAgent;
  modeLabel: string;
  showDismiss: boolean;
  onDismiss: () => void;
  onClose: () => void;
  onOpenTask: (taskId: number) => void;
}

/** 캐릭터 상세: 담당 작업, 활동 기록, 변경 파일, 사용량 (스펙 4.5) */
export function AgentPanel({ office, modeLabel, showDismiss, onDismiss, onClose, onOpenTask }: Props) {
  const { agent } = office;
  const [overview, setOverview] = useState<AgentOverview | null>(null);
  const [files, setFiles] = useState<FileChange[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  // 상태가 바뀌면(작업 시작·종료) 다시 읽는다
  const stateKey = `${office.state}:${office.active.map((r) => r.runId).join(",")}:${office.last?.runId ?? ""}`;
  useEffect(() => {
    api
      .getAgentOverview(agent.id)
      .then(setOverview)
      .catch((e) => setError(errorMessage(e)));
    const focusTask = office.active[0]?.taskId ?? office.last?.taskId;
    setFiles(null);
    if (focusTask) {
      api
        .getTaskDiff(focusTask)
        .then((d) => setFiles(d.available ? d.files : null))
        .catch(() => setFiles(null));
    }
  }, [agent.id, stateKey]);

  const p = agent.profile;
  return (
    <aside className="agent-panel">
      <header className="card-header">
        <div>
          <h2>
            <span className="dot" style={{ background: agent.color }} /> {agent.name}
          </h2>
          <div className="small muted">
            {agent.role}
            {p.specialties.length > 0 && ` · ${p.specialties.join(", ")}`}
          </div>
        </div>
        <button onClick={onClose} aria-label="닫기">
          ✕
        </button>
      </header>

      <section>
        <div className="row small">
          <span className="badge info">{modeLabel}</span>
          {showDismiss && (
            <button className="link" onClick={onDismiss}>
              오류 확인함
            </button>
          )}
        </div>
        {office.active.length > 0 ? (
          <ul className="plain">
            {office.active.map((r) => (
              <li key={r.runId} className="row small">
                <StatusBadge status={r.status} />
                <button className="link" onClick={() => onOpenTask(r.taskId)}>
                  #{r.taskId} {r.taskTitle}
                </button>
              </li>
            ))}
          </ul>
        ) : (
          office.last && (
            <div className="row small muted">
              마지막: <StatusBadge status={office.last.status} />
              <button className="link" onClick={() => onOpenTask(office.last!.taskId)}>
                #{office.last.taskId} {office.last.taskTitle}
              </button>
            </div>
          )
        )}
      </section>

      {files && files.length > 0 && (
        <section>
          <h3>변경 파일</h3>
          <ul className="plain mono small">
            {files.slice(0, 12).map((f) => (
              <li key={f.path}>
                <span className={`fs fs-${f.status}`}>{f.status}</span> {f.path}
              </li>
            ))}
            {files.length > 12 && <li className="muted">외 {files.length - 12}개</li>}
          </ul>
        </section>
      )}

      {overview && (
        <>
          <section>
            <h3>사용량 (전체)</h3>
            <UsageNumbers t={overview.usage} compact />
            <SourceBadges t={overview.usage} />
          </section>
          <section>
            <h3>최근 작업</h3>
            {overview.recent.length === 0 ? (
              <p className="muted small">아직 맡은 작업이 없습니다.</p>
            ) : (
              <ul className="plain">
                {overview.recent.map((t) => (
                  <li key={t.id} className="row small">
                    <StatusBadge status={t.latestRun?.status} />
                    <IntegrationBadge task={t} />
                    <button className="link" onClick={() => onOpenTask(t.id)}>
                      {t.title}
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </section>
        </>
      )}

      <section className="small muted">
        <div>도구: {agent.tools.join(", ")}</div>
        {p.scopePaths.length > 0 && <div>작업 범위: {p.scopePaths.join(", ")}</div>}
        {(p.limits.maxMinutes || p.limits.maxBudgetUsd || p.limits.maxTurns) && (
          <div>
            제한: {[
              p.limits.maxMinutes && `${p.limits.maxMinutes}분`,
              p.limits.maxBudgetUsd && `$${p.limits.maxBudgetUsd}`,
              p.limits.maxTurns && `${p.limits.maxTurns}턴`,
            ]
              .filter(Boolean)
              .join(" · ")}
          </div>
        )}
      </section>
      {error && <p className="error small">{error}</p>}
    </aside>
  );
}
