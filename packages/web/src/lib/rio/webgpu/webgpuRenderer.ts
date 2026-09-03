/**
 * rio 引擎的 WebGPU 渲染器。设计蓝本是 sugarloaf grid/（Ghostty 渲染器移植）：
 *
 * - bg pass：cols×rows 的 rgba8unorm 纹理 + 一个全屏三角形，片元按像素查格；
 * - text pass：每字形 / 装饰一个 24 字节实例，每行固定容量槽位（cols × 4），只有
 *   脏行 writeBuffer 自己的槽，逐行 draw；50 个 draw 在一个 pass 里可忽略，省掉了
 *   sugarloaf 那种每帧 concat；
 * - block 光标与失焦空心框走 uniform 在 shader 里画，bar / underline 光标是独立的
 *   1 实例缓冲第三次 draw：光标移动 / 闪烁不脏任何行；
 * - 颜色全在 CPU 折算成 premultiplied RGBA8，shader 无状态位。
 *
 * 脏行：snapshot() 会重置 WASM 侧的 dirtyRows，所以先并进自己的 pending，上传成功
 * 后才清；任何失败路径置 forceFull。noop 帧不取 getCurrentTexture，画布保留上一帧。
 *
 * 契约见 ../renderer.ts：构造同步、自行 setCellSize + 订阅 onUpdate、不在 render()
 * 里 fit()。GPU 设备由 ../gpu.ts 持有，pipeline 按 canvas format 挂在设备上共享。
 */

import type { Terminal } from "rioterm";
import { getShared, type GpuHandle } from "../gpu.js";
import {
  RendererInitError,
  registerWebgpuRenderer,
  type CellHit,
  type HoverLink,
  type RendererStats,
  type RioAppearance,
  type RioRenderer,
} from "../renderer.js";
import { toGpuTheme } from "../theme.js";
import { colorBucket, createCanvasRasterizer, createGpuAtlasTexture, GLYPH_EMPTY, GlyphAtlas } from "./atlas.js";
import { buildPalette, channels, type Palette } from "./colors.js";
import { decorationSprite, solidRects } from "./decorations.js";
import {
  buildRow,
  computeDamage,
  FLAG_SOLID,
  INSTANCE_BYTES,
  INSTANCE_WORDS,
  MAX_INSTANCES_PER_CELL,
  packInstance,
  type Damage,
  type FrameKey,
  type RowCtx,
} from "./frame.js";
import { computeMetrics, type CellMetrics, type MeasureFn } from "./metrics.js";
import { BG_SHADER, INSTANCE_VERTEX_LAYOUT, TEXT_SHADER, UNIFORM_BYTES } from "./shaders.js";
import { createSpriteRasterizer, type SpriteRasterizer } from "./spriteRaster.js";
import { isSprite, spriteMetrics, spriteOps, type SpriteMetrics } from "./sprites.js";

const BLINK_MS = 530;
const GRAY_ATLAS_SIZE = 2048;
const COLOR_ATLAS_SIZE = 1024;

const CURSOR_NONE = 0;
const CURSOR_BLOCK = 1;
const CURSOR_BAR = 2;
const CURSOR_UNDERLINE = 3;
const CURSOR_HOLLOW = 4;

interface Pipelines {
  layout0: GPUBindGroupLayout;
  layout1: GPUBindGroupLayout;
  bg: GPURenderPipeline;
  text: GPURenderPipeline;
}

