/**
 * 滚轮事件 → 整行数。rioterm 原版是 `Math.trunc(-deltaY / cellHeight)`，不足一行
 * 直接丢：触控板慢滚每个事件只有几像素，在 scrollback 里根本滚不动。这里把小数
 * 余量攒起来，攒够一行才吐；方向翻转时清掉余量，免得反向滑动先"补"上一段。
 */

const DOM_DELTA_LINE = 1;
const DOM_DELTA_PAGE = 2;

export class WheelAccumulator {
  private remainder = 0;

  /**
   * @param deltaMode WheelEvent.deltaMode（0 像素 / 1 行 / 2 页）
   * @param cellHeight 一行的 CSS 像素高，像素模式按它折算
   * @param pageRows 页模式一页折算多少行；调用方传当前 rows
   * @returns 本次应滚动的整行数，正数朝历史方向（与 rioterm scrollWheel 约定一致）
   */
  push(deltaMode: number, deltaY: number, cellHeight: number, pageRows = 24): number {
    let lines: number;
    if (deltaMode === DOM_DELTA_LINE) lines = -deltaY;
    else if (deltaMode === DOM_DELTA_PAGE) lines = -deltaY * pageRows;
    else lines = cellHeight > 0 ? -deltaY / cellHeight : 0;
    if (!Number.isFinite(lines) || lines === 0) return 0;
    if (this.remainder !== 0 && lines > 0 !== this.remainder > 0) this.remainder = 0;
    const total = this.remainder + lines;
    const whole = Math.trunc(total);
    this.remainder = total - whole;
    // Math.trunc(-0.25) 是 -0，调用方按 === 0 判等会漏
    return whole || 0;
  }

  reset(): void {
    this.remainder = 0;
  }
}
