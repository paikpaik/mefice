//! Git 명령 래퍼 (스펙 4.4).
//!
//! 사용자 작업 트리(메인 폴더)를 바꾸는 명령은 `merge` 하나뿐이며, 호출 전에 충돌 사전 확인과
//! 미커밋 변경 겹침 확인을 통과해야 한다. 나머지 쓰기 명령은 Mefice가 만든 worktree·브랜치에만 쓴다.
//! worktree는 파일 변경 격리 수단이지 보안 샌드박스가 아니다.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

use crate::error::{AppError, AppResult};
use crate::sysproc::{child_path, resolve_binary};

/// Mefice 스냅샷 커밋 작성자. 에이전트 변경임을 이력에서 구분할 수 있게 한다.
const SNAPSHOT_IDENTITY: [&str; 4] = [
    "-c",
    "user.name=Mefice",
    "-c",
    "user.email=mefice@localhost",
];

pub struct Output {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

fn git_bin() -> AppResult<PathBuf> {
    resolve_binary("git").ok_or_else(|| AppError::Internal("git을 찾을 수 없습니다".into()))
}

/// 종료 코드와 상관없이 결과를 돌려준다.
pub fn exec(repo: &Path, args: &[&str]) -> AppResult<Output> {
    let out = Command::new(git_bin()?)
        .args(args)
        .current_dir(repo)
        .env("PATH", child_path())
        .env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()?;
    Ok(Output {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
    })
}

/// 성공하면 stdout(trim), 실패하면 stderr를 담은 에러.
pub fn run(repo: &Path, args: &[&str]) -> AppResult<String> {
    let o = exec(repo, args)?;
    if o.code == 0 {
        Ok(o.stdout.trim().to_string())
    } else {
        Err(AppError::Internal(format!(
            "git {} 실패: {}",
            args.first().unwrap_or(&""),
            if o.stderr.is_empty() {
                o.stdout.trim()
            } else {
                &o.stderr
            }
        )))
    }
}

/// 커밋이 하나 이상 있는 Git 저장소인지 (worktree 격리 가능 여부)
pub fn can_isolate(repo: &Path) -> bool {
    run(repo, &["rev-parse", "--verify", "--quiet", "HEAD"]).is_ok()
}

pub fn head_commit(repo: &Path) -> AppResult<String> {
    run(repo, &["rev-parse", "HEAD"])
}

pub fn current_branch(repo: &Path) -> Option<String> {
    run(repo, &["branch", "--show-current"])
        .ok()
        .filter(|b| !b.is_empty())
}

pub fn worktree_add(repo: &Path, path: &Path, branch: &str, base: &str) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let path = path.to_string_lossy();
    run(repo, &["worktree", "add", "-b", branch, &path, base]).map(|_| ())
}

/// worktree 디렉터리와 메타데이터를 제거한다. 이미 없어도 성공으로 본다.
pub fn worktree_remove(repo: &Path, path: &Path) -> AppResult<()> {
    let p = path.to_string_lossy();
    if path.exists() {
        run(repo, &["worktree", "remove", "--force", &p])?;
    }
    let _ = run(repo, &["worktree", "prune"]);
    Ok(())
}

pub fn delete_branch(repo: &Path, branch: &str) -> AppResult<()> {
    run(repo, &["branch", "-D", branch]).map(|_| ())
}

/// worktree의 모든 변경을 Mefice 브랜치에 커밋한다. 변경이 없으면 현재 HEAD를 돌려준다.
/// 사용자 훅은 에이전트 스냅샷에 적용하지 않는다(--no-verify). 사용자 브랜치 병합 시에는 적용된다.
pub fn snapshot_commit(worktree: &Path, message: &str) -> AppResult<String> {
    run(worktree, &["add", "-A"])?;
    let staged = exec(worktree, &["diff", "--cached", "--quiet"])?;
    if staged.code != 0 {
        let mut args: Vec<&str> = SNAPSHOT_IDENTITY.to_vec();
        args.extend(["commit", "--no-verify", "-q", "-m", message]);
        run(worktree, &args)?;
    }
    head_commit(worktree)
}

