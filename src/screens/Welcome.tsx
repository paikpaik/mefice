import { useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { api, errorMessage, type ProjectView } from "../api";

interface Props {
  onOpened: (project: ProjectView) => void;
}

/** 프로젝트가 없을 때의 시작 화면. 스펙 3장 흐름 1: 프로젝트 폴더 선택. */
export function Welcome({ onOpened }: Props) {
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  async function pickFolder() {
    setError(null);
    const selected = await open({ directory: true, multiple: false, title: "프로젝트 폴더 선택" });
    if (typeof selected !== "string") return;
    setBusy(true);
    try {
      onOpened(await api.openProject(selected));
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <main className="welcome">
      <h1>
        Mefice <span className="muted">미피스</span>
      </h1>
      <p className="muted">나만의 AI 개발팀 사무실. 먼저 작업할 프로젝트 폴더를 선택하세요.</p>
      <button className="primary" onClick={pickFolder} disabled={busy}>
        {busy ? "여는 중…" : "프로젝트 폴더 열기"}
      </button>
      <p className="hint">폴더 내용은 읽기만 하며, 기존 변경 사항을 수정하거나 초기화하지 않습니다.</p>
      {error && <p className="error">{error}</p>}
    </main>
  );
}
