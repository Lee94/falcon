import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { MouseReporter, type MouseReportEvent } from "./mouse.js";

const GRID = { cols: 80, rows: 24 };
const SGR_DRAG = { protocol: 1002, encoding: 1006 };

function ev(over: Partial<MouseReportEvent> = {}): MouseReportEvent {
  return { action: "down", button: 0, col: 4, row: 2, x: 0, y: 0, shift: false, alt: false, ctrl: false, ...over };
}

describe("MouseReporter", () => {
  it("SGR：坐标 1 起，按下 M 结尾，松开 m 结尾且带按键号，拖动按键号 +32", () => {
    const r = new MouseReporter();
    assert.equal(r.report(ev(), SGR_DRAG, GRID), "\x1b[<0;5;3M");
    assert.equal(r.report(ev({ action: "move", col: 6 }), SGR_DRAG, GRID), "\x1b[<32;7;3M");
    assert.equal(r.report(ev({ action: "up", button: 2, col: 6 }), SGR_DRAG, GRID), "\x1b[<2;7;3m");
  });

  it("修饰键进按键号：Shift 4 / Alt 8 / Ctrl 16", () => {
    const r = new MouseReporter();
    assert.equal(r.report(ev({ button: 2, ctrl: true }), SGR_DRAG, GRID), "\x1b[<18;5;3M");
    assert.equal(r.report(ev({ button: 1, shift: true, alt: true }), SGR_DRAG, GRID), "\x1b[<13;5;3M");
  });

  it("?9（X10）只报按下，修饰键抹掉", () => {
    const r = new MouseReporter();
    const mode = { protocol: 9, encoding: 1006 };
    assert.equal(r.report(ev({ ctrl: true, shift: true }), mode, GRID), "\x1b[<0;5;3M");
    assert.equal(r.report(ev({ action: "up" }), mode, GRID), null);
    assert.equal(r.report(ev({ action: "move" }), mode, GRID), null);
  });

  it("?1000 报按下 / 松开，不报移动", () => {
    const r = new MouseReporter();
    const mode = { protocol: 1000, encoding: 1006 };
    assert.equal(r.report(ev(), mode, GRID), "\x1b[<0;5;3M");
    assert.equal(r.report(ev({ action: "move", col: 9 }), mode, GRID), null);
    assert.equal(r.report(ev({ action: "up" }), mode, GRID), "\x1b[<0;5;3m");
  });

  it("?1002 只报按住时的拖动，无按键移动丢掉", () => {
    const r = new MouseReporter();
    assert.equal(r.report(ev({ action: "move", button: 3 }), SGR_DRAG, GRID), null);
    assert.equal(r.report(ev({ action: "move", button: 0 }), SGR_DRAG, GRID), "\x1b[<32;5;3M");
  });

  it("?1003 无按键移动报 35（3 + 32）", () => {
    const r = new MouseReporter();
    const mode = { protocol: 1003, encoding: 1006 };
    assert.equal(r.report(ev({ action: "move", button: 3 }), mode, GRID), "\x1b[<35;5;3M");
  });

  it("协议没开一律不报", () => {
    const r = new MouseReporter();
    assert.equal(r.report(ev(), { protocol: 0, encoding: 1006 }, GRID), null);
    assert.equal(r.report(ev(), { protocol: 0, encoding: 0 }, GRID), null);
  });

  it("无按键却不是移动的事件不成立", () => {
    const r = new MouseReporter();
    assert.equal(r.report(ev({ button: 3 }), { protocol: 1003, encoding: 1006 }, GRID), null);
  });

  it("移动按格去重：同格同键同修饰键只报一次，按下不去重", () => {
    const r = new MouseReporter();
    const move = ev({ action: "move", button: 0 });
    assert.equal(r.report(move, SGR_DRAG, GRID), "\x1b[<32;5;3M");
    assert.equal(r.report(move, SGR_DRAG, GRID), null);
    assert.equal(r.report(ev({ action: "move", button: 0, col: 5 }), SGR_DRAG, GRID), "\x1b[<32;6;3M");
    assert.equal(r.report(ev({ action: "move", button: 0, col: 5, shift: true }), SGR_DRAG, GRID), "\x1b[<36;6;3M");
    assert.equal(r.report(ev({ col: 5 }), SGR_DRAG, GRID), "\x1b[<0;6;3M");
    assert.equal(r.report(ev({ col: 5 }), SGR_DRAG, GRID), "\x1b[<0;6;3M");
  });

  it("reset 后同一格的移动会再报", () => {
    const r = new MouseReporter();
    const move = ev({ action: "move", button: 0 });
    r.report(move, SGR_DRAG, GRID);
    r.reset();
    assert.equal(r.report(move, SGR_DRAG, GRID), "\x1b[<32;5;3M");
  });

  it("?1016 报像素坐标，并按像素去重", () => {
    const r = new MouseReporter();
    const mode = { protocol: 1003, encoding: 1016 };
    assert.equal(r.report(ev({ x: 41, y: 37 }), mode, GRID), "\x1b[<0;41;37M");
    assert.equal(r.report(ev({ action: "move", x: 41, y: 37 }), mode, GRID), "\x1b[<32;41;37M");
    // 同一格内挪了 1 像素也算新位置
    assert.equal(r.report(ev({ action: "move", x: 42, y: 37 }), mode, GRID), "\x1b[<32;42;37M");
    assert.equal(r.report(ev({ action: "move", x: 42, y: 37 }), mode, GRID), null);
  });

  it("默认编码：CSI M 加三个 +32 的单字节，松开报 3", () => {
    const r = new MouseReporter();
    const mode = { protocol: 1000, encoding: 0 };
    // 按键 0 → 32 " "，列 5 → 37 "%"，行 3 → 35 "#"
    assert.equal(r.report(ev(), mode, GRID), "\x1b[M %#");
    // 松开：按键号一律 3 → 35 "#"，中键也一样；坐标字节不变
    assert.equal(r.report(ev({ action: "up", button: 1 }), mode, GRID), "\x1b[M#%#");
  });

  it("默认编码超出 ASCII 的报文丢掉（文本输入通道发不出高位字节）", () => {
    const r = new MouseReporter();
    const mode = { protocol: 1000, encoding: 0 };
    const wide = { cols: 200, rows: 24 };
    // 0 起 94 列 → 1 起 95 → 127，还在范围内
    assert.equal(r.report(ev({ col: 94 }), mode, wide), "\x1b[M \x7f#");
    assert.equal(r.report(ev({ col: 95 }), mode, wide), null);
  });

  it("格坐标出界丢掉", () => {
    const r = new MouseReporter();
    assert.equal(r.report(ev({ col: 80 }), SGR_DRAG, GRID), null);
    assert.equal(r.report(ev({ row: -1 }), SGR_DRAG, GRID), null);
  });
});
