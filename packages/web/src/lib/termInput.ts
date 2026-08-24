/**
 * 移动端键位条 → 终端会话的输入旁路。
 *
 * 软键盘打不出 Esc / Tab / Ctrl / 方向键，键位条要把这些序列直接送进当前
 * 会话的 PTY。TerminalView 建立 WS 时把发送端口注册进来，键位条按 sessionId
 * 取用。不走 zustand store：这是高频、无渲染语义的旁路，不值得让全部
 * selector 陪跑（同一理由见 store 里 sidebarAutoHidden 的注释）。
 */

export interface TermInputPort {
  /** 原样发往 PTY（TerminalView 接到 WS 的 input 消息上） */
  send(data: string): void;
  /** DECCKM 应用光标键模式：开着时方向键要发 SS3（ESC O A），否则 CSI */
  appCursorKeys(): boolean;
}

const ports = new Map<string, TermInputPort>();

export function registerTermInput(sessionId: string, port: TermInputPort): () => void {
  ports.set(sessionId, port);
  return () => {
    // 引擎切换时新 TerminalView 可能先注册、旧的后清理，别把新端口摘掉
    if (ports.get(sessionId) === port) ports.delete(sessionId);
  };
}

export function getTermInput(sessionId: string): TermInputPort | undefined {
  return ports.get(sessionId);
}

/** 键位条上的键。粘滞 Ctrl 是修饰态不是键，单独处理 */
export type ExtraKey = "esc" | "tab" | "up" | "down" | "left" | "right";

/**
 * 键 → 字节序列。方向键按 DECCKM 分流：zellij / vim 开应用模式后只认 SS3，
 * 发错形态的直接后果是"方向键在 TUI 里没反应"。Ctrl+方向键（词间跳转）
 * 走 CSI 1;5 修饰形态，此时 DECCKM 不参与（xterm 对带修饰键的方向键一律发 CSI）。
 */
export function extraKeySeq(key: ExtraKey, appCursor: boolean, ctrl = false): string {
  switch (key) {
    case "esc":
      return "\x1b";
    case "tab":
      return "\t";
    case "up":
      return ctrl ? "\x1b[1;5A" : appCursor ? "\x1bOA" : "\x1b[A";
    case "down":
      return ctrl ? "\x1b[1;5B" : appCursor ? "\x1bOB" : "\x1b[B";
    case "right":
      return ctrl ? "\x1b[1;5C" : appCursor ? "\x1bOC" : "\x1b[C";
    case "left":
      return ctrl ? "\x1b[1;5D" : appCursor ? "\x1bOD" : "\x1b[D";
  }
}

/**
 * 粘滞 Ctrl：键位条点一下 Ctrl，下一个从软键盘敲进来的字符变成控制字节。
 * 一次性——下一个单字符输入无论转没转成功都熄灭；长亮的修饰键在触屏上
 * 必然被遗忘，然后在某次输入 c 时变成一记谁也没想按的 Ctrl+C。
 */
let ctrlArmed = false;
const ctrlListeners = new Set<() => void>();

export function isCtrlArmed(): boolean {
  return ctrlArmed;
}

export function setCtrlArmed(v: boolean): void {
  if (ctrlArmed === v) return;
  ctrlArmed = v;
  for (const fn of ctrlListeners) fn();
}

/** 给 useSyncExternalStore 用；键位条据此点亮 Ctrl 键帽 */
export function subscribeCtrl(fn: () => void): () => void {
  ctrlListeners.add(fn);
  return () => ctrlListeners.delete(fn);
}

/** 单个字符转控制字节；转不了返回 null（调用方原样发） */
export function ctrlByte(ch: string): string | null {
  if (ch.length !== 1) return null;
  if (ch === " ") return "\x00"; // Ctrl+Space = NUL（Emacs set-mark）
  if (ch === "?") return "\x7f";
  const code = ch.toUpperCase().charCodeAt(0);
  // @ A-Z [ \ ] ^ _ → 0x00-0x1f，终端惯例：大小写同义
  if (code >= 0x40 && code <= 0x5f) return String.fromCharCode(code - 0x40);
  return null;
}

/**
 * TerminalView 的 onData 旁路：Ctrl 点亮时把这份输入试着转成控制字节。
 * 多字符输入（IME 提交、粘贴、方向键序列）不消耗粘滞态——用户点了 Ctrl
 * 再选中文候选词，多半是想接着按那个真正的组合键。
 */
export function takeStickyCtrl(data: string): string {
  if (!ctrlArmed || data.length !== 1) return data;
  setCtrlArmed(false);
  return ctrlByte(data) ?? data;
}

/**
 * beforeinput 的删除类 inputType → 发给 PTY 的序列；不该拦的返回 null。
 *
 * 软键盘长按退格不发重复 keydown，只对"有内容的输入框"重复发
 * deleteContentBackward 这类编辑事件（见 termAdapter 里 wireSoftKeyRepeat
 * 的哨兵机制）。IME 组合期间的内部删除（deleteCompositionText /
 * deleteByComposition）与剪切、拖拽不是用户在按退格，拦了会毁组合输入。
 * 词删除（长按加速后的 deleteWordBackward）只折算一个退格：哨兵是假内容，
 * 词边界无从谈起，宁可少删不可多删。
 */
export function deleteSeq(inputType: string): string | null {
  if (!inputType.startsWith("delete")) return null;
  if (
    inputType === "deleteByCut" ||
    inputType === "deleteByDrag" ||
    inputType.includes("omposition")
  ) {
    return null;
  }
  return inputType.includes("Forward") ? "\x1b[3~" : "\x7f";
}

/**
 * xterm 6.1.0-beta.302 的触摸滚动 bug（实测）：手指接触期间发出的 SGR 滚轮
 * 报文正常（\x1b[<65;25;37M），抬手后的惯性阶段 Gesture 的合成事件没有
 * pageX/pageY，坐标算成 NaN 被原样序列化成 \x1b[<65;NaN;NaNM。zellij 的
 * CSI 解析在 'N' 处中断，剩余字节当普通输入落进 pane——触摸滚动就往终端里
 * 打出 "NaN;NaNM" 乱码。
 *
 * 发给 PTY 前用最近一次有效报文的坐标修复（惯性滚动因此保住；对 mojito 的
 * zellij 单 pane 布局，滚轮坐标本就不影响落点），无坐标可用时整条丢弃。
 * xterm 修掉之后这两个函数可以整体删除。
 */
const SGR_MOUSE_RE = /^\x1b\[<(\d+|NaN);(\d+|NaN);(\d+|NaN)([Mm])$/;

/** 合法 SGR 鼠标报文里的 "x;y"；不是报文或坐标已损坏时返回 null */
export function mouseReportCoord(data: string): string | null {
  const m = SGR_MOUSE_RE.exec(data);
  if (!m || data.includes("NaN")) return null;
  return `${m[2]};${m[3]}`;
}

/** 修复坐标为 NaN 的 SGR 鼠标报文；返回 null = 修不了，这条别发 */
export function repairMouseReport(data: string, lastCoord: string | null): string | null {
  if (!data.startsWith("\x1b[<") || !data.includes("NaN")) return data;
  const m = SGR_MOUSE_RE.exec(data);
  if (!m) return data; // 带 NaN 却不是完整报文的怪东西，不该出现；原样放行
  if (m[1] === "NaN") return null; // 按键号都坏了，无从修
  return lastCoord ? `\x1b[<${m[1]};${lastCoord}${m[4]}` : null;
}
