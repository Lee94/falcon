/**
 * 终端引擎适配层：TerminalView 只面向 TermAdapter 接口，不关心底下是
 * xterm.js 还是 rioterm（实验性，Rio 的 Rust VT 核心编译成 WASM）。
 *
 * rio 引擎的 DOM 装配、渲染器（自研 WebGPU，不可用时回落 rioterm 自带的 canvas）
 * 与事件接线都在 lib/rio/ 下，连同 1.1MB 的 wasm 走动态 import，只在选中 rio
 * 引擎时下载。rio 引擎的已知短板：鼠标点击不会上报给 TUI（滚轮会，由 Rust 侧
 * 编码）；canvas 回退渲染器不支持光标闪烁。
 */

import { Terminal, type ITheme } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { WebglAddon } from "@xterm/addon-webgl";
import type { RioAppearance, RioHandle, RioRendererKind, WebGpuFailReason } from "./rio/open.js";

export type { RioRendererKind, WebGpuFailReason } from "./rio/open.js";
import { termFontStack, type TermPref } from "./term.js";
import { isUsableTermSize } from "./termFit.js";
import { deleteSeq } from "./termInput.js";
import { osc52ClipboardText, writeBrowserClipboard } from "./osc52.js";

export interface TermAdapterHooks {
  /** 用户输入（按键、鼠标上报、DA 应答），发往 PTY */
  onData(data: string): void;
  /** 格子数变了；接收方自行按 cols/rows 去重 */
  onResize(): void;
  /**
   * 异步引擎首次就绪，或引擎自发换了渲染器（DPR 变化、GPU 设备丢失）；应补一次量尺寸。
   * 宿主自己发起的 applyAppearance / refreshMetrics 不触发——那两条路径本来紧接着 fit()
   */
  onReady(): void;
  /** 命中全局快捷键的按键，终端不该吃 */
  isGlobalKey(e: KeyboardEvent): boolean;
  /** 引擎加载失败（wasm 拉不下来等），提示用户切回 xterm */
  onEngineError?(message: string): void;
  /** rio 引擎渲染器落定或变化。requested 是 webgpu 而 active 是 canvas 时宿主提示一次 */
  onRenderer?(info: RendererInfo): void;
}

export interface RendererInfo {
  requested: RioRendererKind;
  active: RioRendererKind;
  reason?: WebGpuFailReason;
  cause: "open" | "device-lost" | "dpr";
}

export interface TermAdapterInit {
  pref: TermPref;
  theme: ITheme;
  scrollback: number;
  hooks: TermAdapterHooks;
}

export interface TermAdapter {
  /** 未就绪时为引擎默认值；调用方以 fit() 成功与否决定能不能发给 PTY */
  readonly cols: number;
  readonly rows: number;
  open(host: HTMLElement): void;
  write(data: Uint8Array): void;
  /** RIS 级重置：replay 整份快照前清掉旧 VT 状态 */
  reset(): void;
  /** 按宿主容器适配格子；容器/引擎/字体没就绪返回 false（别把假尺寸发给 PTY） */
  fit(): boolean;
  /** 字体加载完成后的重排（xterm 重建字形 atlas；rio 重测 cell 后换一个渲染器） */
  refreshMetrics(): void;
  /** 热改外观。rio 引擎只换渲染器，Terminal 与 VT 状态不动 */
  applyAppearance(pref: TermPref, theme: ITheme): void;
  /** DECCKM 应用光标键模式是否开着；移动端键位条据此决定方向键发 SS3 还是 CSI */
  appCursorKeys(): boolean;
  focus(): void;
  paste(text: string): void;
  getSelection(): string;
  clear(): void;
  dispose(): void;
}

export function createTermAdapter(init: TermAdapterInit): TermAdapter {
  return init.pref.engine === "rio" ? new RioAdapter(init) : new XtermAdapter(init);
}

