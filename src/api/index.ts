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

export const api = {
  getAppState: () => invoke<AppState>("get_app_state"),
  openProject: (path: string) => invoke<ProjectView>("open_project", { path }),
  refreshProject: (id: number) => invoke<ProjectView>("refresh_project", { id }),
  updateProjectInstructions: (id: number, instructions: string) =>
    invoke<Project>("update_project_instructions", { id, instructions }),
  updateSettings: (settings: AppSettings) => invoke<AppSettings>("update_settings", { settings }),
  checkEnvironment: () => invoke<EnvironmentReport>("check_environment"),
};

/** Rust AppError는 문자열로 직렬화되어 온다. */
export function errorMessage(e: unknown): string {
  return typeof e === "string" ? e : e instanceof Error ? e.message : String(e);
}
