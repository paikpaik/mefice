/** 아직 구현되지 않은 화면. 어떤 단계에서 구현되는지 표시한다. */
export function Placeholder({ title, stage, description }: { title: string; stage: number; description: string }) {
  return (
    <div className="placeholder">
      <h2>{title}</h2>
      <p className="muted">{description}</p>
      <span className="badge">구현 예정 · {stage}단계</span>
    </div>
  );
}