function buildPipelines(device: GPUDevice, format: GPUTextureFormat): Pipelines {
  const layout0 = device.createBindGroupLayout({
    label: "rio.layout0",
    entries: [
      { binding: 0, visibility: GPUShaderStage.VERTEX | GPUShaderStage.FRAGMENT, buffer: { type: "uniform" } },
      { binding: 1, visibility: GPUShaderStage.FRAGMENT, texture: { sampleType: "float" } },
    ],
  });
  const layout1 = device.createBindGroupLayout({
    label: "rio.layout1",
    entries: [
      { binding: 0, visibility: GPUShaderStage.FRAGMENT, texture: { sampleType: "float" } },
      { binding: 1, visibility: GPUShaderStage.FRAGMENT, texture: { sampleType: "float" } },
    ],
  });
  const bgModule = device.createShaderModule({ label: "rio.bg", code: BG_SHADER });
  const textModule = device.createShaderModule({ label: "rio.text", code: TEXT_SHADER });
  const bg = device.createRenderPipeline({
    label: "rio.bg",
    layout: device.createPipelineLayout({ bindGroupLayouts: [layout0] }),
    vertex: { module: bgModule, entryPoint: "vs" },
    fragment: { module: bgModule, entryPoint: "fs", targets: [{ format }] },
    primitive: { topology: "triangle-list" },
  });
  const premultiplied: GPUBlendComponent = { srcFactor: "one", dstFactor: "one-minus-src-alpha", operation: "add" };
  const text = device.createRenderPipeline({
    label: "rio.text",
    layout: device.createPipelineLayout({ bindGroupLayouts: [layout0, layout1] }),
    vertex: { module: textModule, entryPoint: "vs", buffers: [INSTANCE_VERTEX_LAYOUT] },
    fragment: {
      module: textModule,
      entryPoint: "fs",
      targets: [{ format, blend: { color: premultiplied, alpha: premultiplied } }],
    },
    primitive: { topology: "triangle-strip" },
  });
  return { layout0, layout1, bg, text };
}

function browserMeasure(): MeasureFn {
  const canvas =
    typeof OffscreenCanvas !== "undefined" ? new OffscreenCanvas(1, 1) : document.createElement("canvas");
  const ctx = canvas.getContext("2d") as OffscreenCanvasRenderingContext2D | CanvasRenderingContext2D | null;
  if (!ctx) throw new RendererInitError("no-context", "2d context unavailable");
  return (font, text) => {
    ctx.font = font;
    const tm = ctx.measureText(text);
    return { width: tm.width, ascent: tm.fontBoundingBoxAscent ?? 0, descent: tm.fontBoundingBoxDescent ?? 0 };
  };
}

function nextPow2(n: number): number {
  let p = 1;
  while (p < n) p *= 2;
  return p;
}

export class WebgpuRenderer implements RioRenderer {
  readonly canvas: HTMLCanvasElement;
  readonly cellWidth: number;
  readonly cellHeight: number;
  readonly stats: RendererStats = { frames: 0, rowsUploaded: 0, drawsSkipped: 0 };

  get element(): HTMLElement {
    return this.canvas;
  }

  private readonly term: Terminal;
  private readonly device: GPUDevice;
  private readonly queue: GPUQueue;
  private readonly format: GPUTextureFormat;
  private readonly context: GPUCanvasContext;
  private readonly pipelines: Pipelines;
  private readonly m: CellMetrics;
  private readonly palette: Palette;
  private readonly atlas: GlyphAtlas;
  private readonly sprites: SpriteRasterizer;
  private readonly spriteM: SpriteMetrics;
  private readonly grayTexture: GPUTexture;
  private readonly colorTexture: GPUTexture;
  private readonly uniformBuf: GPUBuffer;
  private readonly uniformData = new ArrayBuffer(UNIFORM_BYTES);
  private readonly uniformU32 = new Uint32Array(this.uniformData);
  private readonly uniformF32 = new Float32Array(this.uniformData);
  private readonly cursorBuf: GPUBuffer;
  private readonly cursorWords = new Uint32Array(INSTANCE_WORDS);
  private readonly bindGroup1: GPUBindGroup;
  private readonly maxDim: number;
  private readonly cursorStyle: RioAppearance["cursorStyle"];
  private readonly cursorBlink: boolean;
  private readonly rowCtx: RowCtx;

  // 网格相关，allocGrid 重建
  private cols = 0;
  private rows = 0;
  private rowCap = 0;
  private bgTex: GPUTexture | null = null;
  private bindGroup0: GPUBindGroup | null = null;
  private fgBuf: GPUBuffer | null = null;
  private fgCapacity = 0;
  private fgRow = new Uint32Array(0);
  private bgRow = new Uint32Array(0);
  private rowCounts = new Uint32Array(0);
  private pending = new Uint8Array(0);
  private forceFull = true;
  private prevKey: FrameKey | null = null;

  private hover: HoverLink | null = null;
  private focused = false;
  private blinkOn = true;
  private blinkTimer: number | null = null;
  private lastActivity = 0;
  private cursorDirty = true;
  private cursorKind = CURSOR_NONE;
  private raf = 0;
  private disposed = false;
  private readonly sub: { dispose(): void };
  private readonly onVisibility = () => this.startBlink();

