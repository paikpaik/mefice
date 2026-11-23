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

/** 실행 제한 (스펙 4.3) */
export interface Limits {
  /** 실행 시간(분) */
  maxMinutes: number | null;
  /** 사용량 상한 (API 정가 환산 USD) */
  maxBudgetUsd: number | null;
  /** 에이전트 턴 수 상한 */
  maxTurns: number | null;
}

/** 고용 프로필: 외형·자리·전문 분야·수행 능력·작업 범위·제한 */
export interface AgentProfile {
  templateId: string | null;
  avatar: string;
  /** 0부터 시작하는 책상 번호. null이면 자동 배정 */
  desk: number | null;
  specialties: string[];
  capabilities: string[];
  scopePaths: string[];
  limits: Limits;
}

export const emptyProfile = (): AgentProfile => ({
  templateId: null,
  avatar: "a",
  desk: null,
  specialties: [],
  capabilities: [],
  scopePaths: [],
  limits: { maxMinutes: null, maxBudgetUsd: null, maxTurns: null },
});

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
  kind: "pm" | "worker" | "assistant";
  profile: AgentProfile;
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
  profile: AgentProfile;
}

// ---------- 고용 · 사무실 (6단계) ----------

export interface AgentTemplate {
  id: string;
  role: string;
  summary: string;
  instructions: string;
  tools: string[];
  permissionMode: PermissionMode;
  specialtySuggestions: string[];
  capabilities: string[];
  color: string;
  avatar: string;
}

export interface TemplateCatalog {
  templates: AgentTemplate[];
  capabilities: string[];
  deskCount: number;
}

export type OfficeState = "idle" | "queued" | "working" | "needs_input" | "error" | "done";

export interface RunRef {
  runId: number;
  taskId: number;
  taskTitle: string;
  status: RunStatus;
  endedAt: number | null;
}

export interface OfficeAgent {
  agent: Agent;
  state: OfficeState;
  active: RunRef[];
  last: RunRef | null;
}

// ---------- 대화 (v0.2 A단계 1번) ----------

export interface ChatOptions {
  /** 절약 모드: 도구 제한으로 비용 절감 (능력도 줄어듦) */
  economy: boolean;
  permissionMode: PermissionMode;
  model: string | null;
}

export const defaultChatOptions = (): ChatOptions => ({ economy: false, permissionMode: "default", model: null });

export interface ChatSummary {
  id: number;
  title: string;
  createdAt: number;
  status: RunStatus | null;
  updatedAt: number;
}

export interface ChatDetail {
  id: number;
  title: string;
  options: ChatOptions;
  run: Run | null;
  events: StoredEvent[];
  pending: Approval[];
}

/** 승인 결정: 이번만 / 이 세션 동안 / 거부 */
export type Decision = "allow" | "allow_session" | "deny";

export interface RunDelta {
  runId: number;
  taskId: number;
  text: string;
}

export interface RunActivity {
  runId: number;
  taskId: number;
  /** requesting | thinking | writing | tool:<이름> | idle */
  activity: string;
}

/** 종료 확인 창에 보여줄 진행 중 실행 */
export interface ActiveRunInfo {
  runId: number;
  taskId: number;
  taskTitle: string;
  agentName: string;
  status: RunStatus;
  /** 격리 작업 공간이면 중단해도 사용자 폴더에 부분 변경이 남지 않는다 */
  isolated: boolean;
}

export interface AgentOverview {
  agent: Agent;
  recent: TaskSummary[];
  usage: UsageTotals;
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

/** 카드 표시용 도구 상세 (일부 도구만, 길이 제한) */
export interface ToolDetail {
  filePath?: string;
  oldString?: string;
  newString?: string;
  content?: string;
  command?: string;
  description?: string;
  todos?: Todo[];
  questions?: AskQuestion[];
  plan?: string;
}

export interface Todo {
  content: string;
  status: "pending" | "in_progress" | "completed";
  activeForm: string;
}

export interface AskQuestion {
  question: string;
  header?: string;
  options: { label: string; description?: string }[];
  multiSelect?: boolean;
}

export type RunEventBody =
  | { kind: "started"; sessionId: string; model: string | null; auth?: string | null }
  | { kind: "user_message"; text: string }
  | { kind: "text"; text: string }
  | { kind: "tool_use"; toolUseId: string; name: string; summary: string; input?: ToolDetail | null }
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
  /** 런타임 권한 제안 (세션 허용에 사용) */
  suggestions: unknown;
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
  quitRequested: "quit-requested",
  runDelta: "run-delta",
  runActivity: "run-activity",
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
  startChat: (text: string, options: ChatOptions) => invoke<number>("start_chat", { text, options }),
  chatSend: (chatId: number, text: string) => invoke<void>("chat_send", { chatId, text }),
  chatInterrupt: (chatId: number) => invoke<void>("chat_interrupt", { chatId }),
  chatSetOptions: (chatId: number, options: ChatOptions) => invoke<void>("chat_set_options", { chatId, options }),
  listChats: () => invoke<ChatSummary[]>("list_chats"),
  getChat: (chatId: number) => invoke<ChatDetail>("get_chat", { chatId }),
  decideApproval: (approvalId: number, decision: Decision, answers?: Record<string, string>, message?: string) =>
    invoke<void>("decide_approval", { approvalId, decision, answers: answers ?? null, message: message ?? null }),

  getActiveRuns: () => invoke<ActiveRunInfo[]>("get_active_runs"),
  confirmQuit: () => invoke<void>("confirm_quit"),
  keepRunningInBackground: () => invoke<void>("keep_running_in_background"),

  listTemplates: () => invoke<TemplateCatalog>("list_templates"),
  getOfficeState: () => invoke<OfficeAgent[]>("get_office_state"),
  getAgentOverview: (agentId: number) => invoke<AgentOverview>("get_agent_overview", { agentId }),

  /** 사용자 현재 브랜치에 병합. 반환값은 병합 커밋 */
  mergeTask: (taskId: number) => invoke<string>("merge_task", { taskId }),
  discardTaskWorkspace: (taskId: number) => invoke<void>("discard_task_workspace", { taskId }),
};

/** Rust AppError는 문자열로 직렬화되어 온다. */
export function errorMessage(e: unknown): string {
  return typeof e === "string" ? e : e instanceof Error ? e.message : String(e);
}
