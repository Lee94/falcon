import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  bucketFill,
  colorBucket,
  extractAlpha,
  GLYPH_EMPTY,
  GLYPH_FULL,
  GlyphAtlas,
  glyphKey,
  isColorGlyph,
  premultiply,
  ShelfAllocator,
  type AtlasTexture,
  type Raster,
  type Rasterizer,
} from "./atlas.js";
import { rgba } from "./colors.js";

describe("ShelfAllocator", () => {
  it("首块落在 (pad, pad)，同高同架，更高开新架", () => {
    const a = new ShelfAllocator(64, 64, 1);
    assert.deepEqual(a.alloc(10, 10), { x: 1, y: 1 });
    assert.deepEqual(a.alloc(10, 10), { x: 13, y: 1 });
    assert.deepEqual(a.alloc(10, 20), { x: 1, y: 13 });
  });

  it("宽溢出换架，高溢出返回 null，reset 后可重分", () => {
    const a = new ShelfAllocator(32, 32, 1);
    assert.ok(a.alloc(28, 8));
    const second = a.alloc(28, 8);
    assert.deepEqual(second, { x: 1, y: 11 });
    assert.ok(a.alloc(28, 8));
    assert.equal(a.alloc(28, 8), null);
    assert.equal(a.alloc(40, 2), null);
    a.reset();
    assert.deepEqual(a.alloc(28, 8), { x: 1, y: 1 });
  });

  it("随机尺寸：含 padding 的矩形两两不相交且不出界", () => {
    const a = new ShelfAllocator(256, 256, 1);
    let seed = 42;
    const rnd = () => {
      seed = (seed * 1103515245 + 12345) & 0x7fffffff;
      return seed / 0x7fffffff;
    };
    const boxes: { x: number; y: number; w: number; h: number }[] = [];
    for (let i = 0; i < 400; i++) {
      const w = 1 + Math.floor(rnd() * 30);
      const h = 1 + Math.floor(rnd() * 30);
      const p = a.alloc(w, h);
      if (!p) break;
      boxes.push({ x: p.x - 1, y: p.y - 1, w: w + 2, h: h + 2 });
    }
    assert.ok(boxes.length > 50);
    for (const b of boxes) {
      assert.ok(b.x >= 0 && b.y >= 0 && b.x + b.w <= 256 && b.y + b.h <= 256);
    }
    for (let i = 0; i < boxes.length; i++) {
      for (let j = i + 1; j < boxes.length; j++) {
        const p = boxes[i]!;
        const q = boxes[j]!;
        const overlap = p.x < q.x + q.w && q.x < p.x + p.w && p.y < q.y + q.h && q.y < p.y + p.h;
        assert.equal(overlap, false, `box ${i} overlaps ${j}`);
      }
    }
  });
});

describe("像素工具", () => {
  it("extractAlpha 取第 4 通道，premultiply 四舍五入", () => {
    const px = new Uint8ClampedArray([255, 255, 255, 128, 10, 20, 30, 255]);
    assert.deepEqual([...extractAlpha(px, 2, 1)], [128, 255]);
    assert.deepEqual([...premultiply(px)], [128, 128, 128, 128, 10, 20, 30, 255]);
  });
  it("isColorGlyph：像素等于画笔色不算，偏离算，低 alpha 像素的反预乘误差忽略", () => {
    const light: [number, number, number] = [224, 224, 224];
    assert.equal(isColorGlyph(new Uint8ClampedArray([224, 224, 224, 200, 120, 120, 120, 50]), light), false);
    assert.equal(isColorGlyph(new Uint8ClampedArray([255, 200, 0, 200]), light), true);
    assert.equal(isColorGlyph(new Uint8ClampedArray([255, 0, 0, 100]), light), false);
    const tint: [number, number, number] = [32, 96, 160];
    assert.equal(isColorGlyph(new Uint8ClampedArray([32, 96, 160, 255, 33, 95, 161, 130]), tint), false);
    assert.equal(isColorGlyph(new Uint8ClampedArray([255, 204, 0, 255]), tint), true);
  });
  it("colorBucket 只看每通道高 2 位；bucketFill 取桶中点", () => {
    assert.equal(colorBucket(rgba(0x17, 0x17, 0x17)), 0);
    assert.equal(colorBucket(rgba(0x3f, 0x3f, 0x3f)), 0);
    assert.equal(colorBucket(rgba(0x40, 0x3f, 0x3f)), 1 << 4);
    assert.equal(colorBucket(rgba(0xff, 0xff, 0xff)), 63);
    assert.equal(colorBucket(rgba(0xab, 0xb2, 0xbf)), (2 << 4) | (2 << 2) | 2);
    assert.equal(colorBucket(rgba(0x04, 0x51, 0xa5)), (1 << 2) | 2);
    assert.deepEqual(bucketFill(0), [32, 32, 32]);
    assert.deepEqual(bucketFill(63), [224, 224, 224]);
    assert.deepEqual(bucketFill((1 << 4) | (2 << 2) | 3), [96, 160, 224]);
  });
  it("glyphKey 把样式位叠在 21 位码位之上，颜色桶再往上叠且不进符号位", () => {
    assert.equal(glyphKey(0x41, false, false, false), 0x41);
    assert.equal(glyphKey(0x10ffff, true, true, true), 0x10ffff | (7 << 21));
    assert.notEqual(glyphKey(0x41, true, false, false), glyphKey(0x41, false, true, false));
    assert.equal(glyphKey(0x41, false, false, false, 63), 0x41 | (63 << 24));
    assert.ok(glyphKey(0x10ffff, true, true, true, 63) > 0);
    assert.notEqual(glyphKey(0x41, false, false, false, 1), glyphKey(0x41, false, false, false, 2));
  });
});

