/**
 * xterm ITheme（全应用的主题 currency，见 lib/term.ts）→ rio 两种渲染器各自要的主题。
 *
 * - `toRioTheme`：rioterm 自带 CanvasRenderer 要全量字段，且**不能给它半透明色**：
 *   它每帧不清屏、选区背景直接 fillRect，带 alpha 的 selectionBackground（mojito
 *   主题全是 #xxxxxx33 这类）会跨帧累积叠加——拖选时选区逐帧变白直到把文字洗掉。
 *   `opaqueOver` 把它与主题 background 预混成不透明色。
 * - `toGpuTheme`：自研 WebGPU 渲染器在 CPU 侧自己做 over 合成，alpha 原样保留；
 *   selectionForeground 缺省不填，选中文字保留原色（与 xterm 一致）。
 */

import type { ITheme } from "@xterm/xterm";
import type { Theme as RioTheme } from "rioterm";

/** xterm.js 的默认 ANSI 16 色（Tango）。跟随主题只定义底/字时补齐 */
export const XTERM_DEFAULT_ANSI = {
  black: "#2e3436",
  red: "#cc0000",
  green: "#4e9a06",
  yellow: "#c4a000",
  blue: "#3465a4",
  magenta: "#75507b",
  cyan: "#06989a",
  white: "#d3d7cf",
  brightBlack: "#555753",
  brightRed: "#ef2929",
  brightGreen: "#8ae234",
  brightYellow: "#fce94f",
  brightBlue: "#729fcf",
  brightMagenta: "#ad7fa8",
  brightCyan: "#34e2e2",
  brightWhite: "#eeeeec",
} as const;

export type AnsiName = keyof typeof XTERM_DEFAULT_ANSI;
export const ANSI_NAMES = Object.keys(XTERM_DEFAULT_ANSI) as AnsiName[];

export interface GpuTheme extends Record<AnsiName, string> {
  foreground: string;
  background: string;
  cursor: string;
  /** block 光标下字形的反色 */
  cursorAccent: string;
  /** 可带 alpha（#rrggbbaa） */
  selectionBackground: string;
  /** 缺省保留原字色 */
  selectionForeground?: string;
}

/** #rgb / #rrggbb / #rrggbbaa。非法输入返回 undefined，调用方原样透传。 */
export function parseHex(hex: string): { r: number; g: number; b: number; a: number } | undefined {
  const m = /^#([\da-f]{3}|[\da-f]{6}|[\da-f]{8})$/i.exec(hex.trim());
  if (!m) return undefined;
  let h = m[1]!;
  if (h.length === 3) h = `${h[0]}${h[0]}${h[1]}${h[1]}${h[2]}${h[2]}`;
  const n = (i: number) => parseInt(h.slice(i, i + 2), 16);
  return { r: n(0), g: n(2), b: n(4), a: h.length === 8 ? n(6) / 255 : 1 };
}

/** 带 alpha 的颜色与底色预混合成不透明 #rrggbb。 */
export function opaqueOver(color: string, base: string): string {
  const c = parseHex(color);
  if (!c || c.a >= 1) return color;
  const b = parseHex(base) ?? { r: 0, g: 0, b: 0 };
  const mix = (x: number, y: number) =>
    Math.round(x * c.a + y * (1 - c.a))
      .toString(16)
      .padStart(2, "0");
  return `#${mix(c.r, b.r)}${mix(c.g, b.g)}${mix(c.b, b.b)}`;
}

function ansiOf(t: ITheme): Record<AnsiName, string> {
  const out = {} as Record<AnsiName, string>;
  for (const name of ANSI_NAMES) out[name] = t[name] ?? XTERM_DEFAULT_ANSI[name];
  return out;
}

const DEFAULT_SELECTION = "#3465a4";

/** 主题的字/底色，没配置时与 xterm 同款默认（白字黑底）；两种渲染器与 IME 预编辑层共用 */
export function themeBase(t: ITheme): { foreground: string; background: string } {
  return { foreground: t.foreground ?? "#ffffff", background: t.background ?? "#000000" };
}

export function toRioTheme(t: ITheme): RioTheme {
  const { foreground, background } = themeBase(t);
  return {
    foreground,
    background,
    cursor: t.cursor ?? foreground,
    // rio 把选中文字统一染成 selectionForeground；xterm 是半透明覆盖不改字色，
    // 没配置时取 foreground 最接近原观感
    selectionForeground: t.selectionForeground ?? foreground,
    selectionBackground: opaqueOver(t.selectionBackground ?? DEFAULT_SELECTION, background),
    ...ansiOf(t),
  };
}

export function toGpuTheme(t: ITheme): GpuTheme {
  const { foreground, background } = themeBase(t);
  return {
    foreground,
    background,
    cursor: t.cursor ?? foreground,
    cursorAccent: t.cursorAccent ?? background,
    selectionBackground: t.selectionBackground ?? DEFAULT_SELECTION,
    selectionForeground: t.selectionForeground,
    ...ansiOf(t),
  };
}
