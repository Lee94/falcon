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

describe("WheelAccumulator.click（程序接管滚轮）", () => {
  it("鼠标一格上百像素也只算一次点击，多出的整行丢掉不攒到下一次", () => {
    const acc = new WheelAccumulator();
    // 100px / 16px = 6.25 行：rioterm 原版会发 6 条上报
    assert.equal(acc.click(PIXEL, -100, 16), 1);
    // 余量只剩 0.25 行；紧接着一个 4px 触控板事件（0.25 × 0.3）攒不够
    assert.equal(acc.click(PIXEL, -4, 16), 0);
    assert.equal(acc.click(PIXEL, 100, 16), -1);
  });

  it("触控板小事件乘 0.3 阻尼：6px 事件要九次才出一次点击", () => {
    const acc = new WheelAccumulator();
    // 6/16 × 0.3 = 0.1125 行/次，第九次累计 1.0125
    for (let i = 0; i < 8; i++) assert.equal(acc.click(PIXEL, -6, 16), 0, `第 ${i + 1} 次`);
    assert.equal(acc.click(PIXEL, -6, 16), 1);
  });

  it("|deltaY| ≥ 50 不当触控板，不阻尼", () => {
    const acc = new WheelAccumulator();
    // 50/16 = 3.125 行，无阻尼直接够一次
    assert.equal(acc.click(PIXEL, -50, 16), 1);
    const damped = new WheelAccumulator();
    // 49/16 × 0.3 = 0.92 行，阻尼后不够
    assert.equal(damped.click(PIXEL, -49, 16), 0);
  });

  it("行模式 / 页模式每个事件一次点击，方向随符号", () => {
    const acc = new WheelAccumulator();
    assert.equal(acc.click(LINE, -3, 16), 1);
    assert.equal(acc.click(LINE, 3, 16), -1);
    assert.equal(acc.click(PAGE, -1, 16, 40), 1);
  });

  it("方向翻转清余量，与 push 共用同一份余量", () => {
    const acc = new WheelAccumulator();
    assert.equal(acc.click(PIXEL, -40, 16), 0); // 40/16 × 0.3 = 0.75
    assert.equal(acc.click(PIXEL, 40, 16), 0); // 反向从 0 起：-0.75
    assert.equal(acc.click(PIXEL, 40, 16), -1); // -1.5 → -1
    assert.equal(acc.push(PIXEL, 8, 16), -1); // click 剩 -0.5 余量，push 再加 -0.5 恰好 -1
  });
});
