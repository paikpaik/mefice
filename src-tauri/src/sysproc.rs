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

/// 실행 중인 프로세스의 명령줄. 없으면 None. (pid 재사용 여부 확인용)
pub fn process_command(pid: u32) -> Option<String> {
    let out = Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !s.is_empty()).then_some(s)
}

/// 프로세스 그룹 전체에 신호를 보낸다 (런타임이 띄운 도구 명령까지 함께 종료).
/// 런타임은 자기 pid를 그룹 id로 하는 새 그룹으로 실행되므로 pgid = pid.
#[cfg(unix)]
pub fn signal_group(pgid: u32, sig: i32) -> bool {
    if pgid <= 1 {
        return false;
    }
    // SAFETY: 음수 pid로 그룹에 신호를 보낸다. pgid 0·1은 위에서 막아 자기 그룹·init에 보내지 않는다.
    unsafe { libc::kill(-(pgid as i32), sig) == 0 }
}

/// 프로세스 트리(루트 포함)의 (pid, pgid). 부모가 죽으면 자식이 launchd로 넘어가 추적할 수 없으므로
/// 종료 전에 먼저 수집해야 한다.
pub fn process_tree(root: u32) -> Vec<(u32, u32)> {
    let Ok(out) = Command::new("/bin/ps")
        .args(["-A", "-o", "pid=,ppid=,pgid="])
        .output()
    else {
        return vec![];
    };
    let rows: Vec<(u32, u32, u32)> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace().map(|x| x.parse::<u32>().ok());
            Some((it.next()??, it.next()??, it.next()??))
        })
        .collect();
    let mut tree: Vec<(u32, u32)> = rows
        .iter()
        .filter(|r| r.0 == root)
        .map(|r| (r.0, r.2))
        .collect();
    let mut i = 0;
    while i < tree.len() {
        let parent = tree[i].0;
        for r in rows.iter().filter(|r| r.1 == parent) {
            if !tree.iter().any(|t| t.0 == r.0) {
                tree.push((r.0, r.2));
            }
        }
        i += 1;
    }
    tree
}

#[cfg(unix)]
fn signal_tree(tree: &[(u32, u32)], sig: i32) {
    for &(pid, pgid) in tree {
        // 트리 안의 그룹 리더는 그룹째 (Claude Code는 도구 명령을 별도 그룹으로 띄운다 — 실측)
        if pgid == pid {
            signal_group(pid, sig);
        }
        if pid > 1 {
            // SAFETY: 수집한 트리의 pid에만 신호를 보낸다. 0·1은 제외.
            unsafe { libc::kill(pid as i32, sig) };
        }
    }
}

/// 런타임과 그 하위 프로세스(도구 명령 포함)를 SIGTERM → 유예 → SIGKILL 순으로 종료한다.
#[cfg(unix)]
pub fn terminate_tree(root: u32, grace: std::time::Duration) {
    let tree = process_tree(root);
    if tree.is_empty() {
        return;
    }
    signal_tree(&tree, libc::SIGTERM);
    let deadline = std::time::Instant::now() + grace;
    // SAFETY: 신호 0은 존재 여부만 확인한다.
    let alive = || {
        tree.iter()
            .any(|&(pid, _)| unsafe { libc::kill(pid as i32, 0) } == 0)
    };
    while alive() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    if alive() {
        signal_tree(&tree, libc::SIGKILL);
    }
}

/// 즉시 강제 종료 (유예 없음)
#[cfg(unix)]
pub fn kill_tree(root: u32) {
    let tree = process_tree(root);
    signal_tree(&tree, libc::SIGKILL);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 트리_종료는_별도_그룹의_도구_명령까지_끈다() {
        use std::os::unix::process::CommandExt;
        // 런타임 흉내: 자식 하나는 같은 그룹, 하나는 자기 그룹을 새로 만든다 (Claude Code 도구 명령과 같은 모양)
        let mut child = std::process::Command::new("/bin/sh")
            .args([
                "-c",
                "sleep 30 & /usr/bin/perl -e 'setpgrp(0,0); sleep 30' & wait",
            ])
            .process_group(0)
            .spawn()
            .unwrap();
        let pid = child.id();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let tree = process_tree(pid);
        assert!(tree.len() >= 3, "sh + sleep + perl: {tree:?}");
        assert!(
            tree.iter().any(|&(p, g)| p != pid && g == p),
            "별도 그룹 자식이 있어야 재현"
        );

        terminate_tree(pid, std::time::Duration::from_secs(2));
        let _ = child.wait(); // 테스트가 띄운 sh는 직접 거둔다 (실제 고아는 launchd가 거둠)
        std::thread::sleep(std::time::Duration::from_millis(200));
        for &(p, _) in &tree {
            assert!(process_command(p).is_none(), "pid {p}가 남아 있음");
        }
    }
}
