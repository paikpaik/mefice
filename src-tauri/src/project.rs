use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;

use crate::db::now_ms;
use crate::error::{AppError, AppResult};
use crate::sysproc::{resolve_binary, run_capture};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: i64,
    pub path: String,
    pub name: String,
    pub instructions: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_opened_at: i64,
}

impl Project {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get("id")?,
            path: r.get("path")?,
            name: r.get("name")?,
            instructions: r.get("instructions")?,
            created_at: r.get("created_at")?,
            updated_at: r.get("updated_at")?,
            last_opened_at: r.get("last_opened_at")?,
        })
    }
}

/// 프로젝트 폴더의 Git 상태. 저장하지 않고 열 때마다 새로 읽는다 (읽기 전용 명령만 사용).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitInfo {
    pub is_repo: bool,
    pub root: Option<String>,
    pub branch: Option<String>,
    pub head: Option<String>,
    /// 커밋되지 않은 변경(추적·미추적 포함) 파일 수. 사용자 기존 작업 보존 경고에 사용.
    pub dirty_files: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectView {
    #[serde(flatten)]
    pub project: Project,
    pub exists: bool,
    pub git: GitInfo,
}

const SELECT_COLS: &str = "id, path, name, instructions, created_at, updated_at, last_opened_at";

/// 경로를 정규화하고 유효한 디렉토리인지 확인한다.
pub fn normalize_path(raw: &str) -> AppResult<PathBuf> {
    let p = PathBuf::from(raw.trim());
    if !p.is_absolute() {
        return Err(AppError::InvalidInput("절대 경로가 필요합니다".into()));
    }
    let canon = p
        .canonicalize()
        .map_err(|_| AppError::InvalidInput(format!("폴더를 찾을 수 없습니다: {raw}")))?;
    if !canon.is_dir() {
        return Err(AppError::InvalidInput(format!("폴더가 아닙니다: {raw}")));
    }
    Ok(canon)
}

/// 폴더를 프로젝트로 등록(이미 있으면 last_opened_at만 갱신)하고 반환한다.
pub fn upsert(conn: &Connection, path: &Path) -> AppResult<Project> {
    let path_str = path.to_string_lossy().to_string();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path_str.clone());
    let now = now_ms();
    conn.execute(
        "INSERT INTO projects (path, name, instructions, created_at, updated_at, last_opened_at)
         VALUES (?1, ?2, '', ?3, ?3, ?3)
         ON CONFLICT(path) DO UPDATE SET last_opened_at = excluded.last_opened_at",
        params![path_str, name, now],
    )?;
    conn.query_row(
        &format!("SELECT {SELECT_COLS} FROM projects WHERE path = ?1"),
        [&path_str],
        Project::from_row,
    )
    .map_err(Into::into)
}

pub fn get(conn: &Connection, id: i64) -> AppResult<Option<Project>> {
    Ok(conn
        .query_row(
            &format!("SELECT {SELECT_COLS} FROM projects WHERE id = ?1"),
            [id],
            Project::from_row,
        )
        .optional()?)
}

pub fn update_instructions(conn: &Connection, id: i64, instructions: &str) -> AppResult<Project> {
    let changed = conn.execute(
        "UPDATE projects SET instructions = ?1, updated_at = ?2 WHERE id = ?3",
        params![instructions, now_ms(), id],
    )?;
    if changed == 0 {
        return Err(AppError::NotFound(format!("프로젝트 #{id}")));
    }
    get(conn, id)?.ok_or_else(|| AppError::NotFound(format!("프로젝트 #{id}")))
}

pub fn inspect_git(path: &Path) -> GitInfo {
    let not_repo = GitInfo {
        is_repo: false,
        root: None,
        branch: None,
        head: None,
        dirty_files: 0,
    };
    let Some(git) = resolve_binary("git") else {
        return not_repo;
    };

    let inside = run_capture(&git, &["rev-parse", "--is-inside-work-tree"], Some(path));
    if inside.as_deref() != Some("true") {
        return not_repo;
    }
    let root = run_capture(&git, &["rev-parse", "--show-toplevel"], Some(path));
    let branch =
        run_capture(&git, &["branch", "--show-current"], Some(path)).filter(|b| !b.is_empty());
    // 커밋이 없는 새 저장소면 실패하므로 Option 그대로 둔다.
    let head = run_capture(&git, &["rev-parse", "--short", "HEAD"], Some(path));
    let dirty_files = run_capture(&git, &["status", "--porcelain"], Some(path))
        .map(|s| s.lines().filter(|l| !l.is_empty()).count())
        .unwrap_or(0);

    GitInfo {
        is_repo: true,
        root,
        branch,
        head,
        dirty_files,
    }
}

pub fn to_view(project: Project) -> ProjectView {
    let p = PathBuf::from(&project.path);
    let exists = p.is_dir();
    let git = if exists {
        inspect_git(&p)
    } else {
        GitInfo {
            is_repo: false,
            root: None,
            branch: None,
            head: None,
            dirty_files: 0,
        }
    };
    ProjectView {
        project,
        exists,
        git,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    #[test]
    fn 프로젝트_등록_중복없이_갱신() {
        let db = Db::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = normalize_path(dir.path().to_str().unwrap()).unwrap();

        let a = upsert(&conn, &path).unwrap();
        let b = upsert(&conn, &path).unwrap();
        assert_eq!(a.id, b.id);
        assert!(b.last_opened_at >= a.last_opened_at);

        let updated = update_instructions(&conn, a.id, "테스트는 pnpm test").unwrap();
        assert_eq!(updated.instructions, "테스트는 pnpm test");
        // 재등록해도 지침은 보존
        assert_eq!(
            upsert(&conn, &path).unwrap().instructions,
            "테스트는 pnpm test"
        );
    }

    #[test]
    fn 상대경로와_없는경로는_거부() {
        assert!(normalize_path("relative/dir").is_err());
        assert!(normalize_path("/definitely/not/here/mefice").is_err());
    }

    #[test]
    fn git_아닌_폴더() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!inspect_git(dir.path()).is_repo);
    }
}
