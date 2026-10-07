import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  hasWheelReport,
  MIN_THUMB_PX,
  positionForThumbTop,
  thumbGeometry,
  type ScrollState,
} from "./termScroll.js";

const at = (position: number, length = 470, rows = 30): ScrollState => ({ position, length, rows });

describe("thumbGeometry", () => {
  it("没有可滚的历史就不画", () => {
    assert.equal(thumbGeometry(at(0, 0), 300), null);
    assert.equal(thumbGeometry(at(0, 470, 0), 300), null);
    assert.equal(thumbGeometry(at(0), 0), null);
  });

  it("在底部时贴底，滚到顶时贴顶", () => {
    const bottom = thumbGeometry(at(0), 500)!;
    assert.equal(bottom.top + bottom.height, 500);
    const top = thumbGeometry(at(470), 500)!;
    assert.equal(top.top, 0);
  });

  it("高度按视口占总行数的比例", () => {
    // 30 / (470 + 30) = 6%
    assert.equal(thumbGeometry(at(0), 500)!.height, 30);
  });

  it("比例太小时夹到最小高度，两端仍然对得上", () => {
    const s = at(0, 9970, 30); // 0.3% → 1.5px
    const bottom = thumbGeometry(s, 500)!;
    assert.equal(bottom.height, MIN_THUMB_PX);
    assert.equal(bottom.top + bottom.height, 500);
    assert.equal(thumbGeometry(at(9970, 9970, 30), 500)!.top, 0);
  });

  it("历史比视口还短时滑块也不超出轨道", () => {
    const g = thumbGeometry(at(0, 2, 30), 100)!;
    assert.ok(g.height <= 100);
    assert.equal(g.top + g.height, 100);
  });

  it("越界的 position 夹回去", () => {
    assert.equal(thumbGeometry(at(9999), 500)!.top, 0);
  });
});

describe("positionForThumbTop", () => {
  it("是 thumbGeometry 的反函数", () => {
    for (const p of [0, 1, 100, 235, 469, 470]) {
      const s = at(p);
      const g = thumbGeometry(s, 500)!;
      assert.equal(positionForThumbTop(s, 500, g.height, g.top), p);
    }
  });

  it("拖出轨道两端时夹到顶 / 底", () => {
    const s = at(100);
    assert.equal(positionForThumbTop(s, 500, 30, -50), 470);
    assert.equal(positionForThumbTop(s, 500, 30, 9999), 0);
  });

  it("滑块占满轨道时没有可拖的余地", () => {
    assert.equal(positionForThumbTop(at(1, 2, 30), 100, 100, 0), 0);
  });
});

describe("hasWheelReport", () => {
  it("SGR 滚轮上下（含修饰键）", () => {
    assert.ok(hasWheelReport("\x1b[<64;10;5M"));
    assert.ok(hasWheelReport("\x1b[<65;1;1M"));
    assert.ok(hasWheelReport("\x1b[<80;3;4M")); // Ctrl + 滚轮上
    assert.ok(hasWheelReport("abc\x1b[<64;10;5M\x1b[<64;10;5M"));
  });

  it("点击、拖动、松开、普通输入都不算", () => {
    assert.ok(!hasWheelReport("\x1b[<0;10;5M"));
    assert.ok(!hasWheelReport("\x1b[<0;10;5m"));
    assert.ok(!hasWheelReport("\x1b[<32;10;5M"));
    assert.ok(!hasWheelReport("\x1b[<128;10;5M"));
    assert.ok(!hasWheelReport("ls -la\r"));
    assert.ok(!hasWheelReport("\x1b[A"));
  });
});
