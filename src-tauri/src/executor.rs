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
use crate::pm;
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
/// PM 대화 메시지 추가 (payload: pm::Message)
pub const EVENT_PM_MESSAGE: &str = "pm-message";

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
    /// 작업별 worktree 루트 (앱 데이터 폴더 아래, 사용자 레포 밖)
    worktree_root: PathBuf,
}

impl<R: Runtime> Executor<R> {
    pub fn new(app: AppHandle<R>, worktree_root: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            app,
            active: Mutex::new(HashMap::new()),
            pump_lock: Mutex::new(()),
            worktree_root,
        })
    }

    /// 블로킹 작업(git 등)을 비동기 런타임 밖에서 DB와 함께 실행한다.
    async fn blocking<T: Send + 'static>(
        &self,
        f: impl FnOnce(&rusqlite::Connection) -> AppResult<T> + Send + 'static,
    ) -> AppResult<T> {
        let app = self.app.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let db = app.state::<Db>();
            let conn = db.inner().conn()?;
            f(&conn)
        })
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?
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
        // (시작할 Run, 선행 작업 실패로 취소한 Run)
        let to_start = (|| -> AppResult<(Vec<i64>, Vec<i64>)> {
            let conn = self.db().conn()?;
            let max = settings::load(&conn)?.max_concurrency as i64;
            let mut slots = max - task::count_active_runs(&conn)?;
            let (mut start, mut blocked) = (Vec::new(), Vec::new());
            for (run_id, task_id) in task::queued_runs(&conn)? {
                match task::readiness(&conn, task_id)? {
                    task::Readiness::Ready if slots > 0 => {
                        // 다른 pump 호출이 같은 Run을 다시 집지 않도록 즉시 running으로 바꾼다
                        task::set_run_status(&conn, run_id, RunStatus::Running)?;
                        start.push(run_id);
                        slots -= 1;
                    }
                    task::Readiness::Blocked { dependency } => {
                        let msg =
                            format!("선행 작업 #{dependency}이(가) 성공하지 못해 취소했습니다");
                        task::finish_run(&conn, run_id, RunStatus::Cancelled, None, Some(&msg))?;
                        let role = task::get_run(&conn, run_id)?.role;
                        usage::mark_unavailable_if_empty(&conn, run_id, &role)?;
                        blocked.push(run_id);
                    }
                    _ => {}
                }
            }
            Ok((start, blocked))
        })();
        let to_start = to_start.map(|(start, blocked)| {
            for run_id in blocked {
                self.notify_run(run_id);
                let this = Arc::clone(self);
                // 계획의 마지막 작업이면 PM 보고가 필요할 수 있다
                tauri::async_runtime::spawn(async move {
                    this.after_run_finished(run_id).await;
                    this.pump();
                });
            }
            start
        });

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
                usage::mark_unavailable_if_empty(&conn, run_id, &run.role)?;
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
            let this = Arc::clone(self);
            tauri::async_runtime::spawn(async move {
                this.after_run_finished(run_id).await;
                this.pump();
            });
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
        // 격리 작업이면 변경을 Mefice 브랜치에 고정 (diff·병합·PM 보고의 기준)
        let snap = self
            .blocking(move |c| {
                let run = task::get_run(c, run_id)?;
                let t = task::get_task(c, run.task_id)?;
                if t.isolation == task::ISOLATION_WORKTREE {
                    crate::workspace::snapshot(c, &t, run.attempt)?;
                }
                Ok(())
            })
            .await;
        if let Err(e) = snap {
            self.record(
                run_id,
                RunEvent::Notice {
                    text: format!("변경 기록(스냅샷) 실패: {e}"),
                },
            );
        }
        self.notify_run(run_id);
        self.after_run_finished(run_id).await;
        self.pump();
    }

    pub fn emit_message(&self, msg: &pm::Message) {
        let _ = self.app.emit(EVENT_PM_MESSAGE, msg);
    }

    /// PM 턴 결과 → 결정 메시지 기록, 위임 작업 생성
    fn after_pm_turn(
        self: &Arc<Self>,
        run: &task::Run,
        structured: Option<&serde_json::Value>,
        result: Option<&str>,
        is_error: bool,
    ) {
        let fx = self
            .db()
            .conn()
            .and_then(|c| pm::on_pm_turn(&c, run, structured, result, is_error));
        match fx {
            Ok(fx) => {
                for m in &fx.messages {
                    self.emit_message(m);
                }
                if fx.queued {
                    self.pump();
                }
            }
            Err(e) => eprintln!("[executor] PM 결정 처리 실패 run={}: {e}", run.id),
        }
    }

    /// 실행 종료 → PM 쪽 알림, 위임 작업이면 PM에게 결과 보고 (이벤트 기반, 폴링 없음)
    async fn after_run_finished(self: &Arc<Self>, run_id: i64) {
        let fx = self.db().conn().and_then(|c| {
            let run = task::get_run(&c, run_id)?;
            pm::on_run_finished(&c, &run)
        });
        let fx = match fx {
            Ok(fx) => fx,
            Err(e) => {
                eprintln!("[executor] 실행 종료 후처리 실패 run={run_id}: {e}");
                return;
            }
        };
        for m in &fx.messages {
            self.emit_message(m);
        }
        let Some((goal, prompt)) = fx.report else {
            return;
        };
        let delivery = self.db().conn().and_then(|c| {
            let project = crate::project::get(&c, goal.project_id)?
                .ok_or_else(|| AppError::NotFound(format!("프로젝트 #{}", goal.project_id)))?;
            pm::deliver_to_pm(&c, &project, &goal, prompt)
        });
        match delivery {
            Ok(pm::Delivery::Queued) => {} // 호출부에서 pump
            Ok(pm::Delivery::SendTo {
                run_id: pm_run,
                prompt,
            }) => {
                if let Err(e) = self.send_message(pm_run, prompt).await {
                    eprintln!("[executor] PM 보고 전달 실패: {e}");
                }
            }
            Err(e) => {
                let note = self.db().conn().and_then(|c| {
                    pm::add_message(
                        &c,
                        &goal,
                        "system",
                        &format!("PM에게 결과를 전달하지 못했습니다: {e}"),
                        None,
                        None,
                    )
                });
                if let Ok(m) = note {
                    self.emit_message(&m);
                }
            }
        }
    }

    async fn drive_inner(
        self: &Arc<Self>,
        run_id: i64,
        rx: &mut mpsc::UnboundedReceiver<Control>,
    ) -> Outcome {
        let run = match self.db().conn().and_then(|c| task::get_run(&c, run_id)) {
            Ok(r) => r,
            Err(e) => return Outcome::failed(e.to_string()),
        };
        let Some(bin) = resolve_binary("claude") else {
            return Outcome::failed("claude CLI를 찾을 수 없습니다. 설정 > 실행 환경을 확인하세요");
        };
        let mut config = run.config.clone();
        let task_id = run.task_id;
        let root = self.worktree_root.clone();
        let isolated = self
            .blocking(move |c| {
                let t = task::get_task(c, task_id)?;
                if t.isolation != task::ISOLATION_WORKTREE {
                    return Ok(None);
                }
                let path = crate::workspace::prepare(c, &root, &t)?;
                task::set_run_cwd(c, run_id, &path.to_string_lossy())?;
                Ok(Some(path))
            })
            .await;
        let cwd = match isolated {
            Ok(Some(path)) => {
                config.system_append.push_str(
                    "\n\n## 작업 공간\n이 작업은 사용자 저장소와 분리된 git worktree에서 실행됩니다. \
                     변경 사항은 Mefice가 기록하고 사용자가 검토 후 병합하므로 git commit·push·브랜치 변경을 하지 마세요.",
                );
                self.notify_run(run_id);
                path
            }
            Ok(None) => PathBuf::from(&run.cwd),
            Err(e) => return Outcome::failed(format!("작업 공간 준비 실패: {e}")),
        };
        if !cwd.is_dir() {
            return Outcome::failed(format!("작업 폴더가 없습니다: {}", cwd.display()));
        }

        let new_session_id = uuid::Uuid::new_v4().to_string();
        let (session, session_id) = match &run.resume_session_id {
            Some(prev) => (Session::Resume(prev), prev.clone()),
            None => (Session::New(&new_session_id), new_session_id.clone()),
        };
        let mut cmd = claude_code::build_command(&bin, &cwd, &config, session);
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
                                    RunEvent::TurnResult { is_error, result, subtype, usage: turn, session_totals, structured, .. } => {
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
                                        if run.role == "pm" && !cancel_requested {
                                            self.after_pm_turn(&run, structured.as_ref(), result.as_deref(), *is_error);
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
    use crate::pm;
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
        let exec = Executor::new(handle.clone(), data.path().join("worktrees"));

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
                    config: &crate::pm::run_config(&p, &a),
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

    /// 4단계 완료 기준: 목표·범위·완료 조건을 전달하고 결과를 취합한다.
    #[test]
    #[ignore]
    fn live_pm_단일위임과_결과취합() {
        let (env, worker_id) = setup(PermissionMode::AcceptEdits);
        tauri::async_runtime::block_on(async {
            let goal_id = {
                let conn = env.handle.state::<Db>().inner().conn().unwrap();
                // 테스트 비용을 줄이기 위해 PM도 haiku 사용
                let pm = agent::ensure_pm(&conn).unwrap();
                conn.execute("UPDATE agents SET model = 'haiku' WHERE id = ?1", [pm.id])
                    .unwrap();
                let pid = settings::current_project_id(&conn).unwrap().unwrap();
                let p = project::get(&conn, pid).unwrap().unwrap();
                let (goal, _, _) = pm::start_goal(
                    &conn,
                    &p,
                    "작업 디렉터리에 hello.txt를 만들고 내용은 hi 로 해줘. 이 작업은 직접 하지 말고 반드시 팀원에게 위임해.",
                )
                .unwrap();
                goal.id
            };
            env.exec.pump();

            // PM 결과 보고(report)까지 대기. 승인 요청이 오면 기록하고 허용한다.
            let deadline = Instant::now() + Duration::from_secs(240);
            let mut approvals_seen = Vec::new();
            loop {
                let (msgs, pending) = {
                    let conn = env.handle.state::<Db>().inner().conn().unwrap();
                    let msgs = pm::list_messages(&conn, goal_id).unwrap();
                    let mut pending = Vec::new();
                    let mut ids = vec![goal_id];
                    ids.extend(
                        task::list_children(&conn, goal_id)
                            .unwrap()
                            .iter()
                            .map(|t| t.id),
                    );
                    for tid in ids {
                        for r in task::list_runs(&conn, tid).unwrap() {
                            pending.extend(task::pending_approvals(&conn, r.id).unwrap());
                        }
                    }
                    (msgs, pending)
                };
                for ap in pending {
                    approvals_seen.push(ap.tool_name.clone());
                    env.exec.respond_approval(ap.id, true).unwrap();
                }
                let reported = msgs.iter().any(|m| {
                    m.author == "pm" && m.payload.as_ref().is_some_and(|p| p["mode"] == "report")
                });
                let failed = msgs
                    .iter()
                    .any(|m| m.author == "system" && m.text.contains("실패"));
                if reported || failed {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "시간 초과. 메시지: {:#?}",
                    msgs.iter()
                        .map(|m| (&m.author, &m.text))
                        .collect::<Vec<_>>()
                );
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
            // 마지막 PM Run이 끝날 때까지 대기
            let last_pm = {
                let conn = env.handle.state::<Db>().inner().conn().unwrap();
                task::latest_run(&conn, goal_id).unwrap().unwrap().id
            };
            wait_until(&env, last_pm, 60, |r| r.status.is_terminal()).await;

            let conn = env.handle.state::<Db>().inner().conn().unwrap();
            let msgs = pm::list_messages(&conn, goal_id).unwrap();
            for m in &msgs {
                println!("[{}] {}", m.author, m.text.replace('\n', " / "));
            }
            println!("승인 요청된 도구: {approvals_seen:?}");
            let authors: Vec<&str> = msgs.iter().map(|m| m.author.as_str()).collect();
            assert_eq!(authors.first(), Some(&"user"));
            assert!(
                msgs.iter()
                    .any(|m| m.payload.as_ref().is_some_and(|p| p["mode"] == "delegate"))
            );
            assert!(msgs.iter().any(|m| m.text.starts_with("작업 배정")));
            assert!(msgs.iter().any(|m| m.text.starts_with("작업 완료")));
            assert!(
                msgs.last()
                    .unwrap()
                    .payload
                    .as_ref()
                    .is_some_and(|p| p["mode"] == "report")
            );

            let children = task::list_children(&conn, goal_id).unwrap();
            assert_eq!(children.len(), 1);
            assert_eq!(children[0].agent_id, worker_id);
            assert_eq!(
                std::fs::read_to_string(env.proj.path().join("hello.txt"))
                    .unwrap()
                    .trim(),
                "hi"
            );

            let by_role = usage::goal_usage(&conn, goal_id).unwrap();
            for g in &by_role {
                println!("{} → {:?}", g.key, g.totals);
            }
            let roles: Vec<&str> = by_role.iter().map(|g| g.key.as_str()).collect();
            assert!(roles.contains(&"pm") && roles.contains(&"worker"));
            let pm_runs = task::list_runs(&conn, goal_id).unwrap();
            println!("PM 실행 횟수: {}", pm_runs.len());
            assert_eq!(
                pm_runs.len(),
                2,
                "계획 1회 + 취합 1회 (상태 확인 호출 없음)"
            );
            assert_eq!(
                pm_runs[1].resume_session_id, pm_runs[0].session_id,
                "취합은 같은 PM 세션을 이어서"
            );
        });
    }

    /// 프로젝트 폴더를 커밋 하나가 있는 Git 저장소로 만든다.
    fn make_git_repo(env: &Env) {
        let p = env.proj.path();
        crate::git::run(p, &["init", "-q", "-b", "main"]).unwrap();
        std::fs::write(p.join("a.txt"), "original\n").unwrap();
        crate::git::snapshot_commit(p, "init").unwrap();
    }

    fn start_isolated(env: &Env, agent_id: i64, prompt: &str) -> (i64, i64) {
        let conn = env.handle.state::<Db>().inner().conn().unwrap();
        let pid = settings::current_project_id(&conn).unwrap().unwrap();
        let p = project::get(&conn, pid).unwrap().unwrap();
        let a = agent::get(&conn, agent_id).unwrap();
        let t = task::create_task(&conn, p.id, a.id, prompt).unwrap();
        task::set_isolation(&conn, t.id, task::ISOLATION_WORKTREE).unwrap();
        let r = task::create_run(
            &conn,
            task::NewRun {
                task_id: t.id,
                agent_id: a.id,
                prompt,
                resume_session_id: None,
                config: &crate::pm::run_config(&p, &a),
                cwd: &p.path,
            },
        )
        .unwrap();
        (t.id, r.id)
    }

    /// 5단계 완료 기준: 독립적인 두 작업을 병렬 실행하고 파일 충돌을 격리한다.
    #[test]
    #[ignore]
    fn live_병렬_실행과_충돌_격리() {
        let (env, agent_id) = setup(PermissionMode::AcceptEdits);
        make_git_repo(&env);
        let repo = env.proj.path().to_path_buf();
        tauri::async_runtime::block_on(async {
            let (ta, ra) = start_isolated(
                &env,
                agent_id,
                "Write 도구로 a.txt의 내용을 AAA 한 줄로 바꿔. 다른 일은 하지 마.",
            );
            let (tb, rb) = start_isolated(
                &env,
                agent_id,
                "Write 도구로 a.txt의 내용을 BBB 한 줄로 바꿔. 다른 일은 하지 마.",
            );
            env.exec.pump(); // 동시 실행 상한 기본값 2

            let a = wait_until(&env, ra, 120, |r| r.status.is_terminal()).await;
            let b = wait_until(&env, rb, 120, |r| r.status.is_terminal()).await;
            assert_eq!(a.status, RunStatus::Succeeded, "{:?}", a.error);
            assert_eq!(b.status, RunStatus::Succeeded, "{:?}", b.error);
            let overlap = a.started_at.unwrap() < b.ended_at.unwrap()
                && b.started_at.unwrap() < a.ended_at.unwrap();
            println!(
                "A {:?}~{:?} / B {:?}~{:?} → 병렬: {overlap}",
                a.started_at, a.ended_at, b.started_at, b.ended_at
            );
            assert!(overlap, "두 작업의 실행 구간이 겹쳐야 함");
            assert_ne!(a.cwd, b.cwd, "작업마다 다른 작업 공간");
            assert_eq!(
                std::fs::read_to_string(repo.join("a.txt")).unwrap(),
                "original\n",
                "실행 중 사용자 레포는 그대로"
            );

            let conn = env.handle.state::<Db>().inner().conn().unwrap();
            let da = crate::workspace::diff(&conn, ta).unwrap();
            let db_ = crate::workspace::diff(&conn, tb).unwrap();
            println!("A 변경: {:?} / B 변경: {:?}", da.files, db_.files);
            assert!(
                da.can_merge && db_.can_merge,
                "병합 전에는 둘 다 각각 병합 가능"
            );

            crate::workspace::merge(&conn, ta).unwrap();
            assert_eq!(
                std::fs::read_to_string(repo.join("a.txt")).unwrap().trim(),
                "AAA"
            );

            let db2 = crate::workspace::diff(&conn, tb).unwrap();
            assert_eq!(db2.conflicts, vec!["a.txt"], "A 병합 후 B는 충돌로 표시");
            assert!(crate::workspace::merge(&conn, tb).is_err());
            assert_eq!(
                std::fs::read_to_string(repo.join("a.txt")).unwrap().trim(),
                "AAA",
                "충돌 시 레포 변경 없음"
            );
            assert!(crate::git::dirty_files(&repo).unwrap().is_empty());
            crate::workspace::discard(&conn, tb).unwrap();
        });
    }

    /// PM이 독립 작업 두 개로 나눠 위임하고, 둘 다 끝난 뒤 한 번만 취합한다.
    #[test]
    #[ignore]
    fn live_pm_병렬_분할과_묶음_보고() {
        let (env, _worker) = setup(PermissionMode::AcceptEdits);
        make_git_repo(&env);
        tauri::async_runtime::block_on(async {
            let goal_id = {
                let conn = env.handle.state::<Db>().inner().conn().unwrap();
                let pm = agent::ensure_pm(&conn).unwrap();
                conn.execute("UPDATE agents SET model = 'haiku' WHERE id = ?1", [pm.id])
                    .unwrap();
                let pid = settings::current_project_id(&conn).unwrap().unwrap();
                let p = project::get(&conn, pid).unwrap().unwrap();
                pm::start_goal(
                    &conn,
                    &p,
                    "x.txt(내용 x)와 y.txt(내용 y)를 만드는 일을 서로 독립된 두 작업으로 나눠 팀원에게 동시에 위임해. 직접 하지 마.",
                )
                .unwrap()
                .0
                .id
            };
            env.exec.pump();

            let deadline = Instant::now() + Duration::from_secs(300);
            loop {
                let msgs = {
                    let conn = env.handle.state::<Db>().inner().conn().unwrap();
                    pm::list_messages(&conn, goal_id).unwrap()
                };
                if msgs.iter().any(|m| {
                    m.author == "pm" && m.payload.as_ref().is_some_and(|p| p["mode"] == "report")
                }) {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "시간 초과: {:#?}",
                    msgs.iter()
                        .map(|m| (&m.author, &m.text))
                        .collect::<Vec<_>>()
                );
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
            let last_pm = {
                let conn = env.handle.state::<Db>().inner().conn().unwrap();
                task::latest_run(&conn, goal_id).unwrap().unwrap().id
            };
            wait_until(&env, last_pm, 60, |r| r.status.is_terminal()).await;

            let conn = env.handle.state::<Db>().inner().conn().unwrap();
            for m in pm::list_messages(&conn, goal_id).unwrap() {
                println!("[{}] {}", m.author, m.text.replace('\n', " / "));
            }
            let kids = task::list_children(&conn, goal_id).unwrap();
            assert_eq!(kids.len(), 2, "두 작업으로 분할");
            assert!(kids.iter().all(|k| k.isolation == task::ISOLATION_WORKTREE));
            assert!(
                kids.iter()
                    .all(|k| task::dependencies(&conn, k.id).unwrap().is_empty()),
                "독립 작업"
            );
            let pm_runs = task::list_runs(&conn, goal_id).unwrap();
            assert_eq!(pm_runs.len(), 2, "계획 1회 + 묶음 취합 1회");
            for k in &kids {
                let d = crate::workspace::diff(&conn, k.id).unwrap();
                println!(
                    "작업 #{} 변경: {:?}",
                    k.id,
                    d.files.iter().map(|f| &f.path).collect::<Vec<_>>()
                );
                assert!(d.can_merge);
                crate::workspace::merge(&conn, k.id).unwrap();
            }
            let repo = env.proj.path();
            assert!(
                repo.join("x.txt").exists() && repo.join("y.txt").exists(),
                "두 결과 모두 병합"
            );
        });
    }
}
