//! 로컬 실행 관리자 (스펙 4.4, 7장).
//!
//! - 큐: queued Run을 동시 실행 상한 안에서 생성 순서대로 시작한다. (의존성은 5단계)
//! - 프로세스: Run 하나 = 런타임 프로세스 하나. 남은 턴이 없으면 stdin을 닫아 종료시킨다.
//! - 상태 추적은 런타임 출력 이벤트로만 한다. 상태 확인용 모델 호출은 하지 않는다 (스펙 13장).

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, Runtime, Wry};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::ChildStdin;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::runtime::claude_code::{self, Session};
use crate::runtime::{Parsed, RunEvent};
use crate::settings;
use crate::sysproc::resolve_binary;
use crate::task::{self, RunStatus};
use crate::usage;

/// 중단 요청 후 런타임이 스스로 끝나기를 기다리는 시간
const CANCEL_GRACE: Duration = Duration::from_secs(5);
/// stderr를 이벤트로 남기는 최대 줄 수 (로그 폭주 방지)
const MAX_STDERR_EVENTS: usize = 50;

pub const EVENT_RUN_EVENT: &str = "run-event";
pub const EVENT_RUN_UPDATED: &str = "run-updated";
/// 사용량 기록 변경 (payload: task_id)
pub const EVENT_USAGE_UPDATED: &str = "usage-updated";
/// 구독 한도 사용률 갱신 (payload: RateLimitSnapshot)
pub const EVENT_RATE_LIMIT: &str = "rate-limit-updated";

/// 실행 중인 Run에 보내는 제어 메시지
enum Control {
    UserMessage(String, oneshot::Sender<AppResult<()>>),
    Permission {
        request_id: String,
        allow: bool,
        input: Value,
    },
    Cancel,
}

struct Outcome {
    status: RunStatus,
    exit_code: Option<i32>,
    error: Option<String>,
}

impl Outcome {
    fn failed(msg: impl Into<String>) -> Self {
        Self {
            status: RunStatus::Failed,
            exit_code: None,
            error: Some(msg.into()),
        }
    }
}

pub struct Executor<R: Runtime = Wry> {
    app: AppHandle<R>,
    active: Mutex<HashMap<i64, mpsc::UnboundedSender<Control>>>,
    pump_lock: Mutex<()>,
}

impl<R: Runtime> Executor<R> {
    pub fn new(app: AppHandle<R>) -> Arc<Self> {
        Arc::new(Self {
            app,
            active: Mutex::new(HashMap::new()),
            pump_lock: Mutex::new(()),
        })
    }

    fn db(&self) -> &Db {
        self.app.state::<Db>().inner()
    }

    fn sender(&self, run_id: i64) -> Option<mpsc::UnboundedSender<Control>> {
        self.active.lock().ok()?.get(&run_id).cloned()
    }

    /// 이벤트를 저장하고 UI로 보낸다. 호출 시 DB 잠금을 쥐고 있으면 안 된다.
    fn record(&self, run_id: i64, event: RunEvent) {
        let stored = self
            .db()
            .conn()
            .and_then(|c| task::append_event(&c, run_id, &event));
        match stored {
            Ok(s) => {
                let _ = self.app.emit(EVENT_RUN_EVENT, &s);
            }
            Err(e) => eprintln!("[executor] 이벤트 저장 실패 run={run_id}: {e}"),
        }
    }

    /// Run 행을 다시 읽어 UI에 알린다.
    fn notify_run(&self, run_id: i64) {
        let run = self.db().conn().and_then(|c| task::get_run(&c, run_id));
        if let Ok(run) = run {
            let _ = self.app.emit(EVENT_RUN_UPDATED, &run);
        }
    }

    fn set_status(&self, run_id: i64, status: RunStatus) {
        if let Ok(c) = self.db().conn() {
            let _ = task::set_run_status(&c, run_id, status);
        }
        self.notify_run(run_id);
    }

