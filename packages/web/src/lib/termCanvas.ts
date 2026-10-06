/**
 * 工作画布（WorkCanvas）的滚轮层纯函数：滚轮手势的轴向锁定、内部滚动容器是否
 * 还能接这次滚轮、横向手势翻画布。列宽与窗口高度这类排布几何在 lib/layout.ts，
 * DOM 与事件接线在组件里。
 *
 * 画布本身不再滚动（一块画布一屏，见 lib/layout.ts 的 canvasGroups），横向手势
 * 的用处从"滚画布"变成"翻到左 / 右一块画布"。
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
 * 窗口内部滚动容器的溢出度量。画布自己接管横向滚轮（见 WorkCanvas），但文件 /
 * 差异是原生 overflow-auto，xterm 的 viewport 也是 DOM 滚动条——这些还能沿手势
 * 方向滚时，事件必须留给内部，画布不能在 capture 里截走，也不能在 bubble 里
 * preventDefault（那会取消内部的默认滚动）。
 *
 * overflow 用计算值：`visible` 的块即使 scrollWidth 更大也滚不动，不能当滚动容器。
 */
export interface OverflowBox {
  overflowX: string;
  overflowY: string;
  scrollLeft: number;
  scrollTop: number;
  clientWidth: number;
  clientHeight: number;
  scrollWidth: number;
  scrollHeight: number;
}

/** 贴边容差：亚像素 / 缩放会让 max - scrollLeft 剩 0.5px，不能当成还能滚 */
const OVERFLOW_EDGE_PX = 1;

/** `overlay` 是 Chrome 旧值，按可滚处理 */
export function overflowScrollable(value: string): boolean {
  return value === "auto" || value === "scroll" || value === "overlay";
}

/**
 * 这个容器还能沿 axis 消化这次滚轮吗。正 delta = 增加 scroll 偏移（右 / 下），
 * 与 WheelEvent 同号。
 */
export function overflowCanConsume(box: OverflowBox, axis: WheelAxis, deltaPx: number): boolean {
  if (!Number.isFinite(deltaPx) || deltaPx === 0) return false;
  if (axis === "x") {
    if (!overflowScrollable(box.overflowX)) return false;
    const max = box.scrollWidth - box.clientWidth;
    if (max <= OVERFLOW_EDGE_PX) return false;
    return deltaPx < 0 ? box.scrollLeft > OVERFLOW_EDGE_PX : box.scrollLeft < max - OVERFLOW_EDGE_PX;
  }
  if (!overflowScrollable(box.overflowY)) return false;
  const max = box.scrollHeight - box.clientHeight;
  if (max <= OVERFLOW_EDGE_PX) return false;
  return deltaPx < 0 ? box.scrollTop > OVERFLOW_EDGE_PX : box.scrollTop < max - OVERFLOW_EDGE_PX;
}

/** 从内到外，任一容器还能沿该轴滚就归内部 */
export function innerTakesWheel(boxes: OverflowBox[], axis: WheelAxis, deltaPx: number): boolean {
  for (const box of boxes) {
    if (overflowCanConsume(box, axis, deltaPx)) return true;
  }
  return false;
}

/** 一段横向手势累计横移超过这么多才翻画布：比随手的横漂大，又不用滑满一屏 */
export const CANVAS_SWIPE_PX = 80;

/**
 * 横向手势翻画布：一段手势（含 macOS 的惯性尾巴）至多翻一块，累计横移过了门槛才翻。
 * 逐事件翻的话，触控板一次滑动几十条事件会一口气翻到底。
 *
 * 手势的切分与 WheelAxisLock 同一个口径：静默超过 WHEEL_GESTURE_GAP_MS 算新手势。
 * 这段手势里只要有一条被窗口内部（文件长行、差异）吃掉过，整段都不翻——横着滚
 * 一个宽文件滚到头、手还没停，不该顺势把画布也翻走。
 */
export class CanvasSwipe {
  private sum = 0;
  private done = false;
  private last = Number.NEGATIVE_INFINITY;

  private touch(timeStamp: number): void {
    if (timeStamp - this.last > WHEEL_GESTURE_GAP_MS) {
      this.sum = 0;
      this.done = false;
    }
    this.last = timeStamp;
  }

  /** 喂一条归给画布的横向事件。返回 1 = 翻到右边一块，-1 = 左边一块，0 = 不翻 */
  push(deltaPx: number, timeStamp: number): -1 | 0 | 1 {
    this.touch(timeStamp);
    if (this.done || !Number.isFinite(deltaPx)) return 0;
    this.sum += deltaPx;
    if (Math.abs(this.sum) < CANVAS_SWIPE_PX) return 0;
    this.done = true;
    return this.sum > 0 ? 1 : -1;
  }

  /** 这段手势被窗口内部接走了：剩下的事件都不翻 */
  hold(timeStamp: number): void {
    this.touch(timeStamp);
    this.done = true;
  }
}
