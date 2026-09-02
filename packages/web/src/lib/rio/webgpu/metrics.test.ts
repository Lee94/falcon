import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { computeMetrics, fontString, type MeasureFn } from "./metrics.js";

/** 模拟 Menlo 一类等宽字体：advance 0.6em，行盒 0.95em + 0.25em */
const menlo: MeasureFn = (font) => {
  const px = Number(/(\d+(?:\.\d+)?)px/.exec(font)![1]);
  return { width: px * 0.6, ascent: px * 0.95, descent: px * 0.25 };
};

describe("computeMetrics", () => {
  it("dpr 1 / 1.5 / 2 下全部是物理像素整数，CSS 值是物理值 / dpr", () => {
    for (const dpr of [1, 1.5, 2]) {
      const m = computeMetrics({ fontFamily: "Menlo", fontSize: 13, lineHeight: 1, dpr }, menlo);
      for (const k of ["cellW", "cellH", "baseline", "underlineY", "underlineThick", "strikeY", "barW"] as const) {
        assert.equal(Number.isInteger(m[k]), true, `${k}@${dpr}`);
      }
      assert.equal(m.cssCellW, m.cellW / dpr);
      assert.equal(m.cssCellH, m.cellH / dpr);
      assert.equal(m.fontPx, 13 * dpr);
    }
  });

  it("行高按字体行盒 × lineHeight 取整（对齐 xterm），不是 fontSize", () => {
    const m = computeMetrics({ fontFamily: "Menlo", fontSize: 13, lineHeight: 1, dpr: 2 }, menlo);
    assert.equal(m.cellH, Math.ceil((13 * 2 * 0.95 + 13 * 2 * 0.25) * 1));
    const tall = computeMetrics({ fontFamily: "Menlo", fontSize: 13, lineHeight: 1.4, dpr: 2 }, menlo);
    assert.equal(tall.cellH, Math.ceil(13 * 2 * 1.2 * 1.4));
    assert.equal(m.cellW, Math.ceil(13 * 2 * 0.6));
  });

  it("baseline 落在行盒里，装饰不出格", () => {
    for (const fontSize of [10, 13, 24]) {
      for (const lineHeight of [1, 1.6]) {
        const m = computeMetrics({ fontFamily: "Menlo", fontSize, lineHeight, dpr: 1 }, menlo);
        const asc = fontSize * 0.95;
        const desc = fontSize * 0.25;
        assert.ok(m.baseline >= Math.floor(asc) && m.baseline <= m.cellH - Math.floor(desc), `baseline ${fontSize}/${lineHeight}`);
        assert.ok(m.underlineY + m.underlineThick <= m.cellH);
        assert.ok(m.strikeY >= 0 && m.strikeY < m.baseline);
        assert.ok(m.underlineThick >= 1 && m.barW >= 1);
      }
    }
  });

  it("量不到行盒（老内核）时按 1.2 倍行高兜底，不会得到 0", () => {
    const m = computeMetrics({ fontFamily: "x", fontSize: 12, lineHeight: 1, dpr: 1 }, () => ({ width: 7, ascent: 0, descent: 0 }));
    assert.ok(m.cellH >= 12 && m.baseline > 0);
  });

  it("fontString 顺序：italic bold size family", () => {
    assert.equal(fontString(26, "Menlo, monospace", true, true), "italic bold 26px Menlo, monospace");
    assert.equal(fontString(13, "Menlo"), "13px Menlo");
  });
});