    /// 상한 안에서 대기 중인 Run을 시작한다. Run 생성·종료 시마다 호출한다.
    pub fn pump(self: &Arc<Self>) {
        let Ok(_guard) = self.pump_lock.lock() else {
            return;
        };
        let to_start = (|| -> AppResult<Vec<i64>> {
            let conn = self.db().conn()?;
            let max = settings::load(&conn)?.max_concurrency as i64;
            let slots = max - task::count_active_runs(&conn)?;
            if slots <= 0 {
                return Ok(vec![]);
            }
            let ids = task::queued_run_ids(&conn, slots)?;
            // 다른 pump 호출이 같은 Run을 다시 집지 않도록 즉시 running으로 바꾼다
            for id in &ids {
                task::set_run_status(&conn, *id, RunStatus::Running)?;
            }
            Ok(ids)
        })();

        match to_start {
            Ok(ids) => {
                for run_id in ids {
                    let (tx, rx) = mpsc::unbounded_channel();
                    if let Ok(mut active) = self.active.lock() {
                        active.insert(run_id, tx);
                    }
                    let this = Arc::clone(self);
                    tauri::async_runtime::spawn(async move { this.drive(run_id, rx).await });
                }
            }
            Err(e) => eprintln!("[executor] 스케줄링 실패: {e}"),
        }
    }

    // ---------- 외부(명령)에서 호출하는 제어 ----------

    pub async fn send_message(&self, run_id: i64, text: String) -> AppResult<()> {
        if text.trim().is_empty() {
            return Err(AppError::InvalidInput("메시지를 입력하세요".into()));
        }
        let tx = self.sender(run_id).ok_or_else(|| {
            AppError::InvalidInput("실행이 이미 끝났습니다. '이어서 실행'으로 지시하세요".into())
        })?;
        let (reply_tx, reply_rx) = oneshot::channel();
        tx.send(Control::UserMessage(text, reply_tx))
            .map_err(|_| AppError::InvalidInput("실행이 이미 끝났습니다".into()))?;
        reply_rx
            .await
            .map_err(|_| AppError::InvalidInput("실행이 이미 끝났습니다".into()))?
    }

    pub fn cancel(self: &Arc<Self>, run_id: i64) -> AppResult<()> {
        if let Some(tx) = self.sender(run_id) {
            let _ = tx.send(Control::Cancel);
            return Ok(());
        }
        // 아직 시작하지 않은 Run은 바로 취소
        let run = {
            let conn = self.db().conn()?;
            let run = task::get_run(&conn, run_id)?;
            if run.status == RunStatus::Queued {
                task::finish_run(&conn, run_id, RunStatus::Cancelled, None, None)?;
            }
            run
        };
        if run.status == RunStatus::Queued {
            self.record(
                run_id,
                RunEvent::Notice {
                    text: "시작 전에 취소되었습니다".into(),
                },
            );
            self.notify_run(run_id);
            Ok(())
        } else if run.status.is_terminal() {
            Err(AppError::InvalidInput("이미 끝난 실행입니다".into()))
        } else {
            Err(AppError::Internal(
                "실행 프로세스를 찾을 수 없습니다".into(),
            ))
        }
    }

    pub fn respond_approval(&self, approval_id: i64, allow: bool) -> AppResult<()> {
        let approval = {
            let conn = self.db().conn()?;
            let ap = task::get_approval(&conn, approval_id)?;
            if !task::decide_approval(&conn, approval_id, allow)? {
                return Err(AppError::InvalidInput(
                    "이미 처리되었거나 만료된 승인 요청입니다".into(),
                ));
            }
            ap
        };
        let tx = self
            .sender(approval.run_id)
            .ok_or_else(|| AppError::InvalidInput("실행이 이미 끝났습니다".into()))?;
        let _ = tx.send(Control::Permission {
            request_id: approval.request_id.clone(),
            allow,
            input: approval.tool_input.clone(),
        });
        self.record(
            approval.run_id,
            RunEvent::ApprovalDecided {
                approval_id,
                allowed: allow,
            },
        );

        let resume = {
            let conn = self.db().conn()?;
            let run = task::get_run(&conn, approval.run_id)?;
            run.status == RunStatus::WaitingForApproval
                && task::pending_approvals(&conn, approval.run_id)?.is_empty()
        };
        if resume {
            self.set_status(approval.run_id, RunStatus::Running);
        }
        Ok(())
    }

