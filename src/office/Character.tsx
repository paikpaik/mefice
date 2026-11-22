/**
 * 캐릭터 스프라이트 (도형 기반 플레이스홀더).
 *
 * 최종 아트 스타일은 미정(스펙 14장)이라, 사무실 로직과 분리된 이 컴포넌트만 바꾸면 되도록 만든다.
 * 원점(0,0)은 발 위치. 위치 이동은 바깥 <g>의 transform으로 사무실 엔진이 직접 갱신한다.
 */

export type Pose = "stand" | "walk" | "sit" | "type";
export type BubbleKind = "question" | "error" | "done" | "wait" | null;

export const AVATARS = ["a", "b", "c", "d", "e", "f"] as const;
export const AVATAR_LABEL: Record<string, string> = {
  a: "짧은 머리",
  b: "긴 머리",
  c: "모자",
  d: "안경",
  e: "묶은 머리",
  f: "헤드폰",
};

const SKIN = "#f2c9a0";
const HAIR = "#3b2f2a";

interface Props {
  color: string;
  avatar: string;
  pose?: Pose;
  bubble?: BubbleKind;
  badge?: string | null;
}

function Hair({ avatar }: { avatar: string }) {
  switch (avatar) {
    case "b":
      return <path d="M-11,-38 Q0,-50 11,-38 L12,-22 L8,-24 L8,-34 L-8,-34 L-8,-24 L-12,-22 Z" fill={HAIR} />;
    case "c":
      return (
        <g>
          <path d="M-11,-36 Q0,-50 11,-36 Z" fill="#c0392b" />
          <rect x="0" y="-37" width="14" height="3" rx="1.5" fill="#c0392b" />
        </g>
      );
    case "d":
      return (
        <g>
          <path d="M-10,-36 Q0,-48 10,-36 L10,-33 L-10,-33 Z" fill={HAIR} />
          <g stroke="#222" strokeWidth="1.3" fill="none">
            <circle cx="-4" cy="-30" r="3" />
            <circle cx="4" cy="-30" r="3" />
            <line x1="-1" y1="-30" x2="1" y2="-30" />
          </g>
        </g>
      );
    case "e":
      return (
        <g>
          <path d="M-10,-35 Q0,-48 10,-35 Z" fill={HAIR} />
          <circle cx="0" cy="-46" r="4.5" fill={HAIR} />
        </g>
      );
    case "f":
      return (
        <g>
          <path d="M-10,-35 Q0,-47 10,-35 Z" fill={HAIR} />
          <path d="M-11,-30 Q0,-48 11,-30" stroke="#333" strokeWidth="2" fill="none" />
          <rect x="-13" y="-33" width="4" height="7" rx="2" fill="#333" />
          <rect x="9" y="-33" width="4" height="7" rx="2" fill="#333" />
        </g>
      );
    default:
      return <path d="M-10,-35 Q0,-49 10,-35 Q6,-39 0,-39 Q-6,-39 -10,-35 Z" fill={HAIR} />;
  }
}

/** 상태 말풍선. 캐릭터 좌우 반전의 영향을 받지 않게 따로 그린다. */
export function CharacterBubble({ kind }: { kind: BubbleKind }) {
  if (!kind) return null;
  const spec = {
    question: { fill: "#f5b041", text: "?" },
    error: { fill: "#e05555", text: "!" },
    done: { fill: "#2f8a4c", text: "✓" },
    wait: { fill: "#9a9a96", text: "…" },
  }[kind];
  return (
    <g className={`bubble bubble-${kind}`} transform="translate(12,-58)">
      <rect x="-9" y="-11" width="18" height="16" rx="5" fill={spec.fill} />
      <path d="M-3,5 L-6,10 L2,5 Z" fill={spec.fill} />
      <text x="0" y="1" textAnchor="middle" fontSize="11" fontWeight="700" fill="#fff">
        {spec.text}
      </text>
    </g>
  );
}

export function Character({ color, avatar, pose = "stand", bubble = null, badge = null }: Props) {
  // bubble을 넘기면 함께 그린다 (미리보기 등 반전 없는 곳에서 사용)
  const seated = pose === "sit" || pose === "type";
  return (
    <g className={`char pose-${pose}`}>
      <ellipse cx="0" cy="1" rx="11" ry="3" fill="rgba(0,0,0,0.18)" />
      <g className="char-body">
        {/* 다리 */}
        {seated ? (
          <rect x="-8" y="-8" width="16" height="6" rx="2" fill="#4a4a55" />
        ) : (
          <g fill="#4a4a55">
            <rect className="leg leg-l" x="-7" y="-12" width="5" height="12" rx="2" />
            <rect className="leg leg-r" x="2" y="-12" width="5" height="12" rx="2" />
          </g>
        )}
        {/* 몸통 */}
        <rect x="-10" y="-25" width="20" height="16" rx="6" fill={color} />
        {/* 팔 (타이핑 시 앞으로) */}
        <g className="arms" fill={color}>
          <rect x="-13" y="-23" width="5" height="11" rx="2.5" />
          <rect x="8" y="-23" width="5" height="11" rx="2.5" />
        </g>
        {/* 머리 */}
        <circle cx="0" cy="-32" r="9" fill={SKIN} />
        <Hair avatar={avatar} />
        {badge && (
          <g transform="translate(0,-22)">
            <rect x="-7" y="-4" width="14" height="8" rx="2" fill="#fff" opacity="0.9" />
            <text x="0" y="2.5" textAnchor="middle" fontSize="6" fontWeight="700" fill="#333">
              {badge}
            </text>
          </g>
        )}
      </g>
      <CharacterBubble kind={bubble} />
    </g>
  );
}
