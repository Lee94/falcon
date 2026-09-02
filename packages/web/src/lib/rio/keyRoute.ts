/**
 * rio 引擎隐藏 textarea 上的按键分流。纯函数，open.ts 的 keydown/keyup 监听按
 * 返回值决定是交给 rioterm 编码、本地处理剪贴板，还是原样放过。
 *
 * "放过"（ime / global）意味着既不 preventDefault 也不 stopPropagation：
 * - IME 组合中的按键（isComposing，或 Android/老内核只给 keyCode 229）要落进
 *   textarea 走 compositionend，交给 rioterm 编码会把确认候选的 Enter 当真回车发给 PTY；
 * - 命中全局快捷键的按键让它自然冒泡到 window，App 的全局监听在那里。以前 rio
 *   自己的 open() 会在冒泡阶段吃掉它，逼得 RioAdapter 在 capture 阶段截断再克隆
 *   一份重派发；现在 textarea 归我们，不需要那套。
 *
 * 剪贴板快捷键与 rioterm 原版一致：⌘C（mac）/ Ctrl+Shift+C 只在有选区时复制，
 * 否则照常交给终端（⌘ 组合 rioterm 本来就不编码，Ctrl+Shift+C 会按 kitty 协议发）；
 * Ctrl+Shift+V 粘贴（⌘V 走浏览器原生 paste 事件，不经过这里）。
 */

export type KeyRoute = "ime" | "global" | "copy" | "paste" | "terminal";

/** KeyboardEvent 的最小子集，测试里手搓对象即可 */
export interface KeyLike {
  type: string;
  key: string;
  keyCode?: number;
  isComposing?: boolean;
  ctrlKey: boolean;
  metaKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
}

export function routeKey<E extends KeyLike>(
  e: E,
  isGlobalKey: (e: E) => boolean,
  hasSelection: boolean
): KeyRoute {
  if (e.isComposing || e.keyCode === 229) return "ime";
  if (isGlobalKey(e)) return "global";
  // 剪贴板动作只在 keydown 上；keyup 也会经过这里（kitty 协议下 release 同样要编码）
  if (e.type !== "keydown") return "terminal";
  const key = e.key.toLowerCase();
  const macCopy = e.metaKey && !e.ctrlKey && key === "c";
  const ctrlShift = e.ctrlKey && e.shiftKey && !e.metaKey;
  if ((macCopy || (ctrlShift && key === "c")) && hasSelection) return "copy";
  if (ctrlShift && key === "v") return "paste";
  return "terminal";
}
