import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { WheelAccumulator } from "./wheel.js";

const PIXEL = 0;
const LINE = 1;
const PAGE = 2;

describe("WheelAccumulator", () => {
  it("行模式直通，符号翻转（deltaY 向下为正，滚动向历史为正）", () => {
    const acc = new WheelAccumulator();
    assert.equal(acc.push(LINE, -3, 16), 3);
    assert.equal(acc.push(LINE, 2, 16), -2);
  });

  it("像素模式攒余量：三次 0.4 行攒到 1 行", () => {
    const acc = new WheelAccumulator();
    assert.equal(acc.push(PIXEL, -6.4, 16), 0);
    assert.equal(acc.push(PIXEL, -6.4, 16), 0);
    assert.equal(acc.push(PIXEL, -6.4, 16), 1);
    // 余量 0.2 继续攒
    assert.equal(acc.push(PIXEL, -12.8, 16), 1);
  });

  it("方向翻转清掉余量，不会先补反向的一段", () => {
    const acc = new WheelAccumulator();
    acc.push(PIXEL, -12, 16); // +0.75 余量
    assert.equal(acc.push(PIXEL, 4, 16), 0); // 反向 0.25 行，从 0 重新攒
    assert.equal(acc.push(PIXEL, 12, 16), -1); // 0.25 + 0.75 = 1
  });

  it("页模式按 rows 折算", () => {
    const acc = new WheelAccumulator();
    assert.equal(acc.push(PAGE, -1, 16, 40), 40);
  });

  it("cellHeight 为 0 / 非有限值时不动", () => {
    const acc = new WheelAccumulator();
    assert.equal(acc.push(PIXEL, -100, 0), 0);
    assert.equal(acc.push(PIXEL, Number.NaN, 16), 0);
  });

  it("reset 清余量", () => {
    const acc = new WheelAccumulator();
    acc.push(PIXEL, -12, 16);
    acc.reset();
    assert.equal(acc.push(PIXEL, -4, 16), 0);
  });
});
