/**
 * rio 引擎的 DOM 装配与事件接线：Terminal（rioterm 的 WASM VT 核心）+ 可替换的渲染器
 * + 隐藏 textarea + 键鼠/滚轮/剪贴板/链接。
 *
 * 移植自 rioterm 0.1.8 的 src/dom.ts（MIT，raphamorim/riotermjs）。为什么不直接用
 * rioterm 的 open()：它把 CanvasRenderer / DOMRenderer 写死，没有渲染器注入口；
 * 而且渲染器构造时量死 cell 尺寸，改字体只能 serialize → dispose → 重开整个 WASM
 * 实例再猜 VT 模式。自己持有这一层之后，换字体/主题/DPR 只换渲染器，Terminal 不动：
 * `Terminal.setCellSize` 保 cols/rows，内容、alt screen、鼠标模式全都天然保住。
 *
 * 与 rioterm 原版的差异：
 * - 渲染器由 renderer.ts 的工厂创建，WebGPU 不可用或构造失败回落 canvas；
 * - 键盘先过 keyRoute：IME 组合与全局快捷键原样放过（不 preventDefault、不
 *   stopPropagation），RioAdapter 以前的 capture 拦截 + 克隆重派发补丁随之删除；
 * - IME 的提交、预编辑覆盖层与候选框光标锚点内置（rioterm 0.1.x 没接提交，textarea
 *   也常驻视口外，组合中的拼音完全看不见）；
 * - 滚轮分两种口径：程序接管（鼠标上报 / alt screen）时一个事件最多一次点击，按 xterm.js
 *   的门槛与触控板阻尼；本地 scrollback 才按行推并攒余量（触控板慢滚能动）；
 * - 鼠标按键上报（rioterm 没有 API，上游 dom.ts 只做本地选区）：程序开了鼠标协议就把按下 /
 *   松开 / 拖动按 xterm.js 的口径合成报文经 terminal.input() 送出（mouse.ts），协议与编码由
 *   shared 的 TermModeTracker 从输出流跟踪，所以输出必须经 handle.write() 进来；
 * - mouseup 不写剪贴板：TerminalView 已在宿主 mouseup 上按 getSelection() 写，避免双写；
 * - 去掉 predictiveEcho、内置 ResizeObserver（宿主驱动 fit）、autoFocus、confirm() 链接提示；
 * - window 级 mousemove/mouseup 只在拖选期间挂着；
 * - DPR 变化与 device lost 自动换渲染器。
 */

import { TermModeTracker } from "@falcon/shared";
import { handleKeyboardEvent, initWasm, modsOf, Terminal } from "rioterm";
import { browserDprEnv, watchDpr, type DprEnv } from "./dpr.js";
import {
  acquireGpu,
  browserGpuEnv,
  isDev,
  pickRenderer,
  type GpuEnv,
  type GpuHandle,
  type RioRendererKind,
  type WebGpuFailReason,
} from "./gpu.js";
import { imeCursorRect } from "./ime.js";
import { routeKey } from "./keyRoute.js";
import { MouseReporter, type MouseAction, type MouseButton } from "./mouse.js";
import { createRenderer, RendererInitError, type RioAppearance, type RioRenderer } from "./renderer.js";
import { themeBase } from "./theme.js";
import { WheelAccumulator } from "./wheel.js";
// 副作用导入：把 WebGPU 渲染器的工厂注册进 renderer.ts（避免 renderer.ts 反向依赖 webgpu/）
import "./webgpu/webgpuRenderer.js";

export { readRendererOverride } from "./gpu.js";
export type { RioRendererKind, WebGpuFailReason } from "./gpu.js";
export type { RioAppearance, RioRenderer } from "./renderer.js";

export type RendererChangeCause = "dpr" | "device-lost";

