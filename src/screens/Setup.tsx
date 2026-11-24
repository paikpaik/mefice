import { useState } from "react";
import { api, errorMessage, type LoginProgress, type SetupStatus } from "../api";

interface Props {
  status: SetupStatus;
  onStatus: (s: SetupStatus) => void;
  onSkip: () => void;
}

/** 처음 실행 안내 (A단계 8번): AI 실행 도구 설치 → 내 Claude 계정으로 로그인 */
export function Setup({ status, onStatus, onSkip }: Props) {
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [login, setLogin] = useState<LoginProgress | null>(null);
  const [code, setCode] = useState("");

  async function run<T>(label: string, f: () => Promise<T>): Promise<T | undefined> {
    setBusy(label);
    setError(null);
    try {
      return await f();
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(null);
    }
  }

  const recheck = () => run("확인 중…", async () => onStatus(await api.getSetupStatus()));

  async function install() {
    const out = await run("설치 중… (1~2분 걸릴 수 있어요)", api.installClaude);
    if (out !== undefined) {
      setNote(out);
      await recheck();
    }
  }

  async function startLogin() {
    const p = await run("로그인 창을 여는 중…", api.startLogin);
    if (p) {
      setLogin(p);
      if (!p.running) await recheck();
    }
  }

  async function sendCode() {
    const p = await run("확인 중…", () => api.submitLoginCode(code));
    if (p) {
      setLogin(p);
      setCode("");
      await recheck();
    }
  }

  const step = !status.installed ? 1 : status.loggedIn ? 3 : 2;

  return (
    <main className="welcome setup">
      <h1>
        Mefice <span className="muted">미피스</span>
      </h1>
      <p className="muted">시작하기 전에 두 가지만 준비할게요. 로그인 정보는 Mefice에 저장하지 않습니다.</p>

      <ol className="setup-steps">
        <li className={step === 1 ? "current" : "done"}>
          <strong>1. AI 실행 도구 설치</strong>
          {status.installed ? (
            <span className="badge ok">설치됨 {status.version ?? ""}</span>
          ) : (
            <>
              <p className="small muted">Anthropic 공식 설치 프로그램(Claude Code)을 내려받아 설치합니다.</p>
              <button className="primary" disabled={!!busy} onClick={install}>
                설치하기
              </button>
            </>
          )}
        </li>
        <li className={step === 2 ? "current" : step === 3 ? "done" : ""}>
          <strong>2. 내 Claude 계정으로 로그인</strong>
          {status.loggedIn ? (
            <span className="badge ok">로그인됨</span>
          ) : (
            step === 2 && (
              <>
                <p className="small muted">Claude 구독(Pro·Max 등) 계정으로 로그인합니다. 브라우저가 열립니다.</p>
                <button className="primary" disabled={!!busy} onClick={startLogin}>
                  {login?.running ? "로그인 다시 시작" : "로그인하기"}
                </button>
                {login?.url && (
                  <p className="small">
                    브라우저가 열리지 않으면 이 주소를 직접 여세요:
                    <br />
                    <a className="mono setup-url" href={login.url} target="_blank" rel="noreferrer">
                      {login.url}
                    </a>
                  </p>
                )}
                {login?.running && (
                  <div className="row">
                    <input
                      className="grow"
                      placeholder="브라우저에 코드가 보이면 여기에 붙여 넣기"
                      value={code}
                      onChange={(e) => setCode(e.target.value)}
                    />
                    <button disabled={!!busy || !code.trim()} onClick={sendCode}>
                      코드 보내기
                    </button>
                  </div>
                )}
                {login?.message && <pre className="mono small">{login.message}</pre>}
              </>
            )
          )}
        </li>
      </ol>

      <div className="row">
        <button disabled={!!busy} onClick={recheck}>
          다시 확인
        </button>
        {step === 3 ? (
          <button className="primary" onClick={onSkip}>
            시작하기
          </button>
        ) : (
          <button className="link" onClick={onSkip}>
            나중에 하기
          </button>
        )}
      </div>
      {busy && <p className="muted">{busy}</p>}
      {note && <pre className="mono small">{note}</pre>}
      {error && <p className="error">{error}</p>}
    </main>
  );
}
