// Rust 명령(src-tauri/src/commands.rs)과 1:1로 대응하는 타입드 래퍼.
// 프론트엔드는 이 모듈을 통해서만 백엔드를 호출한다.
import { invoke } from "@tauri-apps/api/core";

export interface AppSettings {
  maxConcurrency: number;
  closeToTray: boolean;
}

export interface GitInfo {
  isRepo: boolean;
  root: string | null;
  branch: string | null;
  head: string | null;
  dirtyFiles: number;
}

export interface Project {
  id: number;
  path: string;
  name: string;
  instructions: string;
  createdAt: number;
  updatedAt: number;
  lastOpenedAt: number;
}

export interface ProjectView extends Project {
  exists: boolean;
  git: GitInfo;
}

export interface AppState {
  settings: AppSettings;
  project: ProjectView | null;
}

export interface ToolStatus {
  name: string;
  installed: boolean;
  path: string | null;
  version: string | null;
  required: boolean;
}

export interface EnvironmentReport {
  os: string;
  arch: string;
  tools: ToolStatus[];
}

// ---------- 에이전트 ----------

export type PermissionMode = "default" | "acceptEdits" | "plan";
export const ALL_TOOLS = ["Read", "Glob", "Grep", "Edit", "Write", "Bash"] as const;

export interface Agent {
  id: number;
  name: string;
  role: string;
  instructions: string;
  model: string | null;
  effort: string | null;
  permissionMode: PermissionMode;
  tools: string[];
  color: string;
  /** pm | worker */
  kind: "pm" | "worker";
  createdAt: number;
  updatedAt: number;
}

export interface AgentInput {
  name: string;
  role: string;
  instructions: string;
  model: string | null;
  effort: string | null;
  permissionMode: PermissionMode;
  tools: string[];
  color?: string | null;
}

// ---------- 작업·실행 ----------

export type RunStatus =
  | "queued"
  | "running"
  | "waiting_for_input"
  | "waiting_for_approval"
  | "cancelling"
  | "succeeded"
  | "failed"
  | "cancelled"
  | "interrupted";

export const TERMINAL: RunStatus[] = ["succeeded", "failed", "cancelled", "interrupted"];
export const isTerminal = (s: RunStatus) => TERMINAL.includes(s);

export interface RunConfig {
  agentName: string;
  agentRole: string;
  model: string | null;
  effort: string | null;
  permissionMode: PermissionMode;
  tools: string[];
  systemAppend: string;
}

export interface Run {
  id: number;
  taskId: number;
  agentId: number;
  attempt: number;
  /** 사용량 구분 */
  role: UsageRole;
  /** subscription | api_key — 런타임 시작 후 채워짐 */
  authKind: string | null;
  status: RunStatus;
  prompt: string;
  resumeSessionId: string | null;
  sessionId: string | null;
  config: RunConfig;
  cwd: string;
  exitCode: number | null;
  error: string | null;
  createdAt: number;
  startedAt: number | null;
  endedAt: number | null;
}

export interface Task {
  id: number;
  projectId: number;
  agentId: number;
  parentId: number | null;
  /** direct | pm | delegated */
  mode: "direct" | "pm" | "delegated";
  spec: DelegateSpec | null;
  /** worktree: 작업별 Git worktree에서 실행 / none: 프로젝트 폴더 */
  isolation: "worktree" | "none";
  /** 같은 PM 결정에서 나온 작업 묶음 */
  planRunId: number | null;
  /** 사용자 통합 상태 (모델 실행 완료와 별개) */
  integration: "none" | "merged" | "discarded";
  title: string;
  prompt: string;
  createdAt: number;
}

// ---------- PM (4단계) ----------

export interface DelegateSpec {
  key: string;
  agentId: number;
  paths: string[];
  dependsOn: string[];
  title: string;
  goal: string;
  scope: string;
  inputs: string;
  doneCriteria: string;
}

export interface HireSuggestion {
  role: string;
  reason: string;
  tools: string[];
}

export type PmMode = "direct" | "delegate" | "ask" | "hire" | "report";

