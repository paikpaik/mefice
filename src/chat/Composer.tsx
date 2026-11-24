import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  api,
  errorMessage,
  EVENTS,
  type ChatOptions,
  type ImageAttachment,
  type PermissionMode,
  type SlashCommand,
} from "../api";

const MODE_LABEL: Record<PermissionMode, string> = {
  default: "수정 전 확인",
  acceptEdits: "자동 수정",
  plan: "계획 먼저",
};

const MODELS: { value: string; label: string }[] = [
  { value: "", label: "기본 모델" },
  { value: "haiku", label: "Haiku (빠름·저렴)" },
  { value: "sonnet", label: "Sonnet" },
  { value: "opus", label: "Opus (고성능)" },
];

const MAX_IMAGES = 5;
const MAX_IMAGE_BYTES = 3_700_000;
const IMAGE_TYPES = ["image/png", "image/jpeg", "image/gif", "image/webp"];

/** Mefice가 직접 처리하는 명령 */
const BUILTIN: SlashCommand[] = [{ name: "clear", description: "새 대화 시작", argumentHint: "" }];

type Popup =
  | { kind: "file"; start: number; items: string[]; index: number }
  | { kind: "command"; items: SlashCommand[]; index: number };

function readImage(file: File): Promise<ImageAttachment> {
  return new Promise((resolve, reject) => {
    if (!IMAGE_TYPES.includes(file.type)) return reject(new Error(`지원하지 않는 이미지 형식: ${file.type || file.name}`));
    if (file.size > MAX_IMAGE_BYTES) return reject(new Error("이미지가 너무 큽니다 (약 3.7MB 이하)"));
    const r = new FileReader();
    r.onload = () => resolve({ mediaType: file.type, data: String(r.result).split(",")[1] ?? "" });
    r.onerror = () => reject(r.error);
    r.readAsDataURL(file);
  });
}

/** 커서 앞의 `@검색어` (공백 없이 이어진 부분) */
function mentionAt(text: string, caret: number): { start: number; query: string } | null {
  const m = /(^|\s)@([^\s@]*)$/.exec(text.slice(0, caret));
  return m ? { start: caret - m[2].length - 1, query: m[2] } : null;
}

interface Props {
  busy: boolean;
  options: ChatOptions;
  onOptions: (o: ChatOptions) => void;
  onSend: (text: string, images: ImageAttachment[]) => Promise<void>;
  onStop?: () => void;
  /** /clear */
  onNewChat?: () => void;
}

