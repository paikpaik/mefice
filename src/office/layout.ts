/**
 * 사무실 배치 (논리 좌표, SVG viewBox 1000×640).
 * 위치 이동 경로는 통로(CORRIDOR_Y)와 책상 사이 길(aisle)을 거치게 해 책상을 가로지르지 않게 한다.
 */

export interface Point {
  x: number;
  y: number;
}

export interface Location extends Point {
  id: string;
  /** 통로에서 이 위치까지 가는 경로 (마지막 점 = 위치) */
  approach: Point[];
  /** 이 위치에서 앉는지 */
  sit?: boolean;
}

export const VIEW = { width: 1000, height: 640 };
export const CORRIDOR_Y = 430;

export const DESK_W = 110;
export const DESK_H = 40;

export function deskPos(index: number): Point {
  const col = index % 5;
  const row = Math.floor(index / 5);
  return { x: 110 + col * 170, y: row === 0 ? 110 : 270 };
}

export const PM_DESK: Point = { x: 920, y: 110 };

function seatLocation(id: string, desk: Point, aisleX: number): Location {
  const seat = { x: desk.x, y: desk.y + 52 };
  return {
    id,
    ...seat,
    sit: true,
    approach: [
      { x: aisleX, y: CORRIDOR_Y },
      { x: aisleX, y: seat.y },
      seat,
    ],
  };
}

export function deskLocation(index: number): Location {
  const d = deskPos(index);
  return seatLocation(`desk-${index}`, d, d.x + 85);
}

export const PM_LOCATION: Location = seatLocation("desk-pm", PM_DESK, PM_DESK.x - 60);

/** 사장 자리 (PM 자리 아래) */
export const BOSS_DESK: Point = { x: 920, y: 270 };
export const BOSS_LOCATION: Location = seatLocation("desk-boss", BOSS_DESK, BOSS_DESK.x - 60);

function spot(id: string, x: number, y: number, sit = false): Location {
  return { id, x, y, sit, approach: [{ x, y: CORRIDOR_Y }, { x, y }] };
}

/** 대기 중 들르는 곳 */
export const LOUNGE: Location[] = [
  spot("sofa-1", 95, 575, true),
  spot("sofa-2", 155, 575, true),
  spot("sofa-3", 215, 575, true),
  spot("coffee", 420, 545),
  spot("books", 600, 560),
  spot("plant", 735, 575),
  spot("water", 900, 545),
];

/** 출발 위치 → 도착 위치 경로 */
export function route(from: Location | null, to: Location): Point[] {
  if (!from) return [{ x: to.x, y: to.y }];
  if (from.id === to.id) return [];
  const back = from.approach.slice(0, -1).reverse();
  return [...back, ...to.approach];
}
