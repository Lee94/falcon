/**
 * 鼠标按键上报：DOM 鼠标事件 → 发给程序的 VT 报文。纯函数，零 DOM。
 *
 * rioterm 0.1.8 的 WASM 只导出 scroll_wheel 一个鼠标入口，按下 / 松开 / 拖动没有 API，
 * 上游 dom.ts 也只做本地选区：zellij 切 tab、vim 点光标这类 TUI 点击在 rio 下全是哑的。
 * 报文在这里按 xterm.js（MouseStateService 的协议筛选 / 编码 + MouseService._triggerMouseEvent
 * 的去重）合成，经 terminal.input() 原样送到 onData，两引擎发出的字节一致：
 *
 * - 协议决定哪些事件出门：?9 只报按下且抹掉修饰键；?1000 报按下 / 松开；?1002 再加按住时的
 *   拖动；?1003 连无按键移动也报。移动报文按格（?1016 按像素）去重，同一格里晃鼠标不刷屏。
 * - 编码决定报文形状：默认是 CSI M 加三个 (值+32) 的单字节，松开一律报 3（无按键）；
 *   ?1006 SGR 是 CSI < b;x;y M / m，松开能带按键号；?1016 同 SGR 但坐标是像素。
 * - 默认编码超过 ASCII 的报文丢掉：往后端的输入通道是 JSON 文本帧，没有二进制口，0x80 以上
 *   的字节会被当成码点 UTF-8 编码成两字节，程序按单字节解析必错。xterm 引擎那边这种报文走
 *   triggerBinaryEvent（onBinary），而 adapter 只接了 onData，同样发不出去；现代 TUI 都开
 *   ?1006，实际只影响不开 SGR 的老程序在第 96 列 / 行之后的点击。
 *
 * 滚轮不在这里：rioterm 的 scroll_wheel 自己会报，见 wheel.ts。
 */

export type MouseAction = "down" | "up" | "move";

/** 0 左 / 1 中 / 2 右 / 3 无按键（只在 ?1003 的移动里出现） */
export type MouseButton = 0 | 1 | 2 | 3;

export interface MouseReportEvent {
  action: MouseAction;
  button: MouseButton;
  /** 0 起的格坐标；报文里 +1 */
  col: number;
  row: number;
  /** 终端左上角起的 CSS 像素坐标，只有 ?1016 用；调用方先夹到画布内 */
  x: number;
  y: number;
  shift: boolean;
  alt: boolean;
  ctrl: boolean;
}

export interface MouseMode {
  /** 0 / 9 / 1000 / 1002 / 1003，见 TermModeTracker.mouseProtocol */
  protocol: number;
  /** 0 / 1006 / 1016，见 TermModeTracker.mouseEncoding */
  encoding: number;
}

const MOD_SHIFT = 4;
const MOD_ALT = 8;
const MOD_CTRL = 16;
/** 移动报文在按键号上加的位 */
const MOTION = 32;
const BUTTON_NONE: MouseButton = 3;
/** 默认编码把值 +32 塞进单字节；文本通道发不出 0x80 以上的字节，见文件头 */
const DEFAULT_MAX_BYTE = 0x7f;

export class MouseReporter {
  /** 最近一条通过筛选的事件，移动报文按它去重（xterm.js 的 _lastEvent） */
  private last: MouseReportEvent | null = null;

  /**
   * @param grid 当前格数，格坐标出界的事件丢掉
   * @returns 要发给程序的报文；协议不要这个事件、与上一条移动重复或编码不了时为 null
   */
  report(e: MouseReportEvent, mode: MouseMode, grid: { cols: number; rows: number }): string | null {
    if (e.col < 0 || e.col >= grid.cols || e.row < 0 || e.row >= grid.rows) return null;
    // 无按键只有移动一种形态
    if (e.button === BUTTON_NONE && e.action !== "move") return null;
    const ev = restrict(e, mode.protocol);
    if (!ev) return null;
    if (ev.action === "move" && this.last && sameEvent(this.last, ev, mode.encoding === 1016)) return null;
    this.last = ev;
    return encode(ev, mode.encoding);
  }

  reset(): void {
    this.last = null;
  }
}

/** 协议筛选（xterm.js 的 DEFAULT_PROTOCOLS[*].restrict）；返回可能被改写的副本 */
function restrict(e: MouseReportEvent, protocol: number): MouseReportEvent | null {
  switch (protocol) {
    case 9:
      // X10：只报按下，没有修饰键
      return e.action === "down" ? { ...e, shift: false, alt: false, ctrl: false } : null;
    case 1000:
      return e.action === "move" ? null : e;
    case 1002:
      // 只报按住时的拖动
      return e.action === "move" && e.button === BUTTON_NONE ? null : e;
    case 1003:
      return e;
    default:
      return null;
  }
}

function sameEvent(a: MouseReportEvent, b: MouseReportEvent, pixels: boolean): boolean {
  if (pixels ? a.x !== b.x || a.y !== b.y : a.col !== b.col || a.row !== b.row) return false;
  return (
    a.button === b.button &&
    a.action === b.action &&
    a.shift === b.shift &&
    a.alt === b.alt &&
    a.ctrl === b.ctrl
  );
}

function eventCode(e: MouseReportEvent, sgr: boolean): number {
  let code = (e.ctrl ? MOD_CTRL : 0) | (e.shift ? MOD_SHIFT : 0) | (e.alt ? MOD_ALT : 0) | (e.button & 3);
  if (e.action === "move") code |= MOTION;
  // 只有 SGR 能在松开时报按键号，其余编码一律报"无按键"
  else if (e.action === "up" && !sgr) code |= BUTTON_NONE;
  return code;
}

function encode(e: MouseReportEvent, encoding: number): string | null {
  if (encoding === 1006 || encoding === 1016) {
    const final = e.action === "up" ? "m" : "M";
    const px = encoding === 1016 ? e.x : e.col + 1;
    const py = encoding === 1016 ? e.y : e.row + 1;
    return `\x1b[<${eventCode(e, true)};${px};${py}${final}`;
  }
  const b = eventCode(e, false) + 32;
  const x = e.col + 1 + 32;
  const y = e.row + 1 + 32;
  if (b > DEFAULT_MAX_BYTE || x > DEFAULT_MAX_BYTE || y > DEFAULT_MAX_BYTE) return null;
  return `\x1b[M${String.fromCharCode(b, x, y)}`;
}
