/**
 * rio 渲染器契约。rioterm 自带的 CanvasRenderer 结构上已经满足；自研 WebGPU
 * 渲染器照着实现。open.ts 的事件接线只认这份接口，换渲染器对它透明。
 *
 * 硬性要求：
 * 1. 构造同步，构造里自行 `terminal.setCellSize()` 并订阅 `terminal.onUpdate()`
 *    （与 CanvasRenderer 一致），失败同步 throw RendererInitError；
 * 2. `fit()` 只在 floor(px/cell) 算出的格数变化时才 `terminal.resize()`——resize 会
 *    同步 emit onUpdate 触发自己的 schedule()，所以 render() 里绝不能调 fit()；
 * 3. cellWidth/cellHeight 是 CSS 像素。WebGPU 渲染器以物理像素整数为准，CSS 值是
 *    物理值 / dpr，可以是分数；cellAt() 与 fit() 全按 CSS 分数值算。
 *
 * 两种渲染器的 cell 量法不要求一致：换渲染器后宿主总会再 fit 一次，格数变了就正常 resize。
 */

import type { ITheme } from "@xterm/xterm";
import { CanvasRenderer, type Terminal } from "rioterm";
import type { TermCursorStyle } from "../term.js";
import type { GpuHandle, RioRendererKind, WebGpuFailReason } from "./gpu.js";
import { toRioTheme } from "./theme.js";

export interface HoverLink {
  line: number;
  startCol: number;
  endCol: number;
}

export interface CellHit {
  col: number;
  row: number;
  sideRight: boolean;
}

export interface RendererStats {
  /** 实际画过的帧（noop 帧不算） */
  frames: number;
  rowsUploaded: number;
  /** 因 noop 跳过的 render() 调用 */
  drawsSkipped: number;
}

export interface RioRenderer {
  readonly element: HTMLElement;
  readonly cellWidth: number;
  readonly cellHeight: number;
  fit(width: number, height: number): void;
  schedule(): void;
  render(): void;
  setHoverLink(link: HoverLink | null): void;
  cellAt(clientX: number, clientY: number): CellHit;
  /** 光标闪烁 / 失焦空心；canvas 渲染器没有 */
  setFocused?(focused: boolean): void;
  readonly stats?: RendererStats;
  dispose(): void;
}

export interface RioAppearance {
  fontFamily: string;
  fontSize: number;
  lineHeight: number;
  theme: ITheme;
  cursorStyle: TermCursorStyle;
  /** canvas 渲染器忽略 */
  cursorBlink: boolean;
}

export class RendererInitError extends Error {
  constructor(
    readonly reason: WebGpuFailReason,
    message?: string
  ) {
    super(message ?? `rio renderer init failed: ${reason}`);
    this.name = "RendererInitError";
  }
}

/** WebGPU 渲染器的构造函数由 webgpu/ 子目录在模块加载时注册，避免本文件反向依赖它 */
export type WebgpuRendererFactory = (
  terminal: Terminal,
  gpu: GpuHandle,
  appearance: RioAppearance
) => RioRenderer;

let webgpuFactory: WebgpuRendererFactory | null = null;

export function registerWebgpuRenderer(factory: WebgpuRendererFactory): void {
  webgpuFactory = factory;
}

export function createRenderer(
  kind: RioRendererKind,
  terminal: Terminal,
  appearance: RioAppearance,
  gpu: GpuHandle | null
): RioRenderer {
  if (kind === "webgpu") {
    if (!gpu) throw new RendererInitError("unsupported", "no GPU device");
    if (!webgpuFactory) throw new RendererInitError("unsupported", "webgpu renderer not loaded");
    return webgpuFactory(terminal, gpu, appearance);
  }
  return new CanvasRenderer(terminal, {
    fontFamily: appearance.fontFamily,
    fontSize: appearance.fontSize,
    lineHeight: appearance.lineHeight,
    cursorStyle: appearance.cursorStyle,
    theme: toRioTheme(appearance.theme),
  });
}
