# mefice-mvp — Mefice MVP 전체 구현 계획 (스펙 12장 7단계 기준)

## 플랜 실행 이력

### 3단계 완료: 2026-11-20

**결과**: 성공

**런타임 사용량 검증 결과 (Claude Code 2.1.304)**:
- `result.usage`: 턴 단위, CLI 내부 보조 호출 **제외** (예: usage 출력 3 vs modelUsage 출력 22)
- `result.modelUsage`, `total_cost_usd`: 세션 누적, `--resume` 후에도 **이어짐**
- → 집계는 같은 세션 직전 기록과의 누적값 차이(delta)로 계산. 이어서 실행해도 이중 집계 없음
- `system/init.apiKeySource == "none"` → 구독 로그인 인증
- `rate_limit_event.unifiedWindows`: 계정 전체 구독 한도 사용률 (five_hour, seven_day)
- haiku 단순 질의 첫 실행도 정가 환산 ~$0.02 (대부분 시스템 프롬프트 캐시 생성) → 7단계 기준선 측정 필요

**실제 변경 파일**:
- `db/migrations.rs` — v3: `runs.role`, `runs.auth_kind`, `usage_records`(턴별 delta + 세션 누적 스냅샷)
- `runtime/mod.rs`, `runtime/claude_code.rs` — `SessionTotals`, `RateLimitSnapshot`, `Started.auth`, 턴 비용 필드에 누적값을 넣지 않도록 수정
- `usage.rs` — 기록(reported/partial/unavailable), 작업·프로젝트 집계(역할별·에이전트별·기간), 구독 한도 저장, 시작 시 미제공 보충
- `executor.rs` — 턴마다 사용량 기록, 인증 방식 저장, 구독 한도 갱신 이벤트
- `lib.rs` — `recover_on_startup()` (interrupted 처리 + 미제공 보충)
- `commands.rs` — `get_usage_summary`, `get_task_usage`, `get_rate_limit`
- `src/components/Usage.tsx`, `src/screens/UsageScreen.tsx` — 출처 배지, 사용량 표, 구독 한도 미터
- `src/screens/Tasks.tsx` — 상태별 필터(작업 보드), 실행별 사용량 패널

**계획과의 차이**:
- 출처 구분을 `runtime_reported/estimated/unavailable` → `reported/partial/unavailable`로 변경. 런타임이 수치를 주므로 Mefice가 추정할 일이 없고, "턴 사용량만 있는 경우"가 실제로 필요한 구분이었음
- 구독 한도 사용률 표시 추가 (계획에 없었지만 스펙 5장 "구독 한도와 API 가격 구분"에 필요)
- 2단계에서 저장된 실행은 사용량을 소급 계산하지 않음 (해당 실행 데이터 0건)

**검증**:
- 단위 테스트 27개 통과 (누적값 차이, 재개 시 이중 집계 방지, 미제공·부분 구분, 기간 필터, 강제 종료 후 파일 DB 재시작 복원), clippy 경고 0, `pnpm build` 성공
- 실제 CLI 통합 테스트 3개 통과: 실행 1 + 이어서 실행 2의 비용 합($0.021565 + $0.010367)이 세션 누적($0.031932)과 일치, 인증 `subscription` 기록, 구독 한도 수집
- 실제 앱 실행으로 DB v2 → v3 마이그레이션 확인

**잔존 작업**:
- PM 역할(`role = pm`) 사용량은 4단계에서 PM Run이 생기면 자동 집계
- 검토(`review`) 역할은 리뷰어 위임이 생기는 단계에서 지정

### 2단계 완료: 2026-11-20

**결과**: 성공

**런타임 프로토콜 검증 결과 (Claude Code 2.1.304)**:
- 승인 요청을 받으려면 `--permission-prompt-tool stdio`(도움말에 없는 옵션) + stdin으로 `initialize` 제어 요청 필요. 둘 중 하나라도 없으면 승인 없이 자동 거부됨
- `interrupt` 제어 요청 → 현재 턴이 `result`로 종료. `result` 후에도 프로세스는 다음 메시지를 기다리고, stdin을 닫으면 exit 0
- `result.usage`는 턴 단위, `total_cost_usd`는 세션 누적이며 API 정가 기준(`costBasis: list`)
- `--strict-mcp-config --tools <목록>` 지정 시 첫 호출 캐시 생성 토큰 ~35k → ~5–10k