    // ---------- Run 구동 ----------

    async fn drive(self: Arc<Self>, run_id: i64, mut rx: mpsc::UnboundedReceiver<Control>) {
        let outcome = self.drive_inner(run_id, &mut rx).await;

        if let Ok(mut active) = self.active.lock() {
            active.remove(&run_id);
        }
        if let Ok(conn) = self.db().conn() {
            let _ = task::finish_run(
                &conn,
                run_id,
                outcome.status,
                outcome.exit_code,
                outcome.error.as_deref(),
            );
            // 런타임이 사용량을 한 번도 주지 않은 Run은 "미제공"으로 남긴다 (0으로 가정하지 않음)
            if let Ok(run) = task::get_run(&conn, run_id) {
                let _ = usage::mark_unavailable_if_empty(&conn, run_id, &run.role);
                let _ = self.app.emit(EVENT_USAGE_UPDATED, run.task_id);
            }
        }
        self.notify_run(run_id);
        self.pump();
    }

    async fn drive_inner(&self, run_id: i64, rx: &mut mpsc::UnboundedReceiver<Control>) -> Outcome {
        let run = match self.db().conn().and_then(|c| task::get_run(&c, run_id)) {
            Ok(r) => r,
            Err(e) => return Outcome::failed(e.to_string()),
        };
        let Some(bin) = resolve_binary("claude") else {
            return Outcome::failed("claude CLI를 찾을 수 없습니다. 설정 > 실행 환경을 확인하세요");
        };
        let cwd = PathBuf::from(&run.cwd);
        if !cwd.is_dir() {
            return Outcome::failed(format!("작업 폴더가 없습니다: {}", run.cwd));
        }

        let new_session_id = uuid::Uuid::new_v4().to_string();
        let (session, session_id) = match &run.resume_session_id {
            Some(prev) => (Session::Resume(prev), prev.clone()),
            None => (Session::New(&new_session_id), new_session_id.clone()),
        };
        let mut cmd = claude_code::build_command(&bin, &cwd, &run.config, session);
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            // 앱이 종료되면 자식도 정리 (재시작 시 interrupted로 표시됨)
            .kill_on_drop(true);

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return Outcome::failed(format!("런타임 실행 실패: {e}")),
        };
        if let Ok(conn) = self.db().conn() {
            let _ = task::mark_run_started(&conn, run_id, &session_id);
            let _ = task::set_run_pid(&conn, run_id, child.id());
        }
        self.notify_run(run_id);

        let mut stdin = child.stdin.take();
        let mut stdout = BufReader::new(child.stdout.take().expect("stdout piped")).lines();
        let mut stderr = BufReader::new(child.stderr.take().expect("stderr piped")).lines();

        self.record(
            run_id,
            RunEvent::UserMessage {
                text: run.prompt.clone(),
            },
        );
        write_line(&mut stdin, &claude_code::encode_initialize()).await;
        write_line(&mut stdin, &claude_code::encode_user_message(&run.prompt)).await;

        let mut current_session = session_id.clone();
        let mut pending_turns: u32 = 1;
        let mut cancel_requested = false;
        let mut kill_deadline: Option<Instant> = None;
        let mut last_result: Option<(bool, Option<String>)> = None;
        let mut stderr_open = true;
        let mut stderr_events = 0usize;
        let mut stderr_tail: VecDeque<String> = VecDeque::new();