export interface PmDecision {
  mode: PmMode;
  reply: string;
  tasks: DelegateSpec[];
  hire: HireSuggestion | null;
}

export interface Message {
  id: number;
  projectId: number;
  goalTaskId: number;
  author: "user" | "pm" | "system";
  text: string;
  /** PM 메시지면 PmDecision, 시스템 메시지면 { taskId, ... } */
  payload: Record<string, unknown> | null;
  runId: number | null;
  createdAt: number;
}

export interface GoalSummary extends TaskSummary {
  lastMessage: Message | null;
  activeChildren: number;
}

export interface GoalDetail extends TaskSummary {
  messages: Message[];
  children: TaskSummary[];
  usageByRole: UsageGroup[];
}

export interface TaskSummary extends Task {
  agentName: string;
  latestRun: Run | null;
  /** 선행 작업 id */
  dependsOn: number[];
}

export interface TaskDetail extends TaskSummary {
  runs: Run[];
}

export interface Usage {
  inputTokens: number | null;
  outputTokens: number | null;
  cacheReadTokens: number | null;
  cacheCreationTokens: number | null;
  costUsd: number | null;
}

export interface SessionTotals {
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens: number;
  cacheCreationTokens: number;
  costUsd: number | null;
  models: string[];
}

// ---------- 작업 공간 · 통합 (5단계) ----------

export interface FileChange {
  path: string;
  /** A 추가 / M 수정 / D 삭제 */
  status: string;
  additions: number | null;
  deletions: number | null;
}

export interface Workspace {
  taskId: number;
  path: string;
  branch: string;
  baseCommit: string;
  baseRef: string | null;
  headCommit: string | null;
  mergedCommit: string | null;
  createdAt: number;
  removedAt: number | null;
}

export interface TaskDiff {
  available: boolean;
  reason: string | null;
  integration: Task["integration"];
  workspace: Workspace | null;
  files: FileChange[];
  patch: string;
  truncated: boolean;
  /** 사용자 현재 HEAD와 병합 시 충돌할 파일 */
  conflicts: string[];
  /** 사용자 미커밋 변경과 겹치는 파일 */
  dirtyOverlap: string[];
  canMerge: boolean;
}

// ---------- 사용량 (3단계) ----------

export type UsageRole = "pm" | "worker" | "review";

/** 출처별 기록 수를 함께 담는다. costUsd가 null이면 "미제공"(0원이 아님). */
export interface UsageTotals {
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens: number;
  cacheCreationTokens: number;
  costUsd: number | null;
  reported: number;
  partial: number;
  unavailable: number;
}

export interface RunUsage {
  runId: number;
  attempt: number;
  role: UsageRole;
  totals: UsageTotals;
}

export interface UsageGroup {
  key: string;
  label: string;
  totals: UsageTotals;
}

export interface UsageSummary {
  totals: UsageTotals;
  byRole: UsageGroup[];
  byAgent: UsageGroup[];
  runCount: number;
  authKinds: string[];
}

export interface RateLimitWindow {
  name: string;
  utilization: number;
  resetsAt: number | null;
}

export interface RateLimitSnapshot {
  status: string | null;
  windows: RateLimitWindow[];
  observedAt: number;
}

export type RunEventBody =
  | { kind: "started"; sessionId: string; model: string | null; auth?: string | null }
  | { kind: "user_message"; text: string }
  | { kind: "text"; text: string }
  | { kind: "tool_use"; toolUseId: string; name: string; summary: string }
  | { kind: "tool_result"; toolUseId: string; isError: boolean; preview: string; truncated: boolean }
  | { kind: "approval_requested"; approvalId: number; toolName: string; summary: string }
  | { kind: "approval_decided"; approvalId: number; allowed: boolean }
  | { kind: "permission_denied"; toolName: string; message: string }
  | {
      kind: "turn_result";
      isError: boolean;
      subtype: string;
      result: string | null;
      usage: Usage;
      durationMs: number | null;
      numTurns: number | null;
      sessionTotals?: SessionTotals | null;
    }
  | { kind: "stderr"; text: string }
  | { kind: "notice"; text: string };

