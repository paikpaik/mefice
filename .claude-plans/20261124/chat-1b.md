# chat-1b — 대화 화면 보완 (`@파일`, 이미지, 슬래시 명령, 모델 선택, 컨텍스트·비용)

## 플랜 실행 이력

### 완료: 2026-11-24

**결과**: 성공 (불안정 테스트 1건 기록)

**실제 변경 파일**:
- `db/migrations.rs` — v8 `runs.attachments`
- `runtime/mod.rs` — `UserMessage.images`, `TurnResult.context_tokens/context_window`, `Compacted` 이벤트, `SlashCommand`, `ImageAttachment`·`validate_images`, `Parsed::Commands`
- `runtime/claude_code.rs` — 이미지 메시지·`set_model` 인코딩, 초기화 응답 명령 목록, 압축 이벤트, 로컬 명령 출력 알림, 직전 호출 토큰·컨텍스트 창
- `task.rs` — Run 첨부 저장·꺼내기(보낸 뒤 비움)
- `executor.rs` — 메시지 이미지, 모델 전환, 명령 목록 보관·이벤트, 강제 종료 테스트에 자식 정리 가드·진단 출력
- `chat.rs` — 첨부 포함 시작·보내기·재개, 파일 목록(.gitignore 반영, 무거운 폴더 제외)·검색
- `commands.rs`, `lib.rs` — `search_project_files`, `get_slash_commands`, 모델 즉시 전환
- `src/chat/Composer.tsx` (신규) — `@` 자동완성, `/` 메뉴(`/clear`는 새 대화), 이미지 첨부(붙여넣기·끌어다 놓기·선택), 모델 선택
- `src/chat/ChatScreen.tsx` — 새 입력창 연결, 컨텍스트·비용 표시줄, 이미지 장수·압축 표시
- `src/api/index.ts`, `styles.css`

**계획과의 차이**:
- 압축 결과를 문자열 알림 대신 구조화 이벤트(`Compacted`)로 저장 — 압축 직후 컨텍스트 표시가 이전 값으로 남던 문제 때문

**검증**:
- 단위 테스트 65개 통과, clippy 경고 0
- 실제 CLI 통합 테스트 `live_대화_보완기능`: 이미지 → "빨강", `@파일` → "KIWI-42", 모델 전환 → "Sonnet 5.5", `/compact` → 44,150 → 9,486 토큰, 컨텍스트 수치·명령 80개 수신
- 기존 통합 테스트 회귀: 10/11 통과. `live_앱_강제종료_후_복구`가 2회 시간 초과(자식 실행이 120초 안에 도구 명령을 시작하지 않음) 후 단독 4회 연속 통과 → **불안정 테스트로 기록**, 실패 시 자식 실행 상태를 출력하는 진단 추가. 실패 시 자식이 남던 문제(이전 세션부터 2개 잔류)를 Drop 가드로 수정
- 화면: 목 API 검증 페이지 캡처(파일 자동완성 팝업, 이미지 장수, 압축 알림, 컨텍스트·비용 표시줄)
- 실제 앱 DB v7 → v8 마이그레이션 확인

**잔존 작업**:
- 사고 과정 표시: 런타임이 내용을 거의 보내지 않아 "생각 중" 표시만 유지
- `live_앱_강제종료_후_복구` 불안정 원인 확인 (다음 실패 시 진단 출력으로)

---

## 목표

스펙 v0.2 A단계 1번의 나머지. 공식 Claude Code 대화 UI의 보조 기능을 갖춘다.

## 사전 실험 결과 (Claude Code 2.1.304, haiku)

| 항목 | 결과 |
|---|---|
| `@파일` | 메시지 속 `@경로`를 런타임이 직접 읽어 붙인다 (도구 호출 없이 내용 답변) |
| 이미지 | `message.content`를 `[{type:image, source:{type:base64,...}}, {type:text}]`로 보내면 인식 |
| 슬래시 명령 | `/compact`를 메시지로 보내면 압축 (`system/compact_boundary`: pre/post 토큰). 쓸 수 있는 명령 목록은 `initialize` 응답의 `commands` |
| 모델 전환 | `control_request{subtype:set_model, model}` → 즉시 적용 (`<local-command-stdout>` 알림) |
| 컨텍스트 | `result.usage.iterations` 마지막 항목(직전 호출 토큰), `modelUsage.*.contextWindow` |

## 설계

- `@파일`: 업무 폴더 파일 목록 명령(git이면 `ls-files`로 .gitignore 반영, 아니면 폴더 탐색·무거운 폴더 제외) → 입력창 자동완성. 내용 첨부는 런타임에 맡긴다
- 이미지: 붙여넣기·끌어다 놓기 → base64로 메시지에 첨부. 큐에서 시작하는 Run을 위해 `runs.attachments`에 보관(장수·크기 제한). 이벤트에는 장수만 기록
- 슬래시 명령: `/`로 시작하면 메뉴. `/clear`는 새 대화(화면 처리), 나머지는 메시지로 전달. 목록은 초기화 응답에서 받아 보관
- 모델: 입력창에서 선택, 실행 중 대화에는 `set_model`로 즉시 적용
- 컨텍스트·비용: 턴 결과에 직전 호출 토큰·컨텍스트 창 저장 → "컨텍스트 x / 창", 대화 비용(정가 환산)
- 알림: 압축 결과, 런타임 로컬 명령 출력

## 변경 범위

| 파일 | 변경 |
|---|---|
| `db/migrations.rs` | v8: `runs.attachments` |
| `runtime/*` | `UserMessage.images`, `TurnResult.context_tokens/context_window`, 압축·로컬 명령 알림, 명령 목록 파싱, 이미지 메시지·`set_model` 인코딩 |
| `task.rs` | Run 첨부 저장·조회 |
| `executor.rs` | 메시지에 이미지, 모델 전환, 명령 목록 보관 |
| `chat.rs`, `commands.rs` | 첨부 포함 시작·보내기, 파일 목록, 명령 목록, 모델 즉시 전환 |
| `src/chat/*` | 자동완성(@, /), 이미지 첨부, 모델 선택, 컨텍스트·비용 표시 |

## 위험도

**LOW~MEDIUM** — 메시지 인코딩 변경(문자열 → 이미지 포함 배열)은 첨부가 있을 때만.

## 검증 방법

- 단위: 파서(압축·명령 목록·컨텍스트), 인코딩(이미지·모델), 파일 목록(.gitignore 반영)
- 실제 CLI 통합: 이미지 첨부 대화, `@파일`, 대화 중 모델 전환, `/compact` 알림, 컨텍스트 수치
- 기존 통합 테스트 회귀
- 화면 캡처 검증