  constructor(term: Terminal, gpu: GpuHandle, appearance: RioAppearance) {
    this.term = term;
    this.device = gpu.device;
    this.queue = gpu.device.queue;
    this.format = gpu.format;
    this.cursorStyle = appearance.cursorStyle;
    this.cursorBlink = appearance.cursorBlink;
    this.maxDim = gpu.device.limits.maxTextureDimension2D;

    const dpr = window.devicePixelRatio || 1;
    this.m = computeMetrics(
      { fontFamily: appearance.fontFamily, fontSize: appearance.fontSize, lineHeight: appearance.lineHeight, dpr },
      browserMeasure()
    );
    this.cellWidth = this.m.cssCellW;
    this.cellHeight = this.m.cssCellH;
    this.palette = buildPalette(toGpuTheme(appearance.theme));

    this.canvas = document.createElement("canvas");
    this.canvas.style.display = "block";
    const context = this.canvas.getContext("webgpu");
    if (!context) throw new RendererInitError("no-context");
    this.context = context;

    this.pipelines = getShared(gpu, `rio.pipelines.${this.format}`, () => buildPipelines(this.device, this.format));

    const graySize = Math.min(GRAY_ATLAS_SIZE, this.maxDim);
    const colorSize = Math.min(COLOR_ATLAS_SIZE, this.maxDim);
    const gray = createGpuAtlasTexture(this.device, graySize, false);
    const color = createGpuAtlasTexture(this.device, colorSize, true);
    this.grayTexture = gray.texture;
    this.colorTexture = color.texture;
    this.atlas = new GlyphAtlas(gray, color, createCanvasRasterizer(this.m, appearance.fontFamily));
    this.spriteM = spriteMetrics(this.m);
    this.sprites = createSpriteRasterizer(this.spriteM);
    this.bindGroup1 = this.device.createBindGroup({
      label: "rio.atlases",
      layout: this.pipelines.layout1,
      entries: [
        { binding: 0, resource: gray.texture.createView() },
        { binding: 1, resource: color.texture.createView() },
      ],
    });
    this.uniformBuf = this.device.createBuffer({
      label: "rio.uniform",
      size: UNIFORM_BYTES,
      usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST,
    });
    this.cursorBuf = this.device.createBuffer({
      label: "rio.cursor",
      size: INSTANCE_BYTES,
      usage: GPUBufferUsage.VERTEX | GPUBufferUsage.COPY_DST,
    });

    this.rowCtx = {
      cols: 0,
      palette: this.palette,
      glyphs: this.atlas.table,
      lookup: (cp, text, bold, italic, wide, fg) => {
        // 盒线 / 块元素 / braille / powerline 自己画，与字体无关；cluster（带附标）不走这里
        if (text === null && isSprite(cp)) {
          const id = this.atlas.getSprite(`sprite:${cp}:${this.m.cellW}x${this.m.cellH}`, () => {
            const ops = spriteOps(cp, this.spriteM);
            return ops ? this.sprites.raster(ops) : null;
          });
          if (id !== GLYPH_EMPTY) return id;
          // 没有对应的 sprite（表里没覆盖的码位）退回字体字形
        }
        return this.atlas.get(cp, text, bold, italic, wide, colorBucket(fg));
      },
      decor: { undercurl: GLYPH_EMPTY, dotted: GLYPH_EMPTY, dashed: GLYPH_EMPTY },
      solid: {
        underline: solidRects("underline", this.m),
        double: solidRects("double", this.m),
        strikeout: solidRects("strikeout", this.m),
      },
      clusterText: (row, col) => this.term.clusterText(row, col),
      selection: null,
      hover: null,
    };
    this.setupDecorations();

    this.allocGrid(term.options.cols, term.options.rows);
    this.configure();
    // rioterm 只拿它做 CSI 14 t 一类的像素尺寸上报，给 CSS 值即可
    term.setCellSize(this.m.cssCellW, this.m.cssCellH);
    this.sub = term.onUpdate(() => this.onTerminalUpdate());
    document.addEventListener("visibilitychange", this.onVisibility);
    this.focused = document.activeElement !== null && document.activeElement !== document.body;
    this.startBlink();
    this.schedule();
  }