**실제 변경 파일**:
- `db/migrations.rs` — v2: `agents`, `tasks`, `runs`, `run_events`, `approvals`
- `agent.rs` — 에이전트 CRUD, 권한 모드(default/acceptEdits/plan, bypass 미제공), 허용 도구 목록
- `runtime/mod.rs` — `RunConfig`(설정 스냅샷), `Usage`, 공통 `RunEvent`
- `runtime/claude_code.rs` — 명령 구성, stream-json 파서, 입력 인코더
- `task.rs` — Task/Run/RunEvent/Approval 저장소, 재시작 시 미완료 Run → `interrupted`
- `executor.rs` — 큐(동시 실행 상한), 프로세스 구동, 승인·추가 지시·중단(5초 후 강제 종료)
- `commands.rs`, `lib.rs` — 명령 14개 추가
- `sysproc.rs` — 자식 프로세스용 로그인 셸 PATH
- `src/screens/Tasks.tsx`, `Team.tsx`, `components/StatusBadge.tsx`, `api/index.ts`, `styles.css`

**계획과의 차이**:
- 승인 흐름(`waiting_for_approval`)을 2단계에서 구현 (권한 기본값을 "승인 필요"로 정했고, 프로젝트 폴더에서 직접 실행되므로 필수)
- 단순 FIFO 큐와 동시 실행 상한도 2단계에서 구현 (의존성은 5단계 그대로)
- 재시작 시 `interrupted` 처리를 3단계에서 앞당김
- "작업 보드" 탭을 직접 실행 + 목록 + 상세 화면으로 우선 구현

**검증**:
- 단위 테스트 20개 통과, clippy 경고 0, `pnpm build` 성공
- 실제 CLI 통합 테스트 2개 통과 (`cargo test live_ -- --ignored`): 승인 후 파일 쓰기 성공, 실행 중 중단 → cancelled
- 기존 v1 DB → v2 마이그레이션을 실제 앱 실행으로 확인

**잔존 작업**:
- UI 수동 확인 (에이전트 추가 → 직접 실행 → 승인/거부 → 추가 지시 → 중단 → 이어서 실행)
- `waiting_for_input`(런타임의 질문 도구) 미지원 — 질문은 텍스트로 받고 "이어서 실행"으로 답하는 방식
- 사용량 집계(PM/작업자 구분, 출처 표시)는 3단계

### 1단계 완료: 2026-11-20

**결과**: 성공

**실제 변경 파일**:
- `package.json`, `vite.config.ts`, `tsconfig*.json`, `index.html` — create-tauri-app(react-ts) 스캐폴딩, `plugin-opener` 제거, `plugin-dialog` 추가
- `src-tauri/Cargo.toml` — `rusqlite`(bundled), `thiserror`, `tauri-plugin-dialog`, dev: `tempfile`
- `src-tauri/tauri.conf.json`, `capabilities/default.json` — 제품명·창 크기, 권한은 `core:default` + `dialog:allow-open`만
- `src-tauri/src/error.rs` — `AppError` (문자열로 직렬화)
- `src-tauri/src/db/{mod,migrations}.rs` — WAL·FK·busy_timeout, `user_version` 기반 마이그레이션 v1 (`settings`, `projects`)
- `src-tauri/src/settings.rs` — `AppSettings`(maxConcurrency=2, closeToTray=true), 현재 프로젝트 ID
- `src-tauri/src/project.rs` — 폴더 정규화·등록, Git 상태 읽기 전용 검사
- `src-tauri/src/sysproc.rs` — GUI 앱 PATH 문제 대응 실행 파일 탐색 (PATH → 로그인 셸 → 흔한 경로)
- `src-tauri/src/env_check.rs` — git/claude 설치·버전 검사
- `src-tauri/src/commands.rs`, `lib.rs` — Tauri 명령 6개, 앱 데이터 디렉토리에 DB 생성
- `src/api/index.ts`, `src/App.tsx`, `src/screens/*`, `src/styles.css` — 시작 화면, 탭 레이아웃, 설정 화면

**계획과의 차이**:
- React 18 → 19 (최신 템플릿 기본값)
- `get_settings` 명령은 `get_app_state`에 통합, `refresh_project` 추가

**검증**:
- `cargo test` 8개 통과 (파일 DB를 닫고 다시 여는 복원 테스트 포함), `cargo clippy` 경고 0, `pnpm build` 성공
- `pnpm tauri build --debug --bundles app` 성공, `open`으로 실행 시 `~/Library/Application Support/dev.mefice.app/mefice.db` 생성·마이그레이션 v1 확인

**잔존 작업**:
- UI 수동 확인(폴더 선택 → 지침/설정 변경 → 재실행 복원)은 사용자 확인 필요
- `closeToTray`는 값 저장만 됨. 실제 트레이 동작은 7단계(앱 수명주기)에서 구현

---

## 목표

