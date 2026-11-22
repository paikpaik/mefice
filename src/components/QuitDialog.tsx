import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api, errorMessage, EVENTS, type ActiveRunInfo } from "../api";
import { StatusBadge } from "./StatusBadge";

/** 앱 종료 확인 (스펙 10장): 진행 중인 작업과 중단 영향을 보여주고 종료 방식을 고른다. */
export function QuitDialog() {
  const [runs, setRuns] = useState<ActiveRunInfo[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const un = listen<ActiveRunInfo[]>(EVENTS.quitRequested, ({ payload }) => {
      setRuns(payload);
      setError(null);
    });
    return () => {
      un.then((f) => f());
    };
  }, []);

  if (!runs) return null;
  const unisolated = runs.filter((r) => !r.isolated && r.status !== "queued");

  async function act(fn: () => Promise<void>, close: boolean) {
    setBusy(true);
    try {
      await fn();
      if (close) setRuns(null);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="modal-backdrop">
      <div className="modal" role="dialog" aria-labelledby="quit-title">
        <h2 id="quit-title">진행 중인 작업이 {runs.length}개 있습니다</h2>
        <ul className="plain">
          {runs.map((r) => (
            <li key={r.runId} className="row small">
              <StatusBadge status={r.status} />
              <span>{r.taskTitle}</span>
              <span className="muted">{r.agentName}</span>
              <span className="muted">{r.isolated ? "격리 작업 공간" : "프로젝트 폴더"}</span>
            </li>
          ))}
        </ul>
        <div className="small muted">
          <p>종료하면 모든 실행을 중단합니다. 중단된 작업은 다시 실행할 때 처음부터 또는 이어서 실행할 수 있습니다.</p>
          <p>격리 작업 공간의 변경은 보존되어 다음 실행 때 변경 내역에서 확인할 수 있습니다.</p>
          {unisolated.length > 0 && (
            <p className="warn-text">
              프로젝트 폴더에서 직접 실행 중인 작업 {unisolated.length}개는 일부만 바뀐 상태로 남을 수 있습니다.
            </p>
          )}
        </div>
        <div className="row">
          <button className="primary" disabled={busy} onClick={() => act(() => api.confirmQuit(), false)}>
            {busy ? "중단하는 중…" : "작업 중단 후 종료"}
          </button>
          <button disabled={busy} onClick={() => act(() => api.keepRunningInBackground(), true)}>
            창 닫고 백그라운드에서 계속
          </button>
          <button disabled={busy} onClick={() => setRuns(null)}>
            취소
          </button>
        </div>
        {error && <p className="error small">{error}</p>}
      </div>
    </div>
  );
}