        loop {
            tokio::select! {
                line = stdout.next_line() => {
                    let Ok(Some(line)) = line else { break };
                    for parsed in claude_code::parse_line(&line) {
                        match parsed {
                            Parsed::Event(ev) => {
                                match &ev {
                                    RunEvent::Started { session_id, auth, .. } => {
                                        current_session = session_id.clone();
                                        if let Ok(c) = self.db().conn() {
                                            let _ = task::set_run_session(&c, run_id, session_id);
                                            if let Some(a) = auth {
                                                let _ = task::set_run_auth(&c, run_id, a);
                                            }
                                        }
                                    }
                                    RunEvent::TurnResult { is_error, result, subtype, usage: turn, session_totals, .. } => {
                                        let recorded = self.db().conn().and_then(|c| usage::record_turn(
                                            &c, run_id, Some(&current_session), &run.role, turn, session_totals.as_ref(),
                                        ));
                                        match recorded {
                                            Ok(()) => { let _ = self.app.emit(EVENT_USAGE_UPDATED, run.task_id); }
                                            Err(e) => eprintln!("[executor] 사용량 기록 실패 run={run_id}: {e}"),
                                        }
                                        pending_turns = pending_turns.saturating_sub(1);
                                        let err_text = is_error.then(|| result.clone().unwrap_or_else(|| subtype.clone()));
                                        last_result = Some((*is_error, err_text));
                                        if pending_turns == 0 {
                                            stdin = None; // stdin 닫기 → 런타임 정상 종료
                                        }
                                    }
                                    _ => {}
                                }
                                self.record(run_id, ev);
                            }
                            Parsed::PermissionRequest { request_id, tool_name, input, description } => {
                                if cancel_requested {
                                    write_line(&mut stdin, &claude_code::encode_permission_response(&request_id, false, &input)).await;
                                    continue;
                                }
                                let created = self.db().conn().and_then(|c| task::create_approval(
                                    &c, run_id, &request_id, &tool_name, &input, description.as_deref(),
                                ));
                                match created {
                                    Ok(ap) => {
                                        self.record(run_id, RunEvent::ApprovalRequested {
                                            approval_id: ap.id,
                                            summary: claude_code::summarize_tool_input(&tool_name, &input),
                                            tool_name,
                                        });
                                        self.set_status(run_id, RunStatus::WaitingForApproval);
                                    }
                                    Err(e) => {
                                        // 기록 실패 시 안전하게 거부
                                        eprintln!("[executor] 승인 요청 저장 실패: {e}");
                                        write_line(&mut stdin, &claude_code::encode_permission_response(&request_id, false, &input)).await;
                                    }
                                }
                            }
                            Parsed::RateLimit(mut snapshot) => {
                                snapshot.observed_at = crate::db::now_ms();
                                if let Ok(c) = self.db().conn() {
                                    let _ = usage::save_rate_limit(&c, &snapshot);
                                }
                                let _ = self.app.emit(EVENT_RATE_LIMIT, &snapshot);
                            }
                        }
                    }
                }
                line = stderr.next_line(), if stderr_open => {
                    match line {
                        Ok(Some(l)) if !l.trim().is_empty() => {
                            if stderr_tail.len() == 20 { stderr_tail.pop_front(); }
                            stderr_tail.push_back(l.clone());
                            if stderr_events < MAX_STDERR_EVENTS {
                                stderr_events += 1;
                                self.record(run_id, RunEvent::Stderr { text: l });
                            }
                        }
                        Ok(Some(_)) => {}
                        _ => stderr_open = false,
                    }
                }
                ctrl = rx.recv() => {
                    match ctrl {
                        Some(Control::UserMessage(text, reply)) => {
                            if cancel_requested || stdin.is_none() {
                                let _ = reply.send(Err(AppError::InvalidInput("실행이 끝나는 중입니다. '이어서 실행'으로 지시하세요".into())));
                            } else if write_line(&mut stdin, &claude_code::encode_user_message(&text)).await {
                                pending_turns += 1;
                                self.record(run_id, RunEvent::UserMessage { text });
                                let _ = reply.send(Ok(()));
                            } else {
                                let _ = reply.send(Err(AppError::Internal("런타임에 메시지를 전달하지 못했습니다".into())));
                            }
                        }
                        Some(Control::Permission { request_id, allow, input }) => {
                            write_line(&mut stdin, &claude_code::encode_permission_response(&request_id, allow, &input)).await;
                        }
                        Some(Control::Cancel) if !cancel_requested => {
                            cancel_requested = true;
                            self.set_status(run_id, RunStatus::Cancelling);
                            self.record(run_id, RunEvent::Notice { text: "중단 요청".into() });
                            // 대기 중인 승인은 거부로 응답해 런타임이 멈춰 있지 않게 한다
                            let pending = self.db().conn().and_then(|c| task::pending_approvals(&c, run_id)).unwrap_or_default();
                            for ap in pending {
                                if let Ok(c) = self.db().conn() { let _ = task::decide_approval(&c, ap.id, false); }
                                write_line(&mut stdin, &claude_code::encode_permission_response(&ap.request_id, false, &ap.tool_input)).await;
                            }
                            write_line(&mut stdin, &claude_code::encode_interrupt("mefice-interrupt")).await;
                            stdin = None;
                            kill_deadline = Some(Instant::now() + CANCEL_GRACE);
                        }
                        Some(Control::Cancel) | None => {}
                    }
                }
                _ = tokio::time::sleep_until(kill_deadline.unwrap_or_else(Instant::now)), if kill_deadline.is_some() => {
                    kill_deadline = None;
                    self.record(run_id, RunEvent::Notice { text: "응답이 없어 프로세스를 강제 종료합니다".into() });
                    let _ = child.start_kill();
                }
            }
        }