class XtermAdapter implements TermAdapter {
  private term: Terminal;
  private fitAddon: FitAddon;
  private host: HTMLElement | null = null;
  private pref: TermPref;
  private hooks: TermAdapterHooks;

  constructor(init: TermAdapterInit) {
    const { pref, theme, scrollback, hooks } = init;
    this.pref = pref;
    this.hooks = hooks;
    const term = new Terminal({
      fontFamily: termFontStack(pref),
      fontSize: pref.fontSize,
      lineHeight: pref.lineHeight,
      cursorStyle: pref.cursorStyle,
      cursorBlink: pref.cursorBlink,
      scrollback,
      // unicode.activeVersion 是 proposed API；不打开会在设 11 时直接抛
      allowProposedApi: true,
      // 默认 Unicode 6 把大量 CJK / emoji / 图标当成 1 格，后一个字符会盖掉右半
      rescaleOverlappingGlyphs: true,
      theme,
    });
    // 全局快捷键命中时把按键交给应用层：xterm 不处理，事件照样冒泡到 window。
    // Ctrl+C / Ctrl+R / Ctrl+W / Esc 不在快捷键表里，因此行为与原生终端一致。
    term.attachCustomKeyEventHandler((e) => e.type !== "keydown" || !hooks.isGlobalKey(e));
    this.fitAddon = new FitAddon();
    term.loadAddon(this.fitAddon);
    term.loadAddon(new WebLinksAddon());
    term.loadAddon(new Unicode11Addon());
    term.unicode.activeVersion = "11";
    // Claude Code / vim 选中即复制走 OSC 52；xterm.js 默认丢弃。只写不读。
    term.parser.registerOscHandler(52, (data) => {
      const text = osc52ClipboardText(data);
      if (text !== undefined) void writeBrowserClipboard(text).catch(() => undefined);
      return true;
    });
    term.onData(hooks.onData);
    term.onResize(hooks.onResize);
    this.term = term;
  }

  get cols(): number {
    return this.term.cols;
  }

  get rows(): number {
    return this.term.rows;
  }

  open(host: HTMLElement): void {
    this.host = host;
    this.term.open(host);
    // 触屏才需要软键盘退格连删的哨兵（桌面硬键盘的 keydown 自带重复）
    if (navigator.maxTouchPoints > 0 || "ontouchstart" in window) {
      this.wireSoftKeyRepeat(host);
    }
    // WebGL 渲染器：高吞吐输出（构建日志、TUI 全屏重绘）下吞吐比默认的
    // DOM 渲染器高一个数量级。WebGL2 不可用或上下文被回收（GPU 重置、
    // 开的 tab 超过浏览器上下文上限时最老的会被丢）就回落 DOM 渲染器。
    try {
      const webgl = new WebglAddon();
      webgl.onContextLoss(() => webgl.dispose());
      this.term.loadAddon(webgl);
    } catch {
      // 软渲染 / 老 GPU 环境，DOM 渲染器兜底
    }
  }

