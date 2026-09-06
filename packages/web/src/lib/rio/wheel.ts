/**
 * 滚轮事件 → 滚动量。两种口径，取决于滚轮归谁：
 *
 * - `push()`：本地 scrollback 用，像素按 cellHeight 折成行。rioterm 原版是
 *   `Math.trunc(-deltaY / cellHeight)`，不足一行直接丢：触控板慢滚每个事件只有几像素，
 *   在 scrollback 里根本滚不动。这里把小数余量攒起来，攒够一行才吐；方向翻转时清掉余量，
 *   免得反向滑动先"补"上一段。
 *
 * - `click()`：程序接管了滚轮（鼠标上报 / alt screen）时用，一个 DOM 事件最多折成一次
 *   点击。rioterm 的 scroll_wheel 会把 n 行翻成 n 条鼠标上报 / n 个方向键，而 zellij 收到
 *   每条上报自己再滚 3 行，鼠标一格（macOS 上 deltaY 常有上百像素、五六行）就冲出十几行。
 *   xterm.js 的口径（MouseService._consumeWheelEvent）是行数只当门槛、每个事件最多发一条，
 *   多出的整行丢掉，且 |deltaY| < 50 的事件当作触控板再乘 0.3。这里照抄，两种引擎手感一致。
 */

const DOM_DELTA_PIXEL = 0;
const DOM_DELTA_LINE = 1;
const DOM_DELTA_PAGE = 2;

/** xterm.js 的启发式：单个像素事件 |deltaY| 小于它就当触控板 */
const TRACKPAD_MAX_DELTA = 50;
const TRACKPAD_DAMPING = 0.3;

export class WheelAccumulator {
  private remainder = 0;

  /**
   * @param deltaMode WheelEvent.deltaMode（0 像素 / 1 行 / 2 页）
   * @param cellHeight 一行的 CSS 像素高，像素模式按它折算
   * @param pageRows 页模式一页折算多少行；调用方传当前 rows
   * @returns 本次应滚动的整行数，正数朝历史方向（与 rioterm scrollWheel 约定一致）
   */
  push(deltaMode: number, deltaY: number, cellHeight: number, pageRows = 24): number {
    return this.accumulate(toLines(deltaMode, deltaY, cellHeight, pageRows));
  }

  /**
   * 程序接管滚轮时的口径：攒够一行算一次点击，多出的整行丢掉。参数同 `push()`。
   * @returns 1 朝历史方向、-1 朝底部、0 还没攒够
   */
  click(deltaMode: number, deltaY: number, cellHeight: number, pageRows = 24): -1 | 0 | 1 {
    let lines = toLines(deltaMode, deltaY, cellHeight, pageRows);
    if (deltaMode === DOM_DELTA_PIXEL && Math.abs(deltaY) < TRACKPAD_MAX_DELTA) lines *= TRACKPAD_DAMPING;
    const whole = this.accumulate(lines);
    return whole > 0 ? 1 : whole < 0 ? -1 : 0;
  }

  reset(): void {
    this.remainder = 0;
  }

  private accumulate(lines: number): number {
    if (!Number.isFinite(lines) || lines === 0) return 0;
    if (this.remainder !== 0 && lines > 0 !== this.remainder > 0) this.remainder = 0;
    const total = this.remainder + lines;
    const whole = Math.trunc(total);
    this.remainder = total - whole;
    // Math.trunc(-0.25) 是 -0，调用方按 === 0 判等会漏
    return whole || 0;
  }
}

function toLines(deltaMode: number, deltaY: number, cellHeight: number, pageRows: number): number {
  if (deltaMode === DOM_DELTA_LINE) return -deltaY;
  if (deltaMode === DOM_DELTA_PAGE) return -deltaY * pageRows;
  return cellHeight > 0 ? -deltaY / cellHeight : 0;
}
