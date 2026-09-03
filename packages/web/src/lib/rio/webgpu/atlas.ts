/**
 * 字形图集：字形 / 装饰 / sprite 都光栅化成小位图塞进两张纹理（R8 灰度 + RGBA8
 * 彩色），GPU 按整数坐标 textureLoad，不经 sampler，相邻字形不会被双线性抹进来。
 *
 * 光栅化用离屏 2D canvas 的 fillText，画笔颜色取真实前景色（量化到颜色桶），不能
 * 永远画白字：Chrome（Skia）会按画笔亮度查 gamma 预混表修正灰度遮罩，白字最重、
 * 黑字最轻，同一字形覆盖量差 12%（2026-09 实测，F 竖笔 2.88 vs 2.65px），浅色主题
 * 下画白字再在 GPU 着色比 xterm / DOM 文字明显粗一圈。这张表只看画笔每通道的高 2 位
 * （灰阶扫描跳变点正好在 64 / 128 / 192，150 个随机色与其量化色遮罩逐像素全等），
 * 所以图集 key 多 6 bit 颜色桶、按桶画字，桶内取中点做代表色。
 *
 * 彩色字体（Apple Color Emoji / Noto Color Emoji）会无视 fillStyle 画成彩色，像素
 * 颜色偏离画笔色就进 RGBA 图集（彩色字形跨桶共享，不重复占位），否则取 alpha 通道
 * 进 R8——零 Unicode 表，与系统字体回退行为天然一致。
 *
 * 位图只开 ink 盒那么大（measureText 的 actualBoundingBox*，四边各加 1px）：ASCII
 * 的 ink 盒平均只占格子 40%，图集容量翻倍以上；同时保留溢出（斜体尾巴、Nerd 图标、
 * CJK 回退字体比 cellH 高）——GPU 每帧从 buffer 全量重画，没有残影问题，溢出到
 * 邻格是原生终端的行为。
 *
 * 图集满了不扩容：调 reset() 清分配器与表，渲染器随后全量重建所有行（旧区域不会
 * 再被引用，纹理本身不用清）。
 */

import type { Rgba } from "./colors.js";
import { fontString, type CellMetrics } from "./metrics.js";

/** 位图的 alpha 通道；随机尺寸装箱与 padding 由 ShelfAllocator 的测试保证 */
export class ShelfAllocator {
  private shelves: { x: number; y: number; width: number; height: number }[] = [];
  private nextY = 0;

  constructor(
    readonly width: number,
    readonly height: number,
    readonly pad = 1
  ) {}

  /** 返回位图左上角（已含 padding 偏移）；放不下返回 null */
  alloc(w: number, h: number): { x: number; y: number } | null {
    const pw = w + this.pad * 2;
    const ph = h + this.pad * 2;
    if (pw > this.width || ph > this.height) return null;
    // 同高优先，其次浪费最少的架子（架高 ≥ 需求且浪费 ≤ 10%）
    let best = -1;
    let bestWaste = Number.POSITIVE_INFINITY;
    for (let i = 0; i < this.shelves.length; i++) {
      const s = this.shelves[i]!;
      if (s.height < ph || s.width - s.x < pw) continue;
      const waste = (s.height - ph) / s.height;
      if (waste > 0.1) continue;
      const score = s.height === ph ? waste - 1 : waste;
      if (score < bestWaste) {
        bestWaste = score;
        best = i;
      }
    }
    if (best >= 0) {
      const s = this.shelves[best]!;
      const x = s.x;
      s.x += pw;
      return { x: x + this.pad, y: s.y + this.pad };
    }
    if (this.nextY + ph > this.height) return null;
    const shelf = { x: pw, y: this.nextY, width: this.width, height: ph };
    this.shelves.push(shelf);
    this.nextY += ph;
    return { x: this.pad, y: shelf.y + this.pad };
  }

  reset(): void {
    this.shelves = [];
    this.nextY = 0;
  }
}

/** getImageData 的 RGBA（未预乘）→ R8 alpha */
export function extractAlpha(rgba: Uint8ClampedArray | Uint8Array, w: number, h: number): Uint8Array {
  const n = w * h;
  const out = new Uint8Array(n);
  for (let i = 0; i < n; i++) out[i] = rgba[i * 4 + 3]!;
  return out;
}

