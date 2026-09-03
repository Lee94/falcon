/**
 * 一帧的 CPU 侧工作：把 rioterm snapshot 的打包格子变成 GPU 要的两样东西——
 * 每格一个 u32 底色（进 bg 纹理）和每个字形 / 装饰一个 24 字节实例（进顶点缓冲），
 * 以及"这一帧哪些行要重建"的判定。全是纯函数，DOM / GPU 都不碰。
 *
 * 实例 24 字节 = 6 个 u32，与 shaders.ts 的顶点布局逐字段对应（测试回读断言钉死）：
 *   w0 glyphX | glyphY<<16   (uint16x2)   w3 col | row<<16   (uint16x2)
 *   w1 glyphW | glyphH<<16   (uint16x2)   w4 rgba            (unorm8x4)
 *   w2 bearX | bearY<<16     (sint16x2)   w5 flags           (uint32)
 *
 * 格子格式来自 rioterm core.ts（rio-vt 的 StyleFlags 位），这里复制一份常量而不
 * import "rioterm"：那个模块顶层就引 wasm 胶水，node 里跑测试引不进来。
 */

import { GLYPH_FLAG_COLOR, GLYPH_FULL, type GlyphTable } from "./atlas.js";
import { dimColor, over, resolveColor, type Palette, type Rgba } from "./colors.js";
import type { Rect } from "./decorations.js";

export const CELL_WORDS = 4;
export const STYLE_INVERSE = 1 << 0;
export const STYLE_BOLD = 1 << 1;
export const STYLE_ITALIC = 1 << 2;
export const STYLE_DIM = 1 << 3;
export const STYLE_HIDDEN = 1 << 4;
export const STYLE_STRIKEOUT = 1 << 5;
export const STYLE_UNDERLINE = 1 << 6;
export const STYLE_DOUBLE_UNDERLINE = 1 << 7;
export const STYLE_UNDERCURL = 1 << 8;
export const STYLE_DOTTED_UNDERLINE = 1 << 9;
export const STYLE_DASHED_UNDERLINE = 1 << 10;
export const WIDE_WIDE = 1;
export const WIDE_SPACER = 2;
export const CELL_HAS_CLUSTER = 1 << 23;

export const INSTANCE_WORDS = 6;
export const INSTANCE_BYTES = INSTANCE_WORDS * 4;
/** 字形 + 双下划线两段 + 删除线 */
export const MAX_INSTANCES_PER_CELL = 4;

export const FLAG_COLOR_ATLAS = 1;
export const FLAG_SOLID = 2;

export interface Selection {
  startLine: number;
  startCol: number;
  endLine: number;
  endCol: number;
  isBlock: boolean;
}

export interface HoverLink {
  line: number;
  startCol: number;
  endCol: number;
}

export interface RowCtx {
  cols: number;
  palette: Palette;
  glyphs: GlyphTable;
  /**
   * text 非 null 表示 grapheme cluster 的完整文本；fg 是折算完 inverse / dim / 选区的
   * 前景色，图集按它的颜色桶画字（见 atlas.ts 文件头）。返回 id / GLYPH_EMPTY / GLYPH_FULL
   */
  lookup(cp: number, text: string | null, bold: boolean, italic: boolean, wide: boolean, fg: Rgba): number;
  /** 波浪 / 点 / 虚线 sprite 的 id（GLYPH_EMPTY 表示没有） */
  decor: { undercurl: number; dotted: number; dashed: number };
  /** 直线类装饰的矩形，按 metrics 预先算好 */
  solid: { underline: Rect[]; double: Rect[]; strikeout: Rect[] };
  clusterText(row: number, col: number): string | undefined;
  selection: Selection | null;
  hover: HoverLink | null;
}

export function packInstance(
  out: Uint32Array,
  at: number,
  gx: number,
  gy: number,
  gw: number,
  gh: number,
  bx: number,
  by: number,
  col: number,
  row: number,
  color: Rgba,
  flags: number
): void {
  out[at] = (gx & 0xffff) | ((gy & 0xffff) << 16);
  out[at + 1] = (gw & 0xffff) | ((gh & 0xffff) << 16);
  out[at + 2] = (bx & 0xffff) | ((by & 0xffff) << 16);
  out[at + 3] = (col & 0xffff) | ((row & 0xffff) << 16);
  out[at + 4] = color;
  out[at + 5] = flags;
}

