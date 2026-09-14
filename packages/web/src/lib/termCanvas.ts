/**
 * 工作画布（WorkCanvas）的滚动层纯函数：滚轮手势的轴向锁定、手势结束后的对齐目标、
 * 把活动列滚进视口。列宽与窗口高度这类排布几何在 lib/layout.ts，DOM 与事件接线在组件里。
 */

/**
 * 两次滚轮事件间隔超过它就算新手势。macOS 触控板的惯性事件间隔在 16–50ms，
 * 人手两次连续滑动之间通常远大于此；取 120ms 兼顾两边。
 */
export const WHEEL_GESTURE_GAP_MS = 120;
/** 锁定后改判轴向的门槛：另一根轴单次至少这么多像素、且比主轴大一倍以上 */
export const WHEEL_AXIS_FLIP_MIN_PX = 8;

export type WheelAxis = "x" | "y";

export interface WheelSample {
  deltaX: number;
  deltaY: number;
  timeStamp: number;
}

/**
 * 滚轮手势的轴向锁定。
 *
 * 触控板两指滑动几乎不可能只有一根轴，逐事件比较 |dx| / |dy| 会让一段横向滑动里
 * 混进几条被当成纵向的事件（终端跟着翻两行），反过来纵向滚终端时画布也会横漂。
 * 原生平台的做法是手势开始时定轴、整段手势只认这根轴，这里照做：
 *
 * - 手势第一条有位移的事件定轴（相等算纵向：终端是主要消费者，宁可少滚画布）；
 * - 同一手势内不改判，除非另一根轴单次位移明显更大（惯性还没停用户就换了方向滑，
 *   macOS 会立刻停掉惯性开始新手势，事件之间没有间隔可以判断）；
 * - 静默超过 WHEEL_GESTURE_GAP_MS 视为新手势重新定轴。
 */
export class WheelAxisLock {
  private axis: WheelAxis | null = null;
  private last = Number.NEGATIVE_INFINITY;

  /** 本事件归哪根轴；两轴都是 0 的空事件返回 null 且不改变状态 */
  classify(s: WheelSample): WheelAxis | null {
    const ax = Math.abs(s.deltaX);
    const ay = Math.abs(s.deltaY);
    if (ax === 0 && ay === 0) return null;
    if (this.axis !== null && s.timeStamp - this.last > WHEEL_GESTURE_GAP_MS) this.axis = null;
    this.last = s.timeStamp;
    const dominant: WheelAxis = ax > ay ? "x" : "y";
    if (this.axis === null) {
      this.axis = dominant;
    } else if (dominant !== this.axis) {
      const major = Math.max(ax, ay);
      const minor = Math.min(ax, ay);
      if (major >= WHEEL_AXIS_FLIP_MIN_PX && major > minor * 2) this.axis = dominant;
    }
    return this.axis;
  }

  reset(): void {
    this.axis = null;
    this.last = Number.NEGATIVE_INFINITY;
  }
}

const DOM_DELTA_LINE = 1;
const DOM_DELTA_PAGE = 2;

/** 把 WheelEvent 的 delta 折成像素：Firefox 的鼠标滚轮给的是行 / 页 */
export function wheelDeltaPx(delta: number, deltaMode: number, lineSize = 16, pageSize = 800): number {
  if (deltaMode === DOM_DELTA_LINE) return delta * lineSize;
  if (deltaMode === DOM_DELTA_PAGE) return delta * pageSize;
  return delta;
}

/**
 * 手势结束后的对齐目标：离最近一条列左边不超过 proximity 才吸过去，否则停在原地。
 * 用 CSS scroll-snap 做不到：手动改 scrollLeft 会被 Chrome 当作程序化滚动立刻吸附，
 * 触控板每条事件才几像素，永远滚不出吸附半径。
 *
 * @param lefts 各列左边相对画布内容盒原点的偏移（已扣 padding）
 * @param maxScroll scrollWidth - clientWidth；最后一列吸不到边时按能滚到的最远处算
 * @returns 目标 scrollLeft；null = 不用动
 */
export function settleTarget(
  scrollLeft: number,
  lefts: number[],
  proximity: number,
  maxScroll: number
): number | null {
  let best: number | null = null;
  let bestDist = Number.POSITIVE_INFINITY;
  for (const left of lefts) {
    const target = Math.min(Math.max(0, left), Math.max(0, maxScroll));
    const dist = Math.abs(target - scrollLeft);
    if (dist < bestDist) {
      bestDist = dist;
      best = target;
    }
  }
  if (best === null || bestDist > proximity || bestDist < 1) return null;
  return best;
}

/**
 * 把一列滚进视口需要的 scrollLeft：已经整列可见就不动；在左边露不全就对齐左边，
 * 在右边露不全就对齐右边（列比视口还宽时也按左边对齐）。
 *
 * @param viewport 画布内容盒宽度（clientWidth 扣掉左右 padding）
 * @param left / width 列相对内容盒原点的偏移与宽度
 */
export function revealScrollLeft(opts: {
  scrollLeft: number;
  viewport: number;
  left: number;
  width: number;
}): number | null {
  const { scrollLeft, viewport, left, width } = opts;
  if (width >= viewport) return Math.abs(scrollLeft - left) < 1 ? null : left;
  if (left < scrollLeft) return left;
  const right = left + width;
  if (right > scrollLeft + viewport) return right - viewport;
  return null;
}