/** getImageData 的 RGBA（未预乘）→ 预乘 RGBA8，blend 用 one / one-minus-src-alpha */
export function premultiply(rgba: Uint8ClampedArray | Uint8Array): Uint8Array {
  const out = new Uint8Array(rgba.length);
  for (let i = 0; i < rgba.length; i += 4) {
    const a = rgba[i + 3]!;
    out[i] = Math.round((rgba[i]! * a) / 255);
    out[i + 1] = Math.round((rgba[i + 1]! * a) / 255);
    out[i + 2] = Math.round((rgba[i + 2]! * a) / 255);
    out[i + 3] = a;
  }
  return out;
}

/** 画笔颜色桶数：每通道高 2 位 */
export const COLOR_BUCKETS = 64;

/**
 * 前景色 → 颜色桶：r 高 2 位 << 4 | g 高 2 位 << 2 | b 高 2 位。Chrome 的字形遮罩只随
 * 画笔每通道的高 2 位变（见文件头）；其它内核没测，按同样粒度分桶至少不会比画白字差。
 */
export function colorBucket(fg: Rgba): number {
  return (((fg >>> 6) & 3) << 4) | (((fg >>> 14) & 3) << 2) | ((fg >>> 22) & 3);
}

/** 桶的代表色（桶中点）。Chrome 桶内任何颜色遮罩相同；CoreText 那种随亮度连续变的内核取中点误差最小 */
export function bucketFill(bucket: number): [number, number, number] {
  return [((bucket >> 4) & 3) * 64 + 32, ((bucket >> 2) & 3) * 64 + 32, (bucket & 3) * 64 + 32];
}

/**
 * 有足够不透明的像素颜色偏离画笔色就是彩色字形（彩色字体无视 fillStyle）。
 * 只看 alpha ≥ 128 的像素：getImageData 反预乘后，低 alpha 像素的颜色误差可达十几。
 */
export function isColorGlyph(
  rgba: Uint8ClampedArray | Uint8Array,
  fill: [number, number, number],
  threshold = 24
): boolean {
  for (let i = 0; i < rgba.length; i += 4) {
    if (rgba[i + 3]! < 128) continue;
    const d = Math.abs(rgba[i]! - fill[0]) + Math.abs(rgba[i + 1]! - fill[1]) + Math.abs(rgba[i + 2]! - fill[2]);
    if (d > threshold) return true;
  }
  return false;
}

/** 单码位字形的数字 key：cp 占 21 位，样式位往上叠，颜色桶占 24..29 位（不进符号位）。不做字符串分配 */
export function glyphKey(cp: number, bold: boolean, italic: boolean, wide: boolean, bucket = 0): number {
  return (
    (cp & 0x1fffff) |
    ((bold ? 1 : 0) << 21) |
    ((italic ? 1 : 0) << 22) |
    ((wide ? 1 : 0) << 23) |
    ((bucket & 0x3f) << 24)
  );
}

export interface Raster {
  /** R8（color=false）或预乘 RGBA8（color=true），行主序无 stride */
  data: Uint8Array;
  width: number;
  height: number;
  /** 位图左上角相对 cell 左上角的偏移，物理 px，可为负 */
  bearingX: number;
  bearingY: number;
  color: boolean;
}

export interface Rasterizer {
  /** 空 ink 盒返回 null；bucket 是 colorBucket() 的颜色桶，决定画笔颜色 */
  raster(text: string, bold: boolean, italic: boolean, wide: boolean, bucket: number): Raster | null;
  dispose(): void;
}

/** Nerd 图标 / CJK 回退字体可能比格子大，允许溢出到这个倍数，再大就缩 */
const MAX_OVERFLOW = 1.5;

