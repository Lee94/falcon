export interface ImeCursor {
  line: number;
  col: number;
}

export interface ImeGrid {
  cols: number;
  rows: number;
  cellWidth: number;
  cellHeight: number;
  displayOffset: number;
}

export interface ImeCursorRect {
  left: number;
  top: number;
  width: number;
  height: number;
  /** 预编辑覆盖层从光标格起向右最多能延伸到网格右边缘的宽度 */
  maxWidth: number;
}

/**
 * 把 Rio 的光标格子换成隐藏 textarea 与预编辑覆盖层的 CSS 像素位置。
 *
 * 浏览器把输入法候选框锚在真正接收 composition 事件的元素上，而不是 canvas
 * 画出的光标上。回滚区打开时实时光标不在 viewport 内，沿用上一个有效位置，
 * 与 xterm CompositionHelper 的 isCursorInViewport 门控一致。
 */
export function imeCursorRect(cursor: ImeCursor, grid: ImeGrid): ImeCursorRect | null {
  const { cols, rows, cellWidth, cellHeight, displayOffset } = grid;
  if (
    ![cursor.line, cursor.col, cols, rows, cellWidth, cellHeight, displayOffset].every(
      Number.isFinite
    ) ||
    cols < 1 ||
    rows < 1 ||
    cellWidth <= 0 ||
    cellHeight <= 0 ||
    displayOffset !== 0
  ) {
    return null;
  }

  const row = Math.floor(cursor.line);
  if (row < 0 || row >= Math.floor(rows)) return null;
  const col = Math.min(Math.max(Math.floor(cursor.col), 0), Math.floor(cols) - 1);
  return {
    left: col * cellWidth,
    top: row * cellHeight,
    // 0×0 的输入元素会让部分系统输入法退回窗口角落；至少占一个完整格子，
    // 候选框才会自然落在当前行的下方。
    width: Math.max(1, cellWidth),
    height: Math.max(1, cellHeight),
    // 组合串不换行，超出网格的部分由覆盖层自己裁掉，不能溢到终端外面
    maxWidth: (Math.floor(cols) - col) * cellWidth,
  };
}