export function inSelection(sel: Selection, row: number, col: number): boolean {
  if (row < sel.startLine || row > sel.endLine) return false;
  if (sel.isBlock) return col >= sel.startCol && col <= sel.endCol;
  if (row === sel.startLine && col < sel.startCol) return false;
  if (row === sel.endLine && col > sel.endCol) return false;
  return true;
}

type Underline = "single" | "double" | "undercurl" | "dotted" | "dashed" | null;

/** 位测顺序与 rio-grid 一致 */
function underlineOf(flags: number): Underline {
  if (flags & STYLE_UNDERLINE) return "single";
  if (flags & STYLE_DOUBLE_UNDERLINE) return "double";
  if (flags & STYLE_UNDERCURL) return "undercurl";
  if (flags & STYLE_DOTTED_UNDERLINE) return "dotted";
  if (flags & STYLE_DASHED_UNDERLINE) return "dashed";
  return null;
}

/**
 * 重建一行：底色写进 outBg[0..cols)，实例写进 outFg（容量 ≥ cols × MAX_INSTANCES_PER_CELL × 6）。
 * 返回实例数；图集满返回 -1，调用方 reset 图集后全量重来。
 *
 * 装饰与字形独立判定：空格上的下划线也要画（rioterm 的 canvas 渲染器把 cp ≤ 32 的格子
 * 整个跳过，zsh 提示符 / ls 里带下划线的空格就丢了）。
 */
export function buildRow(cells: Uint32Array, row: number, ctx: RowCtx, outBg: Uint32Array, outFg: Uint32Array): number {
  const { cols, palette: p, glyphs } = ctx;
  const base = row * cols * CELL_WORDS;
  const sel = ctx.selection;
  const rowHasSel = sel !== null && row >= sel.startLine && row <= sel.endLine;
  const hover = ctx.hover !== null && ctx.hover.line === row ? ctx.hover : null;
  let n = 0;

  for (let col = 0; col < cols; col++) {
    const idx = base + col * CELL_WORDS;
    const w0 = cells[idx]!;
    const fgWord = cells[idx + 1]!;
    const bgWord = cells[idx + 2]!;
    const flags = cells[idx + 3]!;

    const inverse = (flags & STYLE_INVERSE) !== 0;
    let fg = resolveColor(inverse ? bgWord : fgWord, !inverse, p);
    let bg = resolveColor(inverse ? fgWord : bgWord, inverse, p);
    if (flags & STYLE_DIM) fg = dimColor(fg);
    if (rowHasSel && inSelection(sel!, row, col)) {
      bg = over(p.selBg, bg);
      if (p.selFg !== null) fg = p.selFg;
    }
    outBg[col] = bg;

    if (flags & STYLE_HIDDEN) continue;

    const wide = (w0 >>> 21) & 0b11;
    const cp = w0 & 0x1fffff;
    const bold = (flags & STYLE_BOLD) !== 0;
    const italic = (flags & STYLE_ITALIC) !== 0;

    if (wide !== WIDE_SPACER && cp > 32) {
      const text = w0 & CELL_HAS_CLUSTER ? (ctx.clusterText(row, col) ?? null) : null;
      // block 光标下的字形在 shader 里换成 cursorFg 色，遮罩仍按 fg 的桶取，差一格无所谓
      const id = ctx.lookup(cp, text, bold, italic, wide === WIDE_WIDE, fg);
      if (id === GLYPH_FULL) return -1;
      if (id >= 0) {
        packInstance(
          outFg,
          n * INSTANCE_WORDS,
          glyphs.x[id]!,
          glyphs.y[id]!,
          glyphs.w[id]!,
          glyphs.h[id]!,
          glyphs.bx[id]!,
          glyphs.by[id]!,
          col,
          row,
          fg,
          glyphs.flags[id]! & GLYPH_FLAG_COLOR ? FLAG_COLOR_ATLAS : 0
        );
        n++;
      }
    }

    let underline = underlineOf(flags);
    if (underline === null && hover !== null && col >= hover.startCol && col <= hover.endCol) underline = "single";
    if (underline === "single" || underline === "double") {
      for (const r of underline === "single" ? ctx.solid.underline : ctx.solid.double) {
        packInstance(outFg, n * INSTANCE_WORDS, 0, 0, r.w, r.h, r.x, r.y, col, row, fg, FLAG_SOLID);
        n++;
      }
    } else if (underline !== null) {
      const id = ctx.decor[underline];
      if (id >= 0) {
        packInstance(
          outFg,
          n * INSTANCE_WORDS,
          glyphs.x[id]!,
          glyphs.y[id]!,
          glyphs.w[id]!,
          glyphs.h[id]!,
          glyphs.bx[id]!,
          glyphs.by[id]!,
          col,
          row,
          fg,
          0
        );
        n++;
      }
    }
    if (flags & STYLE_STRIKEOUT) {
      for (const r of ctx.solid.strikeout) {
        packInstance(outFg, n * INSTANCE_WORDS, 0, 0, r.w, r.h, r.x, r.y, col, row, fg, FLAG_SOLID);
        n++;
      }
    }
  }
  return n;
}

