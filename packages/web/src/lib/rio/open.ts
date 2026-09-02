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
 * - IME 的 compositionend / input(insertText) 内置（rioterm 0.1.x 没接，中文提交不了）；
 * - 滚轮攒余量（触控板慢滚在 scrollback 里能动）；
 * - mouseup 不写剪贴板：TerminalView 已在宿主 mouseup 上按 getSelection() 写，避免双写；
 * - 去掉 predictiveEcho、内置 ResizeObserver（宿主驱动 fit）、autoFocus、confirm() 链接提示；
 * - window 级 mousemove/mouseup 只在拖选期间挂着；
 * - DPR 变化与 device lost 自动换渲染器。
 */

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
import { routeKey } from "./keyRoute.js";
import { createRenderer, RendererInitError, type RioAppearance, type RioRenderer } from "./renderer.js";
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
  } satisfies Partial<CSSStyleDeclaration>);
  textarea.setAttribute("autocorrect", "off");
  textarea.setAttribute("autocapitalize", "off");
  textarea.setAttribute("spellcheck", "false");
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

  // ---- 键盘 ----
  on(textarea, "keydown", (e) => {
    // 只有可能是复制快捷键时才去问选区（FFI 调用），普通按键不付这个成本
    const maybeCopy = (e.metaKey && e.key === "c") || (e.ctrlKey && e.shiftKey);
    const hasSelection = maybeCopy && !!terminal.getSelection();
    switch (routeKey(e, opts.isGlobalKey, hasSelection)) {
      case "ime":
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
  on(textarea, "compositionend", (e) => {
    if (e.data) terminal.input(e.data);
    textarea.value = "";
  });
  on(textarea, "input", (e) => {
    const ie = e as InputEvent;
    if (ie.isComposing) return;
    if (ie.inputType === "insertText" && ie.data) terminal.input(ie.data);
    textarea.value = "";
  });
  on(textarea, "focus", () => renderer.setFocused?.(true));
  on(textarea, "blur", () => renderer.setFocused?.(false));

  // OSC 52：程序写系统剪贴板
  const clipboardSub = terminal.onClipboardWrite((text) => {
    void opts.writeClipboard(text).catch(() => undefined);
  });
  disposers.push(() => clipboardSub.dispose());

  // ---- 鼠标选区 / 链接 ----
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
      const lines = wheel.push(e.deltaMode, e.deltaY, renderer.cellHeight, terminal.options.rows);
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
    container.insertBefore(renderer.element, textarea);
    hoverCell = null;
    container.style.cursor = "";
    renderer.setFocused?.(document.activeElement === textarea);
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