`Mefice-Spec.md`(v0.1)의 MVP 범위를 12장 구현 순서대로 구현한다. 각 단계는 스펙의 완료 기준을 통과해야 다음 단계로 넘어간다. 이번 세션은 **1단계(Tauri 셸·프로젝트·SQLite)** 를 완료한다.

## 확정한 결정 (스펙 14장 미결정 항목 중)

| 항목 | 결정 | 근거 |
|---|---|---|
| 최초 지원 OS | macOS (arm64) | 개발 환경, 스펙 제안과 일치 |
| 런타임 | Claude Code CLI 2.1.x (`claude -p`) | `--output-format stream-json`, `--input-format stream-json`, `--session-id`/`--resume`, `--permission-prompts host`, usage(입력/출력/캐시) 제공 |
| 인증 | 런타임 자체 로그인 사용 (Mefice는 자격증명을 저장하지 않음) | 스펙 10장 |
| 스택 | Tauri 2 + React 18/TS (Vite, pnpm) + SQLite(`rusqlite`, bundled) | 스펙 7장 |
| 마이그레이션 | `PRAGMA user_version` 기반 순차 마이그레이션 (Rust 내장 SQL) | 외부 도구 없이 단순하게 |

미결정 유지: PM 추가 사용량 허용 기준(7단계 실험 후), 캐릭터 아트(6단계 전 플레이스홀더 사용), 배포·서명.

## 현재 상태 (AS-IS)

- 레포에 `Mefice-Spec.md`만 존재, 커밋 없음.
- 툴체인: Node 24, pnpm 10, Rust 1.100(이번 세션 설치), git 2.43, `claude` 2.1.304.

## 변경 후 상태 (TO-BE) — 최종 디렉토리 구조

```
mefice/
├─ src/                      # React + TS
│  ├─ api/                   # Tauri invoke/event 래퍼 (타입 포함)
│  ├─ screens/               # Office, PmChat, Board, TaskDetail, Team, Settings
│  ├─ office/                # 캐릭터·애니메이션 (로컬 로직, 모델 호출 없음)
│  └─ App.tsx
├─ src-tauri/
│  └─ src/
│     ├─ db/                 # 연결, 마이그레이션, 리포지토리
│     ├─ project.rs          # 폴더 연결, Git/환경 검사
│     ├─ runtime/            # 어댑터 trait + claude_code.rs
│     ├─ executor/           # 큐, 의존성, 동시 실행 상한, 프로세스 제어
│     ├─ git/                # worktree·브랜치·diff
│     ├─ pm/                 # PM 프롬프트, 계획 JSON 파싱
│     ├─ commands.rs         # Tauri 명령 경계 (프론트에 셸 직접 노출 금지)
│     └─ lib.rs
└─ Mefice-Spec.md
```

## 단계별 계획

### 1단계: Tauri 셸, 프로젝트, SQLite  ← 이번 세션

1. Tauri 2 + React-TS 스캐폴딩 (pnpm), `tauri-plugin-dialog`(폴더 선택) 추가.
2. SQLite: 앱 데이터 디렉토리에 `mefice.db`, WAL 모드, 마이그레이션 v1 (`settings`, `projects`).
3. Rust 명령: `get_app_state`, `open_project(path)`, `update_project_instructions`, `get_settings`, `update_settings`, `check_environment`.
   - 프로젝트 검사: 폴더 존재, Git 저장소 여부·현재 브랜치·미커밋 변경 수(읽기 전용 — 기존 변경을 건드리지 않음).
   - 환경 검사: `git`, `claude` 설치·버전.
4. UI: 프로젝트 미선택 시 시작 화면 → 선택 후 기본 레이아웃(사무실 / PM 채팅 / 작업 보드 / 팀 / 설정 탭). 설정 화면에서 프로젝트 정보·기본 지침·동시 실행 상한(기본 2) 편집.
5. **완료 기준**: 앱 종료 후 재실행 시 마지막 프로젝트·지침·설정이 복원된다.

### 2단계: 런타임 어댑터와 직접 실행

1. `RuntimeAdapter` trait: `start(task_ctx) -> RunHandle`, `send_message`, `cancel`, 출력 → 공통 `RunEvent` 변환.
2. Claude Code 어댑터: `claude -p --output-format stream-json --input-format stream-json --verbose --session-id <uuid> --model ... --permission-mode ...` 를 `tokio::process`로 실행, cwd = 작업 디렉토리. stdout JSONL 파싱.
3. 마이그레이션 v2: `agent_templates`, `agents`, `tasks`, `runs`, `run_events`.
4. 직접 실행 모드 UI: 에이전트 선택 → 작업 입력 → 실시간 출력 → 중단(SIGTERM → 타임아웃 후 SIGKILL, 상태 `cancelling → cancelled`).
5. **완료 기준**: 작업 실행·출력 확인·중단 가능.