export function createCanvasRasterizer(m: CellMetrics, fontFamily: string): Rasterizer {
  const canvas: OffscreenCanvas | HTMLCanvasElement =
    typeof OffscreenCanvas !== "undefined" ? new OffscreenCanvas(8, 8) : document.createElement("canvas");
  // willReadFrequently：否则每次 getImageData 都是一次 GPU→CPU 同步读回
  const ctx = canvas.getContext("2d", { willReadFrequently: true }) as
    | OffscreenCanvasRenderingContext2D
    | CanvasRenderingContext2D
    | null;
  if (!ctx) throw new Error("2d context unavailable");
  const fills = Array.from({ length: COLOR_BUCKETS }, (_, b) => bucketFill(b));
  const fillCss = fills.map(([r, g, b]) => `rgb(${r},${g},${b})`);

  return {
    raster(text, bold, italic, wide, bucket) {
      const font = fontString(m.fontPx, fontFamily, bold, italic);
      ctx.font = font;
      const tm = ctx.measureText(text);
      const left = Math.ceil(tm.actualBoundingBoxLeft) + 1;
      const right = Math.ceil(tm.actualBoundingBoxRight) + 1;
      const asc = Math.ceil(tm.actualBoundingBoxAscent) + 1;
      const desc = Math.ceil(tm.actualBoundingBoxDescent) + 1;
      let w = left + right;
      let h = asc + desc;
      if (!(w > 2) || !(h > 2) || !Number.isFinite(w + h)) return null;

      // 比允许的溢出还宽 / 高就整体缩放进格子（emoji 回退字体常见）
      const maxW = Math.ceil(m.cellW * (wide ? 2 : 1) * MAX_OVERFLOW);
      const maxH = Math.ceil(m.cellH * MAX_OVERFLOW);
      const scale = Math.min(1, maxW / w, maxH / h);
      let penX = left;
      let penY = asc;
      if (scale < 1) {
        w = Math.ceil(w * scale);
        h = Math.ceil(h * scale);
        penX = Math.ceil(left * scale);
        penY = Math.ceil(asc * scale);
      }
      if (canvas.width < w || canvas.height < h) {
        canvas.width = Math.max(canvas.width, w);
        canvas.height = Math.max(canvas.height, h);
      }
      // 改尺寸会重置 context 状态；不改时也要清干净上一个字形
      ctx.setTransform(1, 0, 0, 1, 0, 0);
      ctx.clearRect(0, 0, w, h);
      ctx.font = font;
      ctx.textBaseline = "alphabetic";
      const fill = fills[bucket & 0x3f]!;
      ctx.fillStyle = fillCss[bucket & 0x3f]!;
      if (scale < 1) {
        ctx.setTransform(scale, 0, 0, scale, 0, 0);
        ctx.fillText(text, left, asc);
        ctx.setTransform(1, 0, 0, 1, 0, 0);
      } else {
        ctx.fillText(text, penX, penY);
      }
      const img = ctx.getImageData(0, 0, w, h).data;
      const color = isColorGlyph(img, fill);
      return {
        data: color ? premultiply(img) : extractAlpha(img, w, h),
        width: w,
        height: h,
        bearingX: -penX,
        bearingY: m.baseline - penY,
        color,
      };
    },
    dispose() {
      canvas.width = 0;
      canvas.height = 0;
    },
  };
}

export const GLYPH_EMPTY = -1;
export const GLYPH_FULL = -2;

export const GLYPH_FLAG_COLOR = 1;

/** SoA 字形表，buildRow 按 id 直接取字段打包实例 */
export class GlyphTable {
  x = new Uint16Array(256);
  y = new Uint16Array(256);
  w = new Uint16Array(256);
  h = new Uint16Array(256);
  bx = new Int16Array(256);
  by = new Int16Array(256);
  flags = new Uint8Array(256);
  size = 0;

  push(x: number, y: number, w: number, h: number, bx: number, by: number, flags: number): number {
    if (this.size === this.x.length) this.grow();
    const id = this.size++;
    this.x[id] = x;
    this.y[id] = y;
    this.w[id] = w;
    this.h[id] = h;
    this.bx[id] = bx;
    this.by[id] = by;
    this.flags[id] = flags;
    return id;
  }

  clear(): void {
    this.size = 0;
  }

  private grow(): void {
    const n = this.x.length * 2;
    const grow = <T extends Uint16Array | Int16Array | Uint8Array>(a: T): T => {
      const b = new (a.constructor as new (n: number) => T)(n);
      b.set(a);
      return b;
    };
    this.x = grow(this.x);
    this.y = grow(this.y);
    this.w = grow(this.w);
    this.h = grow(this.h);
    this.bx = grow(this.bx);
    this.by = grow(this.by);
    this.flags = grow(this.flags);
  }
}

/** 图集纹理的最小接口，便于把 GPU 上传与装箱逻辑分开测 */
export interface AtlasTexture {
  readonly size: number;
  upload(x: number, y: number, raster: Raster): void;
}

