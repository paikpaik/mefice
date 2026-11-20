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
