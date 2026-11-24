//! 처음 실행 안내: AI 실행 도구(Claude Code CLI) 설치·로그인 (스펙 v0.2, A단계 8번).
//!
//! - 로그인 정보는 저장하지 않는다. `claude auth status`에서 로그인 여부와 방식만 읽는다
//!   (출력에 들어 있는 이메일·조직 정보는 읽지도 저장하지도 않는다).
//! - 설치는 공식 설치 스크립트를 그대로 실행한다.
//! - 로그인은 `claude auth login`을 띄우고, 브라우저 주소와 코드 입력을 화면으로 중계한다.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};
use crate::sysproc::{child_path, resolve_binary, run_capture};

pub const INSTALL_COMMAND: &str = "curl -fsSL https://claude.ai/install.sh | bash";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupStatus {
    pub installed: bool,
    pub version: Option<String>,
    /// 설치돼 있지 않거나 확인 실패면 None
    pub logged_in: Option<bool>,
    /// claude.ai(구독) | console(API) 등
    pub auth_method: Option<String>,
}

/// `claude auth status` 출력 중 필요한 필드만 (나머지는 역직렬화하지 않고 버린다)
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthStatus {
    logged_in: bool,
    #[serde(default)]
    auth_method: Option<String>,
}

fn parse_auth(json: &str) -> Option<(bool, Option<String>)> {
    serde_json::from_str::<AuthStatus>(json)
        .ok()
        .map(|a| (a.logged_in, a.auth_method))
}

pub fn status() -> SetupStatus {
    let Some(bin) = resolve_binary("claude") else {
        return SetupStatus {
            installed: false,
            version: None,
            logged_in: None,
            auth_method: None,
        };
    };
    let version =
        run_capture(&bin, &["--version"], None).and_then(|v| v.lines().next().map(str::to_string));
    // 로그인 안 됨이면 종료 코드가 0이 아닐 수 있어 stdout을 직접 읽는다
    let auth = Command::new(&bin)
        .args(["auth", "status"])
        .env("PATH", child_path())
        .stdin(Stdio::null())
        .output()
        .ok()
        .and_then(|o| parse_auth(String::from_utf8_lossy(&o.stdout).trim()));
    if auth.as_ref().is_some_and(|a| a.0) {
        cancel_login(); // 로그인 끝났으면 남은 로그인 프로세스 정리
    }
    SetupStatus {
        installed: true,
        version,
        logged_in: auth.as_ref().map(|a| a.0),
        auth_method: auth.and_then(|a| a.1),
    }
}

fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// 공식 설치 스크립트 실행 (macOS·Linux). 결과 출력의 마지막 몇 줄을 돌려준다.
pub fn install() -> AppResult<String> {
    if !cfg!(unix) {
        return Err(AppError::Internal(
            "이 운영체제에서는 자동 설치를 지원하지 않습니다. 공식 안내 페이지를 참고해 설치해 주세요.".into(),
        ));
    }
    let out = Command::new("/bin/bash")
        .args(["-c", INSTALL_COMMAND])
        .env("PATH", child_path())
        .stdin(Stdio::null())
        .output()
        .map_err(|e| AppError::Internal(format!("설치를 시작하지 못했습니다: {e}")))?;
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if out.status.success() {
        Ok(tail(text.trim(), 6))
    } else {
        Err(AppError::Internal(format!(
            "설치에 실패했습니다:\n{}",
            tail(text.trim(), 10)
        )))
    }
}

struct Login {
    child: Child,
    output: Arc<Mutex<String>>,
}

static LOGIN: Mutex<Option<Login>> = Mutex::new(None);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginProgress {
    /// 브라우저가 열리지 않을 때 직접 열 주소
    pub url: Option<String>,
    /// 로그인 프로세스가 아직 진행 중인지 (코드 입력을 기다리는 중일 수 있음)
    pub running: bool,
    /// 끝났다면 마지막 출력
    pub message: Option<String>,
}

fn find_url(text: &str) -> Option<String> {
    text.split_whitespace()
        .find(|w| w.starts_with("https://"))
        .map(|w| w.trim_end_matches(['.', ',', ')', '"']).to_string())
}

