# mefice-mvp — Mefice MVP 전체 구현 계획 (스펙 12장 7단계 기준)

## 플랜 실행 이력

### 6단계 완료: 2026-11-22

**결과**: 성공

**런타임 검증 (Claude Code 2.1.304)**:
- `--max-budget-usd` → `subtype: error_max_budget_usd`, `terminal_reason: budget_exhausted`
- `--max-turns`(도움말에 없음) → `subtype: error_max_turns`
- 시간 제한 옵션은 없음 → 실행 관리자 타이머로 구현

**설계**:
- 역할 템플릿 5종(백엔드·프론트엔드·QA·리뷰어·조사)을 코드에 내장. 코드 수정 역할은 worktree 격리를 전제로 "편집 자동 허용", 리뷰어·조사는 읽기 전용
- 고용 프로필(`agents.profile` JSON): 템플릿, 외형(아바타·색상), 자리, 전문 분야, 수행 능력, 작업 범위, 제한(시간·사용량·턴)
- 전문 분야·수행 능력·작업 범위는 시스템 프롬프트와 PM 팀 요약에 반영. 작업 범위는 지침이며 권한 강제 아님
- 수행 능력에 "코드 리뷰"가 있으면 사용량 역할을 review로 집계 (스펙 5장 선택적 검토)
- 제한: 사용량·턴은 런타임 인자, 시간은 타이머 → 사용자 중단과 같은 절차로 멈추되 결과는 실패로 기록(PM이 실패 보고를 받도록)
- 사무실: SVG 한 장, 캐릭터 이동은 rAF로 transform만 직접 갱신(React는 자세·말풍선 변경 시에만 렌더), 통로·책상 사이 길 경유 경로, 창 숨김 시 정지, reduced-motion 시 즉시 이동
- 상태 매핑(스펙 4.5): 작업 없음→휴게 공간 순회, 실행 대기→자리 `…`, 작업 중→자리 타이핑, 입력 필요→`?`, 오류→`!`(10분 또는 확인 클릭까지), 완료→`✓` 8초 후 휴식
- 상태 갱신은 run-updated 이벤트 기반(300ms 묶음), 시간 경과 판단은 로컬 시계. 백엔드 폴링·모델 호출 없음
- 캐릭터 스프라이트는 `office/Character.tsx`로 분리 (아트 교체 지점)

**실제 변경 파일**:
- `db/migrations.rs` — v6: `agents.profile`
- `agent.rs` — `AgentProfile`, `Limits`, 자리 자동 배정·중복 방지, `usage_role`, `assign_missing_desks`
- `templates.rs` (신규) — 역할 템플릿·수행 능력 목록
- `office.rs` (신규) — 에이전트별 사무실 상태·캐릭터 개요
- `runtime/*` — `RunConfig.limits`, `--max-budget-usd`/`--max-turns`, 제한 사유 메시지
- `executor.rs` — 시간 제한 타이머, 제한 사유 표시
- `pm.rs` — 프로필을 지침·팀 요약에 반영, 역할별 Run 생성
- `commands.rs` — `list_templates`, `get_office_state`, `get_agent_overview`, 직접 실행·재실행에서 `usage_role` 사용
- `task.rs` — `create_run`을 테스트 전용으로 (역할 없이 Run 생성 방지)
- `usage.rs` — `agent_usage`
- `src/office/` (신규) — `Office.tsx`, `Character.tsx`, `layout.ts`, `AgentPanel.tsx`, `hire.ts`
- `src/screens/Team.tsx` — 템플릿 선택 → 고용 양식(외형·자리·전문 분야·수행 능력·범위·도구·실행 설정·제한·지침)
- `src/screens/PmChat.tsx` — PM 고용 제안을 템플릿 기본값으로 고용
- `src/screens/Placeholder.tsx` 삭제, `App.tsx`, `api/index.ts`, `styles.css`

**발견·수정한 버그**:
- 작업 화면에서 PM 목표를 이어서/다시 실행하면 Run 역할이 worker로 기록되어 PM 결정 처리가 건너뛰어지던 문제 → `usage_role()`로 역할 지정
- 사무실 엔진이 첫 렌더의 상태를 비교해 매 프레임 React 재렌더를 일으키던 문제 → 엔진 내부에 마지막 반영 값 보관

**검증**:
- 단위 테스트 51개 통과 (자리 배정·중복·기존 작업자 배정, 프로필 정리·검토 역할, 템플릿 도구 제약, 상태→캐릭터 매핑, 제한 인자·메시지), clippy 경고 0, `pnpm build` 성공
- 실제 CLI 통합 테스트 7개 통과 (턴 제한 추가, 이전 단계 회귀 포함)
- 화면 검증: 목 API를 붙인 검증 페이지를 헤드리스 Chrome(CDP)으로 실제 시간 3초·12초 캡처 → 상태별 말풍선, 완료 8초 후 휴식 전환, 휴게 공간 이동·통로 경유·걷기 자세, 캐릭터 상세 패널 확인
- 실제 앱 실행으로 DB v5 → v6 마이그레이션 확인

