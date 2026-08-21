/**
 * 终端引擎适配层：TerminalView 只面向 TermAdapter 接口，不关心底下是
 * xterm.js 还是 rioterm（实验性，Rio 的 Rust VT 核心编译成 WASM）。
 *
 * rioterm 连同 1.1MB 的 wasm 走动态 import，只在选中 rio 引擎时下载。
 * rio 引擎的已知短板（0.1.x）：不支持光标闪烁；鼠标点击不会上报给
 * TUI（滚轮会，由 Rust 侧编码）；改字体/主题要原地重建实例，应用态
 * VT 模式只能尽力恢复。
 */

import { Terminal, type ITheme } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { WebglAddon } from "@xterm/addon-webgl";
import type { OpenOptions as RioOpenOptions, RioTermHandle, Theme as RioTheme } from "rioterm";
import { termFontStack, type TermPref } from "./term.js";
import { isUsableTermSize } from "./termFit.js";
import { osc52ClipboardText, writeBrowserClipboard } from "./osc52.js";

export interface TermAdapterHooks {
  /** 用户输入（按键、鼠标上报、DA 应答），发往 PTY */
  onData(data: string): void;
  /** 格子数变了；接收方自行按 cols/rows 去重 */
  onResize(): void;
  /** 异步引擎首次就绪 / 重建完成；应补一次量尺寸 */
  onReady(): void;
  /** 命中全局快捷键的按键，终端不该吃 */
  isGlobalKey(e: KeyboardEvent): boolean;
  /** 引擎加载/重建失败（wasm 拉不下来等），提示用户切回 xterm */
  onEngineError?(message: string): void;
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
  /** 字体加载完成后的重排（xterm 重建字形 atlas；rio 重测 cell 后重建实例） */
  refreshMetrics(): void;
  /** 热改外观。rio 引擎内部会重建实例，调用方无感知 */
  applyAppearance(pref: TermPref, theme: ITheme): void;
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