  /**
   * 软键盘长按退格连删。
   *
   * 机制：软键盘长按退格不重复发 keydown，只对"有内容的输入框"重复发
   * deleteContentBackward 编辑事件；而 xterm 的隐藏 textarea 恒为空、
   * _inputEvent 只认 insertText（读 6.1.0-beta.302 源码确认），于是只有
   * 首击那记 keydown 变成退格。这里往 textarea 里养一段空格哨兵，让长按
   * 能持续产生删除事件，拦下来转成退格发给 PTY，preventDefault 保住哨兵。
   *
   * 与 xterm 的三处既有逻辑不打架（都读过对应源码）：
   * - 首击去重：xterm 处理过的 Backspace keydown 会 preventDefault，正常
   *   不会再产生编辑事件；个别内核仍产生的，落在 35ms 窗口内被跳过；
   * - IME：CompositionHelper 按 compositionstart 时的 selection 偏移截取
   *   组合文本，哨兵在偏移之前不会混进去；组合期间绝不动 value，补哨兵
   *   一律推迟到 setTimeout(0)，排在 xterm 自己读值的 timeout 之后；
   * - Android 229 路径的 _handleAnyTextareaChanges 按"值变短发一个 \x7f"
   *   兜底，与 preventDefault 后值不变的哨兵互不重复。
   *
   * 监听器随 textarea 被 term.dispose() 移出 DOM 一起失效，不需要显式清理
   * （与 RioAdapter.wireIme 同一套生命周期约定）。rio 引擎的 textarea 由
   * Rust 侧接管，同样的 iOS 长按问题留待 rio 侧解决。
   */
  private wireSoftKeyRepeat(host: HTMLElement): void {
    const textarea = host.querySelector("textarea");
    if (!textarea) return;
    // 4 个空格：连 word-delete 一次吃掉整段也只折算一个退格，随后立刻补回
    const SENTINEL = "    ";
    let lastBackspaceDown = -1;
    let composing = false;

    const refill = () => {
      if (composing) return;
      if (textarea.value !== SENTINEL) textarea.value = SENTINEL;
      textarea.selectionStart = textarea.selectionEnd = SENTINEL.length;
    };
    const refillSoon = () => {
      window.setTimeout(refill, 0);
    };

    textarea.addEventListener("compositionstart", () => {
      composing = true;
    });
    textarea.addEventListener("compositionend", () => {
      composing = false;
      refillSoon();
    });
    textarea.addEventListener(
      "keydown",
      (e) => {
        if (e.key === "Backspace") lastBackspaceDown = e.timeStamp;
      },
      true
    );
    textarea.addEventListener(
      "beforeinput",
      (e) => {
        if (composing || e.isComposing) return;
        const seq = deleteSeq(e.inputType);
        if (seq === null) return;
        e.preventDefault();
        // 首击的 keydown 已经由 xterm 发过 \x7f，同一击派生的编辑事件跳过
        if (e.timeStamp - lastBackspaceDown < 35) return;
        this.hooks.onData(seq);
      },
      true
    );
    // 没被吊销的删除（个别内核）与普通输入之后，把哨兵养回去
    textarea.addEventListener("input", refillSoon);
    textarea.addEventListener("focus", refill);
    refill();
  }

  write(data: Uint8Array): void {
    this.term.write(data);
  }

  reset(): void {
    this.term.reset();
  }

  fit(): boolean {
    const box = {
      width: this.host?.offsetWidth ?? 0,
      height: this.host?.offsetHeight ?? 0,
    };
    if (!isUsableTermSize(this.fitAddon.proposeDimensions(), box)) return false;
    this.fitAddon.fit();
    return true;
  }

  /** 同值改 fontFamily 不会清 atlas；字号微扰一次即可。renderer 没就绪时 swallow。 */
  refreshMetrics(): void {
    if (this.term.rows <= 0) return;
    this.term.options.fontSize = this.pref.fontSize + 0.01;
    this.term.options.fontSize = this.pref.fontSize;
    try {
      this.term.refresh(0, this.term.rows - 1);
    } catch {
      // Viewport 有时还没挂上 dimensions
    }
  }

  applyAppearance(pref: TermPref, theme: ITheme): void {
    this.pref = pref;
    const o = this.term.options;
    o.fontFamily = termFontStack(pref);
    o.fontSize = pref.fontSize;
    o.lineHeight = pref.lineHeight;
    o.cursorStyle = pref.cursorStyle;
    o.cursorBlink = pref.cursorBlink;
    o.theme = theme;
  }

  appCursorKeys(): boolean {
    return this.term.modes.applicationCursorKeysMode;
  }

  focus(): void {
    this.term.focus();
  }

  paste(text: string): void {
    this.term.paste(text);
  }

  getSelection(): string {
    return this.term.getSelection();
  }

  clear(): void {
    this.term.clear();
  }

