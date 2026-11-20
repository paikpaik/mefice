//! Claude Code CLI 어댑터 (검증 버전: 2.1.304).
//!
//! 실행: `claude -p --input-format stream-json --output-format stream-json --verbose
//!        --permission-prompt-tool stdio ...`
//! - stdin/stdout 모두 JSONL. 호스트는 먼저 `initialize` 제어 요청을 보내야 승인 요청(`can_use_tool`)을 받는다.
//! - 승인 응답: `control_response{subtype:success, request_id, response:{behavior:allow|deny, ...}}`
//! - 중단: `control_request{subtype:interrupt}` → 현재 턴이 `result`로 끝난다.
//! - 턴이 끝나도 프로세스는 다음 사용자 메시지를 기다린다. stdin을 닫으면 종료한다.
//!
//! 알 수 없는 type/subtype은 무시한다 (런타임 버전에 따라 이벤트가 추가될 수 있음).

use std::path::Path;

use serde_json::{Value, json};
use tokio::process::Command;

use super::{Parsed, RunConfig, RunEvent, Usage, truncate_chars};

const TOOL_RESULT_PREVIEW_CHARS: usize = 2000;
const SUMMARY_CHARS: usize = 300;

/// 새 세션 또는 이전 세션 이어서 실행
pub enum Session<'a> {
    New(&'a str),
    Resume(&'a str),
}

pub fn build_command(bin: &Path, cwd: &Path, cfg: &RunConfig, session: Session) -> Command {
    let mut cmd = Command::new(bin);
    cmd.current_dir(cwd)
        .env("PATH", crate::sysproc::child_path())
        .args([
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--permission-prompt-tool",
            "stdio",
            "--permission-mode",
            cfg.permission_mode.as_cli(),
            // MCP는 MVP 범위 밖(스펙 10장). 사용자 전역 MCP 도구 정의가 프롬프트에 실리지 않게 한다.
            "--strict-mcp-config",
        ])
        .arg("--tools")
        .arg(cfg.tools.join(","));
    match session {
        Session::New(id) => cmd.args(["--session-id", id]),
        Session::Resume(id) => cmd.args(["--resume", id]),
    };
    if let Some(m) = &cfg.model {
        cmd.args(["--model", m]);
    }
    if let Some(e) = &cfg.effort {
        cmd.args(["--effort", e]);
    }
    if !cfg.system_append.trim().is_empty() {
        cmd.args(["--append-system-prompt", &cfg.system_append]);
    }
    cmd
}

pub fn encode_initialize() -> String {
    json!({"type":"control_request","request_id":"mefice-init","request":{"subtype":"initialize"}})
        .to_string()
}

pub fn encode_user_message(text: &str) -> String {
    json!({"type":"user","message":{"role":"user","content":text}}).to_string()
}

pub fn encode_interrupt(request_id: &str) -> String {
    json!({"type":"control_request","request_id":request_id,"request":{"subtype":"interrupt"}})
        .to_string()
}

pub fn encode_permission_response(request_id: &str, allow: bool, input: &Value) -> String {
    let response = if allow {
        json!({"behavior":"allow","updatedInput":input})
    } else {
        json!({"behavior":"deny","message":"사용자가 이 도구 실행을 거부했습니다."})
    };
    json!({"type":"control_response","response":{"subtype":"success","request_id":request_id,"response":response}})
        .to_string()
}

/// 도구 입력을 한 줄 요약으로 만든다 (전체 입력은 이벤트에 저장하지 않음).
pub fn summarize_tool_input(name: &str, input: &Value) -> String {
    let field = |k: &str| input.get(k).and_then(Value::as_str);
    let s = match name {
        "Bash" => field("command").map(str::to_string),
        "Read" | "Write" | "Edit" => field("file_path").map(str::to_string),
        "Glob" | "Grep" => field("pattern").map(|p| match field("path") {
            Some(path) => format!("{p} @ {path}"),
            None => p.to_string(),
        }),
        _ => None,
    }
    .unwrap_or_else(|| input.to_string());
    truncate_chars(&s, SUMMARY_CHARS).0
}

fn u64_at(v: &Value, key: &str) -> Option<u64> {
    v.get(key).and_then(Value::as_u64)
}

fn str_at(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

fn tool_result_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|i| i.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// stdout 한 줄을 해석한다. 해석할 수 없는 줄은 빈 Vec.
pub fn parse_line(line: &str) -> Vec<Parsed> {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return vec![];
    };
    let ty = v.get("type").and_then(Value::as_str).unwrap_or("");
    let subtype = v.get("subtype").and_then(Value::as_str).unwrap_or("");
    let mut out = Vec::new();

    match (ty, subtype) {
        ("system", "init") => {
            if let Some(session_id) = str_at(&v, "session_id") {
                out.push(Parsed::Event(RunEvent::Started {
                    session_id,
                    model: str_at(&v, "model"),
                }));
            }
        }
        ("system", "permission_denied") => out.push(Parsed::Event(RunEvent::PermissionDenied {
            tool_name: str_at(&v, "tool_name").unwrap_or_default(),
            message: str_at(&v, "message").unwrap_or_default(),
        })),
        ("assistant", _) => {
            let blocks = v
                .pointer("/message/content")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for b in blocks {
                match b.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        let text = str_at(&b, "text").unwrap_or_default();
                        if !text.trim().is_empty() {
                            out.push(Parsed::Event(RunEvent::Text { text }));
                        }
                    }
                    Some("tool_use") => {
                        let name = str_at(&b, "name").unwrap_or_default();
                        let input = b.get("input").cloned().unwrap_or(Value::Null);
                        out.push(Parsed::Event(RunEvent::ToolUse {
                            tool_use_id: str_at(&b, "id").unwrap_or_default(),
                            summary: summarize_tool_input(&name, &input),
                            name,
                        }));
                    }
                    // thinking 등은 저장하지 않는다
                    _ => {}
                }
            }
        }
        ("user", _) => {
            if let Some(blocks) = v.pointer("/message/content").and_then(Value::as_array) {
                for b in blocks {
                    if b.get("type").and_then(Value::as_str) == Some("tool_result") {
                        let text = tool_result_text(b.get("content").unwrap_or(&Value::Null));
                        let (preview, truncated) = truncate_chars(&text, TOOL_RESULT_PREVIEW_CHARS);
                        out.push(Parsed::Event(RunEvent::ToolResult {
                            tool_use_id: str_at(b, "tool_use_id").unwrap_or_default(),
                            is_error: b.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                            preview,
                            truncated,
                        }));
                    }
                }
            }
        }
        ("result", _) => {
            let u = v.get("usage").cloned().unwrap_or(Value::Null);
            out.push(Parsed::Event(RunEvent::TurnResult {
                is_error: v.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                subtype: subtype.to_string(),
                result: str_at(&v, "result"),
                usage: Usage {
                    input_tokens: u64_at(&u, "input_tokens"),
                    output_tokens: u64_at(&u, "output_tokens"),
                    cache_read_tokens: u64_at(&u, "cache_read_input_tokens"),
                    cache_creation_tokens: u64_at(&u, "cache_creation_input_tokens"),
                    cost_usd: v.get("total_cost_usd").and_then(Value::as_f64),
                },
                duration_ms: u64_at(&v, "duration_ms"),
                num_turns: u64_at(&v, "num_turns"),
            }));
        }
        ("control_request", _) => {
            let req = v.get("request").cloned().unwrap_or(Value::Null);
            if req.get("subtype").and_then(Value::as_str) == Some("can_use_tool")
                && let Some(request_id) = str_at(&v, "request_id")
            {
                out.push(Parsed::PermissionRequest {
                    request_id,
                    tool_name: str_at(&req, "tool_name").unwrap_or_default(),
                    input: req.get("input").cloned().unwrap_or(Value::Null),
                    description: str_at(&req, "description"),
                });
            }
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_파싱() {
        let line = r#"{"type":"system","subtype":"init","session_id":"s-1","model":"claude-haiku-5-5","tools":["Read"]}"#;
        assert_eq!(
            parse_line(line),
            vec![Parsed::Event(RunEvent::Started {
                session_id: "s-1".into(),
                model: Some("claude-haiku-5-5".into())
            })]
        );
    }

    #[test]
    fn assistant_텍스트와_도구호출() {
        let line = r#"{"type":"assistant","message":{"content":[
            {"type":"thinking","thinking":""},
            {"type":"text","text":"읽어볼게요"},
            {"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls -la","description":"목록"}}]}}"#;
        let parsed = parse_line(line);
        assert_eq!(parsed.len(), 2);
        assert_eq!(
            parsed[1],
            Parsed::Event(RunEvent::ToolUse {
                tool_use_id: "t1".into(),
                name: "Bash".into(),
                summary: "ls -la".into()
            })
        );
    }

    #[test]
    fn 도구결과는_잘라서_저장() {
        let long = "가".repeat(TOOL_RESULT_PREVIEW_CHARS + 10);
        let line = json!({"type":"user","message":{"role":"user","content":[
            {"type":"tool_result","tool_use_id":"t1","content":long,"is_error":true}]}})
        .to_string();
        match &parse_line(&line)[0] {
            Parsed::Event(RunEvent::ToolResult {
                is_error,
                truncated,
                preview,
                ..
            }) => {
                assert!(*is_error && *truncated);
                assert_eq!(preview.chars().count(), TOOL_RESULT_PREVIEW_CHARS + 1);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn 승인요청_파싱() {
        let line = r#"{"type":"control_request","request_id":"r1","request":{"subtype":"can_use_tool","tool_name":"Write","input":{"file_path":"/a/b.txt","content":"hi"},"description":"b.txt","tool_use_id":"t1"}}"#;
        assert_eq!(
            parse_line(line),
            vec![Parsed::PermissionRequest {
                request_id: "r1".into(),
                tool_name: "Write".into(),
                input: json!({"file_path":"/a/b.txt","content":"hi"}),
                description: Some("b.txt".into()),
            }]
        );
    }

    #[test]
    fn result_사용량_파싱() {
        let line = r#"{"type":"result","subtype":"success","is_error":false,"result":"hello","total_cost_usd":0.0042,
            "duration_ms":1985,"num_turns":2,
            "usage":{"input_tokens":4,"output_tokens":124,"cache_read_input_tokens":64048,"cache_creation_input_tokens":17616}}"#;
        match &parse_line(line)[0] {
            Parsed::Event(RunEvent::TurnResult { usage, result, .. }) => {
                assert_eq!(result.as_deref(), Some("hello"));
                assert_eq!(usage.cache_read_tokens, Some(64048));
                assert_eq!(usage.cost_usd, Some(0.0042));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn 모르는_이벤트와_깨진_줄은_무시() {
        assert!(parse_line(r#"{"type":"rate_limit_event"}"#).is_empty());
        assert!(parse_line(r#"{"type":"system","subtype":"hook_started"}"#).is_empty());
        assert!(parse_line("not json").is_empty());
    }

    #[test]
    fn 승인_응답_인코딩() {
        let v: Value =
            serde_json::from_str(&encode_permission_response("r1", true, &json!({"a":1}))).unwrap();
        assert_eq!(v["response"]["request_id"], "r1");
        assert_eq!(v["response"]["response"]["behavior"], "allow");
        assert_eq!(v["response"]["response"]["updatedInput"]["a"], 1);
    }
}