  constructor(init: TermAdapterInit) {
    const { pref, theme, scrollback, hooks } = init;
    this.pref = pref;
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

type RioModule = typeof import("rioterm");

class RioAdapter implements TermAdapter {
  private hooks: TermAdapterHooks;
  private pref: TermPref;
  private theme: ITheme;
  private scrollback: number;

  private host: HTMLElement | null = null;
  private rio: RioModule | null = null;
  private handle: RioTermHandle | null = null;
  /** 挂载/重建期间到达的输出，按序回放。reset 以 RIS 字符串入队，顺序天然保持 */
  private queue: (string | Uint8Array)[] = [];
  private dims = { cols: 0, rows: 0 };
  private decoder = new TextDecoder();
  private disposed = false;
  private rebuilding = false;
  private wantRebuild = false;
  private wantFocus = false;
  private hostCleanup: (() => void) | null = null;

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
    // rio 把 keydown 挂在内部 textarea 上（冒泡阶段），这里在宿主的捕获阶段先拦：
    // 1) IME 组合中的按键不能给 rio——确认候选的 Enter 会被当成真回车发给 PTY；
    // 2) 全局快捷键（⌘*/Ctrl+Shift+*/Alt+*）不能让 rio 编码成字节发出去。截断后
    //    事件到不了 window 的全局监听，克隆一份直接派发过去，App 侧无感知。
    const gate = (e: KeyboardEvent) => {
      if (e.isComposing || e.keyCode === 229) {
        e.stopPropagation();
        return;
      }
      if (!this.hooks.isGlobalKey(e)) return;
      e.stopPropagation();
      if (e.type !== "keydown") return;
      e.preventDefault();
      window.dispatchEvent(
        new KeyboardEvent("keydown", {
          key: e.key,
          code: e.code,
          ctrlKey: e.ctrlKey,
          metaKey: e.metaKey,
          altKey: e.altKey,
          shiftKey: e.shiftKey,
        })
      );
    };
    host.addEventListener("keydown", gate, true);
    host.addEventListener("keyup", gate, true);
    this.hostCleanup = () => {
      host.removeEventListener("keydown", gate, true);
      host.removeEventListener("keyup", gate, true);
    };
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
    this.dims = { cols, rows };
    if (cols !== beforeCols || rows !== beforeRows) this.hooks.onResize();
    return true;
  }

  refreshMetrics(): void {
    // cell 尺寸在 renderer 构造时量死，字体真正可用后只能重建实例。
    // 初次挂载已等过 fonts.ready，这里主要兜 loadingdone（换自定义字体）。
    this.scheduleRebuild();
  }

  applyAppearance(pref: TermPref, theme: ITheme): void {
    // 重建很重（serialize + 重开 wasm 实例），逐字段比对，没变就不动。
    // theme 来自 resolveTermTheme 的常量表，引用比较即可。
    // cursorBlink 不参与：rio 不支持闪烁，重建也没意义。
    const same =
      termFontStack(pref) === termFontStack(this.pref) &&
      pref.fontSize === this.pref.fontSize &&
      pref.lineHeight === this.pref.lineHeight &&
      pref.cursorStyle === this.pref.cursorStyle &&
      theme === this.theme;
    this.pref = pref;
    this.theme = theme;
    if (!same) this.scheduleRebuild();
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
    this.hostCleanup?.();
    this.hostCleanup = null;
    this.handle?.dispose();
    this.handle = null;
    this.queue = [];
  }

  private push(data: string | Uint8Array): void {
    if (this.handle) this.handle.terminal.write(data);
    else this.queue.push(data);
  }

  private async boot(): Promise<void> {
    try {
      // renderer 构造时量 cell 尺寸，必须等字体就绪，否则按 fallback 量出错的格子
      const [rio] = await Promise.all([import("rioterm"), document.fonts.ready]);
      if (this.disposed || !this.host) return;
      this.rio = rio;
      const handle = await rio.open(this.host, this.openOptions());
      if (this.disposed) {
        handle.dispose();
        return;
      }
      this.adopt(handle);
    } catch (err) {
      if (!this.disposed) this.hooks.onEngineError?.((err as Error).message);
    }
  }

  private scheduleRebuild(): void {
    if (this.disposed) return;
    if (this.rebuilding) {
      this.wantRebuild = true;
      return;
    }
    // 首次挂载还在路上：openOptions 读的就是最新 pref，不需要额外重建
    if (!this.handle) return;
    void this.rebuild();
  }

  private async rebuild(): Promise<void> {
    const rio = this.rio;
    const host = this.host;
    const old = this.handle;
    if (!rio || !host || !old) return;
    this.rebuilding = true;
    this.handle = null; // 重建期间的输出进队列，新实例好了按序回放
    try {
      const vt = old.terminal.serialize();
      const modes = old.terminal.modes();
      old.dispose();
      const handle = await rio.open(host, this.openOptions());
      if (this.disposed) {
        handle.dispose();
        return;
      }
      // serialize 只还原内容/样式/链接，应用态 VT 模式尽力手工补。
      // 鼠标上报变体（1000/1002/1003）拿不到，按 zellij/tmux 常用组合猜。
      handle.terminal.write(vt);
      if (modes.applicationCursorKeys) handle.terminal.write("\x1b[?1h");
      if (modes.bracketedPaste) handle.terminal.write("\x1b[?2004h");
      if (modes.mouseTracking) handle.terminal.write("\x1b[?1002h\x1b[?1006h");
      this.adopt(handle);
    } catch (err) {
      if (!this.disposed) this.hooks.onEngineError?.((err as Error).message);
    } finally {
      this.rebuilding = false;
      if (this.wantRebuild && !this.disposed) {
        this.wantRebuild = false;
        void this.rebuild();
      }
    }
  }

  private adopt(handle: RioTermHandle): void {
    this.handle = handle;
    this.dims = { cols: handle.terminal.options.cols, rows: handle.terminal.options.rows };
    handle.terminal.onData((bytes) => {
      this.hooks.onData(this.decoder.decode(bytes, { stream: true }));
    });
    this.wireIme(handle);
    for (const chunk of this.queue) handle.terminal.write(chunk);
    this.queue = [];
    if (this.wantFocus) {
      this.wantFocus = false;
      handle.focus();
    }
    this.hooks.onReady();
  }

  /**
   * rioterm 0.1.x 没接 composition 事件，中文输入法的提交文本到不了终端，
   * 这里在它自建的 textarea 上自己补。监听器随实例销毁跟着 DOM 一起失效，
   * 不需要显式清理；重建后对新 textarea 重新接线。
   */
  private wireIme(handle: RioTermHandle): void {
    const textarea = this.host?.querySelector("textarea");
    if (!textarea) return;
    const term = handle.terminal;
    textarea.addEventListener("compositionend", (e) => {
      if (e.data) term.input(e.data);
      textarea.value = "";
    });
    textarea.addEventListener("input", (e) => {
      const ie = e as InputEvent;
      if (ie.isComposing) return;
      // 死键、emoji 面板等不走 keydown 的直接插入；普通按键已被 rio
      // preventDefault，不会落进 textarea，也就不会走到这里
      if (ie.inputType === "insertText" && ie.data) term.input(ie.data);
      textarea.value = "";
    });
  }

  private openOptions(): RioOpenOptions {
    return {
      renderer: "canvas",
      // 尺寸自己驱动：格子没量准前不能把假尺寸发给 PTY（见 termFit.ts）
      fit: false,
      autoFocus: false,
      scrollback: this.scrollback,
      fontFamily: termFontStack(this.pref),
      fontSize: this.pref.fontSize,
      lineHeight: this.pref.lineHeight,
      cursorStyle: this.pref.cursorStyle,
      theme: toRioTheme(this.theme),
      // 对齐 xterm 的 WebLinksAddon：点链接直接开新页，不弹 confirm
      linkHandler: {
        activate: (uri) => void window.open(uri, "_blank", "noopener,noreferrer"),
      },
    };
  }
}

/** xterm.js 的默认 ANSI 16 色（Tango）。跟随主题只定义底/字时补齐 rio 要求的全量字段 */
const XTERM_DEFAULT_ANSI = {
  black: "#2e3436",
  red: "#cc0000",
  green: "#4e9a06",
  yellow: "#c4a000",
  blue: "#3465a4",
  magenta: "#75507b",
  cyan: "#06989a",
  white: "#d3d7cf",
  brightBlack: "#555753",
  brightRed: "#ef2929",
  brightGreen: "#8ae234",
  brightYellow: "#fce94f",
  brightBlue: "#729fcf",
  brightMagenta: "#ad7fa8",
  brightCyan: "#34e2e2",
  brightWhite: "#eeeeec",
} as const;

/** #rgb / #rrggbb / #rrggbbaa。非法输入返回 undefined，调用方原样透传。 */
function parseHex(hex: string): { r: number; g: number; b: number; a: number } | undefined {
  const m = /^#([\da-f]{3}|[\da-f]{6}|[\da-f]{8})$/i.exec(hex.trim());
  if (!m) return undefined;
  let h = m[1]!;
  if (h.length === 3) h = `${h[0]}${h[0]}${h[1]}${h[1]}${h[2]}${h[2]}`;
  const n = (i: number) => parseInt(h.slice(i, i + 2), 16);
  return { r: n(0), g: n(2), b: n(4), a: h.length === 8 ? n(6) / 255 : 1 };
}

/**
 * 带 alpha 的颜色与底色预混合成不透明 #rrggbb。
 *
 * rio 的 canvas 渲染器每帧不清屏、选区背景直接 fillRect：半透明色会跨帧
 * 累积叠加——拖选时选区一帧比一帧白，最后把文字整个洗掉。混成不透明色
 * 从根上避开，观感与 xterm 把同一半透明色叠在纯色底上一致。
 */
function opaqueOver(color: string, base: string): string {
  const c = parseHex(color);
  if (!c || c.a >= 1) return color;
  const b = parseHex(base) ?? { r: 0, g: 0, b: 0 };
  const mix = (x: number, y: number) =>
    Math.round(x * c.a + y * (1 - c.a))
      .toString(16)
      .padStart(2, "0");
  return `#${mix(c.r, b.r)}${mix(c.g, b.g)}${mix(c.b, b.b)}`;
}

function toRioTheme(t: ITheme): RioTheme {
  const foreground = t.foreground ?? "#ffffff";
  const background = t.background ?? "#000000";
  return {
    foreground,
    background,
    cursor: t.cursor ?? foreground,
    // rio 把选中文字统一染成 selectionForeground；xterm 是半透明覆盖不改字色，
    // 没配置时取 foreground 最接近原观感
    selectionForeground: t.selectionForeground ?? foreground,
    selectionBackground: opaqueOver(t.selectionBackground ?? "#3465a4", background),
    black: t.black ?? XTERM_DEFAULT_ANSI.black,
    red: t.red ?? XTERM_DEFAULT_ANSI.red,
    green: t.green ?? XTERM_DEFAULT_ANSI.green,
    yellow: t.yellow ?? XTERM_DEFAULT_ANSI.yellow,
    blue: t.blue ?? XTERM_DEFAULT_ANSI.blue,
    magenta: t.magenta ?? XTERM_DEFAULT_ANSI.magenta,
    cyan: t.cyan ?? XTERM_DEFAULT_ANSI.cyan,
    white: t.white ?? XTERM_DEFAULT_ANSI.white,
    brightBlack: t.brightBlack ?? XTERM_DEFAULT_ANSI.brightBlack,
    brightRed: t.brightRed ?? XTERM_DEFAULT_ANSI.brightRed,
    brightGreen: t.brightGreen ?? XTERM_DEFAULT_ANSI.brightGreen,
    brightYellow: t.brightYellow ?? XTERM_DEFAULT_ANSI.brightYellow,
    brightBlue: t.brightBlue ?? XTERM_DEFAULT_ANSI.brightBlue,
    brightMagenta: t.brightMagenta ?? XTERM_DEFAULT_ANSI.brightMagenta,
    brightCyan: t.brightCyan ?? XTERM_DEFAULT_ANSI.brightCyan,
    brightWhite: t.brightWhite ?? XTERM_DEFAULT_ANSI.brightWhite,
  };
}
