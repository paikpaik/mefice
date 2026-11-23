# chat-1a — 대화 화면 핵심 (공식 Claude Code 대화 UI와 같은 기능)

## 플랜 실행 이력

### 완료: 2026-11-24

**결과**: 성공

**실제 변경 파일**:
- `db/migrations.rs` — v7 `approvals.suggestions`
- `agent.rs` — `ensure_assistant`(kind `assistant`), `list_workers`는 `worker`만, PM·어시스턴트 보관 금지
- `runtime/mod.rs`, `runtime/claude_code.rs` — `full_settings`·`keep_alive`·`stream_partial`, `ToolUse.input`, `Parsed::TextDelta`·`Activity`, 승인 제안 파싱, `encode_allow/deny`, `session_permissions`, `with_answers`, `encode_set_mode`, `tool_detail`
- `task.rs` — 승인 제안 저장, 입력 대기 Run을 동시 실행 상한에서 제외
- `executor.rs` — 프로세스 유지·입력 대기·15분 유휴 종료, 턴 중단, 모드 전환, `run-delta`·`run-activity`, `decide`(이번만/세션/거부, 질문 답, 거부 사유)
- `lifecycle.rs` — 입력 대기 대화는 종료 확인 대상에서 제외
- `pm.rs`, `office.rs` — 어시스턴트는 페르소나 없음, 사무실 제외
- `chat.rs` (신규), `commands.rs`, `lib.rs` — 대화 명령 7개
- `src/chat/` (신규) — `ChatScreen.tsx`, `Markdown.tsx`, `ToolCard.tsx`, `RequestCards.tsx`
- `App.tsx`(기본 탭 대화), `api/index.ts`, `styles.css`, `Team.tsx`·`Tasks.tsx`(어시스턴트 제외), `package.json`(`react-markdown`, `remark-gfm`)

**계획과의 차이**: 없음

**검증**:
- 단위 테스트 62개, 실제 CLI 통합 테스트 10개 통과 (`live_대화_흐름` 추가: 프로세스 하나로 7턴, 글자 조각 185개, 턴 중단 후 이어서 대화, 세션 허용 후 같은 명령 무승인 + 사용자 폴더 `.claude` 없음, 질문 답 반영, 대화 중 자동 수정 전환)
- 화면: 목 API 검증 페이지 캡처로 마크다운(표·코드 복사), 도구 카드(바뀐 줄), 권한·질문 카드, 할 일 목록, 입력창 확인. 메시지 카드가 세로 flex에서 찌그러지는 CSS 문제 발견·수정
- 실제 앱 DB v6 → v7 마이그레이션 확인

**잔존 작업 (1-b)**: `@파일` 언급, 이미지 첨부, 슬래시 명령, 입력창 모델 선택, 컨텍스트·사용량 표시, 사고 과정 표시

---

## 목표

스펙 v0.2 A단계 1번. 간단한 일을 처리하는 기본 화면으로, CLI·VS Code 확장·Claude 앱의 대화 UI와 같은 기능을 Mefice 디자인으로 구현한다. 같은 엔진·같은 설정이면 터미널과 같은 비용이어야 한다.

## 사전 실험 결과 (Claude Code 2.1.304, haiku)

| 항목 | 결과 |
|---|---|
| 글자 스트리밍 | `--include-partial-messages` → `stream_event`의 `text_delta` 약 20ms 간격 |
| 도구 입력 스트리밍 | `input_json_delta` (도구 카드 미리 표시 가능) |
| 진행 표시 | `system/status: requesting` |
| 사고 과정 | `thinking_delta`는 오지만 내용이 거의 비어 있음 → "생각 중" 표시만 |
| 할 일 목록 | `TodoWrite` 도구 입력에 전체 목록(content, status, activeForm)이 매번 옴 → 마지막 것만 표시 |
| 질문 | `AskUserQuestion`이 `can_use_tool`로 옴, `updatedInput.answers`로 답하면 이어짐 |
| 계획 승인 | 계획 모드에서 `ExitPlanMode`가 `can_use_tool`로 옴(`input.plan`), 허용하면 일반 모드로 실행 |
| 모드 전환 | `control_request{subtype: set_permission_mode, mode}` → 즉시 적용 |
| 표준 모드 비용 | 캐시가 데워진 뒤 터미널과 같음 ($0.0045 vs $0.0044) |
| **세션 허용 주의** | 제안(`permission_suggestions`)을 그대로 적용하면 `addRules`가 **사용자 폴더 `.claude/settings.local.json`에 영구 저장**됨 (실측). `destination`을 `session`으로 바꾸면 세션에만 적용되고 파일이 생기지 않음 (실측) |

## 설계