        let exit_code = child.wait().await.ok().and_then(|s| s.code());

        if cancel_requested {
            self.record(
                run_id,
                RunEvent::Notice {
                    text: "사용자가 중단했습니다".into(),
                },
            );
            return Outcome {
                status: RunStatus::Cancelled,
                exit_code,
                error: None,
            };
        }
        match last_result {
            Some((false, _)) => Outcome {
                status: RunStatus::Succeeded,
                exit_code,
                error: None,
            },
            Some((true, err)) => Outcome {
                status: RunStatus::Failed,
                exit_code,
                error: err.or_else(|| Some("런타임이 오류로 턴을 끝냈습니다".into())),
            },
            None => {
                let tail = stderr_tail.into_iter().collect::<Vec<_>>().join("\n");
                Outcome {
                    status: RunStatus::Failed,
                    exit_code,
                    error: Some(if tail.is_empty() {
                        format!("런타임이 결과 없이 종료되었습니다 (exit code: {exit_code:?})")
                    } else {
                        tail
                    }),
                }
            }
        }
    }
}

/// stdin에 JSON 한 줄을 쓴다. 실패하면 stdin을 닫힌 것으로 처리한다.
async fn write_line(stdin: &mut Option<ChildStdin>, line: &str) -> bool {
    let Some(w) = stdin.as_mut() else {
        return false;
    };
    let ok = async {
        w.write_all(line.as_bytes()).await?;
        w.write_all(b"\n").await?;
        w.flush().await
    }
    .await
    .is_ok();
    if !ok {
        *stdin = None;
    }
    ok
}

/// 실제 Claude Code CLI를 구동하는 통합 테스트. 모델 사용량이 발생하므로 기본 제외.
/// 실행: `cargo test live_ -- --ignored --nocapture --test-threads=1`
#[cfg(test)]
mod live_tests {
    use super::*;
    use crate::agent::{self, AgentInput, PermissionMode};
    use crate::project;
    use crate::runtime::RunEvent;
    use tauri::test::{MockRuntime, mock_app};

    struct Env {
        _app: tauri::App<MockRuntime>,
        _data: tempfile::TempDir,
        proj: tempfile::TempDir,
        exec: Arc<Executor<MockRuntime>>,
        handle: AppHandle<MockRuntime>,
    }

