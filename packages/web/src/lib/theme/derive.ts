/**
 * 一套 Ghostty 主题颜色 → 整个应用要的全部东西：界面的语义色（shadcn token）、
 * 语法高亮色、终端配色（xterm ITheme）、给 PTY 的深浅线索。纯函数。
 *
 * 界面色全部从主题的底 / 字 / 16 色推出来，规则照 shadcn neutral 两套值反推：
 * "从底色往字色掺 t"就是 shadcn 那些 0.97 / 0.269 之类的灰阶（掺色在 OKLab 里做，
 * 见 color.ts）。深浅两套比例不对称是刻意的——shadcn 自己就是这样：深底上
 * muted-foreground 比浅底上离字色更近，因为同样的 WCAG 对比度在黑底上看着更暗。
 *
 * 语义色（destructive / success / warning）与高亮色取自 ANSI 16 色：先在普通色与
 * 亮色里挑对比度够的，都不够就往字色掺到 3:1——文字级可读性的底线；主题的红绿黄
 * 本来就是给终端里的文字用的，多数主题不需要掺。
 */

import type { ITheme } from "@xterm/xterm";
import { appearanceFromHex, type TermAppearance } from "@falcon/shared";
import { ensureContrast, mix, moreReadable, pickReadable, withAlpha } from "./color.js";
import type { ThemeColors } from "./ghostty.js";

export interface ResolvedTheme {
  colors: ThemeColors;
  /** 按底色亮度判的深浅，决定 `.dark`、color-scheme、给 PTY 的 COLORFGBG */
  appearance: TermAppearance;
  /** 写到 <html> 上的自定义属性，键带 `--` */
  vars: Record<string, string>;
  xterm: ITheme;
  /** 建会话 / 换主题时发给服务端，供 OSC 10/11 应答与 COLORFGBG */
  hint: { appearance: TermAppearance; background: string; foreground: string };
}

/** 文字级最低对比度：13px 正文按 WCAG 该 4.5，主题自带色达不到时退到 3 */
const TEXT_MIN = 3;
const TEXT_WANT = 4.5;
/** 次要文字（muted-foreground）的底线，比语义色略高：它是成段的说明文字 */
const MUTED_MIN = 3.5;

const ANSI_NAMES = [
  "black",
  "red",
  "green",
  "yellow",
  "blue",
  "magenta",
  "cyan",
  "white",
  "brightBlack",
  "brightRed",
  "brightGreen",
  "brightYellow",
  "brightBlue",
  "brightMagenta",
  "brightCyan",
  "brightWhite",
] as const;

const SHIKI_ANSI = [
  "black",
  "red",
  "green",
  "yellow",
  "blue",
  "magenta",
  "cyan",
  "white",
  "bright-black",
  "bright-red",
  "bright-green",
  "bright-yellow",
  "bright-blue",
  "bright-magenta",
  "bright-cyan",
  "bright-white",
];