  dispose(): void {
    this.term.dispose();
  }
}

/**
 * rio 引擎。DOM 装配、事件接线与渲染器生命周期都在 rio/open.ts；这里只管与
 * TermAdapter 契约对接：输出队列、尺寸门控、外观比对、hooks 回调。
 */
class RioAdapter implements TermAdapter {
  private hooks: TermAdapterHooks;
  private pref: TermPref;
  private theme: ITheme;
  private scrollback: number;

  private host: HTMLElement | null = null;
  private handle: RioHandle | null = null;
  /** wasm 首次加载完成前到达的输出，按序回放。reset 以 RIS 字符串入队，顺序天然保持 */
  private queue: (string | Uint8Array)[] = [];
  private decoder = new TextDecoder();
  private disposed = false;
  private wantFocus = false;
  /** 传给 openRio 的外观快照。open 等待期间（wasm 首载窗口不小）改了外观，adopt 时补一次 */
  private openedWith: RioAppearance | null = null;

  constructor(init: TermAdapterInit) {
    this.hooks = init.hooks;
    this.pref = init.pref;
    this.theme = init.theme;
    this.scrollback = init.scrollback;
  }

  get cols(): number {
    return this.handle?.terminal.options.cols ?? 0;
  }

  get rows(): number {
    return this.handle?.terminal.options.rows ?? 0;
  }

  open(host: HTMLElement): void {
    this.host = host;
    void this.boot();
  }

  write(data: Uint8Array): void {
    this.push(data);
  }

  reset(): void {
    // rio 没有 reset()，RIS 等价（清格子、模式、解析器与回滚）
    this.push("\x1bc");
  }

  fit(): boolean {
    const handle = this.handle;
    const host = this.host;
    if (!handle || !host) return false;
    const box = { width: host.offsetWidth, height: host.offsetHeight };
    const { cellWidth, cellHeight } = handle.renderer;
    if (!(cellWidth > 0) || !(cellHeight > 0)) return false;
    // renderer.fit 内部同样按 floor(px/cell) 算，先自己算一遍套 termFit 的门控
    const proposed = {
      cols: Math.floor(box.width / cellWidth),
      rows: Math.floor(box.height / cellHeight),
    };
    if (!isUsableTermSize(proposed, box)) return false;
    const beforeCols = handle.terminal.options.cols;
    const beforeRows = handle.terminal.options.rows;
    handle.renderer.fit(box.width, box.height);
    const { cols, rows } = handle.terminal.options;
    if (cols !== beforeCols || rows !== beforeRows) this.hooks.onResize();
    return true;
  }

  refreshMetrics(): void {
    // 字体真正可用后无条件重建渲染器：字形 atlas 可能已经用 fallback 字体栅格化过，
    // 度量没变也得清。初次挂载已等过 fonts.ready，这里主要兜 loadingdone（换自定义字体）。
    this.handle?.setAppearance(this.currentAppearance());
  }

  applyAppearance(pref: TermPref, theme: ITheme): void {
    // theme 来自 resolveTermTheme 的常量表，引用比较即可
    const before = this.currentAppearance();
    this.pref = pref;
    this.theme = theme;
    const after = this.currentAppearance();
    if (sameAppearance(before, after)) return;
    // 未就绪时只记字段，adopt 会与 openedWith 比对后补上
    this.handle?.setAppearance(after);
  }

  appCursorKeys(): boolean {
    // 未就绪按普通模式给：CSI 形态的兼容面更广
    return this.handle?.terminal.modes().applicationCursorKeys ?? false;
  }

  focus(): void {
    if (this.handle) this.handle.focus();
    else this.wantFocus = true;
  }

  paste(text: string): void {
    this.handle?.terminal.paste(text);
  }

  getSelection(): string {
    return this.handle?.terminal.getSelection() ?? "";
  }

  clear(): void {
    this.push("\x1b[H\x1b[2J\x1b[3J");
  }