**잔존 작업**:
- 시간 제한 타이머는 통합 테스트 없음 (분 단위라 테스트 비용이 큼). 사용자 중단 경로를 재사용하며 그 경로는 통합 테스트로 검증됨
- 최종 아트 스타일 미정 (스펙 14장) — `Character.tsx`만 교체하면 됨
- 사무실 꾸미기·고급 캐릭터 표현은 MVP 이후 (스펙 11장)

### 5단계 완료: 2026-11-22

**결과**: 성공

**설계**:
- worktree 위치: 앱 데이터 폴더 `worktrees/project-<id>/task-<id>` (계획의 `.mefice/worktrees`에서 변경 — 사용자 레포에 폴더를 만들지 않기 위해), 브랜치 `mefice/task-<id>`
- 격리 대상: 위임 작업 + 쓰기 가능한 직접 실행(체크박스로 끌 수 있음). PM은 메인 폴더(미커밋 변경 포함 현재 상태를 봐야 함)
- worktree는 Run 시작 시 생성, 같은 작업의 재실행·이어서 실행은 같은 경로 재사용 (런타임 세션이 cwd에 묶여 있어 경로가 바뀌면 `--resume` 불가)
- 의존 작업은 선행 작업 브랜치 위에서 시작 (선행이 여럿이면 worktree에서 병합)
- 실행 종료 시 Mefice 브랜치에 스냅샷 커밋(`--no-verify`, 작성자 Mefice)
- 병합: 사용자 확정 시에만. ① `git merge-tree --write-tree`로 충돌 사전 확인(작업 트리 무변경) ② 사용자 미커밋 변경과 파일 겹침 확인 ③ 통과 시 `merge --no-ff`. `merge --abort`에 의존하지 않음(미커밋 변경 복원 불가 사례 때문)
- PM 스키마: tasks 최대 5개 + `key`/`paths`/`dependsOn`. 경로 겹침 + 순서 없음 → Mefice가 자동 순차화. Git 격리 불가 시 쓰기 작업을 한 줄로 세움
- 선행 작업이 성공하지 못하면 뒤 작업 취소(사유 기록), 계획 단위로 모두 끝나면 PM 1회 호출로 묶음 보고

**실제 변경 파일**:
- `db/migrations.rs` — v5: `tasks.isolation/plan_run_id/integration`, `task_dependencies`, `workspaces`
- `git.rs` (신규) — worktree·스냅샷·diff·merge-tree 충돌 확인·미커밋 파일·병합
- `workspace.rs` (신규) — prepare/snapshot/diff/merge/discard/snapshot_all
- `task.rs` — 격리·계획·통합 필드, 의존성, `readiness`(Ready/Waiting/Blocked), `queued_runs`, `set_run_cwd`
- `pm.rs` — 스키마 확장, `create_plan`(병렬·순서·겹침·격리), 계획 단위 묶음 보고, git 기준 변경 파일
- `agent.rs` — `can_write`
- `executor.rs` — 의존성 기반 스케줄링·차단 취소, 작업 공간 준비(시스템 프롬프트에 커밋 금지 안내), 종료 스냅샷, worktree 루트
- `commands.rs` — `start_direct_task(isolated)`, `get_task_diff`, `merge_task`, `discard_task_workspace`, 재실행 시 통합 상태 초기화
- `lib.rs` — 시작 시 작업 공간 스냅샷 복구
- `src/components/DiffPanel.tsx` (신규), `Tasks.tsx`(격리 체크박스·변경 내역 탭·선행/통합 배지), `PmChat.tsx`, `App.tsx`, `api/index.ts`, `styles.css`

**계획과의 차이**:
- worktree 위치를 레포 밖으로 변경
- 결과 취합을 작업별이 아니라 계획 단위로 묶음 (PM 호출 수 절감)
- 시작 시 남은 변경 스냅샷 복구 추가

**검증**:
- 단위 테스트 45개 통과 (worktree·스냅샷·충돌 사전 확인·미커밋 겹침 거부·사용자 변경 보존·의존 작업 기준 브랜치·강제 종료 복구·계획 병렬/순서/겹침/격리 불가·묶음 보고·경로 겹침 판정), clippy 경고 0, `pnpm build` 성공
- 실제 CLI 통합 테스트 6개 통과 (이전 단계 회귀 포함)
  - 병렬·충돌 격리: 같은 파일을 다르게 고치는 두 작업이 실행 구간이 겹치게 병렬 실행, 실행 중 사용자 레포 무변경, A 병합 후 B는 충돌로 거부되고 레포 무변경
  - PM 병렬 분할: 독립 2개 작업 → "남은 작업 1개" → 둘 다 끝난 뒤 PM 1회 취합 → 두 결과 병합