export function createGpuAtlasTexture(device: GPUDevice, size: number, color: boolean): AtlasTexture & { texture: GPUTexture } {
  const texture = device.createTexture({
    label: color ? "rio.atlas.color" : "rio.atlas.gray",
    size: { width: size, height: size },
    format: color ? "rgba8unorm" : "r8unorm",
    usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.COPY_DST,
  });
  const bpp = color ? 4 : 1;
  return {
    size,
    texture,
    upload(x, y, r) {
      // writeTexture 的 bytesPerRow 没有 256 对齐要求（那是 copyBufferToTexture 的）
      device.queue.writeTexture(
        { texture, origin: { x, y } },
        // TS 5.7+ 的 Uint8Array<ArrayBufferLike> 与 @webgpu/types 的 BufferSource 对不上，语义无差
        r.data as unknown as BufferSource,
        { bytesPerRow: r.width * bpp, rowsPerImage: r.height },
        { width: r.width, height: r.height }
      );
    },
  };
}

export class GlyphAtlas {
  readonly table = new GlyphTable();
  private byKey = new Map<number, number>();
  private byText = new Map<string, number>();
  /** 彩色字形无视画笔色，按不含颜色桶的 key 共享，免得每个桶各占一份 RGBA 图集 */
  private colorByKey = new Map<number, number>();
  private colorByText = new Map<string, number>();
  private grayAlloc: ShelfAllocator;
  private colorAlloc: ShelfAllocator;

  constructor(
    private readonly gray: AtlasTexture,
    private readonly color: AtlasTexture,
    private readonly rasterizer: Rasterizer
  ) {
    this.grayAlloc = new ShelfAllocator(gray.size, gray.size);
    this.colorAlloc = new ShelfAllocator(color.size, color.size);
  }

  /**
   * 单码位走数字 key；cluster（ZWJ emoji、组合附标）走字符串 key。bucket 是 colorBucket()
   * 的颜色桶：灰度字形每桶各画一份，彩色字形第一次画出来就跨桶共享。
   * 返回 id ≥ 0，或 GLYPH_EMPTY（没有 ink，不发实例）/ GLYPH_FULL（图集满，调用方 reset 后重来）
   */
  get(cp: number, text: string | null, bold: boolean, italic: boolean, wide: boolean, bucket: number): number {
    if (text === null) {
      const key = glyphKey(cp, bold, italic, wide, bucket);
      const hit = this.byKey.get(key);
      if (hit !== undefined) return hit;
      const colorKey = glyphKey(cp, bold, italic, wide);
      const shared = this.colorByKey.get(colorKey);
      if (shared !== undefined) {
        this.byKey.set(key, shared);
        return shared;
      }
      const r = this.rasterizer.raster(String.fromCodePoint(cp), bold, italic, wide, bucket);
      const id = r ? this.place(r) : GLYPH_EMPTY;
      if (id === GLYPH_FULL) return id;
      this.byKey.set(key, id);
      if (r?.color) this.colorByKey.set(colorKey, id);
      return id;
    }
    const colorKey = `${text}|${(bold ? 1 : 0) | (italic ? 2 : 0) | (wide ? 4 : 0)}`;
    const key = `${colorKey}|${bucket}`;
    const hit = this.byText.get(key);
    if (hit !== undefined) return hit;
    const shared = this.colorByText.get(colorKey);
    if (shared !== undefined) {
      this.byText.set(key, shared);
      return shared;
    }
    const r = this.rasterizer.raster(text, bold, italic, wide, bucket);
    const id = r ? this.place(r) : GLYPH_EMPTY;
    if (id === GLYPH_FULL) return id;
    this.byText.set(key, id);
    if (r?.color) this.colorByText.set(colorKey, id);
    return id;
  }

  /** 装饰 / sprite：调用方自己光栅化，按 key 复用 */
  getSprite(key: string, make: () => Raster | null): number {
    const hit = this.byText.get(key);
    if (hit !== undefined) return hit;
    const raster = make();
    const id = raster ? this.place(raster) : GLYPH_EMPTY;
    if (id !== GLYPH_FULL) this.byText.set(key, id);
    return id;
  }

  reset(): void {
    this.byKey.clear();
    this.byText.clear();
    this.colorByKey.clear();
    this.colorByText.clear();
    this.table.clear();
    this.grayAlloc.reset();
    this.colorAlloc.reset();
  }

  dispose(): void {
    this.rasterizer.dispose();
  }

  private place(r: Raster): number {
    const alloc = r.color ? this.colorAlloc : this.grayAlloc;
    const slot = alloc.alloc(r.width, r.height);
    if (!slot) return GLYPH_FULL;
    (r.color ? this.color : this.gray).upload(slot.x, slot.y, r);
    return this.table.push(slot.x, slot.y, r.width, r.height, r.bearingX, r.bearingY, r.color ? GLYPH_FLAG_COLOR : 0);
  }
}