  // ---- 契约 ----

  fit(width: number, height: number): void {
    // 隐藏 / 折叠的容器（display: none、零尺寸 tab）不能把网格折叠掉
    if (width < this.cellWidth || height < this.cellHeight) return;
    const cols = Math.max(2, Math.floor(width / this.cellWidth));
    const rows = Math.max(2, Math.floor(height / this.cellHeight));
    if (cols !== this.term.options.cols || rows !== this.term.options.rows) {
      // resize 同步 emit onUpdate → schedule，这里不用再调
      this.term.resize(cols, rows);
    }
    if (cols !== this.cols || rows !== this.rows) {
      this.allocGrid(cols, rows);
      this.configure();
      this.schedule();
    }
  }

  schedule(): void {
    if (this.raf || this.disposed) return;
    this.raf = requestAnimationFrame(() => {
      this.raf = 0;
      this.render();
    });
  }

  setHoverLink(link: HoverLink | null): void {
    const same =
      link === this.hover ||
      (link !== null &&
        this.hover !== null &&
        link.line === this.hover.line &&
        link.startCol === this.hover.startCol &&
        link.endCol === this.hover.endCol);
    if (same) return;
    this.hover = link;
    this.schedule();
  }

  cellAt(clientX: number, clientY: number): CellHit {
    const rect = this.canvas.getBoundingClientRect();
    const x = clientX - rect.left;
    const y = clientY - rect.top;
    const col = Math.min(Math.max(0, Math.floor(x / this.cellWidth)), this.term.options.cols - 1);
    const row = Math.min(Math.max(0, Math.floor(y / this.cellHeight)), this.term.options.rows - 1);
    const sideRight = x - col * this.cellWidth > this.cellWidth / 2;
    return { col, row, sideRight };
  }

  setFocused(focused: boolean): void {
    if (this.focused === focused) return;
    this.focused = focused;
    this.cursorDirty = true;
    this.startBlink();
    this.schedule();
  }

  render(): void {
    if (this.disposed) return;
    const snap = this.term.snapshot();
    if (snap.cols !== this.cols || snap.rows !== this.rows) {
      this.allocGrid(snap.cols, snap.rows);
      this.configure();
    }
    const key: FrameKey = {
      cols: snap.cols,
      rows: snap.rows,
      displayOffset: snap.displayOffset,
      altScreen: snap.altScreen,
      selection: snap.selection,
      hover: this.hover,
      cursorLine: snap.cursorLine,
      cursorCol: snap.cursorCol,
      cursorVisible: snap.cursorVisible,
    };
    // WASM 的脏标记只此一次机会，先并进自己的
    for (let i = 0; i < snap.rows; i++) if (snap.dirtyRows[i]) this.pending[i] = 1;
    const damage = computeDamage(this.prevKey, key, this.pending, this.forceFull);

    if (damage.kind === "full" || damage.kind === "partial") {
      this.rowCtx.cols = snap.cols;
      this.rowCtx.selection = snap.selection;
      this.rowCtx.hover = this.hover;
      if (!this.rebuildRows(snap.cells, damage)) {
        this.forceFull = true;
        return;
      }
    } else if (damage.kind === "noop" && !this.cursorDirty) {
      this.stats.drawsSkipped++;
      return;
    }

    this.writeUniforms(snap.cursorCol, snap.cursorLine, snap.cursorVisible);
    this.draw();
    this.prevKey = key;
    this.forceFull = false;
    this.cursorDirty = false;
    this.stats.frames++;
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.sub.dispose();
    if (this.raf) cancelAnimationFrame(this.raf);
    this.raf = 0;
    if (this.blinkTimer !== null) window.clearInterval(this.blinkTimer);
    this.blinkTimer = null;
    document.removeEventListener("visibilitychange", this.onVisibility);
    this.atlas.dispose();
    this.sprites.dispose();
    this.bgTex?.destroy();
    this.fgBuf?.destroy();
    this.uniformBuf.destroy();
    this.cursorBuf.destroy();
    this.grayTexture.destroy();
    this.colorTexture.destroy();
    this.context.unconfigure();
    this.canvas.remove();
  }

  // ---- 内部 ----

