import { useEffect, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import {
  api,
  errorMessage,
  type AppSettings,
  type EnvironmentReport,
  type ProjectView,
  type SetupStatus,
} from "../api";

interface Props {
  project: ProjectView;
  settings: AppSettings;
  onProjectChange: (p: ProjectView) => void;
  onSettingsChange: (s: AppSettings) => void;
}

export function Settings({ project, settings, onProjectChange, onSettingsChange }: Props) {
  return (
    <div className="settings">
      <ProjectSection project={project} onChange={onProjectChange} />
      <ExecutionSection settings={settings} onChange={onSettingsChange} />
      <EnvironmentSection />
    </div>
  );
}

function ProjectSection({ project, onChange }: { project: ProjectView; onChange: (p: ProjectView) => void }) {
  const [instructions, setInstructions] = useState(project.instructions);
  const [status, setStatus] = useState<string | null>(null);

  // 다른 프로젝트로 바뀌면 편집 중이던 값을 새 프로젝트 값으로 교체
  useEffect(() => setInstructions(project.instructions), [project.id, project.instructions]);

  const dirty = instructions !== project.instructions;

  async function save() {
    try {
      const saved = await api.updateProjectInstructions(project.id, instructions);
      onChange({ ...project, ...saved });
      setStatus("저장됨");
    } catch (e) {
      setStatus(errorMessage(e));
    }
  }

  async function switchProject() {
    const selected = await open({ directory: true, multiple: false, title: "프로젝트 폴더 선택" });
    if (typeof selected !== "string") return;
    try {
      onChange(await api.openProject(selected));
      setStatus(null);
    } catch (e) {
      setStatus(errorMessage(e));
    }
  }

  async function refresh() {
    try {
      onChange(await api.refreshProject(project.id));
    } catch (e) {
      setStatus(errorMessage(e));
    }
  }

  const { git } = project;
  return (
    <section className="card">
      <header className="card-header">
        <h2>프로젝트</h2>
        <div className="row">
          <button onClick={refresh}>새로고침</button>
          <button onClick={switchProject}>다른 폴더 열기</button>
        </div>
      </header>

      <dl className="kv">
        <dt>이름</dt>
        <dd>{project.name}</dd>
        <dt>경로</dt>
        <dd className="mono">{project.path}</dd>
        <dt>Git</dt>
        <dd>
          {!project.exists ? (
            <span className="badge danger">폴더 없음</span>
          ) : git.isRepo ? (
            <>
              <span className="badge ok">저장소</span> {git.branch ?? "(detached)"}
              {git.head && <span className="mono muted"> @ {git.head}</span>}
              {git.dirtyFiles > 0 && (
                <span className="badge warn" title="Mefice는 이 변경 사항을 덮어쓰지 않습니다">
                  미커밋 변경 {git.dirtyFiles}개
                </span>
              )}
            </>
          ) : (
            <span className="badge warn">Git 저장소 아님 — worktree 격리 실행 불가</span>
          )}
        </dd>
      </dl>

      <label className="field">
        <span>프로젝트 기본 지침</span>
        <small className="muted">모든 에이전트 작업에 함께 전달됩니다. 짧고 구체적으로 적을수록 토큰이 절약됩니다.</small>
        <textarea
          rows={6}
          value={instructions}
          placeholder="예: 패키지 매니저는 pnpm, 테스트는 pnpm test, 커밋 메시지는 한국어"
          onChange={(e) => {
            setInstructions(e.target.value);
            setStatus(null);
          }}
        />
      </label>
      <div className="row">
        <button className="primary" onClick={save} disabled={!dirty}>
          지침 저장
        </button>
        {status && <span className="muted">{status}</span>}
      </div>
    </section>
  );
}

function ExecutionSection({ settings, onChange }: { settings: AppSettings; onChange: (s: AppSettings) => void }) {
  const [error, setError] = useState<string | null>(null);

  async function update(patch: Partial<AppSettings>) {
    setError(null);
    try {
      onChange(await api.updateSettings({ ...settings, ...patch }));
    } catch (e) {
      setError(errorMessage(e));
    }
  }

  return (
    <section className="card">
      <header className="card-header">
        <h2>실행</h2>
      </header>
      <label className="field inline">
        <span>동시 실행 상한</span>
        <select value={settings.maxConcurrency} onChange={(e) => update({ maxConcurrency: Number(e.target.value) })}>
          {[1, 2, 3, 4, 5, 6, 7, 8].map((n) => (
            <option key={n} value={n}>
              {n}
            </option>
          ))}
        </select>
        <small className="muted">PM 호출도 포함됩니다. 변경은 다음 실행부터 적용됩니다.</small>
      </label>
      <label className="field inline">
        <input
          type="checkbox"
          checked={settings.closeToTray}
          onChange={(e) => update({ closeToTray: e.target.checked })}
        />
        <span>창을 닫아도 트레이에 남아 작업 계속</span>
      </label>
      <label className="field inline">
        <input type="checkbox" checked={settings.devMode} onChange={(e) => update({ devMode: e.target.checked })} />
        <span>개발자 모드 (승인 요청의 원래 명령, 팀원의 세부 권한 설정을 보여줍니다)</span>
      </label>
      {error && <p className="error">{error}</p>}
    </section>
  );
}

function EnvironmentSection() {
  const [report, setReport] = useState<EnvironmentReport | null>(null);
  const [setup, setSetup] = useState<SetupStatus | null>(null);
  const [error, setError] = useState<string | null>(null);

  async function load() {
    setError(null);
    try {
      api.getSetupStatus().then(setSetup).catch(() => {});
      setReport(await api.checkEnvironment());
    } catch (e) {
      setError(errorMessage(e));
    }
  }

  useEffect(() => {
    load();
  }, []);

  return (
    <section className="card">
      <header className="card-header">
        <h2>실행 환경</h2>
        <button onClick={load}>다시 검사</button>
      </header>
      {error && <p className="error">{error}</p>}
      {!report ? (
        <p className="muted">검사 중…</p>
      ) : (
        <>
          <p className="muted">
            {report.os} / {report.arch}
          </p>
          {setup && (
            <p>
              Claude 계정:{" "}
              {setup.loggedIn ? (
                <span className="badge ok">로그인됨{setup.authMethod ? ` (${setup.authMethod})` : ""}</span>
              ) : (
                <span className="badge warn">로그인 필요 — 앱을 다시 열면 로그인 안내가 나옵니다</span>
              )}
            </p>
          )}
          <table className="table">
            <thead>
              <tr>
                <th>도구</th>
                <th>상태</th>
                <th>버전</th>
                <th>경로</th>
              </tr>
            </thead>
            <tbody>
              {report.tools.map((t) => (
                <tr key={t.name}>
                  <td className="mono">{t.name}</td>
                  <td>
                    {t.installed ? (
                      <span className="badge ok">설치됨</span>
                    ) : (
                      <span className={`badge ${t.required ? "danger" : "warn"}`}>미설치</span>
                    )}
                  </td>
                  <td className="mono">{t.version ?? "-"}</td>
                  <td className="mono muted">{t.path ?? "-"}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </>
      )}
    </section>
  );
}
