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
/// 글자 조각 (저장하지 않음, payload: {runId, taskId, text})
pub const EVENT_RUN_DELTA: &str = "run-delta";
/// 진행 상태 (저장하지 않음, payload: {runId, taskId, activity})
pub const EVENT_RUN_ACTIVITY: &str = "run-activity";
/// 런타임 슬래시 명령 목록 갱신 (payload: SlashCommand[])
pub const EVENT_COMMANDS: &str = "runtime-commands";

/// 실행 중인 Run에 보내는 제어 메시지
enum Control {
    UserMessage(
        String,
        Vec<crate::runtime::ImageAttachment>,
        oneshot::Sender<AppResult<()>>,
    ),
    /// 승인 응답 (인코딩된 한 줄)
    Permission(String),
    /// Run 종료 (중단)
    Cancel,
    /// 현재 턴만 멈춘다. 대화 프로세스는 유지 (Esc)
    Interrupt,
    /// 대화 중 모드 전환 (default | acceptEdits | plan)
    SetMode(String),
    /// 대화 중 모델 전환
    SetModel(String),
}

/// 승인 결정 (스펙 A단계 1번: 이번만 / 이 세션 동안 / 거부)
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allow,
    AllowSession,
    Deny,
}

/// 대화가 입력 없이 이만큼 지나면 프로세스를 닫는다 (다음 메시지는 세션을 이어서 새 Run)
const CHAT_IDLE_TIMEOUT: Duration = Duration::from_secs(15 * 60);

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
    /// 마지막으로 받은 런타임 슬래시 명령 목록
    commands: Mutex<Vec<crate::runtime::SlashCommand>>,
}

