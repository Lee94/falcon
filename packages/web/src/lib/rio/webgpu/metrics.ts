/**
 * cell 度量。物理像素整数是唯一真相：GPU 里 cell_size × grid_pos 与 bg 片元的
 * floor(pixel / cell_size) 必须对同一组整数达成一致，否则格子边界出缝、字形落在
 * 半像素上发糊（rioterm 的 canvas 渲染器在 CSS px 里 ceil 再乘 dpr，dpr 1.5/2 下
 * 格子就是分数物理像素）。CSS 值是物理值 / dpr，允许是分数，只给 fit / cellAt 用。
 *
 * 行高口径对齐 xterm：cellH = ceil((fontAscent + fontDescent) × lineHeight)，用字体
 * 自身的行盒而不是 fontSize——xterm 量的是 span 的 offsetHeight，就是这个数。
 * 这样 rio 与 xterm 在同一个 pane 里算出相同的行数，切引擎不会把 zellij 会话挤一下；
 * 也给 CJK 回退字体与 emoji 的上下伸留了位置（rioterm 用 fontSize × lineHeight，
 * lineHeight 1 时 g/y 的尾巴直接被裁）。
 */

export interface Measured {
  /** "W" 的 advance，物理 px */
  width: number;
  /** 字体行盒的 ascent / descent（fontBoundingBox*），物理 px。量不到时给 0，这里兜底 */
  ascent: number;
  descent: number;
}

export type MeasureFn = (font: string, text: string) => Measured;

export interface CellMetrics {
  dpr: number;
  /** fontSize × dpr。画字与量字都直接用它，不走 ctx.scale */
  fontPx: number;
  cellW: number;
  cellH: number;
  /** 距 cell 顶的 baseline，整数 */
  baseline: number;
  /** 下划线顶边距 cell 顶 */
  underlineY: number;
  underlineThick: number;
  /** 删除线顶边距 cell 顶 */
  strikeY: number;
  /** bar 光标宽 */
  barW: number;
  cssCellW: number;
  cssCellH: number;
}

export function fontString(fontPx: number, family: string, bold = false, italic = false): string {
  return `${italic ? "italic " : ""}${bold ? "bold " : ""}${fontPx}px ${family}`;
}

export function computeMetrics(
  o: { fontFamily: string; fontSize: number; lineHeight: number; dpr: number },
  measure: MeasureFn
): CellMetrics {
  const dpr = o.dpr > 0 && Number.isFinite(o.dpr) ? o.dpr : 1;
  const fontPx = o.fontSize * dpr;
  const m = measure(fontString(fontPx, o.fontFamily), "W");
  const cellW = Math.max(1, Math.ceil(m.width));
  // 老内核没有 fontBoundingBox*，按常见字体 1.2 倍行高兜底
  let asc = m.ascent;
  let desc = m.descent;
  if (!(asc + desc > 0)) {
    asc = fontPx * 0.95;
    desc = fontPx * 0.25;
  }
  const cellH = Math.max(1, Math.ceil((asc + desc) * o.lineHeight));
  // 字体行盒在 cell 里居中，baseline = 行盒顶 + ascent
  const baseline = clamp(Math.round((cellH - (asc + desc)) / 2 + asc), 1, cellH);
  const underlineThick = Math.max(1, Math.round(fontPx / 12));
  const underlineY = clamp(
    baseline + Math.max(1, Math.round(desc / 2)),
    0,
    Math.max(0, cellH - underlineThick)
  );
  const strikeY = clamp(Math.round(baseline - fontPx * 0.3), 0, Math.max(0, cellH - underlineThick));
  const barW = Math.max(1, Math.round(1.5 * dpr));
  return {
    dpr,
    fontPx,
    cellW,
    cellH,
    baseline,
    underlineY,
    underlineThick,
    strikeY,
    barW,
    cssCellW: cellW / dpr,
    cssCellH: cellH / dpr,
  };
}

function clamp(v: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, v));
}
