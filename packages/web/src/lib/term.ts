/**
 * 终端画面偏好：字体、字号、行高、光标、配色。只影响 xterm，不改界面字体。
 *
 * 存在 localStorage，跟界面主题同一个策略——换机器不跟着走，隐私模式读不到
 * 就用默认。xterm 读不了 CSS 变量，每套主题把十六色写死。
 */

import type { ITheme } from "@xterm/xterm";
import { appearanceFromHex, type TermAppearance } from "@falcon/shared";
import type { ThemeMode } from "./theme.js";

export const TERM_PREF_KEY = "falcon.term";
const TERM_PREF_KEY_LEGACY = "mojito.term";

export const TERM_FONT_SIZE_MIN = 10;
export const TERM_FONT_SIZE_MAX = 24;
export const TERM_LINE_HEIGHT_MIN = 1;
export const TERM_LINE_HEIGHT_MAX = 1.6;

export type TermFontId =
  | "maple"
  | "system"
  | "jetbrains"
  | "cascadia"
  | "fira-code"
  | "ibm-plex"
  | "source-code-pro"
  | "custom";

export type TermCursorStyle = "block" | "bar" | "underline";

/** 终端渲染引擎。rio = 实验性的 rioterm(Rust VT 核心编译成 WASM) */
export type TermEngine = "xterm" | "rio";

export type TermThemeId =
  | "match"
  | "campbell"
  | "one-dark"
  | "dracula"
  | "nord"
  | "tokyo-night"
  | "catppuccin-mocha"
  | "gruvbox-dark"
  | "solarized-dark"
  | "github-dark"
  | "monokai"
  | "light-plus"
  | "solarized-light"
  | "catppuccin-latte"
  | "github-light"
  | "gruvbox-light"
  | "one-light";

export interface TermPref {
  fontId: TermFontId;
  customFamily: string;
  fontSize: number;
  lineHeight: number;
  themeId: TermThemeId;
  cursorStyle: TermCursorStyle;
  cursorBlink: boolean;
  engine: TermEngine;
}

export const DEFAULT_TERM_PREF: TermPref = {
  fontId: "maple",
  customFamily: "",
  fontSize: 13,
  lineHeight: 1,
  themeId: "match",
  cursorStyle: "block",
  cursorBlink: true,
  engine: "xterm",
};

const FONT_IDS: TermFontId[] = [
  "maple",
  "system",
  "jetbrains",
  "cascadia",
  "fira-code",
  "ibm-plex",
  "source-code-pro",
  "custom",
];

export const TERM_FONT_IDS: TermFontId[] = FONT_IDS;

/** 内置 Maple 的 CSS family 名，必须与 maple-mono.css 的 @font-face 一致 */
export const MAPLE_FONT_FAMILY = "Maple Mono NL NF CN";

/** 内置图标字体，必须与 nerd-symbols.css 的 @font-face 一致 */
export const NERD_FONT_FAMILY = "Symbols Nerd Font Mono";

/**
 * Maple CN 不是全集：生僻字、假名、谚文、emoji 都不在字库里。
 * xterm 的 canvas 会按 font-family 列表回退，但只回退「cmap 里没有」的码位，
 * 所以后面必须挂上系统 CJK 和 emoji 字体，否则就是方框。
 *
 * 图标字体必须排第一：Maple 自带的 NF 是宽形，xterm 把 U+E000–F8FF
 * 当成 1 格且拒绝 rescale，Node 的  会被裁掉。Symbols Mono 是 1em 宽。
 */
const FALLBACK_STACK = [
  `"${NERD_FONT_FAMILY}"`,
  '"Cascadia Mono"',
  '"JetBrains Mono"',
  '"SF Mono"',
  "Menlo",
  "Consolas",
  '"Noto Sans Mono CJK SC"',
  '"PingFang SC"',
  '"Hiragino Sans GB"',
  '"Microsoft YaHei UI"',
  '"Noto Sans CJK SC"',
  '"Apple Color Emoji"',
  '"Segoe UI Emoji"',
  '"Noto Color Emoji"',
  "monospace",
].join(", ");

const NAMED_FONTS: Record<Exclude<TermFontId, "maple" | "system" | "custom">, string> = {
  jetbrains: "JetBrains Mono",
  cascadia: "Cascadia Mono",
  "fira-code": "Fira Code",
  "ibm-plex": "IBM Plex Mono",
  "source-code-pro": "Source Code Pro",
};