- 실제 앱 실행으로 DB v4 → v5 마이그레이션 확인

**잔존 작업**:
- worktree는 커밋된 상태에서 시작하므로 사용자 미커밋 변경은 작업자에게 보이지 않음 (UI 안내로 대응)
- 병합 후 사용자 브랜치 훅(pre-merge-commit 등) 실패 시 병합은 정리되고 오류만 표시

### 4단계 완료: 2026-11-20

**결과**: 성공

**런타임 검증 (Claude Code 2.1.304)**:
- `--json-schema <schema>` → `result.structured_output`에 검증된 JSON. 내부적으로 `StructuredOutput` 도구 호출이며 호스트 승인 대상이 아님
- 스키마만으로는 응답 길이가 제한되지 않음 → PM 지침에 "reply는 짧게" 명시

**설계**:
- PM 결정: `mode`(direct/delegate/ask/hire/report) + `reply` + `tasks`(최대 1) + `hire`
- direct는 PM이 같은 Run 안에서 처리 → 작은 작업에 추가 호출 0회
- 목표 하나 = PM 세션 하나. 후속 메시지·결과 취합은 `--resume`으로 같은 세션 사용, 매 메시지에 팀 요약만 첨부
- 모델 호출 시점은 두 가지뿐: 사용자 메시지, 위임 작업 완료·실패 이벤트. 사용자 중단·앱 종료 시 PM 호출 안 함
- 작업자에게는 목표·범위·입력·완료 조건 + "작업 보고" 형식 요구. PM에게는 최종 보고(3000자 제한) + 도구 기록 기반 변경 파일만 전달
- 결정 처리는 턴 결과마다 (실행 중인 PM에 추가 메시지를 보내도 결정 손실 없음)

**실제 변경 파일**:
- `db/migrations.rs` — v4: `agents.kind`, `tasks.spec`, `messages`
- `pm.rs` (신규) — PM 지침·스키마, 실행 설정 스냅샷, 팀 요약, 작업자 지시문·결과 보고 구성, 메시지, `on_pm_turn`, `on_run_finished`, `deliver_to_pm`
- `agent.rs` — `kind`, `ensure_pm`, `list_workers`, PM 보관 금지
- `task.rs` — `parent_id`/`spec`, `create_goal`/`create_delegated`, `create_run_with_role`, `list_children`, `changed_files`, `last_result_text`
- `runtime/*` — `RunConfig.json_schema`, `TurnResult.structured`, StructuredOutput 요약
- `executor.rs` — PM 턴 훅, 실행 종료 훅(보고 전달: 실행 중 PM이면 메시지, 아니면 이어서 실행), `pm-message` 이벤트
- `commands.rs` — `list_goals`, `get_goal`, `pm_send`; 스냅샷 생성을 `pm::run_config`로 이동; 직접 실행에서 PM 제외
- `usage.rs` — `goal_usage` (목표 단위 PM/작업자 사용량)
- `src/screens/PmChat.tsx` (신규), `App.tsx`(기본 탭 PM, 작업 이동), `Tasks.tsx`, `Team.tsx`, `api/index.ts`, `styles.css`

**계획과의 차이**:
- 결정 형식을 텍스트 JSON 파싱 대신 런타임 `--json-schema`로 강제
- PM을 팀 목록 조회 시 자동 생성 (첫 대화 전에 모델 설정 가능)

**검증**:
- 단위 테스트 35개 통과 (위임 생성, 잘못된 agentId·PM 위임 거부, 완료 시에만 보고, 중단 시 PM 미호출, 변경 파일 추출, 실행 중 PM에 메시지 전달·종료 후 세션 이어서), clippy 경고 0, `pnpm build` 성공
- 실제 CLI 통합 테스트 4개 통과. PM 흐름: 사용자 → PM(delegate) → 배정 → 작업자 실행·파일 생성 → 완료 → PM(report). PM 호출 2회(계획+취합), 취합은 같은 세션 재개
- 실측 (haiku, 아주 작은 작업): PM $0.0034 vs 작업자 $0.0019 → 작은 작업은 위임이 손해. 직접 처리 우선 정책의 근거
- 실제 앱 실행으로 DB v3 → v4 마이그레이션 확인

**잔존 작업**:
- 병렬 분할(`tasks` 여러 개)과 의존성은 5단계에서 스키마 `maxItems` 해제와 함께
- 실행 중 질문 도구(`waiting_for_input`)는 미지원 — PM `ask`와 작업자 보고로 대체

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
