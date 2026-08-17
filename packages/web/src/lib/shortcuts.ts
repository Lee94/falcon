/**
 * 全局快捷键。
 *
 * 硬约束：xterm 聚焦时几乎吞掉所有 Ctrl+* 组合（Ctrl+C/D/R/W 全是 shell 语义），
 * 所以全局键**不能**用裸 Ctrl+字母。安全区是 ⌘ 系列（mac）、Ctrl+Shift+*
 * （Win/Linux 惯例上留给应用层）和 Alt+*。Esc 永远归终端，只有对话框 /
 * 命令面板 / 菜单打开时被上层拦截——那时终端必定已失焦。
 *
 * 另一层现实：浏览器自己会吞掉 ⌘T / ⌘W / Ctrl+Shift+T / Ctrl+Shift+W /
 * Ctrl+Tab（保留快捷键，页面 preventDefault 无效）。因此每个命令除了设计稿里
 * 的主键位，都额外注册一个浏览器不碰的 Alt 别名。菜单里显示的是主键位。
 */

export type Command =
  | "palette"
  | "newTerminal"
  | "closeTab"
  | "toggleSidebar"
  | "toggleGitPanel"
  | "reattach"
  | "overview"
  | "nextTab"
  | "prevTab"
  /** 切到第 N 个 tab，N 为 1–9 */
  | `tab${number}`;

export const isMac =
  typeof navigator !== "undefined" &&
  /mac/i.test(navigator.platform || navigator.userAgent);

const MOD = isMac ? "⌘" : "Ctrl+";

/** 菜单、tooltip 里显示的主键位 */
export function chord(cmd: Command): string {
  switch (cmd) {
    case "palette":
      return isMac ? "⌘K" : "Ctrl+Shift+P";
    case "newTerminal":
      return isMac ? "⌘T" : "Ctrl+Shift+T";
    case "closeTab":
      return isMac ? "⌘W" : "Ctrl+Shift+W";
    case "toggleSidebar":
      return isMac ? "⌘B" : "Ctrl+Shift+B";
    case "toggleGitPanel":
      return isMac ? "⌘⇧G" : "Ctrl+Shift+G";
    case "reattach":
      return isMac ? "⌘R" : "Ctrl+Shift+R";
    case "overview":
      return isMac ? "⌘0" : "Alt+0";
    case "nextTab":
      return isMac ? "⌘⇧]" : "Ctrl+Tab";
    case "prevTab":
      return isMac ? "⌘⇧[" : "Ctrl+Shift+Tab";
    default:
      return `${isMac ? "⌘" : "Alt+"}${cmd.slice(3)}`;
  }
}

/** 浏览器保留了主键位时能用的别名，展示在命令面板里 */
export function altChord(cmd: Command): string | null {
  switch (cmd) {
    case "newTerminal":
      return "Alt+T";
    case "closeTab":
      return "Alt+W";
    case "toggleSidebar":
      return "Alt+B";
    case "toggleGitPanel":
      return "Alt+G";
    case "reattach":
      return "Alt+R";
    case "palette":
      return `${MOD}K`;
    case "nextTab":
      return "Alt+]";
    case "prevTab":
      return "Alt+[";
    default:
      return null;
  }
}

const ALT_LETTERS: Record<string, Command> = {
  KeyT: "newTerminal",
  KeyW: "closeTab",
  KeyB: "toggleSidebar",
  KeyG: "toggleGitPanel",
  KeyR: "reattach",
  KeyP: "palette",
};

const MODSHIFT_LETTERS: Record<string, Command> = {
  KeyP: "palette",
  KeyT: "newTerminal",
  KeyW: "closeTab",
  KeyB: "toggleSidebar",
  KeyG: "toggleGitPanel",
  KeyR: "reattach",
};

const MAC_MOD_LETTERS: Record<string, Command> = {
  KeyK: "palette",
  KeyT: "newTerminal",
  KeyW: "closeTab",
  KeyB: "toggleSidebar",
  KeyR: "reattach",
};

function digit(code: string): number | null {
  const m = /^Digit([0-9])$/.exec(code);
  return m ? Number(m[1]) : null;
}

/**
 * 物理键位。优先用 e.code（Alt+字母在 mac 上会变成特殊字符，e.key 认不出来），
 * 但有些输入法、远程桌面和合成事件不带 code，退回按 e.key 推。
 */
function eventCode(e: KeyboardEvent): string {
  if (e.code) return e.code;
  const key = e.key ?? "";
  if (/^[a-zA-Z]$/.test(key)) return `Key${key.toUpperCase()}`;
  if (/^[0-9]$/.test(key)) return `Digit${key}`;
  if (key === "[") return "BracketLeft";
  if (key === "]") return "BracketRight";
  if (key === "Tab") return "Tab";
  return "";
}

/**
 * 命中则返回命令。用 e.code 而不是 e.key：Alt+字母在 mac 上会变成特殊字符，
 * key 认不出来。
 */
export function matchCommand(e: KeyboardEvent): Command | null {
  const mod = isMac ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey;
  const code = eventCode(e);
  if (!code) return null;

  // Alt 别名（两个平台都有），浏览器不会截走
  if (e.altKey && !e.ctrlKey && !e.metaKey && !e.shiftKey) {
    const d = digit(code);
    if (d !== null) return d === 0 ? "overview" : (`tab${d}` as Command);
    if (code === "BracketRight") return "nextTab";
    if (code === "BracketLeft") return "prevTab";
    return ALT_LETTERS[code] ?? null;
  }

  if (!mod) return null;

  if (e.shiftKey) {
    if (isMac) {
      if (code === "BracketRight") return "nextTab";
      if (code === "BracketLeft") return "prevTab";
      if (code === "KeyG") return "toggleGitPanel";
      return null;
    }
    if (code === "Tab") return "prevTab";
    return MODSHIFT_LETTERS[code] ?? null;
  }

  if (!isMac) {
    if (code === "Tab") return "nextTab";
    if (code === "KeyK") return "palette";
    return null;
  }

  const d = digit(code);
  if (d !== null) return d === 0 ? "overview" : (`tab${d}` as Command);
  return MAC_MOD_LETTERS[code] ?? null;
}
