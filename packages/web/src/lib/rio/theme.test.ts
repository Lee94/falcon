import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { opaqueOver, parseHex, toGpuTheme, toRioTheme, XTERM_DEFAULT_ANSI } from "./theme.js";

describe("parseHex", () => {
  it("3 / 6 / 8 位", () => {
    assert.deepEqual(parseHex("#fff"), { r: 255, g: 255, b: 255, a: 1 });
    assert.deepEqual(parseHex("#1e1e2e"), { r: 30, g: 30, b: 46, a: 1 });
    assert.deepEqual(parseHex("#ffffff80"), { r: 255, g: 255, b: 255, a: 128 / 255 });
  });
  it("非法输入 undefined", () => {
    assert.equal(parseHex("red"), undefined);
    assert.equal(parseHex("#12345"), undefined);
  });
});

describe("opaqueOver", () => {
  it("半透明色与底色预混成不透明", () => {
    assert.equal(opaqueOver("#ffffff80", "#000000"), "#808080");
    assert.equal(opaqueOver("#ff000033", "#000000"), "#330000");
  });
  it("不透明或非法输入原样返回", () => {
    assert.equal(opaqueOver("#ffffff", "#000000"), "#ffffff");
    assert.equal(opaqueOver("rebeccapurple", "#000000"), "rebeccapurple");
  });
});

describe("toRioTheme", () => {
  it("补齐 16 色，selectionBackground 预混，selectionForeground 缺省取 foreground", () => {
    const t = toRioTheme({ foreground: "#eeeeee", background: "#101010", selectionBackground: "#ffffff33" });
    assert.equal(t.black, XTERM_DEFAULT_ANSI.black);
    assert.equal(t.brightWhite, XTERM_DEFAULT_ANSI.brightWhite);
    assert.equal(t.selectionForeground, "#eeeeee");
    assert.equal(t.cursor, "#eeeeee");
    assert.equal(t.selectionBackground, opaqueOver("#ffffff33", "#101010"));
  });
});

describe("toGpuTheme", () => {
  it("保留 selectionBackground 的 alpha，selectionForeground 缺省不填", () => {
    const t = toGpuTheme({ foreground: "#eeeeee", background: "#101010", selectionBackground: "#ffffff33", red: "#ff0000" });
    assert.equal(t.selectionBackground, "#ffffff33");
    assert.equal(t.selectionForeground, undefined);
    assert.equal(t.cursorAccent, "#101010");
    assert.equal(t.red, "#ff0000");
    assert.equal(t.green, XTERM_DEFAULT_ANSI.green);
  });
});