- **대화 = Task(mode `chat`) + Run 여러 개**. 기본 에이전트는 시스템 프롬프트를 덧붙이지 않는 "어시스턴트"(kind `assistant`, 팀 목록·위임 대상에서 제외).
- **프로세스 유지**: 대화 Run은 턴이 끝나도 stdin을 닫지 않고 다음 메시지를 기다린다(`waiting_for_input`). 터미널과 같은 동작이며 매 턴 프로세스 시작 지연이 없다. 대기 중 Run은 동시 실행 상한에서 제외한다. 15분 동안 입력이 없으면 닫고, 다음 메시지는 `--resume`으로 새 Run.
- **턴 중단(Esc)**: interrupt만 보내고 프로세스는 유지 (기존 "중단"은 Run 종료).
- **스트리밍**: 글자·진행 상태는 저장하지 않는 임시 이벤트(`run-delta`, `run-activity`)로 화면에만 보내고, 완성된 메시지는 기존처럼 저장한다.
- **도구 상세**: Edit·Write·TodoWrite·Bash·AskUserQuestion·ExitPlanMode는 카드 표시용 입력을 잘라서 저장(diff·할 일·질문·계획 표시).
- **승인 응답 3종**: 이번만 허용 / 이 세션 동안 허용 / 거부. 세션 허용은 `addRules`의 `destination`을 `session`으로 바꾸고, 그 외 제안은 `destination`이 `session`인 것만 보낸다 → 사용자 폴더에 설정 파일을 만들지 않는다.
- **실행 설정**: 표준(사용자 Claude Code 설정 그대로: `--strict-mcp-config`·`--tools` 없음) / 절약(도구 제한). 대화별로 저장, 기본값 표준.
- **모드**: 수정 전 확인(default) / 자동 수정(acceptEdits) / 계획(plan). 대화 중 전환.

## 변경 범위

| 파일 | 변경 내용 |
|---|---|
| `db/migrations.rs` | v7: `approvals.suggestions` |
| `agent.rs` | `ensure_assistant`, `list_workers`는 kind `worker`만 |
| `runtime/mod.rs` | `RunConfig.full_settings`·`keep_alive`·`stream_partial`, `ToolUse.input`(상세), `Parsed::Delta`·`Activity` |
| `runtime/claude_code.rs` | 인자 구성, 부분 메시지·상태 파싱, 도구 상세 추출, 승인 응답(세션 규칙 변환·질문 답), 모드 전환·턴 중단 인코딩 |
| `task.rs` | 승인 제안 저장, 대기 Run을 활성 수에서 제외 |
| `executor.rs` | 프로세스 유지·대기 상태·유휴 종료, 턴 중단, 모드 전환, 임시 이벤트 전송, 승인 결정 3종 |
| `chat.rs` (신규) | 대화 시작·보내기·목록·상세, 대화 설정 |
| `commands.rs`, `lib.rs` | 대화 명령, 승인 응답 확장 |
| `src/chat/*` (신규) | 대화 화면: 세션 목록, 메시지(마크다운·코드 복사), 도구 카드(diff), 할 일, 승인·질문·계획 카드, 입력창(한글 조합 중 Enter 무시) |
| `package.json` | `react-markdown`, `remark-gfm` |

## 위험도

**MEDIUM** — 실행 관리자에 Run 수명 변경(프로세스 유지)이 들어간다. 기존 작업·PM 흐름은 `keep_alive=false`로 그대로 동작해야 한다.

## 주의사항

- 세션 허용이 사용자 폴더에 영구 규칙을 쓰지 않는지 통합 테스트로 확인한다.
- 한글 입력: 조합 중(`isComposing`) Enter는 전송하지 않는다.
- 대기 중 Run도 앱 종료·재시작 복구 대상이다 (기존 경로 재사용).

## 작업 단계

1. 런타임 계층: 설정·인자, 부분 메시지 파싱, 도구 상세, 승인 응답 인코딩 + 단위 테스트
2. 실행 관리자: 프로세스 유지·턴 중단·모드 전환·임시 이벤트 + 실제 CLI 통합 테스트
3. 대화 백엔드(`chat.rs`)와 명령
4. 대화 화면
5. 회귀 (v0.1 단위·통합 테스트), 화면 검증

## 검증 방법

- 단위: 파서(부분 메시지·도구 상세), 세션 규칙 변환, 대기 Run 상한 제외
- 실제 CLI 통합: 같은 프로세스로 2턴 대화, 턴 중단 후 이어서 대화, 세션 허용 후 같은 명령 무승인 + 사용자 폴더에 `.claude` 없음, 질문 답변, 모드 전환
- 기존 통합 테스트 10개 회귀 통과
- 화면: 목 API 검증 페이지 스크린샷
