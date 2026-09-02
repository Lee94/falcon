/**
 * rio 打包颜色字 → u32 RGBA8。GPU 侧不认任何"命名色 / 索引色 / 选区"状态位，
 * 调色板、inverse、dim、选区合成全在 CPU 这里折算完再上传。
 *
 * u32 的字节序：r 在最低字节（r | g<<8 | b<<16 | a<<24）。写进 Uint32Array 后在
 * 小端机器上就是 [r, g, b, a] 四个字节，rgba8unorm 纹理与 unorm8x4 顶点属性都按
 * 这个顺序读，不需要任何 swizzle。
 *
 * 打包字格式（rioterm core.ts）：kind << 24 | payload。kind 0 = NAMED（payload < 16
 * 是 ANSI 16 色，256 fg、257 bg、258 cursor、259..266 dim black..white、267 light fg、
 * 268 dim fg），1 = INDEXED（256 色表），2 = RGB（0xRRGGBB）。
 */

import { ANSI_NAMES, parseHex, type GpuTheme } from "../theme.js";

/** u32：r | g<<8 | b<<16 | a<<24 */
export type Rgba = number;

export const COLOR_NAMED = 0;
export const COLOR_INDEXED = 1;
export const COLOR_RGB = 2;

const NAMED_FOREGROUND = 256;
const NAMED_BACKGROUND = 257;
const NAMED_CURSOR = 258;
const NAMED_DIM_BLACK = 259;
const NAMED_LIGHT_FOREGROUND = 267;
const NAMED_DIM_FOREGROUND = 268;

export function rgba(r: number, g: number, b: number, a = 255): Rgba {
  return ((r & 0xff) | ((g & 0xff) << 8) | ((b & 0xff) << 16) | ((a & 0xff) << 24)) >>> 0;
}

export function channels(c: Rgba): [number, number, number, number] {
  return [c & 0xff, (c >>> 8) & 0xff, (c >>> 16) & 0xff, (c >>> 24) & 0xff];
}

/** 解析 #rgb / #rrggbb / #rrggbbaa；非法输入用 fallback */
export function parseColor(hex: string | undefined, fallback: Rgba): Rgba {
  if (!hex) return fallback;
  const c = parseHex(hex);
  if (!c) return fallback;
  return rgba(c.r, c.g, c.b, Math.round(c.a * 255));
}

export interface Palette {
  /** 256 色表，0..15 来自主题 */
  table: Uint32Array;
  fg: Rgba;
  bg: Rgba;
  cursor: Rgba;
  /** block 光标下字形的反色 */
  cursorFg: Rgba;
  /** 保留 alpha，合成时 over 到格子底色上 */
  selBg: Rgba;
  /** null = 选中文字保留原色 */
  selFg: Rgba | null;
}

const CUBE_STEPS = [0, 95, 135, 175, 215, 255];

export function buildPalette(theme: GpuTheme): Palette {
  const fg = parseColor(theme.foreground, rgba(255, 255, 255));
  const bg = parseColor(theme.background, rgba(0, 0, 0));
  const table = new Uint32Array(256);
  ANSI_NAMES.forEach((name, i) => {
    table[i] = parseColor(theme[name], fg);
  });
  let i = 16;
  for (const r of CUBE_STEPS) for (const g of CUBE_STEPS) for (const b of CUBE_STEPS) table[i++] = rgba(r, g, b);
  for (let k = 0; k < 24; k++) {
    const v = 8 + k * 10;
    table[i++] = rgba(v, v, v);
  }
  return {
    table,
    fg,
    bg,
    cursor: parseColor(theme.cursor, fg),
    cursorFg: parseColor(theme.cursorAccent, bg),
    selBg: parseColor(theme.selectionBackground, rgba(255, 255, 255, 77)),
    selFg: theme.selectionForeground ? parseColor(theme.selectionForeground, fg) : null,
  };
}

/** 各通道 × 2/3 向下取整，alpha 不动（与 rioterm theme.ts 的 dim() 一致） */
export function dimColor(c: Rgba): Rgba {
  const r = ((c & 0xff) * 2) / 3;
  const g = (((c >>> 8) & 0xff) * 2) / 3;
  const b = (((c >>> 16) & 0xff) * 2) / 3;
  return rgba(r | 0, g | 0, b | 0, (c >>> 24) & 0xff);
}

function named(value: number, isFg: boolean, p: Palette): Rgba {
  if (value < 16) return p.table[value]!;
  switch (value) {
    case NAMED_FOREGROUND:
      return p.fg;
    case NAMED_BACKGROUND:
      return p.bg;
    case NAMED_CURSOR:
      return p.cursor;
    case NAMED_LIGHT_FOREGROUND:
      return p.table[15]!;
    case NAMED_DIM_FOREGROUND:
      return dimColor(p.fg);
    default:
      if (value >= NAMED_DIM_BLACK && value <= NAMED_DIM_BLACK + 7) {
        return dimColor(p.table[value - NAMED_DIM_BLACK]!);
      }
      return isFg ? p.fg : p.bg;
  }
}

export function resolveColor(word: number, isFg: boolean, p: Palette): Rgba {
  const kind = word >>> 24;
  const payload = word & 0xffffff;
  switch (kind) {
    case COLOR_RGB:
      return rgba((payload >>> 16) & 0xff, (payload >>> 8) & 0xff, payload & 0xff);
    case COLOR_INDEXED:
      return payload < 256 ? p.table[payload]! : p.fg;
    case COLOR_NAMED:
    default:
      return named(payload, isFg, p);
  }
}

/** src（可带 alpha）over 不透明 dst，输出不透明 */
export function over(src: Rgba, dst: Rgba): Rgba {
  const a = (src >>> 24) & 0xff;
  if (a >= 255) return (src | 0xff000000) >>> 0;
  if (a === 0) return (dst | 0xff000000) >>> 0;
  const inv = 255 - a;
  const mix = (s: number, d: number) => Math.round((s * a + d * inv) / 255);
  return rgba(
    mix(src & 0xff, dst & 0xff),
    mix((src >>> 8) & 0xff, (dst >>> 8) & 0xff),
    mix((src >>> 16) & 0xff, (dst >>> 16) & 0xff)
  );
}
