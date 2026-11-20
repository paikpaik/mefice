import { useEffect, useState } from "react";
import { api, errorMessage, type AppSettings, type ProjectView } from "./api";
import { Welcome } from "./screens/Welcome";
import { Settings } from "./screens/Settings";
import { Placeholder } from "./screens/Placeholder";
import { Tasks } from "./screens/Tasks";
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
  const [tab, setTab] = useState<Tab>("office");

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
        {tab === "office" && (
          <Placeholder title="사무실" stage={6} description="고용한 에이전트가 상태에 따라 일하거나 쉬는 2D 사무실" />
        )}
        {tab === "pm" && (
          <Placeholder title="PM 채팅" stage={4} description="작업 목표를 입력하면 PM이 직접 수행·단일 위임·병렬 분할을 결정" />
        )}
        {tab === "board" && <Tasks onGoTeam={() => setTab("team")} />}
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