  private setupDecorations(): void {
    const key = (k: string) => `deco:${k}:${this.m.cellW}x${this.m.cellH}:${this.m.underlineThick}`;
    this.rowCtx.decor = {
      undercurl: this.atlas.getSprite(key("undercurl"), () => decorationSprite("undercurl", this.m)),
      dotted: this.atlas.getSprite(key("dotted"), () => decorationSprite("dotted", this.m)),
      dashed: this.atlas.getSprite(key("dashed"), () => decorationSprite("dashed", this.m)),
    };
  }

  private allocGrid(cols: number, rows: number): void {
    this.cols = cols;
    this.rows = rows;
    this.rowCap = cols * MAX_INSTANCES_PER_CELL;
    this.bgTex?.destroy();
    this.bgTex = this.device.createTexture({
      label: "rio.bg",
      size: { width: cols, height: rows },
      format: "rgba8unorm",
      usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.COPY_DST,
    });
    this.bindGroup0 = this.device.createBindGroup({
      label: "rio.grid",
      layout: this.pipelines.layout0,
      entries: [
        { binding: 0, resource: { buffer: this.uniformBuf } },
        { binding: 1, resource: this.bgTex.createView() },
      ],
    });
    const need = rows * this.rowCap * INSTANCE_BYTES;
    if (need > this.fgCapacity) {
      this.fgBuf?.destroy();
      this.fgCapacity = nextPow2(need);
      this.fgBuf = this.device.createBuffer({
        label: "rio.fg",
        size: this.fgCapacity,
        usage: GPUBufferUsage.VERTEX | GPUBufferUsage.COPY_DST,
      });
    }
    this.fgRow = new Uint32Array(this.rowCap * INSTANCE_WORDS);
    this.bgRow = new Uint32Array(cols);
    this.rowCounts = new Uint32Array(rows);
    this.pending = new Uint8Array(rows);
    this.forceFull = true;
    this.prevKey = null;
  }

  /** canvas 物理尺寸与 CSS 尺寸；configure 幂等且便宜，尺寸变了顺手调一次 */
  private configure(): void {
    const w = Math.min(this.maxDim, this.cols * this.m.cellW);
    const h = Math.min(this.maxDim, this.rows * this.m.cellH);
    if (this.canvas.width !== w || this.canvas.height !== h) {
      this.canvas.width = w;
      this.canvas.height = h;
    }
    this.canvas.style.width = `${this.cols * this.cellWidth}px`;
    this.canvas.style.height = `${this.rows * this.cellHeight}px`;
    this.context.configure({ device: this.device, format: this.format, alphaMode: "opaque" });
  }

  /** 返回 false 表示这一帧没能完成（图集清空后仍放不下），调用方保留 forceFull */
  private rebuildRows(cells: Uint32Array, damage: Damage): boolean {
    const all = damage.kind === "full";
    const rowsMask = damage.kind === "partial" ? damage.rows : null;
    for (let row = 0; row < this.rows; row++) {
      if (!all && !rowsMask![row]) continue;
      if (this.uploadRow(cells, row)) continue;
      // 图集满：清空重来，旧 id 全部失效，所有行都要重发
      this.atlas.reset();
      this.setupDecorations();
      for (let r = 0; r < this.rows; r++) {
        if (!this.uploadRow(cells, r)) {
          console.error("rio: glyph atlas overflow even after reset; frame dropped");
          return false;
        }
      }
      return true;
    }
    return true;
  }

  private uploadRow(cells: Uint32Array, row: number): boolean {
    const n = buildRow(cells, row, this.rowCtx, this.bgRow, this.fgRow);
    if (n < 0) return false;
    this.queue.writeTexture(
      { texture: this.bgTex!, origin: { x: 0, y: row } },
      this.bgRow,
      { bytesPerRow: this.cols * 4 },
      { width: this.cols, height: 1 }
    );
    if (n > 0) {
      this.queue.writeBuffer(this.fgBuf!, row * this.rowCap * INSTANCE_BYTES, this.fgRow, 0, n * INSTANCE_WORDS);
    }
    this.rowCounts[row] = n;
    this.pending[row] = 0;
    this.stats.rowsUploaded++;
    return true;
  }

