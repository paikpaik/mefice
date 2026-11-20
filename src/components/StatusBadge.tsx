import type { RunStatus } from "../api";

const LABEL: Record<RunStatus, string> = {
  queued: "실행 대기",
  running: "작업 중",
  waiting_for_input: "입력 대기",
  waiting_for_approval: "승인 대기",
  cancelling: "중단 중",
  succeeded: "완료",
  failed: "실패",
  cancelled: "중단됨",
  interrupted: "중단 확인",
};

const TONE: Record<RunStatus, string> = {
  queued: "",
  running: "info",
  waiting_for_input: "warn",
  waiting_for_approval: "warn",
  cancelling: "warn",
  succeeded: "ok",
  failed: "danger",
  cancelled: "",
  interrupted: "danger",
};

export function StatusBadge({ status }: { status: RunStatus | null | undefined }) {
  if (!status) return <span className="badge">실행 없음</span>;
  return <span className={`badge ${TONE[status]}`}>{LABEL[status]}</span>;
}