// ---- 脏行判定 ----

export interface FrameKey {
  cols: number;
  rows: number;
  displayOffset: number;
  altScreen: boolean;
  selection: Selection | null;
  hover: HoverLink | null;
  cursorLine: number;
  cursorCol: number;
  cursorVisible: boolean;
}

export type Damage =
  | { kind: "noop" }
  | { kind: "cursor" }
  | { kind: "partial"; rows: Uint8Array }
  | { kind: "full" };

function sameSelection(a: Selection | null, b: Selection | null): boolean {
  if (a === b) return true;
  if (a === null || b === null) return false;
  return (
    a.startLine === b.startLine &&
    a.startCol === b.startCol &&
    a.endLine === b.endLine &&
    a.endCol === b.endCol &&
    a.isBlock === b.isBlock
  );
}

function sameHover(a: HoverLink | null, b: HoverLink | null): boolean {
  if (a === b) return true;
  if (a === null || b === null) return false;
  return a.line === b.line && a.startCol === b.startCol && a.endCol === b.endCol;
}

/** 选区覆盖的视口行区间（裁到 [0, rows)），没有返回 null */
export function selectionRows(sel: Selection | null, rows: number): [number, number] | null {
  if (sel === null) return null;
  const a = Math.max(0, Math.min(sel.startLine, sel.endLine));
  const b = Math.min(rows - 1, Math.max(sel.startLine, sel.endLine));
  return a <= b ? [a, b] : null;
}

/**
 * `pending` 是调用方维护的每行脏标记：snapshot() 会重置 WASM 侧的 dirtyRows，
 * 一帧中途失败（图集满重建、异常）脏信息就丢了，所以 WASM 的标记先并进 pending，
 * 上传成功后才清。
 *
 * displayOffset ≠ 0（在看历史）时每帧全量：WASM 在滚动时是否把所有行置脏没有
 * 文档保证，11k 格全量重建约 0.5ms，保守换确定。
 */
export function computeDamage(prev: FrameKey | null, next: FrameKey, pending: Uint8Array, forceFull: boolean): Damage {
  if (
    prev === null ||
    forceFull ||
    prev.cols !== next.cols ||
    prev.rows !== next.rows ||
    prev.displayOffset !== next.displayOffset ||
    next.displayOffset !== 0 ||
    prev.altScreen !== next.altScreen
  ) {
    return { kind: "full" };
  }
  const rows = new Uint8Array(next.rows);
  let any = false;
  for (let i = 0; i < next.rows; i++) {
    if (pending[i]) {
      rows[i] = 1;
      any = true;
    }
  }
  if (!sameSelection(prev.selection, next.selection)) {
    for (const range of [selectionRows(prev.selection, next.rows), selectionRows(next.selection, next.rows)]) {
      if (!range) continue;
      for (let i = range[0]; i <= range[1]; i++) rows[i] = 1;
      any = true;
    }
  }
  if (!sameHover(prev.hover, next.hover)) {
    for (const h of [prev.hover, next.hover]) {
      if (h && h.line >= 0 && h.line < next.rows) {
        rows[h.line] = 1;
        any = true;
      }
    }
  }
  if (any) return { kind: "partial", rows };
  if (
    prev.cursorLine !== next.cursorLine ||
    prev.cursorCol !== next.cursorCol ||
    prev.cursorVisible !== next.cursorVisible
  ) {
    return { kind: "cursor" };
  }
  return { kind: "noop" };
}