export interface RioOpenOptions {
  /** 请求的渲染器；实际生效的看 handle.rendererKind */
  renderer: RioRendererKind;
  scrollback: number;
  appearance: RioAppearance;
  /** 命中全局快捷键的按键，终端不吃、原样放行 */
  isGlobalKey(e: KeyboardEvent): boolean;
  /** 点击 OSC 8 / 检测到的 URL */
  activateLink(uri: string): void;
  writeClipboard(text: string): Promise<void>;
  readClipboard(): Promise<string>;
  /** 只报告 open.ts 自己发起的换渲染器（DPR 变化、设备丢失）；宿主发起的 setAppearance 不报 */
  onRendererChanged?(active: RioRendererKind, cause: RendererChangeCause): void;
  /** 依赖注入：测试与"模拟 WebGPU 不可用"用 */
  env?: { gpu?: GpuEnv; dpr?: DprEnv };
}

export interface RioHandle {
  readonly terminal: Terminal;
  /**
   * 输出一律从这里进，别直接 terminal.write()：先喂给 VT 模式跟踪（鼠标上报要知道程序开的
   * 协议与编码），再交给 terminal。RIS（adapter 的 reset）同样走这里，跟踪器随之清零。
   */
  write(data: string | Uint8Array): void;
  /** getter：换渲染器后指向新的 */
  readonly renderer: RioRenderer;
  readonly rendererKind: RioRendererKind;
  /** 请求 webgpu 但在跑 canvas 时有值 */
  readonly fallbackReason: WebGpuFailReason | undefined;
  focus(): void;
  /** 同步。只换渲染器（重新量 cell → terminal.setCellSize），Terminal 不动；宿主随后 fit */
  setAppearance(appearance: RioAppearance): void;
  dispose(): void;
}

interface Built {
  renderer: RioRenderer;
  kind: RioRendererKind;
  reason?: WebGpuFailReason;
}