/** 대화 입력창: @파일 자동완성, /명령 메뉴, 이미지 첨부, 모드·모델·절약 설정 */
export function Composer({ busy, options, onOptions, onSend, onStop, onNewChat }: Props) {
  const [text, setText] = useState("");
  const [images, setImages] = useState<ImageAttachment[]>([]);
  const [sending, setSending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [popup, setPopup] = useState<Popup | null>(null);
  const [commands, setCommands] = useState<SlashCommand[]>([]);
  const [dragOver, setDragOver] = useState(false);
  const ref = useRef<HTMLTextAreaElement>(null);
  const fileInput = useRef<HTMLInputElement>(null);
  const searchSeq = useRef(0);

  useEffect(() => {
    api.getSlashCommands().then(setCommands).catch(() => {});
    const un = listen<SlashCommand[]>(EVENTS.runtimeCommands, ({ payload }) => setCommands(payload));
    return () => {
      un.then((f) => f());
    };
  }, []);

  // Esc: 팝업이 없으면 응답 멈추기
  useEffect(() => {
    if (!busy || !onStop) return;
    const h = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !popup) onStop();
    };
    window.addEventListener("keydown", h);
    return () => window.removeEventListener("keydown", h);
  }, [busy, onStop, popup]);

  const addFiles = async (files: FileList | File[]) => {
    setError(null);
    const list = Array.from(files).filter((f) => f.type.startsWith("image/"));
    if (images.length + list.length > MAX_IMAGES) {
      setError(`이미지는 한 번에 ${MAX_IMAGES}장까지 보낼 수 있습니다`);
      return;
    }
    try {
      const read = await Promise.all(list.map(readImage));
      setImages((prev) => [...prev, ...read]);
    } catch (e) {
      setError(errorMessage(e));
    }
  };

  // 입력이 바뀔 때 @파일·/명령 팝업 갱신
  const updatePopup = (value: string, caret: number) => {
    const m = mentionAt(value, caret);
    if (m) {
      const seq = ++searchSeq.current;
      api
        .searchProjectFiles(m.query)
        .then((items) => {
          if (seq === searchSeq.current) setPopup(items.length ? { kind: "file", start: m.start, items, index: 0 } : null);
        })
        .catch(() => setPopup(null));
      return;
    }
    if (/^\/[^\s]*$/.test(value)) {
      const q = value.slice(1).toLowerCase();
      const all = [...BUILTIN, ...commands.filter((c) => c.name !== "clear")];
      const items = all.filter((c) => c.name.toLowerCase().startsWith(q)).slice(0, 12);
      setPopup(items.length ? { kind: "command", items, index: 0 } : null);
      return;
    }
    setPopup(null);
  };

  const choose = (i: number) => {
    if (!popup) return;
    const el = ref.current;
    if (popup.kind === "file") {
      const caret = el?.selectionStart ?? text.length;
      const next = `${text.slice(0, popup.start)}@${popup.items[i]} ${text.slice(caret)}`;
      setText(next);
      const pos = popup.start + popup.items[i].length + 2;
      requestAnimationFrame(() => el?.setSelectionRange(pos, pos));
    } else {
      setText(`/${popup.items[i].name} `);
    }
    setPopup(null);
    el?.focus();
  };

  const send = async () => {
    const t = text.trim();
    if ((!t && images.length === 0) || sending) return;
    if (t === "/clear") {
      setText("");
      onNewChat?.();
      return;
    }
    setSending(true);
    setError(null);
    try {
      await onSend(t, images);
      setText("");
      setImages([]);
      setPopup(null);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setSending(false);
    }
  };

  const modelValue = options.model ?? "";
  const models = MODELS.some((m) => m.value === modelValue) ? MODELS : [...MODELS, { value: modelValue, label: modelValue }];

  return (
    <div
      className={`composer chat-composer ${dragOver ? "drag-over" : ""}`}
      onDragOver={(e) => {
        if (Array.from(e.dataTransfer.items).some((i) => i.type.startsWith("image/"))) {
          e.preventDefault();
          setDragOver(true);
        }
      }}
      onDragLeave={() => setDragOver(false)}
      onDrop={(e) => {
        e.preventDefault();
        setDragOver(false);
        addFiles(e.dataTransfer.files);
      }}
    >
      {popup && (
        <ul className="popup" role="listbox">
          {popup.kind === "file"
            ? popup.items.map((f, i) => (
                <li key={f} className={i === popup.index ? "active" : ""} onMouseDown={(e) => (e.preventDefault(), choose(i))}>
                  <span className="mono">{f.split("/").pop()}</span>
                  <span className="muted small mono">{f}</span>
                </li>
              ))
            : popup.items.map((c, i) => (
                <li key={c.name} className={i === popup.index ? "active" : ""} onMouseDown={(e) => (e.preventDefault(), choose(i))}>
                  <span className="mono">/{c.name}</span>
                  <span className="muted small">{c.description}</span>
                </li>
              ))}
        </ul>
      )}

      {images.length > 0 && (
        <div className="attachments">
          {images.map((img, i) => (
            <div key={i} className="thumb">
              <img src={`data:${img.mediaType};base64,${img.data}`} alt={`첨부 이미지 ${i + 1}`} />
              <button aria-label="첨부 삭제" onClick={() => setImages(images.filter((_, j) => j !== i))}>
                ✕
              </button>
            </div>
          ))}
        </div>
      )}

      <textarea
        ref={ref}
        rows={3}
        value={text}
        placeholder={
          busy
            ? "응답 중에도 이어서 보낼 수 있습니다"
            : "메시지 입력 · @로 파일 언급 · /로 명령 · 이미지 붙여넣기 (Enter 전송, Shift+Enter 줄바꿈)"
        }
        onChange={(e) => {
          setText(e.target.value);
          updatePopup(e.target.value, e.target.selectionStart);
        }}
        onPaste={(e) => {
          const files = Array.from(e.clipboardData.files).filter((f) => f.type.startsWith("image/"));
          if (files.length) {
            e.preventDefault();
            addFiles(files);
          }
        }}
        onKeyDown={(e) => {
          if (e.nativeEvent.isComposing) return; // 한글 조합 중
          if (popup) {
            const n = popup.items.length;
            if (e.key === "ArrowDown") return e.preventDefault(), setPopup({ ...popup, index: (popup.index + 1) % n } as Popup);
            if (e.key === "ArrowUp") return e.preventDefault(), setPopup({ ...popup, index: (popup.index - 1 + n) % n } as Popup);
            if (e.key === "Enter" || e.key === "Tab") return e.preventDefault(), choose(popup.index);
            if (e.key === "Escape") return e.preventDefault(), setPopup(null);
          }
          if (e.key === "Enter" && !e.shiftKey) {
            e.preventDefault();
            send();
          }
        }}
        onBlur={() => setTimeout(() => setPopup(null), 120)}
      />
      <div className="row composer-bar">
        <button className="icon-btn" title="이미지 첨부 (붙여넣기·끌어다 놓기도 됩니다)" onClick={() => fileInput.current?.click()}>
          이미지
        </button>
        <input
          ref={fileInput}
          type="file"
          accept={IMAGE_TYPES.join(",")}
          multiple
          hidden
          onChange={(e) => {
            if (e.target.files) addFiles(e.target.files);
            e.target.value = "";
          }}
        />
        <select
          value={options.permissionMode}
          onChange={(e) => onOptions({ ...options, permissionMode: e.target.value as PermissionMode })}
          title="수정 전 확인: 파일을 바꾸기 전에 묻습니다 · 자동 수정: 파일 수정은 묻지 않습니다 · 계획 먼저: 계획을 보여주고 승인받은 뒤 실행합니다"
        >
          {(Object.keys(MODE_LABEL) as PermissionMode[]).map((m) => (
            <option key={m} value={m}>
              {MODE_LABEL[m]}
            </option>
          ))}
        </select>
        <select
          value={modelValue}
          onChange={(e) => onOptions({ ...options, model: e.target.value || null })}
          title="실행 중인 대화에도 바로 적용됩니다"
        >
          {models.map((m) => (
            <option key={m.value} value={m.value}>
              {m.label}
            </option>
          ))}
        </select>
        <label className="row small" title="도구를 줄여 비용을 아낍니다 (약 1/3). 웹 조회·외부 연결 등은 쓸 수 없습니다. 다음 실행부터 적용됩니다.">
          <input type="checkbox" checked={options.economy} onChange={(e) => onOptions({ ...options, economy: e.target.checked })} />
          절약 모드
        </label>
        <span className="grow" />
        {error && <span className="error small">{error}</span>}
        {busy && onStop && (
          <button onClick={onStop} title="Esc">
            멈추기
          </button>
        )}
        <button className="primary" disabled={(!text.trim() && images.length === 0) || sending} onClick={send}>
          보내기
        </button>
      </div>
    </div>
  );
}
