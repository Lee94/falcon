/**
 * 终端画面偏好：字体、字号、行高、光标、引擎。只影响终端，不改界面字体。
 * 配色不在这里——终端与整个界面共用一套主题（lib/theme/），按明暗各选一套。
 *
 * 存在 localStorage，跟主题同一个策略——换机器不跟着走，隐私模式读不到
 * 就用默认。旧版存过 themeId（终端单独配色），读取时直接丢弃，迁移见 lib/theme/pref.ts。
 */

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

export interface TermPref {
  fontId: TermFontId;
  customFamily: string;
  fontSize: number;
  lineHeight: number;
  cursorStyle: TermCursorStyle;
  cursorBlink: boolean;
  engine: TermEngine;
}

export const DEFAULT_TERM_PREF: TermPref = {
  fontId: "maple",
  customFamily: "",
  fontSize: 13,
  lineHeight: 1,
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
  const cursorStyle: TermCursorStyle =
    raw.cursorStyle === "bar" || raw.cursorStyle === "underline" || raw.cursorStyle === "block"
      ? raw.cursorStyle
      : DEFAULT_TERM_PREF.cursorStyle;
  return {
    fontId,
    customFamily: typeof raw.customFamily === "string" ? raw.customFamily.slice(0, 120) : "",
    fontSize: clampFontSize(Number(raw.fontSize)),
    lineHeight: clampLineHeight(Number(raw.lineHeight)),
    cursorStyle,
    cursorBlink: raw.cursorBlink !== false,
    engine: raw.engine === "rio" ? "rio" : "xterm",
  };
}
