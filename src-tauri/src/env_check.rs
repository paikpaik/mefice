use serde::Serialize;

use crate::sysproc::{resolve_binary, run_capture};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolStatus {
    pub name: String,
    pub installed: bool,
    pub path: Option<String>,
    pub version: Option<String>,
    /// 이 도구가 MVP 동작에 필수인지 (UI에서 경고 수준 결정)
    pub required: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentReport {
    pub os: String,
    pub arch: String,
    pub tools: Vec<ToolStatus>,
}

fn check(name: &str, required: bool) -> ToolStatus {
    match resolve_binary(name) {
        Some(path) => ToolStatus {
            name: name.into(),
            installed: true,
            version: run_capture(&path, &["--version"], None)
                .and_then(|v| v.lines().next().map(str::to_string)),
            path: Some(path.to_string_lossy().into()),
            required,
        },
        None => ToolStatus {
            name: name.into(),
            installed: false,
            path: None,
            version: None,
            required,
        },
    }
}

/// 실행 환경 검사. 버전 확인만 하며 모델은 호출하지 않는다.
pub fn check_environment() -> EnvironmentReport {
    EnvironmentReport {
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        tools: vec![check("git", true), check("claude", true)],
    }
}