    fn setup(mode: PermissionMode) -> (Env, i64) {
        let app = mock_app();
        let data = tempfile::tempdir().unwrap();
        let proj = tempfile::tempdir().unwrap();
        app.manage(Db::open(&data.path().join("t.db")).unwrap());
        let handle = app.handle().clone();
        let exec = Executor::new(handle.clone());

        let agent_id = {
            let conn = handle.state::<Db>().inner().conn().unwrap();
            let path = project::normalize_path(proj.path().to_str().unwrap()).unwrap();
            let p = project::upsert(&conn, &path).unwrap();
            settings::set_current_project_id(&conn, p.id).unwrap();
            agent::create(
                &conn,
                &AgentInput {
                    name: "테스터".into(),
                    role: "QA".into(),
                    instructions: String::new(),
                    model: Some("haiku".into()),
                    effort: None,
                    permission_mode: mode,
                    tools: vec!["Read".into(), "Write".into()],
                    color: None,
                },
            )
            .unwrap()
            .id
        };
        (
            Env {
                _app: app,
                _data: data,
                proj,
                exec,
                handle,
            },
            agent_id,
        )
    }

    fn start(env: &Env, agent_id: i64, prompt: &str) -> i64 {
        let run_id = {
            let conn = env.handle.state::<Db>().inner().conn().unwrap();
            let pid = settings::current_project_id(&conn).unwrap().unwrap();
            let p = project::get(&conn, pid).unwrap().unwrap();
            let a = agent::get(&conn, agent_id).unwrap();
            let t = task::create_task(&conn, p.id, a.id, prompt).unwrap();
            task::create_run(
                &conn,
                task::NewRun {
                    task_id: t.id,
                    agent_id: a.id,
                    prompt,
                    resume_session_id: None,
                    config: &crate::commands::snapshot(&p, &a),
                    cwd: &p.path,
                },
            )
            .unwrap()
            .id
        };
        env.exec.pump();
        run_id
    }

    fn run(env: &Env, id: i64) -> task::Run {
        task::get_run(&env.handle.state::<Db>().inner().conn().unwrap(), id).unwrap()
    }

