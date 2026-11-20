use serde::{Serialize, Serializer};

/// 앱 전역 에러. Tauri 명령 경계에서 문자열로 직렬화되어 프론트엔드로 전달된다.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("데이터베이스 오류: {0}")]
    Db(#[from] rusqlite::Error),

    #[error("입출력 오류: {0}")]
    Io(#[from] std::io::Error),

    #[error("직렬화 오류: {0}")]
    Json(#[from] serde_json::Error),

    #[error("{0}")]
    InvalidInput(String),

    #[error("찾을 수 없음: {0}")]
    NotFound(String),

    #[error("내부 오류: {0}")]
    Internal(String),
}

impl Serialize for AppError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

pub type AppResult<T> = Result<T, AppError>;
