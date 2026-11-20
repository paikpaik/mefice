//! 외부 프로그램 탐색·실행 헬퍼.
//!
//! macOS에서 Finder/Dock으로 실행한 GUI 앱은 셸 프로필의 PATH를 상속받지 않는다.
//! 그래서 `claude`, `git` 같은 CLI를 찾을 때 (1) 현재 PATH, (2) 로그인 셸의 `command -v`,
//! (3) 흔한 설치 경로 순으로 탐색한다.

use std::path::{Path, PathBuf};
use std::process::Command;

const COMMON_DIRS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"];

/// 실행 파일의 절대 경로를 찾는다. 찾지 못하면 None.
pub fn resolve_binary(name: &str) -> Option<PathBuf> {
    // 1) 현재 프로세스 PATH
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let p = dir.join(name);
            if is_executable(&p) {
                return Some(p);
            }
        }
    }

    // 2) 로그인 셸에게 물어보기 (사용자 프로필의 PATH 반영)
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    if let Ok(out) = Command::new(shell)
        .args(["-lc", &format!("command -v {name}")])
        .output()
        && out.status.success()
    {
        let s = String::from_utf8_lossy(&out.stdout);
        if let Some(line) = s.lines().map(str::trim).find(|l| l.starts_with('/')) {
            let p = PathBuf::from(line);
            if is_executable(&p) {
                return Some(p);
            }
        }
    }

    // 3) 흔한 설치 경로 + 사용자 로컬 경로
    let mut dirs: Vec<PathBuf> = COMMON_DIRS.iter().map(PathBuf::from).collect();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".claude/local"));
    }
    dirs.into_iter()
        .map(|d| d.join(name))
        .find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

/// 자식 프로세스에 넘겨줄 PATH. 로그인 셸의 PATH와 현재 PATH를 합친다 (한 번만 계산).
/// GUI 앱에서 실행한 런타임이 훅·도구 실행 시 node 등을 찾지 못하는 문제를 막는다.
pub fn child_path() -> &'static str {
    static PATH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PATH.get_or_init(|| {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
        let login = Command::new(shell)
            .args(["-lc", "printf %s \"$PATH\""])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        let current = std::env::var("PATH").unwrap_or_default();
        let mut seen = std::collections::HashSet::new();
        login
            .split(':')
            .chain(current.split(':'))
            .chain(COMMON_DIRS.iter().copied())
            .filter(|d| !d.is_empty() && seen.insert(d.to_string()))
            .collect::<Vec<_>>()
            .join(":")
    })
}

/// 명령을 실행하고 성공 시 stdout(trim)을 돌려준다. 실패하면 None.
pub fn run_capture(bin: &Path, args: &[&str], cwd: Option<&Path>) -> Option<String> {
    let mut cmd = Command::new(bin);
    cmd.args(args);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let out = cmd.output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}
