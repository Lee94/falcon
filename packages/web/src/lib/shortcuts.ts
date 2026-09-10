/**
 * 全局快捷键。键位跟 VS Code 对齐——命令面板是 ⌘⇧P / Ctrl+Shift+P / F1，
 * 其余能对上的也用同一套（⌘B 侧栏、⌘⇧E 文件、⌘⇧] / ⌘⇧[ 切 tab）。
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
  | "quickOpen"
  | "newTerminal"
  | "closeTab"
  | "toggleSidebar"
  | "toggleGitPanel"
  | "toggleChangesPanel"
  | "toggleForwardPanel"
  | "toggleFilesPanel"
  | "toggleMeeglePanel"
  | "reattach"
  | "overview"
  | "nextTab"
  | "prevTab"
  /** 切到第 N 个 tab，N 为 1–9 */
  | `tab${number}`;

export const isMac =
  typeof navigator !== "undefined" &&
  /mac/i.test(navigator.platform || navigator.userAgent);

/** 菜单、tooltip 里显示的主键位。mac 可注入，方便单测两套表。 */
export function chord(cmd: Command, mac = isMac): string {
  switch (cmd) {
    case "palette":
      return mac ? "⌘⇧P" : "Ctrl+Shift+P";
    case "quickOpen":
      // Win 不能绑 Ctrl+P：那是 shell 的上一条历史。浏览器里 ⌘P 也常被打印截走，
      // 菜单仍显示 VS Code 主键位，真正在网页里能用的是 Alt+P。
      return mac ? "⌘P" : "Alt+P";
    case "newTerminal":
      return mac ? "⌘T" : "Ctrl+Shift+T";
    case "closeTab":
      return mac ? "⌘W" : "Ctrl+Shift+W";
    case "toggleSidebar":
      return mac ? "⌘B" : "Ctrl+Shift+B";
    case "toggleGitPanel":
      return mac ? "⌘⇧G" : "Ctrl+Shift+G";
    case "toggleChangesPanel":
      return mac ? "⌘⇧U" : "Ctrl+Shift+U";
    case "toggleForwardPanel":
      return mac ? "⌘⇧F" : "Ctrl+Shift+F";
    case "toggleFilesPanel":
      return mac ? "⌘⇧E" : "Ctrl+Shift+E";
    case "toggleMeeglePanel":
      return mac ? "⌘⇧M" : "Ctrl+Shift+M";
    case "reattach":
      return mac ? "⌘R" : "Ctrl+Shift+R";
    case "overview":
      return mac ? "⌘0" : "Alt+0";
    case "nextTab":
      return mac ? "⌘⇧]" : "Ctrl+Tab";
    case "prevTab":
      return mac ? "⌘⇧[" : "Ctrl+Shift+Tab";
    default:
      return `${mac ? "⌘" : "Alt+"}${cmd.slice(3)}`;
  }
}

/** 浏览器保留了主键位时能用的别名 */
export function altChord(cmd: Command, mac = isMac): string | null {
  switch (cmd) {
    case "newTerminal":
      return "Alt+T";
    case "closeTab":
      return "Alt+W";
    case "toggleSidebar":
      return "Alt+B";
    case "toggleGitPanel":
      return "Alt+G";
    case "toggleChangesPanel":
      return "Alt+U";
    case "toggleForwardPanel":
      return "Alt+F";
    case "toggleFilesPanel":
      return "Alt+E";
    case "toggleMeeglePanel":
      return "Alt+M";
    case "reattach":
      return "Alt+R";
    case "palette":
      // VS Code 主键位之外：旧的 ⌘/Ctrl+K，以及 F1
      return mac ? "⌘K" : "Ctrl+K";
    case "quickOpen":
      return "Alt+P";
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
  KeyU: "toggleChangesPanel",
  KeyF: "toggleForwardPanel",
  KeyE: "toggleFilesPanel",
  KeyM: "toggleMeeglePanel",
  KeyR: "reattach",
  KeyP: "quickOpen",
};

const MODSHIFT_LETTERS: Record<string, Command> = {
  KeyP: "palette",
  KeyT: "newTerminal",
  KeyW: "closeTab",
  KeyB: "toggleSidebar",
  KeyG: "toggleGitPanel",
  KeyU: "toggleChangesPanel",
  KeyF: "toggleForwardPanel",
  KeyE: "toggleFilesPanel",
  KeyM: "toggleMeeglePanel",
  KeyR: "reattach",
};

const MAC_MOD_LETTERS: Record<string, Command> = {
  // 旧主键位。VS Code 里 ⌘K 是 chord 前缀，这里没有 chord，留着当额外入口
  KeyK: "palette",
  KeyP: "quickOpen",
  KeyT: "newTerminal",
  KeyW: "closeTab",
  KeyB: "toggleSidebar",
  KeyR: "reattach",
};

const MAC_MODSHIFT_LETTERS: Record<string, Command> = {
  KeyP: "palette",
  KeyG: "toggleGitPanel",
  KeyU: "toggleChangesPanel",
  KeyF: "toggleForwardPanel",
  KeyE: "toggleFilesPanel",
  KeyM: "toggleMeeglePanel",
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
  if (key === "F1") return "F1";
  if (key === "`" || key === "~") return "Backquote";
  return "";
}

/**
 * 命中则返回命令。用 e.code 而不是 e.key：Alt+字母在 mac 上会变成特殊字符，
 * key 认不出来。mac 可注入，方便单测两套表。
 */
export function matchCommand(e: KeyboardEvent, mac = isMac): Command | null {
  const mod = mac ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey;
  const code = eventCode(e);
  if (!code) return null;

  // F1：VS Code 命令面板，两平台、无修饰键
  if (code === "F1" && !e.altKey && !e.ctrlKey && !e.metaKey && !e.shiftKey) {
    return "palette";
  }

  // VS Code「新建终端」：两平台都是 Ctrl+Shift+`（Mac 也用 Ctrl 不是 ⌘）。
  // 不能用裸 Ctrl+字母，但 Ctrl+Shift 是安全区；浏览器也不截这一组。
  if (e.ctrlKey && e.shiftKey && !e.altKey && !e.metaKey && code === "Backquote") {
    return "newTerminal";
  }

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
    if (mac) {
      if (code === "BracketRight") return "nextTab";
      if (code === "BracketLeft") return "prevTab";
      return MAC_MODSHIFT_LETTERS[code] ?? null;
    }
    if (code === "Tab") return "prevTab";
    return MODSHIFT_LETTERS[code] ?? null;
  }

  if (!mac) {
    if (code === "Tab") return "nextTab";
    if (code === "KeyK") return "palette";
    return null;
  }

  const d = digit(code);
  if (d !== null) return d === 0 ? "overview" : (`tab${d}` as Command);
  return MAC_MOD_LETTERS[code] ?? null;
}
