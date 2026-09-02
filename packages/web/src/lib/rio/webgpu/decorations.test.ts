import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { decorationSprite, solidRects } from "./decorations.js";
import { computeMetrics, type CellMetrics } from "./metrics.js";

const metrics = (fontSize: number, dpr = 1, lineHeight = 1): CellMetrics =>
  computeMetrics({ fontFamily: "m", fontSize, lineHeight, dpr }, (font) => {
    const px = Number(/(\d+(?:\.\d+)?)px/.exec(font)![1]);
    return { width: px * 0.6, ascent: px * 0.95, descent: px * 0.25 };
  });

const column = (r: { data: Uint8Array; width: number; height: number }, x: number) => {
  const out: number[] = [];
  for (let y = 0; y < r.height; y++) out.push(r.data[y * r.width + x]!);
  return out;
};

describe("solidRects", () => {
  it("各类矩形不出格；double 两段有间隙", () => {
    for (const m of [metrics(10), metrics(13, 2), metrics(24, 1.5, 1.6), metrics(8)]) {
      for (const kind of ["underline", "double", "strikeout", "cursorBar", "cursorUnderline"] as const) {
        for (const r of solidRects(kind, m)) {
          assert.ok(r.x >= 0 && r.y >= 0 && r.w >= 1 && r.h >= 1, `${kind} size`);
          assert.ok(r.x + r.w <= m.cellW && r.y + r.h <= m.cellH, `${kind} in cell ${JSON.stringify(r)} ${m.cellW}x${m.cellH}`);
        }
      }
      const [a, b] = solidRects("double", m);
      assert.ok(b!.y >= a!.y + a!.h);
    }
  });
});

describe("decorationSprite", () => {
  it("dotted / dashed 从亮开始、按各自周期排布，两格拼接仍是同一周期", () => {
    for (const m of [metrics(13, 2), metrics(10), metrics(24, 1.5)]) {
      const t = m.underlineThick;
      const seg = Math.max(2, Math.round(m.cellW / 2));
      const gap = Math.max(1, Math.round(seg / 2));
      const expect = { dotted: (x: number) => Math.floor(x / t) % 2 === 0, dashed: (x: number) => x % (seg + gap) < seg };
      for (const kind of ["dotted", "dashed"] as const) {
        const s = decorationSprite(kind, m);
        assert.equal(s.width, m.cellW);
        assert.equal(s.height, t);
        assert.equal(s.data[0], 255);
        for (let x = 0; x < s.width; x++) {
          assert.equal(column(s, x).every((v) => v === (expect[kind](x) ? 255 : 0)), true, `${kind} x=${x}`);
        }
        assert.ok(column(s, s.width - 1).length === t);
        assert.ok([...s.data].some((v) => v === 0), "有暗段");
      }
    }
  });

  it("undercurl：首列与末列相接、波形左右对称、不出格", () => {
    for (const m of [metrics(10), metrics(13, 2), metrics(24, 1.5), metrics(8)]) {
      const s = decorationSprite("undercurl", m);
      assert.equal(s.width, m.cellW);
      assert.ok(s.bearingY >= 0 && s.bearingY + s.height <= m.cellH, `in cell ${s.bearingY}+${s.height} <= ${m.cellH}`);
      const first = column(s, 0);
      const last = column(s, s.width - 1);
      // 端点导数为零：首末列的笔画中心差不超过一个像素
      const center = (c: number[]) => {
        const ys = c.map((v, y) => (v ? y : -1)).filter((y) => y >= 0);
        return (ys[0]! + ys[ys.length - 1]!) / 2;
      };
      assert.ok(Math.abs(center(first) - center(last)) <= 1, `seam ${center(first)} vs ${center(last)}`);
      for (let x = 0; x < Math.floor(s.width / 2); x++) {
        assert.deepEqual(column(s, x), column(s, s.width - 1 - x), `symmetry at ${x}`);
      }
      // 每列都有笔画
      for (let x = 0; x < s.width; x++) assert.ok(column(s, x).some((v) => v === 255), `column ${x} empty`);
    }
  });
});