fn progress(login: &mut Login) -> LoginProgress {
    let out = login.output.lock().map(|o| o.clone()).unwrap_or_default();
    let exited = login.child.try_wait().ok().flatten();
    LoginProgress {
        url: find_url(&out),
        running: exited.is_none(),
        message: exited.map(|_| tail(out.trim(), 4)),
    }
}

/// `claude auth login` 시작. 주소가 나오거나 끝날 때까지 잠깐 기다린다.
pub fn start_login() -> AppResult<LoginProgress> {
    cancel_login();
    let bin = resolve_binary("claude")
        .ok_or_else(|| AppError::Internal("먼저 AI 실행 도구를 설치해 주세요.".into()))?;
    let mut child = Command::new(bin)
        .args(["auth", "login"])
        .env("PATH", child_path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| AppError::Internal(format!("로그인을 시작하지 못했습니다: {e}")))?;
    let output = Arc::new(Mutex::new(String::new()));
    for stream in [
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
    ]
    .into_iter()
    .flatten()
    {
        let buf = output.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stream).lines().map_while(Result::ok) {
                if let Ok(mut b) = buf.lock() {
                    b.push_str(&line);
                    b.push('\n');
                }
            }
        });
    }
    let mut login = Login { child, output };
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut p = progress(&mut login);
    while p.running && p.url.is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
        p = progress(&mut login);
    }
    if p.running {
        *LOGIN
            .lock()
            .map_err(|_| AppError::Internal("lock".into()))? = Some(login);
    }
    Ok(p)
}

/// 브라우저에서 받은 코드를 로그인 프로세스에 전달
pub fn submit_login_code(code: &str) -> AppResult<LoginProgress> {
    let mut guard = LOGIN
        .lock()
        .map_err(|_| AppError::Internal("lock".into()))?;
    let login = guard.as_mut().ok_or_else(|| {
        AppError::Internal("진행 중인 로그인이 없습니다. 다시 시작해 주세요.".into())
    })?;
    let stdin = login
        .child
        .stdin
        .as_mut()
        .ok_or_else(|| AppError::Internal("코드를 전달할 수 없습니다".into()))?;
    writeln!(stdin, "{}", code.trim()).map_err(|e| AppError::Internal(e.to_string()))?;
    stdin.flush().ok();
    drop(guard);
    std::thread::sleep(Duration::from_secs(3));
    let mut guard = LOGIN
        .lock()
        .map_err(|_| AppError::Internal("lock".into()))?;
    Ok(guard.as_mut().map(progress).unwrap_or(LoginProgress {
        url: None,
        running: false,
        message: None,
    }))
}

pub fn cancel_login() {
    if let Ok(mut g) = LOGIN.lock()
        && let Some(mut l) = g.take()
    {
        let _ = l.child.kill();
        let _ = l.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 로그인_상태는_필요한_필드만_읽는다() {
        let json = r#"{"loggedIn":true,"authMethod":"claude.ai","email":"x@y.z","orgId":"o","orgName":"n"}"#;
        assert_eq!(parse_auth(json), Some((true, Some("claude.ai".into()))));
        assert_eq!(parse_auth(r#"{"loggedIn":false}"#), Some((false, None)));
        assert_eq!(parse_auth("Not logged in"), None);
    }

    #[test]
    fn 주소_찾기() {
        assert_eq!(
            find_url(
                "Opening browser...\nIf it does not open, visit: https://claude.ai/oauth?x=1.\n"
            ),
            Some("https://claude.ai/oauth?x=1".into())
        );
        assert_eq!(find_url("no url"), None);
    }

    /// 실제 설치된 CLI로 상태 확인 (로그인돼 있는 개발 환경 전제)
    #[test]
    #[ignore]
    fn live_설치_로그인_상태() {
        let st = status();
        assert!(st.installed, "{st:?}");
        assert_eq!(st.logged_in, Some(true), "{st:?}");
        assert!(st.auth_method.is_some());
    }
}