    async fn wait_until(
        env: &Env,
        id: i64,
        secs: u64,
        pred: impl Fn(&task::Run) -> bool,
    ) -> task::Run {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            let r = run(env, id);
            if pred(&r) {
                return r;
            }
            assert!(
                Instant::now() < deadline,
                "시간 초과: 현재 상태 {:?}",
                r.status
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    fn events(env: &Env, id: i64) -> Vec<RunEvent> {
        let conn = env.handle.state::<Db>().inner().conn().unwrap();
        task::list_events(&conn, id)
            .unwrap()
            .into_iter()
            .map(|e| e.event)
            .collect()
    }

    #[test]
    #[ignore]
    fn live_승인후_파일쓰기_성공() {
        let (env, agent_id) = setup(PermissionMode::Default);
        tauri::async_runtime::block_on(async {
            let id = start(
                &env,
                agent_id,
                "Write 도구로 현재 작업 디렉터리에 hello.txt를 만들고 내용은 hi 로 해. 다른 일은 하지 마.",
            );
            wait_until(&env, id, 90, |r| r.status == RunStatus::WaitingForApproval).await;

            let ap = {
                let conn = env.handle.state::<Db>().inner().conn().unwrap();
                task::pending_approvals(&conn, id).unwrap().remove(0)
            };
            assert_eq!(ap.tool_name, "Write");
            assert!(
                !env.proj.path().join("hello.txt").exists(),
                "승인 전에는 파일이 없어야 함"
            );
            env.exec.respond_approval(ap.id, true).unwrap();

            let done = wait_until(&env, id, 90, |r| r.status.is_terminal()).await;
            let evs = events(&env, id);
            for e in &evs {
                println!("{e:?}");
            }
            assert_eq!(done.status, RunStatus::Succeeded, "error: {:?}", done.error);
            assert!(done.session_id.is_some());
            assert_eq!(
                std::fs::read_to_string(env.proj.path().join("hello.txt"))
                    .unwrap()
                    .trim(),
                "hi"
            );
            assert!(evs.iter().any(|e| matches!(e, RunEvent::Started { .. })));
            assert!(
                evs.iter()
                    .any(|e| matches!(e, RunEvent::ApprovalDecided { allowed: true, .. }))
            );
            assert!(evs.iter().any(|e| matches!(
                e,
                RunEvent::TurnResult {
                    is_error: false,
                    ..
                }
            )));
            assert!(
                env.exec.sender(id).is_none(),
                "끝난 Run은 활성 목록에서 제거"
            );
        });
    }

    #[test]
    #[ignore]
    fn live_실행중_중단() {
        let (env, agent_id) = setup(PermissionMode::Default);
        tauri::async_runtime::block_on(async {
            let id = start(
                &env,
                agent_id,
                "1부터 2000까지 숫자를 한 줄에 하나씩 텍스트로만 출력해. 도구는 쓰지 마.",
            );
            wait_until(&env, id, 60, |r| {
                r.session_id.is_some() && r.status == RunStatus::Running
            })
            .await;
            tokio::time::sleep(Duration::from_secs(2)).await;
            env.exec.cancel(id).unwrap();
            let done = wait_until(&env, id, 20, |r| r.status.is_terminal()).await;
            assert_eq!(done.status, RunStatus::Cancelled);
            assert!(
                events(&env, id)
                    .iter()
                    .any(|e| matches!(e, RunEvent::Notice { text } if text.contains("중단")))
            );
        });
    }

    #[test]
    #[ignore]
    fn live_이어서_실행_사용량_이중집계_없음() {
        let (env, agent_id) = setup(PermissionMode::Plan);
        tauri::async_runtime::block_on(async {
            let r1 = start(&env, agent_id, "숫자 하나만 답해: 1+1");
            let done1 = wait_until(&env, r1, 90, |r| r.status.is_terminal()).await;
            assert_eq!(done1.status, RunStatus::Succeeded, "{:?}", done1.error);
            assert_eq!(done1.auth_kind.as_deref(), Some("subscription"));

            // 같은 세션 이어서 실행
            let r2 = {
                let conn = env.handle.state::<Db>().inner().conn().unwrap();
                task::create_run(
                    &conn,
                    task::NewRun {
                        task_id: done1.task_id,
                        agent_id,
                        prompt: "방금 답에 1을 더한 숫자 하나만 답해",
                        resume_session_id: done1.session_id.as_deref(),
                        config: &done1.config,
                        cwd: &done1.cwd,
                    },
                )
                .unwrap()
                .id
            };
            env.exec.pump();
            let done2 = wait_until(&env, r2, 90, |r| r.status.is_terminal()).await;
            assert_eq!(done2.status, RunStatus::Succeeded, "{:?}", done2.error);
            assert_eq!(done2.session_id, done1.session_id);

            let conn = env.handle.state::<Db>().inner().conn().unwrap();
            let per_run = usage::task_usage(&conn, done1.task_id).unwrap();
            let (c1, c2) = (
                per_run[0].totals.cost_usd.unwrap(),
                per_run[1].totals.cost_usd.unwrap(),
            );
            let final_cum: f64 = conn
                .query_row(
                    "SELECT cum_cost_usd FROM usage_records WHERE run_id = ?1 ORDER BY id DESC LIMIT 1",
                    [r2],
                    |r| r.get(0),
                )
                .unwrap();
            println!("run1=${c1:.6} run2=${c2:.6} 세션 누적=${final_cum:.6}");
            println!("run1={:?}\nrun2={:?}", per_run[0].totals, per_run[1].totals);
            assert!(c1 > 0.0 && c2 > 0.0);
            assert!(
                (c1 + c2 - final_cum).abs() < 1e-9,
                "실행별 합 = 세션 누적 (이중 집계 없음)"
            );
            assert_eq!(per_run[0].totals.reported + per_run[1].totals.reported, 2);

            let rl = usage::load_rate_limit(&conn).unwrap();
            println!("구독 한도: {rl:?}");
            assert!(rl.is_some_and(|s| s.observed_at > 0));
        });
    }
}