export type StoredEvent = RunEventBody & {
  id: number;
  runId: number;
  seq: number;
  createdAt: number;
};

export interface Approval {
  id: number;
  runId: number;
  requestId: string;
  toolName: string;
  toolInput: unknown;
  description: string | null;
  status: "pending" | "allowed" | "denied" | "expired";
  createdAt: number;
  decidedAt: number | null;
}

/** Rust 이벤트 이름 (src-tauri/src/executor.rs) */
export const EVENTS = {
  runEvent: "run-event",
  runUpdated: "run-updated",
  usageUpdated: "usage-updated",
  rateLimitUpdated: "rate-limit-updated",
  pmMessage: "pm-message",
} as const;

export const api = {
  getAppState: () => invoke<AppState>("get_app_state"),
  openProject: (path: string) => invoke<ProjectView>("open_project", { path }),
  refreshProject: (id: number) => invoke<ProjectView>("refresh_project", { id }),
  updateProjectInstructions: (id: number, instructions: string) =>
    invoke<Project>("update_project_instructions", { id, instructions }),
  updateSettings: (settings: AppSettings) => invoke<AppSettings>("update_settings", { settings }),
  checkEnvironment: () => invoke<EnvironmentReport>("check_environment"),

  listAgents: () => invoke<Agent[]>("list_agents"),
  createAgent: (input: AgentInput) => invoke<Agent>("create_agent", { input }),
  updateAgent: (id: number, input: AgentInput) => invoke<Agent>("update_agent", { id, input }),
  archiveAgent: (id: number) => invoke<void>("archive_agent", { id }),

  startDirectTask: (agentId: number, prompt: string, isolated: boolean) =>
    invoke<TaskSummary>("start_direct_task", { agentId, prompt, isolated }),
  rerunTask: (taskId: number) => invoke<Run>("rerun_task", { taskId }),
  continueTask: (taskId: number, prompt: string) => invoke<Run>("continue_task", { taskId, prompt }),
  listTasks: () => invoke<TaskSummary[]>("list_tasks"),
  getTaskDetail: (taskId: number) => invoke<TaskDetail>("get_task_detail", { taskId }),
  getRunEvents: (runId: number) => invoke<StoredEvent[]>("get_run_events", { runId }),
  getPendingApprovals: (runId: number) => invoke<Approval[]>("get_pending_approvals", { runId }),
  sendRunMessage: (runId: number, text: string) => invoke<void>("send_run_message", { runId, text }),
  cancelRun: (runId: number) => invoke<void>("cancel_run", { runId }),
  respondApproval: (approvalId: number, allow: boolean) =>
    invoke<void>("respond_approval", { approvalId, allow }),

  getUsageSummary: (sinceMs: number | null) => invoke<UsageSummary>("get_usage_summary", { sinceMs }),
  getTaskUsage: (taskId: number) => invoke<RunUsage[]>("get_task_usage", { taskId }),
  getRateLimit: () => invoke<RateLimitSnapshot | null>("get_rate_limit"),

  listGoals: () => invoke<GoalSummary[]>("list_goals"),
  getGoal: (goalId: number) => invoke<GoalDetail>("get_goal", { goalId }),
  /** goalId가 null이면 새 목표. 반환값은 목표 id */
  pmSend: (goalId: number | null, text: string) => invoke<number>("pm_send", { goalId, text }),

  getTaskDiff: (taskId: number) => invoke<TaskDiff>("get_task_diff", { taskId }),
  /** 사용자 현재 브랜치에 병합. 반환값은 병합 커밋 */
  mergeTask: (taskId: number) => invoke<string>("merge_task", { taskId }),
  discardTaskWorkspace: (taskId: number) => invoke<void>("discard_task_workspace", { taskId }),
};

/** Rust AppError는 문자열로 직렬화되어 온다. */
export function errorMessage(e: unknown): string {
  return typeof e === "string" ? e : e instanceof Error ? e.message : String(e);
}
