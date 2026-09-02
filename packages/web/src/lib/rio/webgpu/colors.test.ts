import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { toGpuTheme } from "../theme.js";
import {
  buildPalette,
  channels,
  COLOR_INDEXED,
  COLOR_NAMED,
  COLOR_RGB,
  dimColor,
  over,
  parseColor,
  resolveColor,
  rgba,
} from "./colors.js";

const theme = toGpuTheme({
  foreground: "#c0c0c0",
  background: "#101010",
  cursor: "#ff00ff",
  cursorAccent: "#000000",
  selectionBackground: "#ffffff80",
  red: "#ff0000",
  brightWhite: "#fafafa",
});
const p = buildPalette(theme);
const word = (kind: number, payload: number) => ((kind << 24) | payload) >>> 0;

describe("rgba 字节序", () => {
  it("r 在最低字节，写进 Uint32Array 后小端字节是 [r,g,b,a]", () => {
    const c = rgba(0x11, 0x22, 0x33, 0x44);
    const bytes = new Uint8Array(new Uint32Array([c]).buffer);
    assert.deepEqual([...bytes], [0x11, 0x22, 0x33, 0x44]);
    assert.deepEqual(channels(c), [0x11, 0x22, 0x33, 0x44]);
    assert.equal(rgba(255, 255, 255) >>> 0, 0xffffffff);
  });
});

describe("buildPalette", () => {
  it("0..15 来自主题（缺省补 Tango），cube 与灰阶按 xterm 256 色表", () => {
    assert.equal(p.table[1], rgba(255, 0, 0));
    assert.equal(p.table[15], rgba(0xfa, 0xfa, 0xfa));
    assert.equal(p.table[2], parseColor("#4e9a06", 0)); // Tango green
    assert.equal(p.table[196], rgba(255, 0, 0));
    assert.equal(p.table[16], rgba(0, 0, 0));
    assert.equal(p.table[231], rgba(255, 255, 255));
    assert.equal(p.table[232], rgba(8, 8, 8));
    assert.equal(p.table[255], rgba(238, 238, 238));
  });
  it("选区背景保留 alpha，selectionForeground 缺省为 null", () => {
    assert.deepEqual(channels(p.selBg), [255, 255, 255, 128]);
    assert.equal(p.selFg, null);
    assert.equal(p.cursor, rgba(255, 0, 255));
    assert.equal(p.cursorFg, rgba(0, 0, 0));
  });
});

describe("resolveColor", () => {
  it("RGB 直通，INDEXED 查表、越界兜底 fg", () => {
    assert.equal(resolveColor(word(COLOR_RGB, 0x123456), true, p), rgba(0x12, 0x34, 0x56));
    assert.equal(resolveColor(word(COLOR_INDEXED, 196), true, p), rgba(255, 0, 0));
    assert.equal(resolveColor(word(COLOR_INDEXED, 300), true, p), p.fg);
  });
  it("NAMED：ANSI16、fg/bg/cursor、dim 系列、light fg、未知按 isFg 兜底", () => {
    assert.equal(resolveColor(word(COLOR_NAMED, 1), true, p), rgba(255, 0, 0));
    assert.equal(resolveColor(word(COLOR_NAMED, 256), true, p), p.fg);
    assert.equal(resolveColor(word(COLOR_NAMED, 257), false, p), p.bg);
    assert.equal(resolveColor(word(COLOR_NAMED, 258), true, p), p.cursor);
    assert.equal(resolveColor(word(COLOR_NAMED, 259 + 1), true, p), dimColor(rgba(255, 0, 0)));
    assert.equal(resolveColor(word(COLOR_NAMED, 267), true, p), p.table[15]);
    assert.equal(resolveColor(word(COLOR_NAMED, 268), true, p), dimColor(p.fg));
    assert.equal(resolveColor(word(COLOR_NAMED, 999), true, p), p.fg);
    assert.equal(resolveColor(word(COLOR_NAMED, 999), false, p), p.bg);
  });
});

describe("dimColor / over", () => {
  it("dim 各通道 ×2/3 向下取整，alpha 不动", () => {
    assert.deepEqual(channels(dimColor(rgba(255, 128, 3, 200))), [170, 85, 2, 200]);
  });
  it("over：alpha 0 保留底色、alpha 255 全覆盖、半透明线性混合，输出恒不透明", () => {
    const dst = rgba(0, 0, 0);
    assert.equal(over(rgba(255, 255, 255, 0), dst), rgba(0, 0, 0));
    assert.equal(over(rgba(10, 20, 30, 255), dst), rgba(10, 20, 30));
    assert.deepEqual(channels(over(rgba(255, 255, 255, 128), dst)), [128, 128, 128, 255]);
    assert.deepEqual(channels(over(rgba(255, 0, 0, 51), rgba(0, 0, 255))), [51, 0, 204, 255]);
  });
  it("parseColor 非法输入用 fallback", () => {
    assert.equal(parseColor("red", 7), 7);
    assert.equal(parseColor(undefined, 7), 7);
    assert.deepEqual(channels(parseColor("#ffffff33", 0)), [255, 255, 255, 51]);
  });
});