export async function openRio(host: HTMLElement, opts: RioOpenOptions): Promise<RioHandle> {
  const gpuEnv = opts.env?.gpu ?? browserGpuEnv();
  // wasm 首次加载 ≈1.1MB，与 GPU 设备获取并行；initWasm 内部 ??= 缓存，重复调用免费
  const [, gpuResult] = await Promise.all([
    initWasm(),
    opts.renderer === "webgpu" ? acquireGpu(gpuEnv) : Promise.resolve(null),
  ]);

  const terminal = new Terminal({ scrollback: opts.scrollback });
  // 鼠标按键上报要知道程序开的是哪种协议与编码，rioterm 的 modes() 只有一个布尔位：
  // 在输出流上自己跟踪 DEC 私有模式，与服务端回放前缀是同一个跟踪器（回放前缀也在流里，
  // 重连后同样能对上）。字节流按 UTF-8 流式解码，序列被 chunk 切开也能续上。
  const modes = new TermModeTracker();
  const modeDecoder = new TextDecoder();
  const write = (data: string | Uint8Array) => {
    modes.track(typeof data === "string" ? data : modeDecoder.decode(data, { stream: true }));
    terminal.write(data);
  };
  let gpu: GpuHandle | null = gpuResult?.ok ? gpuResult.gpu : null;
  let appearance = opts.appearance;
  let disposed = false;

  /** 构造渲染器；webgpu 构造失败（无 context / 设备没了）退到 canvas，旧渲染器由调用方处理 */
  const build = (kind: RioRendererKind): Built => {
    if (kind === "webgpu") {
      try {
        return { renderer: createRenderer("webgpu", terminal, appearance, gpu), kind: "webgpu" };
      } catch (err) {
        const reason = err instanceof RendererInitError ? err.reason : "no-context";
        console.warn("rio: webgpu renderer init failed, falling back to canvas", err);
        return { renderer: createRenderer("canvas", terminal, appearance, null), kind: "canvas", reason };
      }
    }
    return { renderer: createRenderer("canvas", terminal, appearance, null), kind: "canvas" };
  };

  const picked = pickRenderer(opts.renderer, gpuResult);
  const first = build(picked.kind);
  let renderer = first.renderer;
  let rendererKind = first.kind;
  let fallbackReason: WebGpuFailReason | undefined = picked.reason ?? first.reason;

  const container = document.createElement("div");
  container.style.position = "relative";
  container.style.overflow = "hidden";
  container.appendChild(renderer.element);

  // 隐藏 textarea 持有焦点，键盘 / paste 事件与 IME 组合都落在它身上
  const textarea = document.createElement("textarea");
  Object.assign(textarea.style, {
    position: "absolute",
    left: "-9999px",
    top: "0",
    width: "0",
    height: "0",
    opacity: "0",
    padding: "0",
    margin: "0",
    border: "0",
    resize: "none",
    overflow: "hidden",
    pointerEvents: "none",
  } satisfies Partial<CSSStyleDeclaration>);
  textarea.setAttribute("autocorrect", "off");
  textarea.setAttribute("autocapitalize", "off");
  textarea.setAttribute("spellcheck", "false");
  textarea.wrap = "off";

  // 预编辑覆盖层：组合中的拼音 / 假名落在透明 textarea 里看不见，渲染器也画不了——
  // 组合串还没进 PTY，WASM 里没有它的格子。与 xterm 的 composition-view 同一做法：
  // 按终端字体把组合串画在光标格上，组合结束即隐藏。
  const preedit = document.createElement("div");
  Object.assign(preedit.style, {
    position: "absolute",
    display: "none",
    left: "0",
    top: "0",
    overflow: "hidden",
    // macOS 拼音的组合串带分词空格（"ni hao"），nowrap 会把连续空格折叠掉
    whiteSpace: "pre",
    pointerEvents: "none",
    textDecoration: "underline",
    // 组合串超出右边缘时要看到的是末尾而不是开头：rtl 容器让溢出发生在左侧被裁掉，
    // 文本本身用 LRM 钉成 LTR（xterm 同款）
    direction: "rtl",
  } satisfies Partial<CSSStyleDeclaration>);
  container.appendChild(preedit);
  container.appendChild(textarea);
  host.appendChild(container);

  const disposers: Array<() => void> = [];
  const on = <K extends keyof HTMLElementEventMap>(
    el: HTMLElement,
    type: K,
    fn: (ev: HTMLElementEventMap[K]) => void,
    options?: AddEventListenerOptions
  ) => {
    el.addEventListener(type, fn as EventListener, options);
    // remove 时必须传同一个 options（capture 参与匹配）
    disposers.push(() => el.removeEventListener(type, fn as EventListener, options));
  };

  // 浏览器只认实际接收 composition 事件的 textarea，不认 canvas/WebGPU 画出的光标。
  // terminal 输出可能很密，定位按帧合并；非当前 tab 不做额外 WASM update，focus 时补齐。
  // 组合期间 textarea 与预编辑覆盖层同宽：textarea 里的插入符在组合串末尾，系统候选框
  // 才锚在末尾而不是光标格；覆盖层与 textarea 字体必须一致，两边量出的宽度才对得上。
  let imeRaf: number | null = null;
  let lastImePosition = "";
  let lastPreeditPosition = "";
  let lastImeStyle = "";
  let preeditText = "";
  const positionIme = () => {
    const { foreground, background } = themeBase(appearance.theme);
    const style = `${appearance.fontFamily}:${appearance.fontSize}:${foreground}:${background}`;
    if (style !== lastImeStyle) {
      lastImeStyle = style;
      for (const el of [textarea, preedit]) {
        el.style.fontFamily = appearance.fontFamily;
        el.style.fontSize = `${appearance.fontSize}px`;
      }
      preedit.style.color = foreground;
      preedit.style.backgroundColor = background;
    }
    const rect = imeCursorRect(terminal.cursorPosition(), {
      cols: terminal.options.cols,
      rows: terminal.options.rows,
      cellWidth: renderer.cellWidth,
      cellHeight: renderer.cellHeight,
      displayOffset: terminal.displayOffset(),
    });
    if (!rect) return;
    const preeditPosition = `${rect.left}:${rect.top}:${rect.maxWidth}:${rect.height}`;
    if (preeditPosition !== lastPreeditPosition) {
      lastPreeditPosition = preeditPosition;
      Object.assign(preedit.style, {
        left: `${rect.left}px`,
        top: `${rect.top}px`,
        maxWidth: `${rect.maxWidth}px`,
        height: `${rect.height}px`,
        lineHeight: `${rect.height}px`,
      } satisfies Partial<CSSStyleDeclaration>);
    }
    // 读布局只发生在组合期间；覆盖层被右边缘裁掉时 textarea 也只到右边缘，插入符跟着贴边
    const width = preeditText
      ? Math.max(rect.width, preedit.getBoundingClientRect().width)
      : rect.width;
    const position = `${rect.left}:${rect.top}:${width}:${rect.height}`;
    if (position === lastImePosition) return;
    lastImePosition = position;
    Object.assign(textarea.style, {
      left: `${rect.left}px`,
      top: `${rect.top}px`,
      width: `${width}px`,
      height: `${rect.height}px`,
      lineHeight: `${rect.height}px`,
    } satisfies Partial<CSSStyleDeclaration>);
  };
  const setPreedit = (text: string) => {
    if (text === preeditText) return;
    preeditText = text;
    // LRM 把组合串钉成 LTR，外层 rtl 只负责让溢出的开头被裁掉
    preedit.textContent = text ? `\u200E${text}\u200E` : "";
    preedit.style.display = text ? "block" : "none";
    positionIme();
  };
  const runScheduledImePosition = () => {
    imeRaf = null;
    positionIme();
  };
  const scheduleImePosition = () => {
    if (document.activeElement !== textarea || imeRaf != null) return;
    imeRaf = requestAnimationFrame(runScheduledImePosition);
  };
  const cursorSub = terminal.onUpdate(scheduleImePosition);
  disposers.push(() => {
    if (imeRaf != null) cancelAnimationFrame(imeRaf);
    cursorSub.dispose();
  });

  // ---- 键盘 ----
  on(textarea, "keydown", (e) => {
    // 只有可能是复制快捷键时才去问选区（FFI 调用），普通按键不付这个成本
    const maybeCopy = (e.metaKey && e.key === "c") || (e.ctrlKey && e.shiftKey);
    const hasSelection = maybeCopy && !!terminal.getSelection();
    switch (routeKey(e, opts.isGlobalKey, hasSelection)) {
      case "ime":
        // keyCode 229 往往先于 compositionstart；在浏览器读取候选框锚点前同步一次。
        positionIme();
        return;
      case "global":
        return;
      case "copy": {
        const text = terminal.getSelection();
        if (text) void opts.writeClipboard(text).catch(() => undefined);
        e.preventDefault();
        return;
      }
      case "paste":
        void opts
          .readClipboard()
          .then((text) => text && terminal.paste(text))
          .catch(() => undefined);
        e.preventDefault();
        return;
      default:
        if (handleKeyboardEvent(terminal, e)) e.preventDefault();
    }
  });
  on(textarea, "keyup", (e) => {
    const route = routeKey(e, opts.isGlobalKey, false);
    if (route === "ime" || route === "global") return;
    handleKeyboardEvent(terminal, e);
  });
  on(textarea, "paste", (e) => {
    const text = e.clipboardData?.getData("text");
    if (text) terminal.paste(text);
    e.preventDefault();
  });
  // IME：组合结束的提交文本，以及死键 / emoji 面板等不走 keydown 的直接插入。
  // 普通按键已被 handleKeyboardEvent preventDefault，不会落进 textarea，也就不会到这里
  on(textarea, "compositionstart", (e) => {
    positionIme();
    setPreedit(e.data);
  });
  // data 是当前完整组合串（xterm 也只信它，不等 textarea.value 在 input 事件后才更新）
  on(textarea, "compositionupdate", (e) => setPreedit(e.data));
  on(textarea, "compositionend", (e) => {
    // 先收覆盖层再提交：Esc 取消时 data 为空，只需隐藏
    setPreedit("");
    if (e.data) terminal.input(e.data);
    textarea.value = "";
  });
  on(textarea, "input", (e) => {
    const ie = e as InputEvent;
    if (ie.isComposing) return;
    if (ie.inputType === "insertText" && ie.data) terminal.input(ie.data);
    textarea.value = "";
  });
  on(textarea, "focus", () => {
    positionIme();
    renderer.setFocused?.(true);
  });
  on(textarea, "blur", () => renderer.setFocused?.(false));
  positionIme();

  // OSC 52：程序写系统剪贴板
  const clipboardSub = terminal.onClipboardWrite((text) => {
    void opts.writeClipboard(text).catch(() => undefined);
  });
  disposers.push(() => clipboardSub.dispose());

  // ---- 鼠标：上报 / 选区 / 链接 ----
  // 程序开了鼠标协议（zellij 永远开着 ?1002 ?1006）就把按键报给它，本地选区让位；按住 Shift
  // 才绕过上报做本地选区——zellij 自己的文档就叫用户按 Shift 选，Ghostty / Alacritty / Kitty
  // 在 macOS 上也都是 Shift。xterm.js 在 macOS 上要开 macOptionClickForcesSelection 才有绕过键
  // 且是 Option，我们没开（它会顺带关掉 Option 块选），所以 xterm 引擎在 macOS 上没有绕过键，
  // 这是两引擎目前唯一的手感差异。
  const reporter = new MouseReporter();
  const bypassReport = (e: MouseEvent) => e.shiftKey;
  const buttonOf = (e: MouseEvent): MouseButton => (e.button < 3 ? (e.button as MouseButton) : 3);
  /** 移动事件里按住的键：buttons 位图的位序与 button 不同（右键是 2、中键是 4） */
  const heldButton = (e: MouseEvent): MouseButton =>
    e.buttons & 1 ? 0 : e.buttons & 4 ? 1 : e.buttons & 2 ? 2 : 3;
  const reportMouse = (e: MouseEvent, action: MouseAction, button: MouseButton) => {
    const mode = { protocol: modes.mouseProtocol, encoding: modes.mouseEncoding };
    const { col, row } = renderer.cellAt(e.clientX, e.clientY);
    let x = 0;
    let y = 0;
    if (mode.encoding === 1016) {
      // 像素坐标：画布左上角起、夹在画布内的 CSS 像素，与 xterm.js 同口径
      const rect = renderer.element.getBoundingClientRect();
      x = Math.min(Math.max(0, Math.floor(e.clientX - rect.left)), Math.max(0, Math.floor(rect.width) - 1));
      y = Math.min(Math.max(0, Math.floor(e.clientY - rect.top)), Math.max(0, Math.floor(rect.height) - 1));
    }
    const report = reporter.report(
      { action, button, col, row, x, y, shift: e.shiftKey, alt: e.altKey, ctrl: e.ctrlKey },
      mode,
      { cols: terminal.options.cols, rows: terminal.options.rows }
    );
    // send_text 原样到 onData，不做 CRLF / bracketed paste 之类的改写
    if (report) terminal.input(report);
  };
  let reporting = false;
  const onReportMove = (e: MouseEvent) => {
    // 按住期间的拖动；无按键移动由 container 上的 mousemove 按 ?1003 报
    if (e.buttons) reportMouse(e, "move", heldButton(e));
  };
  const endReporting = () => {
    if (!reporting) return;
    reporting = false;
    window.removeEventListener("mousemove", onReportMove);
    window.removeEventListener("mouseup", onReportUp);
  };
  const onReportUp = (e: MouseEvent) => {
    if (e.button <= 2) reportMouse(e, "up", buttonOf(e));
    // 还有别的键按着就继续跟
    if (!e.buttons) endReporting();
  };
  disposers.push(endReporting);

  let selecting = false;
  let downAt: { x: number; y: number } | null = null;
  const onDragMove = (e: MouseEvent) => {
    const { col, row, sideRight } = renderer.cellAt(e.clientX, e.clientY);
    terminal.selectionUpdate(row, col, sideRight);
  };
  const endDrag = () => {
    if (!selecting) return;
    selecting = false;
    window.removeEventListener("mousemove", onDragMove);
    window.removeEventListener("mouseup", onDragUp);
  };
  const onDragUp = (e: MouseEvent) => {
    endDrag();
    // 点击不拖动（<5px）命中链接就打开；拖选不算
    if (downAt && Math.hypot(e.clientX - downAt.x, e.clientY - downAt.y) < 5) {
      const { col, row } = renderer.cellAt(e.clientX, e.clientY);
      const link = terminal.linkAt(row, col);
      if (link) opts.activateLink(link.uri);
    }
    downAt = null;
  };
  on(container, "mousedown", (e) => {
    if (e.button > 2) return;
    if (modes.mouseProtocol && !bypassReport(e)) {
      textarea.focus();
      // 残留的本地选区盖在 TUI 上没意义，点一下就清
      terminal.clearSelection();
      reportMouse(e, "down", buttonOf(e));
      if (!reporting) {
        reporting = true;
        // 按住期间移出终端也要继续报拖动与松开，挂 window；xterm.js 同样做法
        window.addEventListener("mousemove", onReportMove);
        window.addEventListener("mouseup", onReportUp);
      }
      // 右键照样冒到宿主的 contextmenu：xterm 引擎也是报文与菜单都有
      e.preventDefault();
      return;
    }
    if (e.button !== 0) return;
    textarea.focus();
    downAt = { x: e.clientX, y: e.clientY };
    const { col, row, sideRight } = renderer.cellAt(e.clientX, e.clientY);
    const kind = e.detail >= 3 ? "line" : e.detail === 2 ? "word" : e.altKey ? "block" : "simple";
    terminal.selectionBegin(row, col, kind, sideRight);
    selecting = true;
    window.addEventListener("mousemove", onDragMove);
    window.addEventListener("mouseup", onDragUp);
    // 阻止 canvas 抢焦点与页面文字选区；不影响上面显式的 textarea.focus()
    e.preventDefault();
  });
  disposers.push(endDrag);

  on(container, "mousemove", (e) => {
    // ?1003 连无按键移动也报；按住时的移动走 window 上的 onReportMove
    if (modes.mouseProtocol === 1003 && !e.buttons) reportMouse(e, "move", 3);
  });

  let hoverCell: { row: number; col: number } | null = null;
  on(container, "mousemove", (e) => {
    const { col, row } = renderer.cellAt(e.clientX, e.clientY);
    if (hoverCell && hoverCell.row === row && hoverCell.col === col) return;
    hoverCell = { row, col };
    const link = terminal.linkAt(row, col);
    renderer.setHoverLink(link ? { line: row, startCol: link.startCol, endCol: link.endCol } : null);
    container.style.cursor = link ? "pointer" : "";
  });
  on(container, "mouseleave", () => {
    hoverCell = null;
    renderer.setHoverLink(null);
    container.style.cursor = "";
  });

  // ---- 滚轮 ----
  const wheel = new WheelAccumulator();
  on(
    container,
    "wheel",
    (e) => {
      // zellij / vim 这类程序接管了滚轮（鼠标上报或 alt screen）时，scroll_wheel 把 n 行翻成
      // n 条上报，zellij 每条又自己滚 3 行，鼠标一格就冲出十几行。按 xterm.js 的口径：一个
      // DOM 事件最多一次点击，行数只做门槛（见 wheel.ts）。只有本地 scrollback 才按行推。
      const modes = terminal.modes();
      const lines =
        modes.mouseTracking || modes.altScreen
          ? wheel.click(e.deltaMode, e.deltaY, renderer.cellHeight, terminal.options.rows)
          : wheel.push(e.deltaMode, e.deltaY, renderer.cellHeight, terminal.options.rows);
      // 不足一行的事件在攒余量，也算终端消费了：终端占满 pane，页面本来就不该跟着滚
      if (lines === 0) {
        e.preventDefault();
        return;
      }
      const { col, row } = renderer.cellAt(e.clientX, e.clientY);
      // 只有终端用掉了（鼠标上报、alternate scroll、scrollback 动了）才拦事件
      const before = terminal.displayOffset();
      const consumed = terminal.scrollWheel(lines, col, row, modsOf(e as unknown as KeyboardEvent));
      if (consumed || terminal.displayOffset() !== before) e.preventDefault();
    },
    { passive: false }
  );

  // ---- 换渲染器 ----
  /**
   * construct-then-swap：先构造新的（它自己 setCellSize + 订阅 onUpdate），成功了
   * 才 dispose 旧的；新的构造抛异常则旧的原样保留。返回实际生效的渲染器。
   */
  const swapRenderer = (kind: RioRendererKind): RioRendererKind => {
    const built = build(kind);
    renderer.dispose();
    renderer = built.renderer;
    rendererKind = built.kind;
    fallbackReason = built.reason;
    // 画布永远是第一个孩子，预编辑覆盖层与 textarea 都在它后面
    container.prepend(renderer.element);
    hoverCell = null;
    container.style.cursor = "";
    renderer.setFocused?.(document.activeElement === textarea);
    // 新渲染器可能换了 cell 度量；候选框必须跟着同一帧迁移。
    positionIme();
    return built.kind;
  };

  /** 每次换回 webgpu 想要的渲染器种类：请求的是 webgpu 且当前有设备 */
  const wantedKind = (): RioRendererKind => (opts.renderer === "webgpu" && gpu ? "webgpu" : "canvas");

  // device lost：先原地重建（iOS Safari 后台驱逐 GPU 资源后回前台就是这条路，终端
  // 状态在 WASM 里没丢），拿不到新设备再退 canvas。"destroyed" 是我们主动 destroy
  // 才会有的原因，这里从不主动 destroy，出现即忽略。
  const watchLost = (handle: GpuHandle) => {
    void handle.lost.then((info) => {
      if (disposed || gpu !== handle) return;
      gpu = null;
      if (info.reason === "destroyed") return;
      console.warn("rio: GPU device lost, rebuilding", info.message);
      void reinitGpu();
    });
  };
  const reinitGpu = async () => {
    const res = await acquireGpu(gpuEnv);
    if (disposed) return;
    if (res.ok) {
      gpu = res.gpu;
      watchLost(gpu);
    }
    const kind = swapRenderer(wantedKind());
    if (kind === "canvas") fallbackReason = "lost";
    opts.onRendererChanged?.(kind, "device-lost");
  };
  if (gpu) watchLost(gpu);

  const stopDpr = watchDpr(() => {
    if (disposed) return;
    opts.onRendererChanged?.(swapRenderer(rendererKind), "dpr");
  }, opts.env?.dpr ?? browserDprEnv());

  const handle: RioHandle = {
    terminal,
    write,
    get renderer() {
      return renderer;
    },
    get rendererKind() {
      return rendererKind;
    },
    get fallbackReason() {
      return fallbackReason;
    },
    focus: () => textarea.focus(),
    setAppearance(next) {
      if (disposed) return;
      appearance = next;
      swapRenderer(wantedKind());
    },
    dispose() {
      if (disposed) return;
      disposed = true;
      stopDpr();
      // 监听必须先摘干净：terminal.dispose() 之后任何 terminal.* 调用都是 UB
      for (const dispose of disposers) dispose();
      renderer.dispose();
      terminal.dispose();
      container.remove();
      debugHandles()?.delete(handle);
    },
  };
  debugHandles()?.add(handle);
  return handle;
}

/** DEV 下把活着的 handle 挂到 globalThis.__rioHandles，控制台里看 rendererKind / stats 用 */
function debugHandles(): Set<RioHandle> | null {
  if (!isDev()) return null;
  const g = globalThis as Record<string, unknown>;
  return (g.__rioHandles ??= new Set<RioHandle>()) as Set<RioHandle>;
}