function fakeTexture(size: number) {
  const uploads: { x: number; y: number; w: number; h: number }[] = [];
  const tex: AtlasTexture = { size, upload: (x, y, r) => uploads.push({ x, y, w: r.width, h: r.height }) };
  return { tex, uploads };
}

function fakeRasterizer(widthOf: (text: string) => number): Rasterizer & { calls: string[] } {
  const calls: string[] = [];
  return {
    calls,
    raster(text, bold, _italic, _wide, bucket) {
      calls.push(`${text}|${bucket}`);
      const w = widthOf(text);
      if (w === 0) return null;
      const r: Raster = {
        data: new Uint8Array(w * 4),
        width: w,
        height: 4,
        bearingX: -1,
        bearingY: 2,
        color: text === "🙂",
      };
      if (bold) r.bearingX = -2;
      return r;
    },
    dispose() {},
  };
}

describe("GlyphAtlas", () => {
  it("同 key 命中缓存，不重复光栅化；空 ink 返回 GLYPH_EMPTY；彩色进彩色纹理", () => {
    const gray = fakeTexture(64);
    const color = fakeTexture(64);
    const r = fakeRasterizer((t) => (t === " " ? 0 : 6));
    const atlas = new GlyphAtlas(gray.tex, color.tex, r);
    const a = atlas.get(0x41, null, false, false, false, 0);
    const b = atlas.get(0x41, null, false, false, false, 0);
    assert.equal(a, b);
    assert.equal(r.calls.length, 1);
    assert.notEqual(atlas.get(0x41, null, true, false, false, 0), a);
    assert.equal(atlas.get(0x20, null, false, false, false, 0), GLYPH_EMPTY);
    const e = atlas.get(0x1f642, "🙂", false, false, true, 0);
    assert.equal(atlas.table.flags[e], 1);
    assert.equal(color.uploads.length, 1);
    assert.equal(gray.uploads.length, 2);
    assert.equal(atlas.table.bx[a], -1);
    assert.equal(atlas.table.by[a], 2);
  });

  it("灰度字形按颜色桶各画一份（画笔色不同）；彩色字形无视画笔色，跨桶共享", () => {
    const gray = fakeTexture(64);
    const color = fakeTexture(64);
    const r = fakeRasterizer(() => 6);
    const atlas = new GlyphAtlas(gray.tex, color.tex, r);
    const a0 = atlas.get(0x41, null, false, false, false, 0);
    const a63 = atlas.get(0x41, null, false, false, false, 63);
    assert.notEqual(a0, a63);
    assert.equal(atlas.get(0x41, null, false, false, false, 63), a63);
    assert.deepEqual(r.calls, ["A|0", "A|63"]);
    const e0 = atlas.get(0x1f642, null, false, false, true, 0);
    assert.equal(atlas.get(0x1f642, null, false, false, true, 63), e0);
    assert.equal(atlas.get(0x1f642, null, false, false, true, 63), e0);
    assert.equal(color.uploads.length, 1);
    const c5 = atlas.get(0x1f642, "🙂", false, false, true, 5);
    assert.equal(atlas.get(0x1f642, "🙂", false, false, true, 9), c5);
    assert.equal(color.uploads.length, 2);
    assert.equal(r.calls.length, 4);
    atlas.reset();
    assert.equal(atlas.get(0x1f642, null, false, false, true, 7), 0);
    assert.equal(color.uploads.length, 3);
  });

  it("图集满返回 GLYPH_FULL，reset 后从头分配", () => {
    const gray = fakeTexture(16);
    const color = fakeTexture(16);
    const atlas = new GlyphAtlas(gray.tex, color.tex, fakeRasterizer(() => 6));
    const ids: number[] = [];
    for (let cp = 0x41; cp < 0x60; cp++) {
      const id = atlas.get(cp, null, false, false, false, 0);
      if (id === GLYPH_FULL) break;
      ids.push(id);
    }
    assert.ok(ids.length >= 2 && ids.length < 0x1f);
    assert.equal(atlas.get(0x7a, null, false, false, false, 0), GLYPH_FULL);
    atlas.reset();
    assert.equal(atlas.get(0x7a, null, false, false, false, 0), 0);
  });

  it("getSprite 按 key 复用，make 返回 null 记作 GLYPH_EMPTY", () => {
    const atlas = new GlyphAtlas(fakeTexture(64).tex, fakeTexture(64).tex, fakeRasterizer(() => 6));
    let made = 0;
    const make = () => {
      made++;
      return { data: new Uint8Array(4), width: 2, height: 2, bearingX: 0, bearingY: 0, color: false };
    };
    const id = atlas.getSprite("u", make);
    assert.equal(atlas.getSprite("u", make), id);
    assert.equal(made, 1);
    assert.equal(atlas.getSprite("none", () => null), GLYPH_EMPTY);
  });
});