export function deriveTheme(colors: ThemeColors): ResolvedTheme {
  const bg = colors.background;
  const fg = colors.foreground;
  const appearance = appearanceFromHex(bg);
  const dark = appearance === "dark";
  const P = colors.palette;

  /** 底色往字色掺 */
  const tone = (t: number) => mix(bg, fg, t);
  /** ANSI 语义色：普通色优先，亮色备选，都不够就往字色掺 */
  const sem = (i: number) => ensureContrast(pickReadable([P[i]!, P[i + 8]!], bg, TEXT_WANT), bg, TEXT_MIN, fg);

  // shadcn 浅色的 primary 就是字色本身，深色比字色略暗一档
  const primary = dark ? mix(fg, bg, 0.075) : fg;
  const secondary = tone(dark ? 0.15 : 0.035);
  // 字色本身不够深（3024 Day 这类灰字）时按 shadcn 比例掺出来的 muted 会淡到读不出，保个底
  const mutedForeground = ensureContrast(mix(fg, bg, dark ? 0.33 : 0.44), bg, MUTED_MIN, fg);
  const border = withAlpha(fg, 0.1);
  const ring = withAlpha(fg, dark ? 0.45 : 0.3);
  const destructive = sem(1);
  const success = sem(2);
  const warning = sem(3);
  const blue = sem(4);
  const magenta = sem(5);
  const cyan = sem(6);

  const vars: Record<string, string> = {
    "--background": bg,
    "--foreground": fg,
    "--card": dark ? tone(0.07) : bg,
    "--card-foreground": fg,
    "--popover": dark ? tone(0.15) : bg,
    "--popover-foreground": fg,
    "--primary": primary,
    "--primary-foreground": bg,
    "--secondary": secondary,
    "--secondary-foreground": fg,
    "--muted": secondary,
    "--muted-foreground": mutedForeground,
    "--accent": tone(dark ? 0.27 : 0.035),
    "--accent-foreground": fg,
    "--destructive": destructive,
    "--destructive-foreground": moreReadable(bg, fg, destructive),
    "--success": success,
    "--success-foreground": moreReadable(bg, fg, success),
    "--warning": warning,
    "--warning-foreground": moreReadable(bg, fg, warning),
    "--border": border,
    "--input": withAlpha(fg, dark ? 0.15 : 0.1),
    "--ring": ring,
    "--sidebar": tone(dark ? 0.07 : 0.017),
    "--sidebar-foreground": fg,
    "--sidebar-primary": primary,
    "--sidebar-primary-foreground": bg,
    "--sidebar-accent": secondary,
    "--sidebar-accent-foreground": fg,
    "--sidebar-border": border,
    "--sidebar-ring": ring,
    // 主机身份色的饱和度 / 亮度：色相由 hostColor.ts 哈希，深浅在这里按明暗定
    "--host-s": dark ? "34%" : "48%",
    "--host-l": dark ? "56%" : "38%",
    // History 提交图泳道色，色相尽量分开：蓝 黄 绿 紫 青 红
    "--graph-1": blue,
    "--graph-2": warning,
    "--graph-3": success,
    "--graph-4": magenta,
    "--graph-5": cyan,
    "--graph-6": destructive,
    // 界面文字选区跟终端选区同色；selection-foreground 是 cell-foreground 时保留原色
    "--selection": colors.selectionBackground,
    "--selection-foreground": colors.selectionForeground ?? "currentcolor",
    // shiki 的 css-variables 主题（lib/highlight.ts）读这些
    "--shiki-foreground": fg,
    "--shiki-background": bg,
    "--shiki-token-comment": ensureContrast(P[8]!, bg, TEXT_MIN, fg),
    "--shiki-token-keyword": magenta,
    "--shiki-token-string": success,
    "--shiki-token-string-expression": success,
    "--shiki-token-constant": warning,
    "--shiki-token-function": blue,
    "--shiki-token-parameter": fg,
    "--shiki-token-punctuation": mutedForeground,
    "--shiki-token-link": blue,
    "--shiki-token-inserted": success,
    "--shiki-token-deleted": destructive,
    "--shiki-token-changed": warning,
  };
  SHIKI_ANSI.forEach((name, i) => {
    vars[`--shiki-ansi-${name}`] = P[i]!;
  });

  const xterm: ITheme = {
    background: bg,
    foreground: fg,
    cursor: colors.cursorColor,
    cursorAccent: colors.cursorText,
    selectionBackground: colors.selectionBackground,
    selectionForeground: colors.selectionForeground ?? undefined,
  };
  ANSI_NAMES.forEach((name, i) => {
    xterm[name] = P[i]!;
  });
  if (colors.extended) xterm.extendedAnsi = extendedAnsi(colors.extended);

  return {
    colors,
    appearance,
    vars,
    xterm,
    hint: { appearance, background: bg, foreground: fg },
  };
}

/**
 * xterm 的 extendedAnsi 要整份 240 色（16–255）。缺省值按 xterm 256 色标准生成
 * （6×6×6 色立方 + 24 级灰），与 Ghostty `+show-config --default` 打出来的一致。
 */
export function extendedAnsi(overrides: Record<number, string>): string[] {
  const out: string[] = [];
  const step = (n: number) => (n === 0 ? 0 : 55 + n * 40);
  const hex = (n: number) => n.toString(16).padStart(2, "0");
  for (let i = 0; i < 216; i++) {
    const r = Math.floor(i / 36);
    const g = Math.floor(i / 6) % 6;
    const b = i % 6;
    out.push(`#${hex(step(r))}${hex(step(g))}${hex(step(b))}`);
  }
  for (let i = 0; i < 24; i++) {
    const v = hex(8 + i * 10);
    out.push(`#${v}${v}${v}`);
  }
  for (const [k, v] of Object.entries(overrides)) {
    const idx = Number(k) - 16;
    if (idx >= 0 && idx < 240) out[idx] = v;
  }
  return out;
}