### 3단계: 이벤트·이력·사용량

1. `run_events` 순번 기반 영속화, 앱 재시작 시 이력 로드.
2. 마이그레이션 v3: `usage` (run_id, role[pm/worker/review], input/output/cache_read/cache_creation, cost_usd, source[runtime_reported/estimated/unavailable]).
3. 구독 한도와 API 환산 비용을 별도 표기.
4. 재시작 시 `running`인데 프로세스가 없는 Run → `interrupted`.
5. **완료 기준**: 새로 열어도 이력 확인, 사용량 수치 출처 표시.

### 4단계: PM과 단일 위임

1. PM = 특수 에이전트. 시스템 프롬프트로 `direct | delegate | split` 결정과 작업 명세를 **구조화 JSON**으로 출력하게 함.
2. PM에게는 프로젝트 지침 + 팀 구성 요약 + 사용자 메시지만 전달 (전체 대화 재전달 금지).
3. 작업자 결과는 요약·변경 파일·검증 결과만 PM에 반환. PM 호출은 이벤트(완료·실패·질문) 시에만.
4. `messages` 테이블, PM 채팅 UI, 고용 제안(사용자 확정).
5. **완료 기준**: 목표·범위·완료 조건 전달 후 결과 취합.

### 5단계: 의존성·병렬 실행·worktree

1. `task_dependencies`, 스케줄러: 의존성 해소 + 동시 실행 상한(PM 호출 포함) 준수.
2. 코드 수정 작업은 `.mefice/worktrees/<task-id>` + `mefice/<task-id>` 브랜치. 원본 작업 트리는 건드리지 않음.
3. 결과 diff 표시, 사용자 확정 시 병합(충돌 시 표시만, 자동 해결 없음).
4. 수정 범위 겹침 감지 시 순차 실행.
5. **완료 기준**: 독립 두 작업 병렬 실행, 파일 충돌 격리.

### 6단계: 고용 UI와 사무실

1. 역할 템플릿(백엔드/프론트엔드/QA/리뷰어/조사), 능력·권한·실행 설정·제한 선택.
2. 2D 사무실 (Canvas 또는 DOM+CSS), 상태 → 시각 표현 매핑(스펙 4.5), 대기 시 로컬 랜덤 이동.
3. `document.visibilitychange`로 숨김 시 애니메이션 정지.
4. **완료 기준**: 고용·상태 표시·대기 이동.

### 7단계: 효율·장애 검증

1. 비교 시나리오(작은 수정 / 독립 두 작업 / 순차 의존)로 직접 CLI vs Mefice 직접 실행 vs PM 분배 측정.
2. 중단·앱 강제 종료 후 상태 일치 확인. 창 닫기 → 트레이, 종료 시 진행 작업 확인 다이얼로그.
3. **완료 기준**: 스펙 13장 합격 조건 체크리스트 통과.

## 영향성

| 영향 대상 | 영향 내용 |
|---|---|
| 사용자 프로젝트 폴더 | 1단계: 읽기 전용. 5단계부터 `.mefice/worktrees` 생성 (원본 브랜치·작업 트리 미변경) |
| 사용자 홈 | 앱 데이터 디렉토리(`~/Library/Application Support/dev.mefice.app`)에 DB 생성 |

## Breaking Changes

없음 (신규 프로젝트).

## 위험도

**MEDIUM** — 신규 코드라 회귀 위험은 없지만, 런타임(Claude Code CLI) 출력 포맷 의존성과 프로세스·worktree 관리가 핵심 리스크.

## 주의사항

- 프론트엔드에 임의 셸 실행 권한(shell plugin)을 노출하지 않는다. 모든 외부 프로세스는 Rust 명령 경계에서만 실행.
- 사용자 저장소의 기존 변경을 덮어쓰거나 reset/checkout 하지 않는다.
- 대기·상태 확인을 위한 모델 호출 0회 (스펙 13장).
- stream-json 이벤트 스키마는 런타임 버전에 따라 변할 수 있으므로 어댑터에서 알 수 없는 필드/타입은 무시하고 원문을 보존한다.

## 검증 방법 (1단계)

1. `pnpm tauri build --debug` 또는 `cargo check` 성공, `pnpm build`(tsc) 성공.
2. Rust 단위 테스트: 마이그레이션 적용·재적용(멱등), 설정 저장·조회, 프로젝트 저장·조회.
3. 수동: `pnpm tauri dev` → 폴더 선택 → 지침·동시 실행 수 변경 → 종료 → 재실행 시 복원 확인.

## 참조 규칙

- `Mefice-Spec.md` 7·8·10·12장 — 기술 구조, 데이터 모델, 로컬 권한, 단계별 완료 기준