export function termFontStack(pref: TermPref): string {
  // 图标字体永远打头：换正文字体不该把  /  弄丢
  if (pref.fontId === "maple") {
    return `"${NERD_FONT_FAMILY}", "${MAPLE_FONT_FAMILY}", ${FALLBACK_STACK}`;
  }
  if (pref.fontId === "system") return FALLBACK_STACK;
  if (pref.fontId === "custom") {
    const custom = pref.customFamily.trim();
    return custom
      ? `"${NERD_FONT_FAMILY}", ${quoteFamily(custom)}, "${MAPLE_FONT_FAMILY}", ${FALLBACK_STACK}`
      : `"${NERD_FONT_FAMILY}", "${MAPLE_FONT_FAMILY}", ${FALLBACK_STACK}`;
  }
  return `"${NERD_FONT_FAMILY}", "${NAMED_FONTS[pref.fontId]}", "${MAPLE_FONT_FAMILY}", ${FALLBACK_STACK}`;
}

function quoteFamily(name: string): string {
  if (/^["'].*["']$/.test(name) || name.includes(",")) return name;
  return `"${name.replaceAll('"', "")}"`;
}

export const TERM_THEME_GROUPS: { id: "follow" | "dark" | "light"; themes: TermThemeId[] }[] = [
  { id: "follow", themes: ["match"] },
  {
    id: "dark",
    themes: [
      "campbell",
      "one-dark",
      "dracula",
      "nord",
      "tokyo-night",
      "catppuccin-mocha",
      "gruvbox-dark",
      "solarized-dark",
      "github-dark",
      "monokai",
    ],
  },
  {
    id: "light",
    themes: [
      "light-plus",
      "solarized-light",
      "catppuccin-latte",
      "github-light",
      "gruvbox-light",
      "one-light",
    ],
  },
];

export const TERM_THEME_LABELS: Record<Exclude<TermThemeId, "match">, string> = {
  campbell: "Campbell",
  "one-dark": "One Dark",
  dracula: "Dracula",
  nord: "Nord",
  "tokyo-night": "Tokyo Night",
  "catppuccin-mocha": "Catppuccin Mocha",
  "gruvbox-dark": "Gruvbox Dark",
  "solarized-dark": "Solarized Dark",
  "github-dark": "GitHub Dark",
  monokai: "Monokai",
  "light-plus": "Light+",
  "solarized-light": "Solarized Light",
  "catppuccin-latte": "Catppuccin Latte",
  "github-light": "GitHub Light",
  "gruvbox-light": "Gruvbox Light",
  "one-light": "One Light",
};

/**
 * 跟随界面时的两套。深色沿用 shadcn neutral 的底/字，ANSI 用 xterm 默认
 * （本来就是给深底配的）。浅色必须整套覆盖：xterm 默认 white 接近纯白，
 * 白底上直接消失。浅色取 VS Code Light+，白底上公认可读。
 */
const MATCH_THEMES: Record<ThemeMode, ITheme> = {
  dark: {
    background: "#0a0a0a",
    foreground: "#fafafa",
    cursor: "#fafafa",
    cursorAccent: "#0a0a0a",
    selectionBackground: "#ffffff33",
  },
  light: {
    background: "#ffffff",
    foreground: "#171717",
    cursor: "#171717",
    cursorAccent: "#ffffff",
    selectionBackground: "#00000026",
    black: "#000000",
    red: "#cd3131",
    green: "#00bc00",
    yellow: "#949800",
    blue: "#0451a5",
    magenta: "#bc05bc",
    cyan: "#0598bc",
    white: "#555555",
    brightBlack: "#666666",
    brightRed: "#cd3131",
    brightGreen: "#14ce14",
    brightYellow: "#b5ba00",
    brightBlue: "#0451a5",
    brightMagenta: "#bc05bc",
    brightCyan: "#0598bc",
    brightWhite: "#a5a5a5",
  },
};

const FIXED_THEMES: Record<Exclude<TermThemeId, "match">, ITheme> = {
  campbell: {
    background: "#0c0c0c",
    foreground: "#cccccc",
    cursor: "#ffffff",
    cursorAccent: "#0c0c0c",
    selectionBackground: "#ffffff40",
    black: "#0c0c0c",
    red: "#c50f1f",
    green: "#13a10e",
    yellow: "#c19c00",
    blue: "#0037da",
    magenta: "#881798",
    cyan: "#3a96dd",
    white: "#cccccc",
    brightBlack: "#767676",
    brightRed: "#e74856",
    brightGreen: "#16c60c",
    brightYellow: "#f9f1a5",
    brightBlue: "#3b78ff",
    brightMagenta: "#b4009e",
    brightCyan: "#61d6d6",
    brightWhite: "#f2f2f2",
  },
  "one-dark": {
    background: "#282c34",
    foreground: "#abb2bf",
    cursor: "#528bff",
    cursorAccent: "#282c34",
    selectionBackground: "#3e4451",
    black: "#282c34",
    red: "#e06c75",
    green: "#98c379",
    yellow: "#e5c07b",
    blue: "#61afef",
    magenta: "#c678dd",
    cyan: "#56b6c2",
    white: "#abb2bf",
    brightBlack: "#5c6370",
    brightRed: "#e06c75",
    brightGreen: "#98c379",
    brightYellow: "#e5c07b",
    brightBlue: "#61afef",
    brightMagenta: "#c678dd",
    brightCyan: "#56b6c2",
    brightWhite: "#ffffff",
  },
  dracula: {
    background: "#282a36",
    foreground: "#f8f8f2",
    cursor: "#f8f8f2",
    cursorAccent: "#282a36",
    selectionBackground: "#44475a",
    black: "#21222c",
    red: "#ff5555",
    green: "#50fa7b",
    yellow: "#f1fa8c",
    blue: "#bd93f9",
    magenta: "#ff79c6",
    cyan: "#8be9fd",
    white: "#f8f8f2",
    brightBlack: "#6272a4",
    brightRed: "#ff6e6e",
    brightGreen: "#69ff94",
    brightYellow: "#ffffa5",
    brightBlue: "#d6acff",
    brightMagenta: "#ff92df",
    brightCyan: "#a4ffff",
    brightWhite: "#ffffff",
  },
  nord: {
    background: "#2e3440",
    foreground: "#d8dee9",
    cursor: "#d8dee9",
    cursorAccent: "#2e3440",
    selectionBackground: "#434c5e",
    black: "#3b4252",
    red: "#bf616a",
    green: "#a3be8c",
    yellow: "#ebcb8b",
    blue: "#81a1c1",
    magenta: "#b48ead",
    cyan: "#88c0d0",
    white: "#e5e9f0",
    brightBlack: "#4c566a",
    brightRed: "#bf616a",
    brightGreen: "#a3be8c",
    brightYellow: "#ebcb8b",
    brightBlue: "#81a1c1",
    brightMagenta: "#b48ead",
    brightCyan: "#8fbcbb",
    brightWhite: "#eceff4",
  },
  "tokyo-night": {
    background: "#1a1b26",
    foreground: "#c0caf5",
    cursor: "#c0caf5",
    cursorAccent: "#1a1b26",
    selectionBackground: "#33467c",
    black: "#15161e",
    red: "#f7768e",
    green: "#9ece6a",
    yellow: "#e0af68",
    blue: "#7aa2f7",
    magenta: "#bb9af7",
    cyan: "#7dcfff",
    white: "#a9b1d6",
    brightBlack: "#414868",
    brightRed: "#f7768e",
    brightGreen: "#9ece6a",
    brightYellow: "#e0af68",
    brightBlue: "#7aa2f7",
    brightMagenta: "#bb9af7",
    brightCyan: "#7dcfff",
    brightWhite: "#c0caf5",
  },
  "catppuccin-mocha": {
    background: "#1e1e2e",
    foreground: "#cdd6f4",
    cursor: "#f5e0dc",
    cursorAccent: "#1e1e2e",
    selectionBackground: "#45475a",
    black: "#45475a",
    red: "#f38ba8",
    green: "#a6e3a1",
    yellow: "#f9e2af",
    blue: "#89b4fa",
    magenta: "#f5c2e7",
    cyan: "#94e2d5",
    white: "#bac2de",
    brightBlack: "#585b70",
    brightRed: "#f38ba8",
    brightGreen: "#a6e3a1",
    brightYellow: "#f9e2af",
    brightBlue: "#89b4fa",
    brightMagenta: "#f5c2e7",
    brightCyan: "#94e2d5",
    brightWhite: "#a6adc8",
  },
  "gruvbox-dark": {
    background: "#282828",
    foreground: "#ebdbb2",
    cursor: "#ebdbb2",
    cursorAccent: "#282828",
    selectionBackground: "#504945",
    black: "#282828",
    red: "#cc241d",
    green: "#98971a",
    yellow: "#d79921",
    blue: "#458588",
    magenta: "#b16286",
    cyan: "#689d6a",
    white: "#a89984",
    brightBlack: "#928374",
    brightRed: "#fb4934",
    brightGreen: "#b8bb26",
    brightYellow: "#fabd2f",
    brightBlue: "#83a598",
    brightMagenta: "#d3869b",
    brightCyan: "#8ec07c",
    brightWhite: "#ebdbb2",
  },
  "solarized-dark": {
    background: "#002b36",
    foreground: "#839496",
    cursor: "#93a1a1",
    cursorAccent: "#002b36",
    selectionBackground: "#073642",
    black: "#073642",
    red: "#dc322f",
    green: "#859900",
    yellow: "#b58900",
    blue: "#268bd2",
    magenta: "#d33682",
    cyan: "#2aa198",
    white: "#eee8d5",
    brightBlack: "#002b36",
    brightRed: "#cb4b16",
    brightGreen: "#586e75",
    brightYellow: "#657b83",
    brightBlue: "#839496",
    brightMagenta: "#6c71c4",
    brightCyan: "#93a1a1",
    brightWhite: "#fdf6e3",
  },
  "github-dark": {
    background: "#0d1117",
    foreground: "#e6edf3",
    cursor: "#e6edf3",
    cursorAccent: "#0d1117",
    selectionBackground: "#388bfd33",
    black: "#484f58",
    red: "#ff7b72",
    green: "#3fb950",
    yellow: "#d29922",
    blue: "#58a6ff",
    magenta: "#bc8cff",
    cyan: "#39c5cf",
    white: "#b1bac4",
    brightBlack: "#6e7681",
    brightRed: "#ffa198",
    brightGreen: "#56d364",
    brightYellow: "#e3b341",
    brightBlue: "#79c0ff",
    brightMagenta: "#d2a8ff",
    brightCyan: "#56d4dd",
    brightWhite: "#ffffff",
  },
  monokai: {
    background: "#272822",
    foreground: "#f8f8f2",
    cursor: "#f8f8f2",
    cursorAccent: "#272822",
    selectionBackground: "#49483e",
    black: "#272822",
    red: "#f92672",
    green: "#a6e22e",
    yellow: "#f4bf75",
    blue: "#66d9ef",
    magenta: "#ae81ff",
    cyan: "#a1efe4",
    white: "#f8f8f2",
    brightBlack: "#75715e",
    brightRed: "#f92672",
    brightGreen: "#a6e22e",
    brightYellow: "#f4bf75",
    brightBlue: "#66d9ef",
    brightMagenta: "#ae81ff",
    brightCyan: "#a1efe4",
    brightWhite: "#f9f8f5",
  },
  "light-plus": MATCH_THEMES.light,
  "solarized-light": {
    background: "#fdf6e3",
    foreground: "#657b83",
    cursor: "#657b83",
    cursorAccent: "#fdf6e3",
    selectionBackground: "#eee8d5",
    black: "#073642",
    red: "#dc322f",
    green: "#859900",
    yellow: "#b58900",
    blue: "#268bd2",
    magenta: "#d33682",
    cyan: "#2aa198",
    white: "#eee8d5",
    brightBlack: "#002b36",
    brightRed: "#cb4b16",
    brightGreen: "#586e75",
    brightYellow: "#657b83",
    brightBlue: "#839496",
    brightMagenta: "#6c71c4",
    brightCyan: "#93a1a1",
    brightWhite: "#fdf6e3",
  },
  "catppuccin-latte": {
    background: "#eff1f5",
    foreground: "#4c4f69",
    cursor: "#dc8a78",
    cursorAccent: "#eff1f5",
    selectionBackground: "#acb0be",
    black: "#5c5f77",
    red: "#d20f39",
    green: "#40a02b",
    yellow: "#df8e1d",
    blue: "#1e66f5",
    magenta: "#ea76cb",
    cyan: "#179299",
    white: "#acb0be",
    brightBlack: "#6c6f85",
    brightRed: "#d20f39",
    brightGreen: "#40a02b",
    brightYellow: "#df8e1d",
    brightBlue: "#1e66f5",
    brightMagenta: "#ea76cb",
    brightCyan: "#179299",
    brightWhite: "#bcc0cc",
  },
  "github-light": {
    background: "#ffffff",
    foreground: "#1f2328",
    cursor: "#1f2328",
    cursorAccent: "#ffffff",
    selectionBackground: "#0969da33",
    black: "#24292f",
    red: "#cf222e",
    green: "#116329",
    yellow: "#4d2d00",
    blue: "#0969da",
    magenta: "#8250df",
    cyan: "#1b7c83",
    white: "#6e7781",
    brightBlack: "#57606a",
    brightRed: "#a40e26",
    brightGreen: "#1a7f37",
    brightYellow: "#633c01",
    brightBlue: "#218bff",
    brightMagenta: "#a475f9",
    brightCyan: "#3192aa",
    brightWhite: "#8c959f",
  },
  "gruvbox-light": {
    background: "#fbf1c7",
    foreground: "#3c3836",
    cursor: "#3c3836",
    cursorAccent: "#fbf1c7",
    selectionBackground: "#d5c4a1",
    black: "#fbf1c7",
    red: "#cc241d",
    green: "#98971a",
    yellow: "#d79921",
    blue: "#458588",
    magenta: "#b16286",
    cyan: "#689d6a",
    white: "#7c6f64",
    brightBlack: "#928374",
    brightRed: "#9d0006",
    brightGreen: "#79740e",
    brightYellow: "#b57614",
    brightBlue: "#076678",
    brightMagenta: "#8f3f71",
    brightCyan: "#427b58",
    brightWhite: "#3c3836",
  },
  "one-light": {
    background: "#fafafa",
    foreground: "#383a42",
    cursor: "#526eff",
    cursorAccent: "#fafafa",
    selectionBackground: "#e5e5e6",
    black: "#383a42",
    red: "#e45649",
    green: "#50a14f",
    yellow: "#c18401",
    blue: "#4078f2",
    magenta: "#a626a4",
    cyan: "#0184bc",
    white: "#a0a1a7",
    brightBlack: "#4f525e",
    brightRed: "#e06c75",
    brightGreen: "#98c379",
    brightYellow: "#e5c07b",
    brightBlue: "#61afef",
    brightMagenta: "#c678dd",
    brightCyan: "#56b6c2",
    brightWhite: "#ffffff",
  },
};

export function resolveTermTheme(themeId: TermThemeId, ui: ThemeMode): ITheme {
  if (themeId === "match") return MATCH_THEMES[ui];
  return FIXED_THEMES[themeId];
}

/** 按当前 xterm 底色判深浅，不是按界面主题。浅色 UI + 深色终端配色应报 dark。 */
export function appearanceFromTheme(theme: ITheme): TermAppearance {
  return appearanceFromHex(typeof theme.background === "string" ? theme.background : "#000000");
}

export function termColorHint(themeId: TermThemeId, ui: ThemeMode): {
  appearance: TermAppearance;
  background?: string;
  foreground?: string;
} {
  const theme = resolveTermTheme(themeId, ui);
  return {
    appearance: appearanceFromTheme(theme),
    background: theme.background,
    foreground: theme.foreground,
  };
}

export function clampFontSize(n: number): number {
  if (!Number.isFinite(n)) return DEFAULT_TERM_PREF.fontSize;
  return Math.min(TERM_FONT_SIZE_MAX, Math.max(TERM_FONT_SIZE_MIN, Math.round(n)));
}

export function clampLineHeight(n: number): number {
  if (!Number.isFinite(n)) return DEFAULT_TERM_PREF.lineHeight;
  const snapped = Math.round(n * 20) / 20;
  return Math.min(TERM_LINE_HEIGHT_MAX, Math.max(TERM_LINE_HEIGHT_MIN, snapped));
}

export function loadTermPref(): TermPref {
  try {
    const raw = localStorage.getItem(TERM_PREF_KEY) ?? localStorage.getItem(TERM_PREF_KEY_LEGACY);
    if (!raw) return { ...DEFAULT_TERM_PREF };
    const parsed = JSON.parse(raw) as Partial<TermPref>;
    return sanitizeTermPref(parsed);
  } catch {
    return { ...DEFAULT_TERM_PREF };
  }
}

export function saveTermPref(pref: TermPref): void {
  try {
    localStorage.setItem(TERM_PREF_KEY, JSON.stringify(pref));
  } catch {
    // 写不进去就只在本次页面生效
  }
}

export function sanitizeTermPref(raw: Partial<TermPref>): TermPref {
  const fontId = FONT_IDS.includes(raw.fontId as TermFontId)
    ? (raw.fontId as TermFontId)
    : DEFAULT_TERM_PREF.fontId;
  const themeId =
    raw.themeId === "match" || (raw.themeId && raw.themeId in FIXED_THEMES)
      ? raw.themeId
      : DEFAULT_TERM_PREF.themeId;
  const cursorStyle: TermCursorStyle =
    raw.cursorStyle === "bar" || raw.cursorStyle === "underline" || raw.cursorStyle === "block"
      ? raw.cursorStyle
      : DEFAULT_TERM_PREF.cursorStyle;
  return {
    fontId,
    customFamily: typeof raw.customFamily === "string" ? raw.customFamily.slice(0, 120) : "",
    fontSize: clampFontSize(Number(raw.fontSize)),
    lineHeight: clampLineHeight(Number(raw.lineHeight)),
    themeId,
    cursorStyle,
    cursorBlink: raw.cursorBlink !== false,
    engine: raw.engine === "rio" ? "rio" : "xterm",
  };
}
