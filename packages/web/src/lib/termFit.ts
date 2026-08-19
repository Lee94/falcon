/**
 * 终端格子能不能发给 PTY。
 *
 * FitAddon 在 renderer 还没量出 cell 时 proposeDimensions() 是 undefined，
 * 容器高度还是 0 时会算出 2×1（addon 下限）。xterm 自身默认 80×24。
 * 刷新时这三种都会在字体/布局就绪前冒出来——发给 PTY 就会把会话挤扁。
 */

export interface TermBox {
  width: number;
  height: number;
}

export interface TermDims {
  cols: number;
  rows: number;
}

/** 容器已经撑开，却只够 1 行：格子高度还没量出来（FitAddon 下限就是 1） */
const TALL_ENOUGH_FOR_TWO_ROWS = 48;

export function isUsableTermSize(
  proposed: TermDims | undefined | null,
  box: TermBox
): boolean {
  if (box.width <= 0 || box.height <= 0) return false;
  if (!proposed) return false;
  const { cols, rows } = proposed;
  if (!Number.isFinite(cols) || !Number.isFinite(rows)) return false;
  if (cols < 2 || rows < 1) return false;
  if (box.height >= TALL_ENOUGH_FOR_TWO_ROWS && rows < 2) return false;
  return true;
}
