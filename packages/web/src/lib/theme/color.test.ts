import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  contrast,
  ensureContrast,
  luminance,
  mix,
  moreReadable,
  normalizeHex,
  oklabToRgb,
  parseHex,
  perceptualLightness,
  pickReadable,
  rgbToOklab,
  shiftLightness,
  toHex,
  withAlpha,
} from "./color.js";

describe("hex", () => {
  it("3 / 6 / 8 位都认，非法 undefined", () => {
    assert.deepEqual(parseHex("#fff"), { r: 255, g: 255, b: 255, a: 1 });
    assert.deepEqual(parseHex(" #1e1e2e "), { r: 30, g: 30, b: 46, a: 1 });
    assert.equal(parseHex("#ffffff80")!.a, 128 / 255);
    assert.equal(parseHex("1e1e2e"), undefined);
    assert.equal(parseHex("#12345"), undefined);
  });
  it("normalizeHex 小写六位、丢 alpha，非法原样", () => {
    assert.equal(normalizeHex("#ABC"), "#aabbcc");
    assert.equal(normalizeHex("#ffffff80"), "#ffffff");
    assert.equal(normalizeHex("red"), "red");
  });
  it("withAlpha 追加两位", () => {
    assert.equal(withAlpha("#ffffff", 0.1), "#ffffff1a");
    assert.equal(withAlpha("#000", 1), "#000000ff");
  });
});

describe("luminance / contrast", () => {
  it("黑白 21:1，同色 1:1", () => {
    assert.equal(luminance("#ffffff"), 1);
    assert.equal(luminance("#000000"), 0);
    assert.equal(contrast("#000000", "#ffffff"), 21);
    assert.equal(contrast("#ffffff", "#000000"), 21);
    assert.equal(contrast("#808080", "#808080"), 1);
  });
  it("shadcn 浅色 muted-foreground 在白底上约 4.7:1", () => {
    const c = contrast("#737373", "#ffffff");
    assert.ok(c > 4.6 && c < 4.8, String(c));
  });
});

describe("oklab", () => {
  it("往返恒等", () => {
    for (const hex of ["#000000", "#ffffff", "#1e1e2e", "#f38ba8", "#005661", "#f6edda"]) {
      const c = parseHex(hex)!;
      assert.equal(toHex(oklabToRgb(rgbToOklab(c))), hex);
    }
  });
  it("白 L≈1、黑 L≈0", () => {
    assert.ok(Math.abs(perceptualLightness("#ffffff") - 1) < 0.001);
    assert.ok(Math.abs(perceptualLightness("#000000")) < 0.001);
  });
});

describe("mix", () => {
  it("两端恒等，中间在两者之间", () => {
    assert.equal(mix("#0a0a0a", "#fafafa", 0), "#0a0a0a");
    assert.equal(mix("#0a0a0a", "#fafafa", 1), "#fafafa");
    const mid = perceptualLightness(mix("#0a0a0a", "#fafafa", 0.5));
    assert.ok(mid > 0.5 && mid < 0.6, String(mid));
  });
  it("复现 shadcn neutral：白底往近黑掺 3.5% ≈ oklch(0.97)", () => {
    const l = perceptualLightness(mix("#ffffff", "#171717", 0.035));
    assert.ok(Math.abs(l - 0.97) < 0.01, String(l));
  });
  it("复现 shadcn neutral：黑底往近白掺 7% ≈ oklch(0.205)", () => {
    const l = perceptualLightness(mix("#0a0a0a", "#fafafa", 0.07));
    assert.ok(Math.abs(l - 0.205) < 0.01, String(l));
  });
  it("非法输入原样返回 a", () => {
    assert.equal(mix("nope", "#fff", 0.5), "nope");
  });
});

describe("ensureContrast", () => {
  it("够就不动", () => {
    assert.equal(ensureContrast("#cd3131", "#ffffff", 3, "#000000"), "#cd3131");
  });
  it("不够就往 toward 掺到刚好够", () => {
    const out = ensureContrast("#b5ba00", "#ffffff", 3, "#171717");
    assert.ok(contrast(out, "#ffffff") >= 3, out);
    // 只掺到刚好，不会直接变成 toward
    assert.ok(contrast(out, "#ffffff") < 4, out);
    assert.notEqual(out, "#171717");
  });
  it("toward 自己都不够就返回 toward", () => {
    assert.equal(ensureContrast("#eeeeee", "#ffffff", 4.5, "#cccccc"), "#cccccc");
  });
});

describe("pickReadable / moreReadable", () => {
  it("第一个够的优先", () => {
    assert.equal(pickReadable(["#cd3131", "#ff0000"], "#ffffff", 4.5), "#cd3131");
  });
  it("第一个不够、第二个够就取第二个", () => {
    assert.equal(pickReadable(["#c50f1f", "#e74856"], "#0c0c0c", 4.5), "#e74856");
  });
  it("都不够取对比度最高的", () => {
    assert.equal(pickReadable(["#eeeeee", "#dddddd"], "#ffffff", 4.5), "#dddddd");
  });
  it("moreReadable 选黑白", () => {
    assert.equal(moreReadable("#ffffff", "#000000", "#cd3131"), "#ffffff");
    assert.equal(moreReadable("#1e1e2e", "#cdd6f4", "#f38ba8"), "#1e1e2e");
  });
});

describe("shiftLightness", () => {
  it("只动亮度，色度原样留着", () => {
    // Solarized Light 的暖米底压暗后还得是暖的——往黑掺会把 a/b 一起拉平
    const warm = "#fdf6e3";
    const shaded = shiftLightness(warm, -0.05);
    const a = rgbToOklab(parseHex(warm)!);
    const b = rgbToOklab(parseHex(shaded)!);
    assert.ok(Math.abs(b.L - (a.L - 0.05)) < 0.004, `${shaded} L=${b.L}`);
    assert.ok(Math.abs(b.a - a.a) < 0.004 && Math.abs(b.b - a.b) < 0.004, shaded);
  });
  it("往黑掺会顺带洗掉色度，压同样多的亮度时差得出来", () => {
    // Catppuccin Mocha 的紫灰底：压到同一个亮度，掺黑的那份蓝紫掉了三成
    const bg = "#1e1e2e";
    const before = rgbToOklab(parseHex(bg)!);
    const shifted = rgbToOklab(parseHex(shiftLightness(bg, -0.07))!);
    const mixed = rgbToOklab(parseHex(mix(bg, "#000000", 0.29))!);
    assert.ok(Math.abs(shifted.L - mixed.L) < 0.01, `${shifted.L} vs ${mixed.L}`);
    assert.ok(Math.abs(shifted.b - before.b) < 0.004, `${shifted.b} vs ${before.b}`);
    assert.ok(Math.abs(mixed.b) < Math.abs(before.b) * 0.8, `${mixed.b} vs ${before.b}`);
  });
  it("两端截断，不出 0–1", () => {
    assert.equal(shiftLightness("#ffffff", 0.5), "#ffffff");
    assert.equal(shiftLightness("#000000", -0.5), "#000000");
    assert.equal(shiftLightness("not-a-color", 0.1), "not-a-color");
  });
});