/// 다른 브랜치를 worktree에 병합한다 (선행 작업이 여럿일 때). 충돌 시 중단하고 에러.
pub fn merge_into_worktree(worktree: &Path, branch: &str) -> AppResult<()> {
    let mut args: Vec<&str> = SNAPSHOT_IDENTITY.to_vec();
    args.extend(["merge", "--no-edit", "--no-verify", "-q", branch]);
    let o = exec(worktree, &args)?;
    if o.code != 0 {
        let _ = exec(worktree, &["merge", "--abort"]);
        return Err(AppError::InvalidInput(format!(
            "선행 작업 브랜치 {branch}를 합치는 중 충돌이 났습니다"
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FileChange {
    pub path: String,
    /// A(추가) M(수정) D(삭제) R(이름 변경) 등
    pub status: String,
    /// 바이너리 파일이면 None
    pub additions: Option<u64>,
    pub deletions: Option<u64>,
}

pub fn changed_files(repo: &Path, base: &str, head: &str) -> AppResult<Vec<FileChange>> {
    let range = format!("{base}..{head}");
    let status = run(repo, &["diff", "--name-status", "--no-renames", &range])?;
    let numstat = run(repo, &["diff", "--numstat", "--no-renames", &range])?;
    let mut out: Vec<FileChange> = status
        .lines()
        .filter_map(|l| {
            let (st, path) = l.split_once('\t')?;
            Some(FileChange {
                path: path.to_string(),
                status: st.to_string(),
                additions: None,
                deletions: None,
            })
        })
        .collect();
    for l in numstat.lines() {
        let mut it = l.splitn(3, '\t');
        let (Some(a), Some(d), Some(path)) = (it.next(), it.next(), it.next()) else {
            continue;
        };
        if let Some(fc) = out.iter_mut().find(|f| f.path == path) {
            fc.additions = a.parse().ok();
            fc.deletions = d.parse().ok();
        }
    }
    Ok(out)
}

/// 통합 diff. `max_bytes`를 넘으면 잘라서 돌려준다.
pub fn diff(repo: &Path, base: &str, head: &str, max_bytes: usize) -> AppResult<(String, bool)> {
    let o = exec(
        repo,
        &[
            "diff",
            "--no-color",
            "--no-renames",
            &format!("{base}..{head}"),
        ],
    )?;
    if o.code != 0 {
        return Err(AppError::Internal(format!("git diff 실패: {}", o.stderr)));
    }
    if o.stdout.len() <= max_bytes {
        return Ok((o.stdout, false));
    }
    let mut cut = max_bytes;
    while !o.stdout.is_char_boundary(cut) {
        cut -= 1;
    }
    Ok((o.stdout[..cut].to_string(), true))
}

/// 작업 트리를 건드리지 않고 병합 충돌 파일을 계산한다 (git 2.38+). 충돌이 없으면 빈 Vec.
pub fn merge_conflicts(repo: &Path, ours: &str, theirs: &str) -> AppResult<Vec<String>> {
    let o = exec(
        repo,
        &["merge-tree", "--write-tree", "--name-only", ours, theirs],
    )?;
    match o.code {
        0 => Ok(vec![]),
        // 첫 줄은 결과 트리 OID, 이후 빈 줄 전까지가 충돌 파일 목록
        1 => Ok(o
            .stdout
            .lines()
            .skip(1)
            .take_while(|l| !l.trim().is_empty())
            .map(str::to_string)
            .collect()),
        _ => Err(AppError::Internal(format!(
            "충돌 사전 확인 실패 (git 2.38 이상 필요): {}",
            o.stderr
        ))),
    }
}

/// 메인 작업 트리의 미커밋 변경 파일 (추적·미추적)
pub fn dirty_files(repo: &Path) -> AppResult<Vec<String>> {
    let s = run(
        repo,
        &[
            "status",
            "--porcelain",
            "--untracked-files=all",
            "--no-renames",
        ],
    )?;
    Ok(s.lines()
        .filter(|l| l.len() > 3)
        .map(|l| l[3..].to_string())
        .collect())
}

/// 사용자 현재 브랜치에 병합한다. 사전 확인을 통과한 뒤에만 호출해야 한다.
pub fn merge_branch(repo: &Path, branch: &str, message: &str) -> AppResult<String> {
    let o = exec(repo, &["merge", "--no-ff", "-m", message, branch])?;
    if o.code != 0 {
        // 사전 확인을 통과했는데 실패한 경우(훅 거부 등). 진행 중인 병합이 남지 않게 정리한다.
        let merging = exec(repo, &["rev-parse", "-q", "--verify", "MERGE_HEAD"])?.code == 0;
        if merging {
            let _ = exec(repo, &["merge", "--abort"]);
        }
        return Err(AppError::InvalidInput(format!(
            "병합 실패: {}",
            if o.stderr.is_empty() {
                o.stdout.trim()
            } else {
                &o.stderr
            }
        )));
    }
    head_commit(repo)
}

#[cfg(test)]
pub mod test_repo {
    use super::*;

    /// 커밋 하나가 있는 임시 저장소
    pub fn init() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        run(p, &["init", "-q", "-b", "main"]).unwrap();
        std::fs::write(p.join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(p.join("b.txt"), "b\n").unwrap();
        snapshot_commit(p, "init").unwrap();
        dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktree_스냅샷과_변경파일() {
        let repo = test_repo::init();
        let r = repo.path();
        let base = head_commit(r).unwrap();
        let wt_dir = tempfile::tempdir().unwrap();
        let wt = wt_dir.path().join("t1");
        worktree_add(r, &wt, "mefice/task-1", &base).unwrap();

        std::fs::write(wt.join("a.txt"), "one\nTWO\n").unwrap();
        std::fs::write(wt.join("new.txt"), "n\n").unwrap();
        let head = snapshot_commit(&wt, "t1").unwrap();
        assert_ne!(head, base);
        assert_eq!(
            snapshot_commit(&wt, "again").unwrap(),
            head,
            "변경 없으면 커밋 안 함"
        );

        let files = changed_files(r, &base, "mefice/task-1").unwrap();
        let a = files.iter().find(|f| f.path == "a.txt").unwrap();
        assert_eq!(
            (a.status.as_str(), a.additions, a.deletions),
            ("M", Some(1), Some(1))
        );
        assert_eq!(
            files.iter().find(|f| f.path == "new.txt").unwrap().status,
            "A"
        );
        assert!(diff(r, &base, &head, 1_000_000).unwrap().0.contains("+TWO"));
        assert!(diff(r, &base, &head, 10).unwrap().1, "잘림 표시");

        // 메인 작업 트리는 그대로
        assert_eq!(
            std::fs::read_to_string(r.join("a.txt")).unwrap(),
            "one\ntwo\n"
        );
        assert!(dirty_files(r).unwrap().is_empty());

        worktree_remove(r, &wt).unwrap();
        assert!(!wt.exists());
        delete_branch(r, "mefice/task-1").unwrap();
    }

    #[test]
    fn 충돌_사전확인과_병합() {
        let repo = test_repo::init();
        let r = repo.path();
        let base = head_commit(r).unwrap();
        let wts = tempfile::tempdir().unwrap();
        for (i, content) in [(1, "one\nAAA\n"), (2, "one\nBBB\n"), (3, "x\n")] {
            let wt = wts.path().join(format!("t{i}"));
            worktree_add(r, &wt, &format!("mefice/task-{i}"), &base).unwrap();
            let file = if i == 3 { "b.txt" } else { "a.txt" };
            std::fs::write(wt.join(file), content).unwrap();
            snapshot_commit(&wt, "x").unwrap();
        }
        assert!(
            merge_conflicts(r, "HEAD", "mefice/task-1")
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            merge_conflicts(r, "mefice/task-1", "mefice/task-2").unwrap(),
            vec!["a.txt"]
        );
        assert!(
            merge_conflicts(r, "mefice/task-1", "mefice/task-3")
                .unwrap()
                .is_empty()
        );

        // 사용자 미커밋 변경(다른 파일)이 있어도 병합 후 보존된다
        std::fs::write(r.join("user.txt"), "내 작업\n").unwrap();
        merge_branch(r, "mefice/task-1", "merge t1").unwrap();
        assert_eq!(
            std::fs::read_to_string(r.join("a.txt")).unwrap(),
            "one\nAAA\n"
        );
        assert_eq!(
            std::fs::read_to_string(r.join("user.txt")).unwrap(),
            "내 작업\n"
        );
        assert_eq!(dirty_files(r).unwrap(), vec!["user.txt"]);
    }
}
