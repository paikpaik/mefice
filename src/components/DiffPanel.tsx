import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api, errorMessage, EVENTS, type Run, type TaskDiff, type TaskSummary } from "../api";

/** 통합 상태 배지. 모델 실행 완료와 사용자 검수·통합 완료를 구분해 보여준다 (스펙 9장). */
export function IntegrationBadge({ task }: { task: TaskSummary }) {
  if (task.integration === "merged") return <span className="badge ok">병합됨</span>;
  if (task.integration === "discarded") return <span className="badge">폐기됨</span>;
  if (task.isolation === "worktree" && task.latestRun?.status === "succeeded")
    return <span className="badge warn">검토 대기</span>;
  return null;
}

/** 격리 작업 공간의 변경 내역, 병합 사전 확인, 병합·폐기 (사용자 확정) */
export function DiffPanel({ taskId, onChanged }: { taskId: number; onChanged?: () => void }) {
  const [diff, setDiff] = useState<TaskDiff | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const load = () =>
    api
      .getTaskDiff(taskId)
      .then(setDiff)
      .catch((e) => setError(errorMessage(e)));

  useEffect(() => {
    load();
    // 실행이 끝나면 스냅샷이 생기므로 다시 읽는다
    const un = listen<Run>(EVENTS.runUpdated, ({ payload }) => {
      if (payload.taskId === taskId) load();
    });
    return () => {
      un.then((f) => f());
    };
  }, [taskId]);

  async function act(fn: () => Promise<unknown>, done: string) {
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      await fn();
      setNotice(done);
      await load();
      onChanged?.();
    } catch (e) {
      setError(errorMessage(e));
      await load();
    } finally {
      setBusy(false);
    }
  }

  if (!diff) return <p className="muted">{error ?? "변경 내역을 불러오는 중…"}</p>;
  const ws = diff.workspace;

  return (
    <div className="diff-panel">
      {ws && (
        <div className="row small muted wrap">
          <span className="mono">{ws.branch}</span>
          <span>
            기준 {ws.baseRef ?? "HEAD"} @ <span className="mono">{ws.baseCommit.slice(0, 8)}</span>
          </span>
          {ws.mergedCommit && (
            <span>
              병합 커밋 <span className="mono">{ws.mergedCommit.slice(0, 8)}</span>
            </span>
          )}
        </div>
      )}
      {diff.reason && <p className="muted">{diff.reason}</p>}

      {diff.available && diff.files.length > 0 && (
        <>
          <ul className="file-list">
            {diff.files.map((f) => (
              <li key={f.path} className={diff.conflicts.includes(f.path) || diff.dirtyOverlap.includes(f.path) ? "warn-text" : ""}>
                <span className={`fs fs-${f.status}`}>{f.status}</span>
                <span className="mono">{f.path}</span>
                <span className="small">
                  {f.additions != null ? (
                    <>
                      <span className="add">+{f.additions}</span> <span className="del">−{f.deletions}</span>
                    </>
                  ) : (
                    <span className="muted">바이너리</span>
                  )}
                </span>
              </li>
            ))}
          </ul>

          {diff.conflicts.length > 0 && (
            <p className="error small">
              현재 브랜치와 충돌이 예상됩니다: {diff.conflicts.join(", ")}. 자동 충돌 해결은 하지 않습니다. 직접 병합하거나
              작업을 다시 맡기세요.
            </p>
          )}
          {diff.dirtyOverlap.length > 0 && (
            <p className="error small">
              커밋하지 않은 내 변경과 겹칩니다: {diff.dirtyOverlap.join(", ")}. 덮어쓰지 않도록 병합을 막았습니다. 먼저 커밋하거나
              정리하세요.
            </p>
          )}

          <div className="row">
            <button
              className="primary"
              disabled={busy || !diff.canMerge}
              onClick={() => act(() => api.mergeTask(taskId), "현재 브랜치에 병합했습니다.")}
              title="충돌·미커밋 변경 겹침을 다시 확인한 뒤 git merge --no-ff 로 병합합니다"
            >
              현재 브랜치에 병합
            </button>
            <button
              disabled={busy || diff.integration !== "none"}
              onClick={() => {
                if (confirm("이 작업의 변경을 버리고 작업 공간을 정리할까요? 되돌릴 수 없습니다.")) {
                  act(() => api.discardTaskWorkspace(taskId), "변경을 버리고 작업 공간을 정리했습니다.");
                }
              }}
            >
              폐기
            </button>
          </div>

          <details className="patch" open={diff.patch.length < 20000}>
            <summary className="small muted">
              diff 보기{diff.truncated && " (너무 커서 일부만 표시)"}
            </summary>
            <pre>
              {diff.patch.split("\n").map((line, i) => (
                <div
                  key={i}
                  className={
                    line.startsWith("+++") || line.startsWith("---")
                      ? "pl-file"
                      : line.startsWith("+")
                        ? "pl-add"
                        : line.startsWith("-")
                          ? "pl-del"
                          : line.startsWith("@@")
                            ? "pl-hunk"
                            : ""
                  }
                >
                  {line || " "}
                </div>
              ))}
            </pre>
          </details>
        </>
      )}

      {diff.available && diff.files.length === 0 && diff.integration === "none" && ws && !ws.removedAt && (
        <button disabled={busy} onClick={() => act(() => api.discardTaskWorkspace(taskId), "작업 공간을 정리했습니다.")}>
          작업 공간 정리
        </button>
      )}

      {notice && <p className="small ok-text">{notice}</p>}
      {error && <p className="error small">{error}</p>}
    </div>
  );
}
