//! 효율 비교 측정 (스펙 5장 "비교 검증", 12장 7단계).
//!
//! 같은 초기 저장소·목표·모델·권한으로 세 방식을 비교한다.
//! - baseline      : 터미널에서 `claude -p`를 그대로 실행 (사용자 설정 그대로 = 기존 터미널 사용 조건)
//! - baseline_same : 터미널 `claude -p`에 Mefice와 같은 도구 제한 인자 (Mefice 자체 추가 비용 분리용)
//! - direct        : Mefice 직접 실행 (격리 작업 공간 → 병합)
//! - pm            : Mefice PM (PM이 직접 처리·위임을 판단)
//! - pm_delegate   : Mefice PM에 위임을 지시 (PM 분배 경로 비용 측정)
//!
//! 완료 여부는 모델에게 묻지 않고 node로 결과를 직접 검사한다. 모델 사용량이 발생하므로 기본 제외.
//! 실행: MEFICE_BENCH_MODEL=haiku MEFICE_BENCH_REPS=2 MEFICE_BENCH_OUT=../docs/bench \
//!       cargo test bench_ -- --ignored --nocapture --test-threads=1

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;
use tauri::Manager;
use tauri::test::{MockRuntime, mock_app};

use crate::agent::{self, AgentInput, PermissionMode};
use crate::db::Db;
use crate::executor::Executor;
use crate::task;
use crate::{git, pm, project, settings, usage, workspace};

const CALC_JS: &str = "// 두 수를 더해 retrun 한다
function add(a, b) {
  return a + b;
}

function sub(a, b) {
  return a - b;
}