  dispose(): void {
    this.disposed = true;
    this.handle?.dispose();
    this.handle = null;
    this.queue = [];
  }

  private push(data: string | Uint8Array): void {
    if (this.handle) this.handle.terminal.write(data);
    else this.queue.push(data);
  }

  private currentAppearance(): RioAppearance {
    return {
      fontFamily: termFontStack(this.pref),
      fontSize: this.pref.fontSize,
      lineHeight: this.pref.lineHeight,
      theme: this.theme,
      cursorStyle: this.pref.cursorStyle,
      cursorBlink: this.pref.cursorBlink,
    };
  }

  private async boot(): Promise<void> {
    try {
      // 渲染器构造时量 cell 尺寸，必须等字体就绪，否则按 fallback 字体量出错的格子
      const [{ openRio, readRendererOverride }] = await Promise.all([
        import("./rio/open.js"),
        document.fonts.ready,
      ]);
      if (this.disposed || !this.host) return;
      const requested: RioRendererKind = readRendererOverride(safeLocalStorage()) ?? "webgpu";
      const appearance = this.currentAppearance();
      this.openedWith = appearance;
      const handle = await openRio(this.host, {
        renderer: requested,
        scrollback: this.scrollback,
        appearance,
        isGlobalKey: this.hooks.isGlobalKey,
        // 对齐 xterm 的 WebLinksAddon：点链接直接开新页，不弹 confirm
        activateLink: (uri) => void window.open(uri, "_blank", "noopener,noreferrer"),
        writeClipboard: writeBrowserClipboard,
        readClipboard: () => navigator.clipboard.readText(),
        onRendererChanged: (active, cause) => {
          this.hooks.onRenderer?.({ requested, active, reason: this.handle?.fallbackReason, cause });
          // 渲染器换了 cell 可能变，补量一次尺寸
          this.hooks.onReady();
        },
      });
      if (this.disposed) {
        handle.dispose();
        return;
      }
      this.adopt(handle, requested);
    } catch (err) {
      if (!this.disposed) this.hooks.onEngineError?.((err as Error).message);
    }
  }

  private adopt(handle: RioHandle, requested: RioRendererKind): void {
    this.handle = handle;
    handle.terminal.onData((bytes) => {
      this.hooks.onData(this.decoder.decode(bytes, { stream: true }));
    });
    // 先把格子量到宿主尺寸再回放队列。队列里通常是整份 replay（重载时 WS 比 wasm +
    // GPU 设备先就绪）：写进默认的 80×24 再 resize，alt screen 里的内容会被截掉，
    // 而服务端按尺寸去重不会再让 zellij 重绘，屏幕就一直空着（2026-09-02 实测）。
    this.fit();
    for (const chunk of this.queue) handle.terminal.write(chunk);
    this.queue = [];
    const latest = this.currentAppearance();
    if (this.openedWith && !sameAppearance(this.openedWith, latest)) handle.setAppearance(latest);
    this.openedWith = null;
    if (this.wantFocus) {
      this.wantFocus = false;
      handle.focus();
    }
    this.hooks.onRenderer?.({
      requested,
      active: handle.rendererKind,
      reason: handle.fallbackReason,
      cause: "open",
    });
    this.hooks.onReady();
  }
}

function sameAppearance(a: RioAppearance, b: RioAppearance): boolean {
  return (
    a.fontFamily === b.fontFamily &&
    a.fontSize === b.fontSize &&
    a.lineHeight === b.lineHeight &&
    a.cursorStyle === b.cursorStyle &&
    a.cursorBlink === b.cursorBlink &&
    a.theme === b.theme
  );
}

/** 沙箱 iframe 里连 `localStorage` 这个属性读取都会抛 SecurityError */
function safeLocalStorage(): Storage | undefined {
  try {
    return globalThis.localStorage;
  } catch {
    return undefined;
  }
}
