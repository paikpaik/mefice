import { useEffect, useState } from "react";
import { api, errorMessage, type AppSettings, type ProjectView } from "./api";
import { Welcome } from "./screens/Welcome";
import { Settings } from "./screens/Settings";
import { Office } from "./office/Office";
import { Tasks } from "./screens/Tasks";
import { PmChat } from "./screens/PmChat";
import { Team } from "./screens/Team";
import { UsageScreen } from "./screens/UsageScreen";
import "./styles.css";

type Tab = "office" | "pm" | "board" | "team" | "usage" | "settings";

const TABS: { id: Tab; label: string }[] = [
  { id: "office", label: "사무실" },
  { id: "pm", label: "PM 채팅" },
  { id: "board", label: "작업" },
  { id: "team", label: "팀" },
  { id: "usage", label: "사용량" },
  { id: "settings", label: "설정" },
];

export default function App() {
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [settings, setSettings] = useState<AppSettings | null>(null);
  const [project, setProject] = useState<ProjectView | null>(null);
  const [tab, setTab] = useState<Tab>("pm");
  // PM 채팅 등에서 특정 작업으로 이동할 때 사용
  const [focusTask, setFocusTask] = useState<{ id: number; nonce: number } | null>(null);
  const openTask = (id: number) => {
    setFocusTask({ id, nonce: Date.now() });
    setTab("board");
  };

  // 앱 시작 시 저장된 설정과 마지막 프로젝트 복원 (1단계 완료 기준)
  useEffect(() => {
    api
      .getAppState()
      .then((s) => {
        setSettings(s.settings);
        setProject(s.project);
      })
      .catch((e) => setLoadError(errorMessage(e)))
      .finally(() => setLoading(false));
  }, []);

  if (loading) return <div className="center muted">불러오는 중…</div>;
  if (loadError || !settings) return <div className="center error">앱 상태를 불러오지 못했습니다: {loadError}</div>;
  if (!project) return <Welcome onOpened={setProject} />;

  return (
    <div className="shell">
      <nav className="sidebar">
        <div className="brand">
          Mefice <span className="muted">미피스</span>
        </div>
        <div className="project-chip" title={project.path}>
          {project.name}
          {project.git.branch && <span className="muted"> · {project.git.branch}</span>}
        </div>
        {TABS.map((t) => (
          <button key={t.id} className={`nav-item ${tab === t.id ? "active" : ""}`} onClick={() => setTab(t.id)}>
            {t.label}
          </button>
        ))}
      </nav>
      <main className="content">
        {tab === "office" && <Office onOpenTask={openTask} onGoTeam={() => setTab("team")} />}
        {tab === "pm" && <PmChat onOpenTask={openTask} onGoTeam={() => setTab("team")} />}
        {tab === "board" && (
          <Tasks
            key={focusTask?.nonce}
            focusTaskId={focusTask?.id ?? null}
            canIsolate={project.git.isRepo && project.git.head != null}
            onGoTeam={() => setTab("team")}
          />
        )}
        {tab === "team" && <Team />}
        {tab === "usage" && <UsageScreen />}
        {tab === "settings" && (
          <Settings
            project={project}
            settings={settings}
            onProjectChange={setProject}
            onSettingsChange={setSettings}
          />
        )}
      </main>
    </div>
  );
}