module.exports = { add, sub };
";
const TEST_JS: &str = "const assert = require(\"node:assert\");
const { add, sub } = require(\"./calc\");
assert.strictEqual(add(1, 2), 3);
assert.strictEqual(sub(5, 3), 2);
console.log(\"ok\");
";
const README: &str = "# calc\n\n간단한 계산 모듈.\n";

struct Scenario {
    id: &'static str,
    label: &'static str,
    prompt: &'static str,
    /// (통과 여부, 상세)
    verify: fn(&Path) -> (bool, String),
}

fn node(dir: &Path, code: &str) -> bool {
    Command::new("node")
        .args(["-e", code])
        .current_dir(dir)
        .output()
        .is_ok_and(|o| o.status.success())
}

fn node_test(dir: &Path) -> bool {
    Command::new("node")
        .arg("test.js")
        .current_dir(dir)
        .output()
        .is_ok_and(|o| o.status.success())
}

fn read(dir: &Path, f: &str) -> String {
    std::fs::read_to_string(dir.join(f)).unwrap_or_default()
}

const SCENARIOS: &[Scenario] = &[
    Scenario {
        id: "s1-small",
        label: "작은 수정",
        prompt: "calc.js의 add 함수 위 주석에 있는 오타 'retrun'을 'return'으로 고쳐줘.",
        verify: |d| {
            let c = read(d, "calc.js");
            let ok = c.contains("return 한다") && !c.contains("retrun") && node_test(d);
            (
                ok,
                format!(
                    "오타 수정 {} · 테스트 {}",
                    !c.contains("retrun"),
                    node_test(d)
                ),
            )
        },
    },
    Scenario {
        id: "s2-parallel",
        label: "독립적인 두 작업",
        prompt: "다음 두 가지를 해줘. (1) calc.js에 두 수를 곱하는 mul(a, b) 함수를 추가하고 module.exports에 포함해. \
                 (2) README.md에 '## 사용법' 섹션을 추가해 add와 sub 사용 예를 적어줘.",
        verify: |d| {
            let mul = node(d, "process.exit(require('./calc').mul(2,3)===6?0:1)");
            let readme = read(d, "README.md").contains("## 사용법");
            let t = node_test(d);
            (
                mul && readme && t,
                format!("mul {mul} · README {readme} · 테스트 {t}"),
            )
        },
    },
    Scenario {
        id: "s3-sequential",
        label: "순차 의존 작업",
        prompt: "calc.js에 div(a, b) 함수를 추가하고(b가 0이면 Error를 throw) module.exports에 포함해. \
                 그 다음 test.js에 div 테스트(정상 나눗셈과 0으로 나누기 에러)를 추가하고 node test.js 로 통과하는지 확인해줘.",
        verify: |d| {
            let div = node(
                d,
                "const {div}=require('./calc');let e=false;try{div(1,0)}catch{e=true};process.exit(div(6,3)===2&&e?0:1)",
            );
            let test_has = read(d, "test.js").contains("div");
            let t = node_test(d);
            (
                div && test_has && t,
                format!("div {div} · 테스트 추가 {test_has} · 테스트 통과 {t}"),
            )
        },
    },
];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Measurement {
    scenario: String,
    mode: String,
    rep: u32,
    success: bool,
    detail: String,
    wall_secs: f64,
    cost_usd: Option<f64>,
    pm_cost_usd: Option<f64>,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_creation_tokens: i64,
    runs: i64,
    pm_runs: i64,
    delegated_tasks: i64,
    error: Option<String>,
}

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    git::run(p, &["init", "-q", "-b", "main"]).unwrap();
    std::fs::write(p.join("calc.js"), CALC_JS).unwrap();
    std::fs::write(p.join("test.js"), TEST_JS).unwrap();
    std::fs::write(p.join("README.md"), README).unwrap();
    git::snapshot_commit(p, "init").unwrap();
    dir
}

// ---------- baseline: 터미널 claude -p ----------

fn run_baseline(s: &Scenario, model: &str, rep: u32, same_tools: bool) -> Measurement {
    let fx = fixture();
    let dir = fx.path();
    let bin = crate::sysproc::resolve_binary("claude").expect("claude");
    let t0 = Instant::now();
    let mut cmd = Command::new(bin);
    cmd.args([
        "-p",
        s.prompt,
        "--model",
        model,
        "--permission-mode",
        "acceptEdits",
    ])
    .args(["--allowedTools", "Bash(node *)", "--output-format", "json"]);
    if same_tools {
        // Mefice 작업자와 같은 도구 제한 (claude_code::build_command 참고)
        cmd.args([
            "--strict-mcp-config",
            "--tools",
            &agent::default_tools().join(","),
        ]);
    }
    let out = cmd
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let wall = t0.elapsed().as_secs_f64();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    let mut m = Measurement {
        scenario: s.id.into(),
        mode: if same_tools {
            "baseline_same"
        } else {
            "baseline"
        }
        .into(),
        rep,
        success: false,
        detail: String::new(),
        wall_secs: wall,
        cost_usd: v.get("total_cost_usd").and_then(Value::as_f64),
        pm_cost_usd: None,
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
        runs: 1,
        pm_runs: 0,
        delegated_tasks: 0,
        error: v
            .get("is_error")
            .and_then(Value::as_bool)
            .filter(|e| *e)
            .map(|_| {
                v.get("subtype")
                    .and_then(Value::as_str)
                    .unwrap_or("error")
                    .to_string()
            }),
    };
    if let Some(models) = v.get("modelUsage").and_then(Value::as_object) {
        for mu in models.values() {
            let g = |k: &str| mu.get(k).and_then(Value::as_i64).unwrap_or(0);
            m.input_tokens += g("inputTokens");
            m.output_tokens += g("outputTokens");
            m.cache_read_tokens += g("cacheReadInputTokens");
            m.cache_creation_tokens += g("cacheCreationInputTokens");
        }
    }
    let (ok, detail) = (s.verify)(dir);
    m.success = ok;
    m.detail = detail;
    m
}

// ---------- Mefice ----------

struct Env {
    _app: tauri::App<MockRuntime>,
    _data: tempfile::TempDir,
    fx: tempfile::TempDir,
    exec: Arc<Executor<MockRuntime>>,
    handle: tauri::AppHandle<MockRuntime>,
    worker_id: i64,
}

impl Env {
    fn new(model: &str) -> Self {
        let app = mock_app();
        let data = tempfile::tempdir().unwrap();
        let fx = fixture();
        app.manage(Db::open(&data.path().join("bench.db")).unwrap());
        let handle = app.handle().clone();
        let exec = Executor::new(handle.clone(), data.path().join("worktrees"));
        let worker_id = {
            let conn = handle.state::<Db>().inner().conn().unwrap();
            let p = project::upsert(
                &conn,
                &project::normalize_path(fx.path().to_str().unwrap()).unwrap(),
            )
            .unwrap();
            settings::set_current_project_id(&conn, p.id).unwrap();
            let pm_agent = agent::ensure_pm(&conn).unwrap();
            conn.execute(
                "UPDATE agents SET model = ?1 WHERE id = ?2",
                rusqlite::params![model, pm_agent.id],
            )
            .unwrap();
            agent::create(
                &conn,
                &AgentInput {
                    name: "개발자".into(),
                    role: "개발".into(),
                    instructions: String::new(),
                    model: Some(model.into()),
                    effort: None,
                    permission_mode: PermissionMode::AcceptEdits,
                    tools: agent::default_tools(),
                    color: None,
                    profile: Default::default(),
                },
            )
            .unwrap()
            .id
        };
        Self {
            _app: app,
            _data: data,
            fx,
            exec,
            handle,
            worker_id,
        }
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, rusqlite::Connection> {
        self.handle.state::<Db>().inner().conn().unwrap()
    }

    fn project_id(&self) -> i64 {
        settings::current_project_id(&self.conn()).unwrap().unwrap()
    }

    /// 모든 실행이 끝날 때까지 기다린다. 승인 요청은 바로 허용한다 (사용자가 승인하는 것과 같음).
    async fn settle(&self, timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        let mut quiet_since: Option<Instant> = None;
        loop {
            let (active, pending) = {
                let c = self.conn();
                let active: i64 = c
                    .query_row(
                        "SELECT COUNT(*) FROM runs WHERE status IN ('queued','running','waiting_for_input','waiting_for_approval','cancelling')",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap();
                let pending: Vec<i64> = c
                    .prepare("SELECT id FROM approvals WHERE status = 'pending'")
                    .unwrap()
                    .query_map([], |r| r.get(0))
                    .unwrap()
                    .collect::<Result<_, _>>()
                    .unwrap();
                (active, pending)
            };
            for id in pending {
                let _ = self.exec.respond_approval(id, true);
            }
            if active == 0 {
                // PM 취합처럼 종료 직후 새 실행이 생길 수 있으므로 잠시 조용한지 확인
                let since = *quiet_since.get_or_insert_with(Instant::now);
                if since.elapsed() > Duration::from_secs(3) {
                    return Ok(());
                }
            } else {
                quiet_since = None;
            }
            if Instant::now() > deadline {
                return Err("시간 초과".into());
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }

    /// 격리 작업 결과를 작업 순서대로 병합한다 (사용자 확정과 같은 경로)
    fn merge_all(&self) -> Vec<String> {
        let c = self.conn();
        let ids: Vec<i64> = c
            .prepare("SELECT task_id FROM workspaces WHERE removed_at IS NULL ORDER BY task_id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let mut errors = Vec::new();
        for id in ids {
            match workspace::diff(&c, id) {
                Ok(d) if d.can_merge => {
                    if let Err(e) = workspace::merge(&c, id) {
                        errors.push(format!("#{id} 병합 실패: {e}"));
                    }
                }
                Ok(d) if d.files.is_empty() => {}
                Ok(d) => errors.push(format!(
                    "#{id} 병합 불가: 충돌 {:?} 겹침 {:?}",
                    d.conflicts, d.dirty_overlap
                )),
                Err(e) => errors.push(format!("#{id} diff 실패: {e}")),
            }
        }
        errors
    }

    fn measure(
        &self,
        s: &Scenario,
        mode: &str,
        rep: u32,
        wall: f64,
        err: Option<String>,
    ) -> Measurement {
        let merge_errors = self.merge_all();
        let c = self.conn();
        let pid = settings::current_project_id(&c).unwrap().unwrap();
        let sum = usage::summary(&c, pid, None).unwrap();
        let pm_cost = sum
            .by_role
            .iter()
            .find(|g| g.key == "pm")
            .and_then(|g| g.totals.cost_usd);
        let count = |sql: &str| -> i64 { c.query_row(sql, [], |r| r.get(0)).unwrap() };
        let (ok, mut detail) = (s.verify)(self.fx.path());
        if !merge_errors.is_empty() {
            detail.push_str(&format!(" · {}", merge_errors.join("; ")));
        }
        let failed_runs: Vec<String> = c
            .prepare("SELECT COALESCE(error, status) FROM runs WHERE status != 'succeeded'")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        Measurement {
            scenario: s.id.into(),
            mode: mode.into(),
            rep,
            success: ok,
            detail,
            wall_secs: wall,
            cost_usd: sum.totals.cost_usd,
            pm_cost_usd: pm_cost,
            input_tokens: sum.totals.input_tokens,
            output_tokens: sum.totals.output_tokens,
            cache_read_tokens: sum.totals.cache_read_tokens,
            cache_creation_tokens: sum.totals.cache_creation_tokens,
            runs: count("SELECT COUNT(*) FROM runs"),
            pm_runs: count("SELECT COUNT(*) FROM runs WHERE role = 'pm'"),
            delegated_tasks: count("SELECT COUNT(*) FROM tasks WHERE mode = 'delegated'"),
            error: err.or_else(|| (!failed_runs.is_empty()).then(|| failed_runs.join("; "))),
        }
    }
}

async fn run_direct(s: &Scenario, model: &str, rep: u32) -> Measurement {
    let env = Env::new(model);
    let t0 = Instant::now();
    let pid = env.project_id(); // 잠금을 쥐기 전에 읽는다 (Mutex 재진입 불가)
    {
        let c = env.conn();
        let p = project::get(&c, pid).unwrap().unwrap();
        let a = agent::get(&c, env.worker_id).unwrap();
        let t = task::create_task(&c, p.id, a.id, s.prompt).unwrap();
        task::set_isolation(&c, t.id, task::ISOLATION_WORKTREE).unwrap();
        task::create_run_with_role(
            &c,
            task::NewRun {
                task_id: t.id,
                agent_id: a.id,
                prompt: s.prompt,
                resume_session_id: None,
                config: &pm::run_config(&p, &a),
                cwd: &p.path,
            },
            a.usage_role(),
        )
        .unwrap();
    }
    env.exec.pump();
    let err = env.settle(Duration::from_secs(600)).await.err();
    env.measure(s, "direct", rep, t0.elapsed().as_secs_f64(), err)
}

const DELEGATE_HINT: &str =
    "\n\n직접 처리하지 말고 팀원에게 위임해. 서로 독립적인 부분이 있으면 나눠서 병렬로 맡겨.";

async fn run_pm(s: &Scenario, model: &str, rep: u32, force_delegate: bool) -> Measurement {
    let env = Env::new(model);
    let t0 = Instant::now();
    let pid = env.project_id();
    let prompt = if force_delegate {
        format!("{}{DELEGATE_HINT}", s.prompt)
    } else {
        s.prompt.to_string()
    };
    {
        let c = env.conn();
        let p = project::get(&c, pid).unwrap().unwrap();
        pm::start_goal(&c, &p, &prompt).unwrap();
    }
    env.exec.pump();
    let err = env.settle(Duration::from_secs(900)).await.err();
    let mode = if force_delegate { "pm_delegate" } else { "pm" };
    env.measure(s, mode, rep, t0.elapsed().as_secs_f64(), err)
}

const MODES: &[&str] = &["baseline", "baseline_same", "direct", "pm", "pm_delegate"];

// ---------- 보고서 ----------

fn mean(xs: &[f64]) -> Option<f64> {
    (!xs.is_empty()).then(|| xs.iter().sum::<f64>() / xs.len() as f64)
}

fn fmt_cost(c: Option<f64>) -> String {
    c.map(|c| format!("${c:.4}"))
        .unwrap_or_else(|| "미제공".into())
}

fn report(ms: &[Measurement], model: &str, reps: u32) -> String {
    let mut out = format!(
        "# Mefice 효율 비교 측정\n\n- 모델: `{model}` · 반복: {reps}회 · 런타임: Claude Code CLI {}\n\
         - 비용은 런타임이 보고한 API 정가 환산값이며 구독 사용량과 같지 않다.\n\
         - baseline은 사용자 설정 그대로의 `claude -p` (전역 MCP·훅 포함). Mefice는 `--strict-mcp-config --tools`로 도구를 제한한다.\n\
         - 실행은 순서대로 진행되어 앞 실행이 데운 프롬프트 캐시를 뒤 실행이 재사용할 수 있다 (캐시 읽기·생성 토큰 참고).\n\n",
        crate::sysproc::resolve_binary("claude")
            .and_then(|b| crate::sysproc::run_capture(&b, &["--version"], None))
            .unwrap_or_default()
    );
    out.push_str("## 요약 (평균)\n\n| 시나리오 | 방식 | 완료 | 비용 | baseline 대비 | 같은 도구 기준 대비 | PM 비용 (비중) | 시간(초) | 실행 수 | 위임 작업 |\n|---|---|---|---|---|---|---|---|---|---|\n");
    for s in SCENARIOS {
        let mean_cost = |mode: &str| {
            mean(
                &ms.iter()
                    .filter(|m| m.scenario == s.id && m.mode == mode)
                    .filter_map(|m| m.cost_usd)
                    .collect::<Vec<_>>(),
            )
        };
        let base = mean_cost("baseline");
        let same = mean_cost("baseline_same");
        for &mode in MODES {
            let g: Vec<&Measurement> = ms
                .iter()
                .filter(|m| m.scenario == s.id && m.mode == mode)
                .collect();
            if g.is_empty() {
                continue;
            }
            let cost = mean(&g.iter().filter_map(|m| m.cost_usd).collect::<Vec<_>>());
            let pm_cost = mean(&g.iter().filter_map(|m| m.pm_cost_usd).collect::<Vec<_>>());
            let pct = |b: Option<f64>| match (cost, b) {
                (Some(c), Some(b)) if b > 0.0 => format!("{:.0}%", c / b * 100.0),
                _ => "-".into(),
            };
            let (ratio, ratio_same) = (pct(base), pct(same));
            let pm_share = match (pm_cost, cost) {
                (Some(p), Some(c)) if c > 0.0 => {
                    format!("{} ({:.0}%)", fmt_cost(Some(p)), p / c * 100.0)
                }
                _ => "-".into(),
            };
            out.push_str(&format!(
                "| {} | {} | {}/{} | {} | {} | {} | {} | {:.0} | {:.1} | {:.1} |\n",
                s.label,
                mode,
                g.iter().filter(|m| m.success).count(),
                g.len(),
                fmt_cost(cost),
                ratio,
                ratio_same,
                pm_share,
                mean(&g.iter().map(|m| m.wall_secs).collect::<Vec<_>>()).unwrap_or(0.0),
                g.iter().map(|m| m.runs as f64).sum::<f64>() / g.len() as f64,
                g.iter().map(|m| m.delegated_tasks as f64).sum::<f64>() / g.len() as f64,
            ));
        }
    }
    out.push_str("\n## 개별 실행\n\n| 시나리오 | 방식 | 회차 | 완료 | 비용 | PM 비용 | 입력 | 출력 | 캐시 읽기 | 캐시 생성 | 시간(초) | 실행/PM | 검사 결과 | 오류 |\n|---|---|---|---|---|---|---|---|---|---|---|---|---|---|\n");
    for m in ms {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {:.0} | {}/{} | {} | {} |\n",
            m.scenario,
            m.mode,
            m.rep,
            if m.success { "✓" } else { "✗" },
            fmt_cost(m.cost_usd),
            fmt_cost(m.pm_cost_usd),
            m.input_tokens,
            m.output_tokens,
            m.cache_read_tokens,
            m.cache_creation_tokens,
            m.wall_secs,
            m.runs,
            m.pm_runs,
            m.detail.replace('|', "/"),
            m.error
                .as_deref()
                .unwrap_or("")
                .replace('|', "/")
                .replace('\n', " "),
        ));
    }
    out
}

#[test]
#[ignore]
fn bench_효율_비교() {
    let model = std::env::var("MEFICE_BENCH_MODEL").unwrap_or_else(|_| "haiku".into());
    let reps: u32 = std::env::var("MEFICE_BENCH_REPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2);
    let out_dir =
        PathBuf::from(std::env::var("MEFICE_BENCH_OUT").unwrap_or_else(|_| "../docs/bench".into()));
    let only = std::env::var("MEFICE_BENCH_ONLY").ok(); // 예: s1-small

    let mut ms: Vec<Measurement> = Vec::new();
    tauri::async_runtime::block_on(async {
        for rep in 1..=reps {
            for s in SCENARIOS
                .iter()
                .filter(|s| only.as_deref().is_none_or(|o| o == s.id))
            {
                for &mode in MODES {
                    let m = match mode {
                        "baseline" => run_baseline(s, &model, rep, false),
                        "baseline_same" => run_baseline(s, &model, rep, true),
                        "direct" => run_direct(s, &model, rep).await,
                        "pm" => run_pm(s, &model, rep, false).await,
                        _ => run_pm(s, &model, rep, true).await,
                    };
                    println!(
                        "[{}/{}] {} {} → {} {} {:.0}s {}",
                        rep,
                        reps,
                        s.id,
                        mode,
                        if m.success { "✓" } else { "✗" },
                        fmt_cost(m.cost_usd),
                        m.wall_secs,
                        m.detail
                    );
                    ms.push(m);
                }
            }
        }
    });

    std::fs::create_dir_all(&out_dir).unwrap();
    let stamp = chrono_like_stamp();
    let base = out_dir.join(format!("{stamp}-{model}"));
    std::fs::write(
        base.with_extension("json"),
        serde_json::to_string_pretty(&ms).unwrap(),
    )
    .unwrap();
    std::fs::write(base.with_extension("md"), report(&ms, &model, reps)).unwrap();
    println!("보고서: {}", base.with_extension("md").display());
}

/// 외부 의존 없이 YYYYMMDD-HHMM (UTC) 문자열
fn chrono_like_stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // 1970-01-01 기준 날짜 계산 (Howard Hinnant 알고리즘)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}",
        rem / 3600,
        (rem % 3600) / 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 시나리오_검사는_초기_상태에서_실패하고_정답에서_통과한다() {
        let fx = fixture();
        let d = fx.path();
        for s in SCENARIOS {
            assert!(!(s.verify)(d).0, "{} 초기 상태는 미완료여야 함", s.id);
        }
        // 정답 적용
        std::fs::write(
            d.join("calc.js"),
            CALC_JS.replace("retrun", "return").replace(
                "module.exports = { add, sub };",
                "function mul(a, b) { return a * b; }\nfunction div(a, b) { if (b === 0) throw new Error(\"0\"); return a / b; }\nmodule.exports = { add, sub, mul, div };",
            ),
        )
        .unwrap();
        std::fs::write(
            d.join("README.md"),
            format!("{README}\n## 사용법\nadd(1,2)\n"),
        )
        .unwrap();
        std::fs::write(
            d.join("test.js"),
            TEST_JS.replace("const { add, sub }", "const { add, sub, div }")
                + "assert.strictEqual(div(6, 3), 2);\nassert.throws(() => div(1, 0));\n",
        )
        .unwrap();
        for s in SCENARIOS {
            let (ok, detail) = (s.verify)(d);
            assert!(ok, "{} 정답은 통과해야 함: {detail}", s.id);
        }
    }

    #[test]
    fn 날짜_문자열() {
        assert_eq!(chrono_like_stamp().len(), 13);
    }
}