  private writeUniforms(cursorCol: number, cursorLine: number, cursorVisible: boolean): void {
    const u = this.uniformU32;
    const f = this.uniformF32;
    u[0] = this.m.cellW;
    u[1] = this.m.cellH;
    u[2] = this.cols;
    u[3] = this.rows;
    u[4] = cursorCol;
    u[5] = cursorLine;
    const shown = cursorVisible && (!this.cursorBlink || !this.focused || this.blinkOn);
    let kind = CURSOR_NONE;
    if (shown) {
      if (!this.focused) kind = CURSOR_HOLLOW;
      else if (this.cursorStyle === "bar") kind = CURSOR_BAR;
      else if (this.cursorStyle === "underline") kind = CURSOR_UNDERLINE;
      else kind = CURSOR_BLOCK;
    }
    u[6] = kind;
    u[7] = 0;
    const [cr, cg, cb] = channels(this.palette.cursor);
    f[8] = cr / 255;
    f[9] = cg / 255;
    f[10] = cb / 255;
    f[11] = 1;
    const [fr, fgc, fb] = channels(this.palette.cursorFg);
    f[12] = fr / 255;
    f[13] = fgc / 255;
    f[14] = fb / 255;
    f[15] = 1;
    f[16] = this.canvas.width;
    f[17] = this.canvas.height;
    f[18] = 0;
    f[19] = 0;
    this.queue.writeBuffer(this.uniformBuf, 0, this.uniformData);

    if (kind === CURSOR_BAR || kind === CURSOR_UNDERLINE) {
      const r = solidRects(kind === CURSOR_BAR ? "cursorBar" : "cursorUnderline", this.m)[0]!;
      packInstance(this.cursorWords, 0, 0, 0, r.w, r.h, r.x, r.y, cursorCol, cursorLine, this.palette.cursor, FLAG_SOLID);
      this.queue.writeBuffer(this.cursorBuf, 0, this.cursorWords);
    }
    this.cursorKind = kind;
  }

  private draw(): void {
    const encoder = this.device.createCommandEncoder();
    const [br, bgc, bb] = channels(this.palette.bg);
    const pass = encoder.beginRenderPass({
      colorAttachments: [
        {
          // 绝不跨帧持有 currentTexture
          view: this.context.getCurrentTexture().createView(),
          loadOp: "clear",
          clearValue: { r: br / 255, g: bgc / 255, b: bb / 255, a: 1 },
          storeOp: "store",
        },
      ],
    });
    pass.setPipeline(this.pipelines.bg);
    pass.setBindGroup(0, this.bindGroup0!);
    pass.draw(3);

    pass.setPipeline(this.pipelines.text);
    pass.setBindGroup(0, this.bindGroup0!);
    pass.setBindGroup(1, this.bindGroup1);
    pass.setVertexBuffer(0, this.fgBuf!);
    for (let row = 0; row < this.rows; row++) {
      const n = this.rowCounts[row]!;
      if (n > 0) pass.draw(4, n, 0, row * this.rowCap);
    }
    if (this.cursorKind === CURSOR_BAR || this.cursorKind === CURSOR_UNDERLINE) {
      pass.setVertexBuffer(0, this.cursorBuf);
      pass.draw(4, 1);
    }
    pass.end();
    this.queue.submit([encoder.finish()]);
  }

  private onTerminalUpdate(): void {
    this.lastActivity = performance.now();
    if (this.cursorBlink && !this.blinkOn) {
      this.blinkOn = true;
      this.cursorDirty = true;
    }
    this.schedule();
  }

  /** 闪烁只在聚焦且页面可见时跑；有输出时保持可见（tick 里按 lastActivity 判） */
  private startBlink(): void {
    if (this.blinkTimer !== null) {
      window.clearInterval(this.blinkTimer);
      this.blinkTimer = null;
    }
    if (!this.blinkOn) {
      this.blinkOn = true;
      this.cursorDirty = true;
    }
    if (!this.cursorBlink || !this.focused || document.hidden || this.disposed) return;
    this.blinkTimer = window.setInterval(() => {
      if (performance.now() - this.lastActivity < BLINK_MS) {
        if (!this.blinkOn) {
          this.blinkOn = true;
          this.cursorDirty = true;
          this.schedule();
        }
        return;
      }
      this.blinkOn = !this.blinkOn;
      this.cursorDirty = true;
      this.schedule();
    }, BLINK_MS);
  }
}

registerWebgpuRenderer((term, gpu, appearance) => new WebgpuRenderer(term, gpu, appearance));