impl<R: Runtime> Executor<R> {
    pub fn new(app: AppHandle<R>, worktree_root: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            app,
            active: Mutex::new(HashMap::new()),
            pump_lock: Mutex::new(()),
            worktree_root,
            commands: Mutex::new(Vec::new()),
        })
    }

    /// 진행 중인 실행 수 (종료 확인용)
    pub fn active_count(&self) -> usize {
        self.active.lock().map(|a| a.len()).unwrap_or(0)
    }

    /// 모든 실행에 중단을 요청한다 (앱 종료 확인 후)
    pub fn cancel_all(self: &Arc<Self>) {
        let ids: Vec<i64> = self
            .active
            .lock()
            .map(|a| a.keys().copied().collect())
            .unwrap_or_default();
        for id in ids {
            let _ = self.cancel(id);
        }
    }

    /// 앱 종료 직전: 남은 런타임 프로세스 그룹을 정리한다 (kill_on_drop은 직계 자식만 끈다)
    pub fn kill_remaining(&self) {
        let Ok(conn) = self.db().conn() else { return };
        let pids: Vec<i64> = conn
            .prepare("SELECT pid FROM runs WHERE pid IS NOT NULL")
            .and_then(|mut s| s.query_map([], |r| r.get(0))?.collect())
            .unwrap_or_default();
        for pid in pids {
            crate::sysproc::kill_tree(pid as u32);
        }
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
        self.send_message_with(run_id, text, vec![]).await
    }

    /// 런타임이 제공하는 슬래시 명령 (처음 실행 전에는 빈 목록)
    pub fn slash_commands(&self) -> Vec<crate::runtime::SlashCommand> {
        self.commands.lock().map(|c| c.clone()).unwrap_or_default()
    }

    /// 대화: 실행 중인 프로세스의 모델을 바꾼다
    pub fn set_model(&self, run_id: i64, model: &str) -> AppResult<()> {
        if let Some(tx) = self.sender(run_id) {
            let _ = tx.send(Control::SetModel(model.to_string()));
        }
        Ok(())
    }

    pub async fn send_message_with(
        &self,
        run_id: i64,
        text: String,
        images: Vec<crate::runtime::ImageAttachment>,
    ) -> AppResult<()> {
        crate::runtime::validate_images(&images)?;
        if text.trim().is_empty() && images.is_empty() {
            return Err(AppError::InvalidInput("메시지를 입력하세요".into()));
        }
        let tx = self.sender(run_id).ok_or_else(|| {
            AppError::InvalidInput("실행이 이미 끝났습니다. '이어서 실행'으로 지시하세요".into())
        })?;
        let (reply_tx, reply_rx) = oneshot::channel();
        tx.send(Control::UserMessage(text, images, reply_tx))
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
        self.decide(
            approval_id,
            if allow {
                Decision::Allow
            } else {
                Decision::Deny
            },
            None,
            None,
        )
    }

    /// 승인·질문·계획 요청에 답한다.
    /// - `answers`: 질문 도구 답 ({질문: 답})
    /// - `message`: 거부 사유 (런타임에 전달되어 다음 행동에 반영)
    pub fn decide(
        &self,
        approval_id: i64,
        decision: Decision,
        answers: Option<Value>,
        message: Option<String>,
    ) -> AppResult<()> {
        let allow = decision != Decision::Deny;
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
        let line = match decision {
            Decision::Deny => claude_code::encode_deny(
                &approval.request_id,
                message
                    .as_deref()
                    .filter(|m| !m.trim().is_empty())
                    .unwrap_or("사용자가 거부했습니다."),
            ),
            _ => {
                let input = match &answers {
                    Some(a) => claude_code::with_answers(&approval.tool_input, a),
                    None => approval.tool_input.clone(),
                };
                let perms = if decision == Decision::AllowSession {
                    claude_code::session_permissions(&approval.suggestions)
                } else {
                    vec![]
                };
                claude_code::encode_allow(&approval.request_id, &input, &perms)
            }
        };
        let tx = self
            .sender(approval.run_id)
            .ok_or_else(|| AppError::InvalidInput("실행이 이미 끝났습니다".into()))?;
        let _ = tx.send(Control::Permission(line));
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

    /// 대화: 현재 턴만 멈춘다 (프로세스 유지)
    pub fn interrupt_turn(&self, run_id: i64) -> AppResult<()> {
        let tx = self
            .sender(run_id)
            .ok_or_else(|| AppError::InvalidInput("실행 중이 아닙니다".into()))?;
        let _ = tx.send(Control::Interrupt);
        Ok(())
    }

    /// 대화: 실행 중인 프로세스의 모드를 바꾼다
    pub fn set_mode(&self, run_id: i64, mode: &str) -> AppResult<()> {
        if let Some(tx) = self.sender(run_id) {
            let _ = tx.send(Control::SetMode(mode.to_string()));
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
            // 작업자 보고의 "기억할 점"을 업무 기억에 반영 (모델 호출 없음)
            if outcome.status == RunStatus::Succeeded
                && let Ok(run) = task::get_run(&conn, run_id)
                && crate::agent::get(&conn, run.agent_id)
                    .is_ok_and(|a| a.kind == crate::agent::KIND_WORKER)
                && let Ok(Some(report)) = task::last_result_text(&conn, run_id)
            {
                let _ = crate::memory::absorb(&conn, run.agent_id, &report);
            }
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

    fn shadow(&self, project_id: i64, cwd: &str) -> crate::checkpoint::Shadow {
        let root = self
            .worktree_root
            .parent()
            .map(|p| p.join("shadow"))
            .unwrap_or_else(|| self.worktree_root.join("shadow"));
        crate::checkpoint::Shadow::new(&root, project_id, std::path::Path::new(cwd))
    }

    /// 대화 응답 전 상태 기록 (실패해도 대화는 계속)
    async fn checkpoint_before(&self, run_id: i64, project_id: i64, cwd: &str) -> Option<String> {
        let shadow = self.shadow(project_id, cwd);
        let r = tauri::async_runtime::spawn_blocking(move || shadow.snapshot("응답 전")).await;
        match r {
            Ok(Ok(c)) => Some(c),
            Ok(Err(e)) => {
                self.record(
                    run_id,
                    RunEvent::Notice {
                        text: format!("되돌리기 기록을 남기지 못했습니다: {e}"),
                    },
                );
                None
            }
            Err(_) => None,
        }
    }

    /// 대화 응답 후 상태 기록 → 바뀐 파일이 있으면 되돌리기 항목을 남긴다
    async fn checkpoint_after(&self, run: &task::Run, project_id: i64, before: String) {
        let shadow = self.shadow(project_id, &run.cwd);
        let b = before.clone();
        let r =
            tauri::async_runtime::spawn_blocking(move || -> AppResult<(String, Vec<String>)> {
                let after = shadow.snapshot("응답 후")?;
                let files = shadow.changed(&b, &after)?;
                Ok((after, files))
            })
            .await;
        let Ok(Ok((after, files))) = r else { return };
        if files.is_empty() {
            return;
        }
        let id = self.db().conn().and_then(|c| {
            crate::checkpoint::insert(&c, run.task_id, run.id, project_id, &before, &after, &files)
        });
        if let Ok(id) = id {
            self.record(
                run.id,
                RunEvent::Checkpoint {
                    checkpoint_id: id,
                    files: crate::checkpoint::listed(&files),
                    total: files.len() as u32,
                },
            );
        }
    }

    /// 대화 응답을 응답 전 상태로 되돌린다 (사용자가 버튼을 누를 때만)
    pub async fn restore_checkpoint(
        &self,
        checkpoint_id: i64,
    ) -> AppResult<crate::checkpoint::RestoreResult> {
        let cp = {
            let c = self.db().conn()?;
            let cp = crate::checkpoint::get(&c, checkpoint_id)?;
            if cp.restored_at.is_some() {
                return Err(AppError::InvalidInput("이미 되돌린 응답입니다".into()));
            }
            let busy = task::latest_run(&c, cp.task_id)?.is_some_and(|r| {
                matches!(
                    r.status,
                    RunStatus::Queued
                        | RunStatus::Running
                        | RunStatus::WaitingForApproval
                        | RunStatus::Cancelling
                )
            });
            if busy {
                return Err(AppError::InvalidInput(
                    "응답이 끝난 뒤에 되돌릴 수 있습니다".into(),
                ));
            }
            cp
        };
        let run = self
            .db()
            .conn()
            .and_then(|c| task::get_run(&c, cp.run_id))?;
        let shadow = self.shadow(cp.project_id, &run.cwd);
        let (b, a) = (cp.before_commit.clone(), cp.after_commit.clone());
        let result = tauri::async_runtime::spawn_blocking(move || shadow.restore(&b, &a))
            .await
            .map_err(|e| AppError::Internal(e.to_string()))??;
        self.db()
            .conn()
            .and_then(|c| crate::checkpoint::mark_restored(&c, checkpoint_id))?;
        self.record(
            cp.run_id,
            RunEvent::Restored {
                checkpoint_id,
                restored: result.restored.len() as u32,
                skipped: result.skipped.clone(),
            },
        );
        Ok(result)
    }

    fn emit_activity(&self, task_id: i64, run_id: i64, activity: &str) {
        let _ = self.app.emit(
            EVENT_RUN_ACTIVITY,
            serde_json::json!({"runId": run_id, "taskId": task_id, "activity": activity}),
        );
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
            .kill_on_drop(true)
            // 새 프로세스 그룹: 강제 종료·앱 재시작 정리 시 도구가 띄운 명령까지 함께 끈다
            .process_group(0);

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

        let images = self
            .db()
            .conn()
            .and_then(|c| task::take_run_attachments(&c, run_id))
            .unwrap_or_default();
        // 대화: 응답 전 폴더 상태 기록 (되돌리기용)
        let chat_project = self
            .db()
            .conn()
            .and_then(|c| task::get_task(&c, run.task_id))
            .ok()
            .filter(|t| t.mode == crate::chat::MODE_CHAT)
            .map(|t| t.project_id);
        let mut turn_before = match chat_project {
            Some(pid) => {
                self.checkpoint_before(run_id, pid, &cwd.to_string_lossy())
                    .await
            }
            None => None,
        };
        self.record(
            run_id,
            RunEvent::UserMessage {
                text: run.prompt.clone(),
                images: images.len() as u32,
            },
        );
        write_line(&mut stdin, &claude_code::encode_initialize()).await;
        write_line(
            &mut stdin,
            &claude_code::encode_user_content(&run.prompt, &images),
        )
        .await;

        let mut current_session = session_id.clone();
        let mut pending_turns: u32 = 1;
        let mut cancel_requested = false;
        let mut kill_deadline: Option<Instant> = None;
        // 시간 제한 (스펙 4.3). 넘으면 중단 절차를 밟고 결과는 실패로 기록한다.
        let mut time_limit: Option<Instant> = config
            .limits
            .max_minutes
            .map(|m| Instant::now() + Duration::from_secs(u64::from(m) * 60));
        let mut limit_hit: Option<String> = None;
        let mut last_result: Option<(bool, Option<String>)> = None;
        let mut stderr_open = true;
        let mut stderr_events = 0usize;
        let mut stderr_tail: VecDeque<String> = VecDeque::new();
        // 대화: 다음 입력을 기다리는 중이면 유휴 종료 시각
        let mut idle_deadline: Option<Instant> = None;
        let mut interrupt_seq = 0u32;

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
                                        if pending_turns == 0
                                            && let (Some(pid), Some(before)) = (chat_project, turn_before.take())
                                        {
                                            let mut r = run.clone();
                                            r.cwd = cwd.to_string_lossy().to_string();
                                            self.checkpoint_after(&r, pid, before).await;
                                        }
                                        let err_text = is_error.then(|| {
                                            claude_code::describe_error(subtype, &config.limits)
                                                .or_else(|| result.clone())
                                                .unwrap_or_else(|| subtype.clone())
                                        });
                                        last_result = Some((*is_error, err_text));
                                        if pending_turns == 0 {
                                            if config.keep_alive && !cancel_requested && stdin.is_some() {
                                                // 대화: 프로세스를 유지하고 다음 메시지를 기다린다 (터미널과 같은 동작)
                                                idle_deadline = Some(Instant::now() + CHAT_IDLE_TIMEOUT);
                                                self.set_status(run_id, RunStatus::WaitingForInput);
                                            } else {
                                                stdin = None; // stdin 닫기 → 런타임 정상 종료
                                            }
                                        }
                                        self.emit_activity(run.task_id, run_id, "idle");
                                        if run.role == "pm" && !cancel_requested {
                                            self.after_pm_turn(&run, structured.as_ref(), result.as_deref(), *is_error);
                                        }
                                    }
                                    _ => {}
                                }
                                self.record(run_id, ev);
                            }
                            Parsed::TextDelta(text) => {
                                let _ = self.app.emit(EVENT_RUN_DELTA, serde_json::json!({"runId": run_id, "taskId": run.task_id, "text": text}));
                            }
                            Parsed::Activity(a) => self.emit_activity(run.task_id, run_id, &a),
                            Parsed::Commands(list) => {
                                if let Ok(mut c) = self.commands.lock() {
                                    *c = list.clone();
                                }
                                let _ = self.app.emit(EVENT_COMMANDS, &list);
                            }
                            Parsed::PermissionRequest { request_id, tool_name, input, description, suggestions } => {
                                if cancel_requested {
                                    write_line(&mut stdin, &claude_code::encode_permission_response(&request_id, false, &input)).await;
                                    continue;
                                }
                                let created = self.db().conn().and_then(|c| task::create_approval(
                                    &c, run_id, &request_id, &tool_name, &input, description.as_deref(), &suggestions,
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
                        Some(Control::UserMessage(text, images, reply)) => {
                            if cancel_requested || stdin.is_none() {
                                let _ = reply.send(Err(AppError::InvalidInput("실행이 끝나는 중입니다. '이어서 실행'으로 지시하세요".into())));
                            } else {
                                // 쉬던 대화에 새 메시지 → 새 응답의 "전" 상태 기록 (되돌리기용)
                                if pending_turns == 0 && turn_before.is_none()
                                    && let Some(pid) = chat_project
                                {
                                    turn_before = self.checkpoint_before(run_id, pid, &cwd.to_string_lossy()).await;
                                }
                                if write_line(&mut stdin, &claude_code::encode_user_content(&text, &images)).await {
                                    pending_turns += 1;
                                    self.record(run_id, RunEvent::UserMessage { text, images: images.len() as u32 });
                                    if idle_deadline.take().is_some() {
                                        self.set_status(run_id, RunStatus::Running);
                                    }
                                    let _ = reply.send(Ok(()));
                                } else {
                                    let _ = reply.send(Err(AppError::Internal("런타임에 메시지를 전달하지 못했습니다".into())));
                                }
                            }
                        }
                        Some(Control::Permission(line)) => {
                            write_line(&mut stdin, &line).await;
                        }
                        Some(Control::Interrupt) if !cancel_requested && idle_deadline.is_none() => {
                            // 턴만 멈춘다: 대기 승인은 거부로 답하고 interrupt를 보낸다. stdin은 유지 → 턴 결과 후 입력 대기
                            let pending = self.db().conn().and_then(|c| task::pending_approvals(&c, run_id)).unwrap_or_default();
                            for ap in pending {
                                if let Ok(c) = self.db().conn() { let _ = task::decide_approval(&c, ap.id, false); }
                                write_line(&mut stdin, &claude_code::encode_deny(&ap.request_id, "사용자가 응답을 멈췄습니다.")).await;
                            }
                            interrupt_seq += 1;
                            write_line(&mut stdin, &claude_code::encode_interrupt(&format!("mefice-turn-{interrupt_seq}"))).await;
                            self.record(run_id, RunEvent::Notice { text: "응답을 멈췄습니다".into() });
                        }
                        Some(Control::SetModel(model)) => {
                            write_line(&mut stdin, &claude_code::encode_set_model(&format!("mefice-model-{model}"), &model)).await;
                        }
                        Some(Control::SetMode(mode)) => {
                            write_line(&mut stdin, &claude_code::encode_set_mode(&format!("mefice-mode-{mode}"), &mode)).await;
                        }
                        Some(Control::Interrupt) => {}
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
                _ = tokio::time::sleep_until(time_limit.unwrap_or_else(Instant::now)), if time_limit.is_some() && !cancel_requested => {
                    time_limit = None;
                    let mins = config.limits.max_minutes.unwrap_or(0);
                    let msg = format!("시간 제한({mins}분)에 도달해 중단했습니다");
                    self.record(run_id, RunEvent::Notice { text: msg.clone() });
                    limit_hit = Some(msg);
                    // 사용자 중단과 같은 절차(대기 승인 거부 → interrupt → stdin 닫기 → 유예 후 강제 종료)를 재사용
                    if let Some(tx) = self.sender(run_id) {
                        let _ = tx.send(Control::Cancel);
                    }
                }
                _ = tokio::time::sleep_until(idle_deadline.unwrap_or_else(Instant::now)), if idle_deadline.is_some() => {
                    // 대화가 오래 쉬면 프로세스를 닫는다. 다음 메시지는 같은 세션을 이어서 새 Run으로 시작
                    idle_deadline = None;
                    stdin = None;
                }
                _ = tokio::time::sleep_until(kill_deadline.unwrap_or_else(Instant::now)), if kill_deadline.is_some() => {
                    kill_deadline = None;
                    self.record(run_id, RunEvent::Notice { text: "응답이 없어 프로세스를 강제 종료합니다".into() });
                    // 도구 명령은 별도 프로세스 그룹으로 뜨므로 프로세스 트리째 종료
                    if let Some(pid) = child.id() {
                        crate::sysproc::kill_tree(pid);
                    }
                    let _ = child.start_kill();
                }
            }
        }

        let exit_code = child.wait().await.ok().and_then(|s| s.code());

        if let Some(msg) = limit_hit {
            // 제한 초과는 사용자 중단이 아니라 실패로 남겨 PM이 실패 보고를 받게 한다
            return Outcome {
                status: RunStatus::Failed,
                exit_code,
                error: Some(msg),
            };
        }
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
                    profile: Default::default(),
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

    /// 6단계: 턴 제한(--max-turns)에 걸리면 실패로 끝나고 읽을 수 있는 사유를 남긴다.
    #[test]
    #[ignore]
    fn live_턴_제한() {
        let (env, agent_id) = setup(PermissionMode::AcceptEdits);
        {
            let conn = env.handle.state::<Db>().inner().conn().unwrap();
            let a = agent::get(&conn, agent_id).unwrap();
            let mut p = a.profile.clone();
            p.limits.max_turns = Some(1);
            agent::update(
                &conn,
                agent_id,
                &AgentInput {
                    name: a.name,
                    role: a.role,
                    instructions: a.instructions,
                    model: a.model,
                    effort: a.effort,
                    permission_mode: a.permission_mode,
                    tools: a.tools,
                    color: None,
                    profile: p,
                },
            )
            .unwrap();
        }
        std::fs::write(env.proj.path().join("a.txt"), "a\n").unwrap();
        tauri::async_runtime::block_on(async {
            let id = start(
                &env,
                agent_id,
                "a.txt를 Read로 읽고, 그다음 b.txt를 Write로 만들어 같은 내용을 써.",
            );
            let done = wait_until(&env, id, 90, |r| r.status.is_terminal()).await;
            println!("상태 {:?} / 사유 {:?}", done.status, done.error);
            assert_eq!(done.status, RunStatus::Failed);
            assert!(done.error.as_deref().unwrap_or("").contains("턴 제한(1회)"));
        });
    }

    /// 강제 종료 재현용 자식: 지정한 DB·폴더로 실제 실행을 시작하고 승인만 처리하며 기다린다.
    /// 부모 테스트(`live_앱_강제종료_후_복구`)가 이 프로세스를 SIGKILL한다. 단독 실행하지 않는다.
    #[test]
    #[ignore]
    fn live_crash_child() {
        let (Ok(db_path), Ok(proj)) = (
            std::env::var("MEFICE_CRASH_DB"),
            std::env::var("MEFICE_CRASH_PROJ"),
        ) else {
            return;
        };
        let app = mock_app();
        app.manage(Db::open(std::path::Path::new(&db_path)).unwrap());
        let handle = app.handle().clone();
        let exec = Executor::new(
            handle.clone(),
            std::path::PathBuf::from(&db_path).with_extension("wt"),
        );
        {
            let conn = handle.state::<Db>().inner().conn().unwrap();
            let p = project::upsert(&conn, &project::normalize_path(&proj).unwrap()).unwrap();
            settings::set_current_project_id(&conn, p.id).unwrap();
            let a = agent::create(
                &conn,
                &AgentInput {
                    name: "t".into(),
                    role: "r".into(),
                    instructions: String::new(),
                    model: Some("haiku".into()),
                    effort: None,
                    permission_mode: PermissionMode::AcceptEdits,
                    tools: vec!["Bash".into()],
                    color: None,
                    profile: Default::default(),
                },
            )
            .unwrap();
            let prompt = "Bash 도구로 sleep 45 를 실행하고, 끝나면 done 이라고 답해.";
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
            .unwrap();
        }
        exec.pump();
        tauri::async_runtime::block_on(async {
            loop {
                let pending: Vec<i64> = {
                    let c = handle.state::<Db>().inner().conn().unwrap();
                    c.prepare("SELECT id FROM approvals WHERE status = 'pending'")
                        .unwrap()
                        .query_map([], |r| r.get(0))
                        .unwrap()
                        .collect::<Result<_, _>>()
                        .unwrap()
                };
                for id in pending {
                    let _ = exec.respond_approval(id, true);
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        });
    }

    /// 7단계 완료 기준: 앱이 실행 도중 강제 종료되어도, 재시작하면 남은 런타임·도구 명령을 정리하고
    /// 실행 상태를 실제와 일치시킨다 (스펙 9·13장).
    #[test]
    #[ignore]
    fn live_앱_강제종료_후_복구() {
        let data = tempfile::tempdir().unwrap();
        let proj = tempfile::tempdir().unwrap();
        let db_path = data.path().join("crash.db");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "executor::live_tests::live_crash_child",
                "--nocapture",
            ])
            .env("MEFICE_CRASH_DB", &db_path)
            .env("MEFICE_CRASH_PROJ", proj.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        // 테스트가 중간에 실패해도 자식(과 그 런타임 트리)을 남기지 않는다
        struct KillOnDrop(u32);
        impl Drop for KillOnDrop {
            fn drop(&mut self) {
                crate::sysproc::kill_tree(self.0);
            }
        }
        let _guard = KillOnDrop(child.id());

        // 런타임이 sleep 도구 명령을 시작할 때까지 대기
        let deadline = Instant::now() + Duration::from_secs(120);
        let (runtime_pid, session) = loop {
            if Instant::now() > deadline {
                // 진단: 자식 실행이 무엇을 했는지 남긴다
                if let Ok(db) = Db::open(&db_path) {
                    let c = db.conn().unwrap();
                    let runs: Vec<(i64, String, Option<String>)> = c
                        .prepare("SELECT id, status, error FROM runs")
                        .unwrap()
                        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                        .unwrap()
                        .collect::<Result<_, _>>()
                        .unwrap();
                    eprintln!("[진단] runs: {runs:?}");
                    for (id, _, _) in &runs {
                        for e in task::list_events(&c, *id).unwrap() {
                            eprintln!("[진단] {:?}", e.event);
                        }
                    }
                }
                panic!("도구 명령이 시작되지 않음");
            }
            std::thread::sleep(Duration::from_millis(500));
            let Ok(db) = Db::open(&db_path) else { continue };
            let conn = db.conn().unwrap();
            let row: Option<(i64, String)> = conn
                .query_row(
                    "SELECT pid, session_id FROM runs WHERE pid IS NOT NULL",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .ok();
            if let Some((pid, sess)) = row {
                // 도구 명령은 claude와 다른 프로세스 그룹이므로 프로세스 트리에서 찾는다
                let tool_started =
                    crate::sysproc::process_tree(pid as u32)
                        .iter()
                        .any(|&(p, _)| {
                            crate::sysproc::process_command(p)
                                .is_some_and(|c| c.contains("sleep 45"))
                        });
                if tool_started {
                    break (pid as u32, sess);
                }
            }
        };

        // 강제 종료 전 런타임 프로세스 트리 기록 (런타임 + 도구 셸 + sleep)
        let tree: Vec<u32> = crate::sysproc::process_tree(runtime_pid)
            .into_iter()
            .map(|(p, _)| p)
            .collect();
        println!("런타임 트리 pid: {tree:?}");
        assert!(tree.len() >= 2, "런타임과 도구 명령이 트리에 있어야 함");

        // 앱 강제 종료 재현
        unsafe { libc::kill(child.id() as i32, libc::SIGKILL) };
        let _ = child.wait();
        std::thread::sleep(Duration::from_millis(500));
        let alive = crate::sysproc::process_command(runtime_pid);
        println!(
            "강제 종료 직후 런타임 생존: {:?}",
            alive.as_deref().map(|c| &c[..c.len().min(60)])
        );
        assert!(
            alive.is_some_and(|c| c.contains(&session)),
            "고아 런타임이 남아 있어야 재현이 된 것"
        );

        // 재시작 복구
        let db = Db::open(&db_path).unwrap();
        let conn = db.conn().unwrap();
        crate::recover_on_startup(&conn).unwrap();
        std::thread::sleep(Duration::from_millis(500));

        // 기록해 둔 트리의 모든 프로세스가 사라졌는지 (launchd가 거두는 시간을 고려해 재시도)
        let deadline = Instant::now() + Duration::from_secs(3);
        let survivors = loop {
            let alive: Vec<String> = tree
                .iter()
                .filter_map(|&p| {
                    crate::sysproc::process_command(p)
                        .map(|c| format!("{p}: {}", &c[..c.len().min(60)]))
                })
                .collect();
            if alive.is_empty() || Instant::now() > deadline {
                break alive;
            }
            std::thread::sleep(Duration::from_millis(200));
        };
        assert!(survivors.is_empty(), "남은 프로세스: {survivors:?}");
        let run = conn
            .query_row("SELECT status FROM runs LIMIT 1", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap();
        assert_eq!(run, "interrupted");
        println!("복구 후: 런타임 종료, 실행 상태 {run}");
    }

    /// v0.2 A단계 1번: 대화 화면의 핵심 흐름을 실제 CLI(표준 모드 = 사용자 설정 그대로)로 검증한다.
    #[test]
    #[ignore]
    fn live_대화_흐름() {
        use crate::chat::{self, ChatOptions};
        use tauri::Listener;

        let (env, _agent) = setup(PermissionMode::Default);
        let deltas = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let d2 = deltas.clone();
        env.handle.listen_any(EVENT_RUN_DELTA, move |_| {
            d2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        let conn_of = || env.handle.state::<Db>().inner().conn().unwrap();
        let proj = || {
            let c = conn_of();
            let pid = settings::current_project_id(&c).unwrap().unwrap();
            project::get(&c, pid).unwrap().unwrap()
        };

        tauri::async_runtime::block_on(async {
            let opts = ChatOptions {
                model: Some("haiku".into()),
                ..Default::default()
            };
            let (chat_id, r1) = {
                let p = proj();
                let c = conn_of();
                let (t, r) = chat::start(&c, &p, "숫자 하나만 답해: 2+3", &opts, &[]).unwrap();
                (t.id, r.id)
            };
            env.exec.pump();
            let idle = |r: &task::Run| r.status == RunStatus::WaitingForInput;
            wait_until(&env, r1, 120, idle).await;
            assert!(
                deltas.load(std::sync::atomic::Ordering::SeqCst) > 0,
                "글자 조각이 와야 함"
            );

            // 1) 두 번째 메시지는 같은 프로세스로
            env.exec
                .send_message(r1, "방금 답에 10을 더한 숫자만 답해".into())
                .await
                .unwrap();
            wait_until(&env, r1, 120, |r| r.status == RunStatus::Running).await;
            wait_until(&env, r1, 120, idle).await;
            assert_eq!(
                task::list_runs(&conn_of(), chat_id).unwrap().len(),
                1,
                "같은 Run(프로세스) 유지"
            );

            // 2) 응답 도중 멈추기 → 대화 유지
            env.exec
                .send_message(
                    r1,
                    "1부터 300까지 한 줄에 하나씩 텍스트로만 출력해. 도구는 쓰지 마.".into(),
                )
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_secs(3)).await;
            env.exec.interrupt_turn(r1).unwrap();
            wait_until(&env, r1, 60, idle).await;

            // 3) 세션 허용: 같은 명령은 다시 묻지 않고, 사용자 폴더에 설정 파일이 생기지 않는다
            env.exec
                .send_message(r1, "Bash로 'touch s1.txt' 를 실행해.".into())
                .await
                .unwrap();
            wait_until(&env, r1, 120, |r| r.status == RunStatus::WaitingForApproval).await;
            let ap = task::pending_approvals(&conn_of(), r1).unwrap().remove(0);
            assert_eq!(ap.tool_name, "Bash");
            env.exec
                .decide(ap.id, Decision::AllowSession, None, None)
                .unwrap();
            wait_until(&env, r1, 120, idle).await;
            let asked = || {
                task::list_events(&conn_of(), r1)
                    .unwrap()
                    .iter()
                    .filter(|e| matches!(e.event, RunEvent::ApprovalRequested { .. }))
                    .count()
            };
            let before = asked();
            env.exec
                .send_message(r1, "Bash로 'touch s1.txt' 를 다시 실행해.".into())
                .await
                .unwrap();
            wait_until(&env, r1, 120, |r| r.status == RunStatus::Running).await;
            wait_until(&env, r1, 120, idle).await;
            assert_eq!(before, asked(), "세션 허용한 명령은 다시 묻지 않음");
            assert!(env.proj.path().join("s1.txt").exists());
            assert!(
                !env.proj.path().join(".claude").exists(),
                "사용자 폴더에 영구 규칙 파일이 생기면 안 됨"
            );

            // 4) 질문 카드
            env.exec.send_message(r1, "AskUserQuestion 도구로 '빨강'과 '파랑' 중 좋아하는 색을 나에게 물어보고, 내 답을 그대로 한 단어로 말해.".into()).await.unwrap();
            wait_until(&env, r1, 120, |r| r.status == RunStatus::WaitingForApproval).await;
            let q = task::pending_approvals(&conn_of(), r1).unwrap().remove(0);
            assert_eq!(q.tool_name, "AskUserQuestion");
            let question = q.tool_input["questions"][0]["question"]
                .as_str()
                .unwrap()
                .to_string();
            env.exec
                .decide(
                    q.id,
                    Decision::Allow,
                    Some(serde_json::json!({ question: "파랑" })),
                    None,
                )
                .unwrap();
            wait_until(&env, r1, 120, idle).await;
            let last_text = task::last_result_text(&conn_of(), r1)
                .unwrap()
                .unwrap_or_default();
            println!("질문 답 반영: {last_text}");
            assert!(last_text.contains("파랑"));

            // 5) 대화 중 모드 전환 → 자동 수정
            let edits = ChatOptions {
                permission_mode: PermissionMode::AcceptEdits,
                ..opts.clone()
            };
            if let Some(run_id) = chat::set_options(&conn_of(), chat_id, &edits).unwrap() {
                env.exec.set_mode(run_id, "acceptEdits").unwrap();
            }
            env.exec
                .send_message(r1, "Write 도구로 w.txt를 만들고 내용은 w 로 해.".into())
                .await
                .unwrap();
            wait_until(&env, r1, 120, |r| r.status == RunStatus::Running).await;
            wait_until(&env, r1, 120, idle).await;
            assert!(env.proj.path().join("w.txt").exists());
            assert!(task::pending_approvals(&conn_of(), r1).unwrap().is_empty());

            let evs = task::list_events(&conn_of(), r1).unwrap();
            let detailed = evs
                .iter()
                .filter(|e| matches!(&e.event, RunEvent::ToolUse { input: Some(_), .. }))
                .count();
            println!(
                "턴 {} · 도구 상세 저장 {}개 · 글자 조각 {}개",
                evs.iter()
                    .filter(|e| matches!(e.event, RunEvent::TurnResult { .. }))
                    .count(),
                detailed,
                deltas.load(std::sync::atomic::Ordering::SeqCst)
            );
            assert!(detailed > 0);

            // 정리: 대화 종료
            env.exec.cancel(r1).unwrap();
            wait_until(&env, r1, 30, |r| r.status.is_terminal()).await;
        });
    }

    /// v0.2 A단계 1-b: 이미지 첨부, @파일, 대화 중 모델 전환, /compact, 컨텍스트 수치, 슬래시 명령 목록
    #[test]
    #[ignore]
    fn live_대화_보완기능() {
        use crate::chat::{self, ChatOptions};
        use crate::runtime::ImageAttachment;

        let (env, _agent) = setup(PermissionMode::Default);
        std::fs::create_dir_all(env.proj.path().join("docs")).unwrap();
        std::fs::write(
            env.proj.path().join("docs/secret.md"),
            "비밀 코드는 KIWI-42 입니다.\n",
        )
        .unwrap();
        // 16x16 빨간 PNG (외부 의존 없이 생성)
        let png = {
            fn crc(t: &[u8], d: &[u8]) -> u32 {
                let mut c = 0xffff_ffffu32;
                for b in t.iter().chain(d) {
                    c ^= u32::from(*b);
                    for _ in 0..8 {
                        c = if c & 1 != 0 {
                            0xedb8_8320 ^ (c >> 1)
                        } else {
                            c >> 1
                        };
                    }
                }
                !c
            }
            fn chunk(out: &mut Vec<u8>, t: &[u8], d: &[u8]) {
                out.extend((d.len() as u32).to_be_bytes());
                out.extend(t);
                out.extend(d);
                out.extend(crc(t, d).to_be_bytes());
            }
            // 행마다 필터 바이트(0) + RGB 16개
            let row: Vec<u8> = std::iter::once(0u8)
                .chain([220u8, 30, 30].repeat(16))
                .collect();
            let raw: Vec<u8> = row.repeat(16);
            // 무압축 deflate 블록 (zlib 헤더 + stored block + adler32)
            let mut z = vec![0x78, 0x01, 0x01];
            z.extend((raw.len() as u16).to_le_bytes());
            z.extend((!(raw.len() as u16)).to_le_bytes());
            z.extend(&raw);
            let (mut a, mut b) = (1u32, 0u32);
            for x in &raw {
                a = (a + u32::from(*x)) % 65521;
                b = (b + a) % 65521;
            }
            z.extend(((b << 16) | a).to_be_bytes());
            let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
            let mut ihdr = Vec::new();
            ihdr.extend(16u32.to_be_bytes());
            ihdr.extend(16u32.to_be_bytes());
            ihdr.extend([8, 2, 0, 0, 0]);
            chunk(&mut out, b"IHDR", &ihdr);
            chunk(&mut out, b"IDAT", &z);
            chunk(&mut out, b"IEND", &[]);
            out
        };
        let b64 = {
            const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            let mut o = String::new();
            for c in png.chunks(3) {
                let n = (u32::from(c[0]) << 16)
                    | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
                    | u32::from(*c.get(2).unwrap_or(&0));
                for i in 0..4 {
                    o.push(if i <= c.len() {
                        T[(n >> (18 - 6 * i) & 63) as usize] as char
                    } else {
                        '='
                    });
                }
            }
            o
        };
        let conn_of = || env.handle.state::<Db>().inner().conn().unwrap();
        tauri::async_runtime::block_on(async {
            let opts = ChatOptions {
                model: Some("haiku".into()),
                ..Default::default()
            };
            let img = ImageAttachment {
                media_type: "image/png".into(),
                data: b64,
            };
            let (chat_id, r1) = {
                let c = conn_of();
                let pid = settings::current_project_id(&c).unwrap().unwrap();
                let p = project::get(&c, pid).unwrap().unwrap();
                let (t, r) = chat::start(
                    &c,
                    &p,
                    "이 이미지의 주된 색을 한 단어로 답해.",
                    &opts,
                    &[img],
                )
                .unwrap();
                (t.id, r.id)
            };
            env.exec.pump();
            let idle = |r: &task::Run| r.status == RunStatus::WaitingForInput;
            wait_until(&env, r1, 120, idle).await;
            let answer = task::last_result_text(&conn_of(), r1)
                .unwrap()
                .unwrap_or_default();
            println!("1) 이미지 → {answer}");
            assert!(answer.contains("빨"), "이미지 인식");
            assert!(
                task::take_run_attachments(&conn_of(), r1)
                    .unwrap()
                    .is_empty(),
                "보낸 뒤 첨부 원본은 지움"
            );
            let ev = task::list_events(&conn_of(), r1).unwrap();
            assert!(
                ev.iter()
                    .any(|e| matches!(e.event, RunEvent::UserMessage { images: 1, .. }))
            );
            assert!(
                !env.exec.slash_commands().is_empty(),
                "초기화 응답에서 슬래시 명령 목록 수신"
            );

            env.exec
                .send_message(
                    r1,
                    "@docs/secret.md 에 적힌 비밀 코드만 답해. 도구는 쓰지 마.".into(),
                )
                .await
                .unwrap();
            wait_until(&env, r1, 120, |r| r.status == RunStatus::Running).await;
            wait_until(&env, r1, 120, idle).await;
            let answer = task::last_result_text(&conn_of(), r1)
                .unwrap()
                .unwrap_or_default();
            println!("2) @파일 → {answer}");
            assert!(answer.contains("KIWI-42"));

            let sonnet = ChatOptions {
                model: Some("sonnet".into()),
                ..opts.clone()
            };
            chat::set_options(&conn_of(), chat_id, &sonnet).unwrap();
            env.exec.set_model(r1, "sonnet").unwrap();
            env.exec
                .send_message(r1, "지금 쓰는 모델 이름만 답해.".into())
                .await
                .unwrap();
            wait_until(&env, r1, 120, |r| r.status == RunStatus::Running).await;
            wait_until(&env, r1, 120, idle).await;
            let answer = task::last_result_text(&conn_of(), r1)
                .unwrap()
                .unwrap_or_default();
            println!("3) 모델 전환 → {answer}");
            assert!(answer.to_lowercase().contains("sonnet"));

            env.exec.send_message(r1, "/compact".into()).await.unwrap();
            wait_until(&env, r1, 120, |r| r.status == RunStatus::Running).await;
            wait_until(&env, r1, 180, idle).await;
            let ev = task::list_events(&conn_of(), r1).unwrap();
            let compact = ev.iter().find_map(|e| match &e.event {
                RunEvent::Compacted {
                    pre_tokens,
                    post_tokens,
                } => Some((*pre_tokens, *post_tokens)),
                _ => None,
            });
            println!("4) /compact → {compact:?}");
            assert!(compact.is_some());
            let ctx = ev.iter().rev().find_map(|e| match &e.event {
                RunEvent::TurnResult {
                    context_tokens: Some(t),
                    context_window: Some(w),
                    ..
                } => Some((*t, *w)),
                _ => None,
            });
            println!(
                "5) 컨텍스트 {ctx:?} · 명령 {}개",
                env.exec.slash_commands().len()
            );
            assert!(ctx.is_some_and(|(t, w)| t > 0 && w >= 100_000));

            env.exec.cancel(r1).unwrap();
            wait_until(&env, r1, 30, |r| r.status.is_terminal()).await;
        });
    }

    /// A단계 3번: 대화 응답에서 명령으로 지운 파일·덮어쓴 파일을 되돌린다 (에이전트가 되살리기 어려운 경우)
    #[test]
    #[ignore]
    fn live_대화_되돌리기() {
        use crate::chat::{self, ChatOptions};
        let (env, _agent) = setup(PermissionMode::Default);
        let w = env.proj.path().to_path_buf();
        std::fs::write(w.join("old.md"), "지우면 안 되는 원본\n").unwrap();
        std::fs::write(w.join("report.md"), "보고서 원본\n").unwrap();
        let conn_of = || env.handle.state::<Db>().inner().conn().unwrap();
        tauri::async_runtime::block_on(async {
            let opts = ChatOptions {
                model: Some("haiku".into()),
                permission_mode: PermissionMode::AcceptEdits,
                ..Default::default()
            };
            let r1 = {
                let c = conn_of();
                let pid = settings::current_project_id(&c).unwrap().unwrap();
                let p = project::get(&c, pid).unwrap().unwrap();
                chat::start(&c, &p, "Bash로 'rm old.md'를 실행하고, Write 도구로 report.md 내용을 '덮어씀' 한 줄로 바꿔. 다른 일은 하지 마.", &opts, &[]).unwrap().1.id
            };
            env.exec.pump();
            // rm은 승인이 필요하다 → 허용
            let deadline = Instant::now() + Duration::from_secs(120);
            loop {
                let r = run(&env, r1);
                if r.status == RunStatus::WaitingForInput {
                    break;
                }
                for ap in task::pending_approvals(&conn_of(), r1).unwrap() {
                    env.exec.decide(ap.id, Decision::Allow, None, None).unwrap();
                }
                assert!(Instant::now() < deadline, "시간 초과: {:?}", r.status);
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
            assert!(!w.join("old.md").exists(), "에이전트가 지움");
            assert_eq!(
                std::fs::read_to_string(w.join("report.md")).unwrap().trim(),
                "덮어씀"
            );

            let cp = task::list_events(&conn_of(), r1)
                .unwrap()
                .into_iter()
                .find_map(|e| match e.event {
                    RunEvent::Checkpoint {
                        checkpoint_id,
                        files,
                        total,
                    } => Some((checkpoint_id, files, total)),
                    _ => None,
                });
            println!("되돌리기 기록: {cp:?}");
            let (id, files, _) = cp.expect("바뀐 파일이 있으면 되돌리기 기록이 남아야 함");
            assert!(
                files.contains(&"old.md".to_string()) && files.contains(&"report.md".to_string())
            );

            let res = env.exec.restore_checkpoint(id).await.unwrap();
            println!("되돌림: {res:?}");
            assert_eq!(
                std::fs::read_to_string(w.join("old.md")).unwrap(),
                "지우면 안 되는 원본\n"
            );
            assert_eq!(
                std::fs::read_to_string(w.join("report.md")).unwrap(),
                "보고서 원본\n"
            );
            assert!(
                env.exec.restore_checkpoint(id).await.is_err(),
                "두 번 되돌리지 않음"
            );
            assert!(!w.join(".git").exists());

            env.exec.cancel(r1).unwrap();
            wait_until(&env, r1, 30, |r| r.status.is_terminal()).await;
        });
    }

    /// A단계 5번: 작업 보고의 "기억할 점"이 업무 기억에 쌓이고, 다음 작업에 전달된다 (모델 추가 호출 없음)
    #[test]
    #[ignore]
    fn live_팀원_기억() {
        let (env, agent_id) = setup(PermissionMode::AcceptEdits);
        let conn_of = || env.handle.state::<Db>().inner().conn().unwrap();
        tauri::async_runtime::block_on(async {
            let r1 = start(
                &env,
                agent_id,
                "a.txt를 만들고 내용은 a 로 해. 그리고 사장인 내가 앞으로 파일 내용은 항상 대문자로 쓰길 원한다는 점을 기억해 둬.",
            );
            let d1 = wait_until(&env, r1, 120, |r| r.status.is_terminal()).await;
            assert_eq!(d1.status, RunStatus::Succeeded, "{:?}", d1.error);
            let mem = crate::memory::get(&conn_of(), agent_id).unwrap();
            println!("업무 기억: {mem}");
            assert!(
                !mem.trim().is_empty(),
                "보고의 기억할 점이 노트에 들어가야 함"
            );

            // 다음 작업의 설정 스냅샷에 기억이 들어간다
            // 내용을 직접 지정하지 않아야 기억한 선호가 적용된다 (지정하면 지정이 우선이라고 팀원이 기록함)
            let r2 = start(&env, agent_id, "b.txt를 만들고 영어 인사말 한 단어를 써.");
            let d2 = wait_until(&env, r2, 120, |r| r.status.is_terminal()).await;
            assert!(d2.config.system_append.contains("내 업무 기억"));
            let b = std::fs::read_to_string(env.proj.path().join("b.txt")).unwrap_or_default();
            println!("두 번째 작업 결과 b.txt: {b:?}");
            let word = b.trim();
            assert!(
                !word.is_empty()
                    && word
                        .chars()
                        .filter(|c| c.is_alphabetic())
                        .all(|c| c.is_uppercase()),
                "기억한 선호(대문자)를 따름: {word}"
            );
        });
    }
}
