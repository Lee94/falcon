import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  GHOSTTY_DEFAULT_BACKGROUND,
  GHOSTTY_DEFAULT_FOREGROUND,
  GHOSTTY_DEFAULT_PALETTE,
  parseGhosttyColor,
  parseGhosttyTheme,
  parseThemeSetting,
  resolveThemeColors,
  serializeGhosttyTheme,
  sourceFromColors,
  type ThemeColors,
} from "./ghostty.js";

const MOCHA = `palette = 0=#45475a
palette = 1=#f38ba8
palette = 2=#a6e3a1
palette = 3=#f9e2af
palette = 4=#89b4fa
palette = 5=#f5c2e7
palette = 6=#94e2d5
palette = 7=#a6adc8
palette = 8=#585b70
palette = 9=#f37799
palette = 10=#89d88b
palette = 11=#ebd391
palette = 12=#74a8fc
palette = 13=#f2aede
palette = 14=#6bd7ca
palette = 15=#bac2de
background = #1e1e2e
foreground = #cdd6f4
cursor-color = #f5e0dc
cursor-text = #1e1e2e
selection-background = #585b70
selection-foreground = #cdd6f4
`;

describe("parseGhosttyColor", () => {
  it("带不带 # 都认，统一小写", () => {
    assert.equal(parseGhosttyColor("#1E1E2E"), "#1e1e2e");
    assert.equal(parseGhosttyColor("1e1e2e"), "#1e1e2e");
    assert.equal(parseGhosttyColor('"#1e1e2e"'), "#1e1e2e");
    assert.equal(parseGhosttyColor("#abc"), "#aabbcc");
  });
  it("X11 颜色名（含与 CSS 不同的四个）与 grayN", () => {
    assert.equal(parseGhosttyColor("red"), "#ff0000");
    assert.equal(parseGhosttyColor("Light Blue"), "#add8e6");
    assert.equal(parseGhosttyColor("green"), "#00ff00");
    assert.equal(parseGhosttyColor("gray"), "#bebebe");
    assert.equal(parseGhosttyColor("gray50"), "#7f7f7f");
    assert.equal(parseGhosttyColor("grey100"), "#ffffff");
  });
  it("认不出 undefined", () => {
    assert.equal(parseGhosttyColor("cell-foreground"), undefined);
    assert.equal(parseGhosttyColor("#12345"), undefined);
    assert.equal(parseGhosttyColor(""), undefined);
  });
});

describe("parseGhosttyTheme", () => {
  it("完整内置主题：22 条全认", () => {
    const src = parseGhosttyTheme(MOCHA);
    assert.equal(src.recognized, 22);
    assert.equal(src.background, "#1e1e2e");
    assert.equal(src.selectionForeground, "#cdd6f4");
    assert.equal(src.palette[0], "#45475a");
    assert.equal(src.palette[15], "#bac2de");
  });
  it("注释、空行、无关键、CRLF、大小写键、无 # 的值", () => {
    const src = parseGhosttyTheme(
      "# 主题\r\n\r\nfont-size = 14\r\nBackground = 282c34\r\nkeybind = cmd+t=new_tab\r\npalette=1 = ff0000\r\n"
    );
    assert.equal(src.recognized, 2);
    assert.equal(src.background, "#282c34");
    assert.equal(src.palette[1], "#ff0000");
  });
  it("cell-foreground / cell-background 特殊值", () => {
    const src = parseGhosttyTheme("cursor-color = cell-foreground\nselection-foreground = Cell-Background\n");
    assert.equal(src.cursorColor, "cell-foreground");
    assert.equal(src.selectionForeground, "cell-background");
    assert.equal(src.recognized, 2);
    // background 不接受特殊值
    assert.equal(parseGhosttyTheme("background = cell-foreground").recognized, 0);
  });
  it("空值 = 恢复默认（去掉之前写的）", () => {
    const src = parseGhosttyTheme("background = #000000\nbackground =\n");
    assert.equal(src.background, undefined);
  });
  it("theme 行与 256 色调色板", () => {
    const src = parseGhosttyTheme("theme = Dracula\npalette = 232=#080808\npalette = 300=#ffffff\n");
    assert.equal(src.theme, "Dracula");
    assert.equal(src.palette[232], "#080808");
    assert.equal(src.palette[300], undefined);
  });
  it("什么都没有 recognized = 0", () => {
    assert.equal(parseGhosttyTheme("hello\n").recognized, 0);
    assert.equal(parseGhosttyTheme("").recognized, 0);
  });
});

describe("parseThemeSetting", () => {
  it("单名 / 双槽", () => {
    assert.deepEqual(parseThemeSetting("Dracula"), { single: "Dracula" });
    assert.deepEqual(parseThemeSetting("light:Catppuccin Latte,dark:Catppuccin Mocha"), {
      light: "Catppuccin Latte",
      dark: "Catppuccin Mocha",
    });
    assert.deepEqual(parseThemeSetting(" dark: Nord , light:Nord Light "), { light: "Nord Light", dark: "Nord" });
    assert.deepEqual(parseThemeSetting(""), {});
  });
});

describe("resolveThemeColors", () => {
  it("完整主题原样", () => {
    const c = resolveThemeColors(parseGhosttyTheme(MOCHA));
    assert.equal(c.background, "#1e1e2e");
    assert.equal(c.cursorColor, "#f5e0dc");
    assert.equal(c.selectionForeground, "#cdd6f4");
    assert.equal(c.palette.length, 16);
    assert.equal(c.extended, undefined);
  });
  it("缺省照 Ghostty：光标 = 前景、光标下字 = 背景、选区 = 前后景互换", () => {
    const c = resolveThemeColors(parseGhosttyTheme("background = #101010\nforeground = #e0e0e0\n"));
    assert.equal(c.cursorColor, "#e0e0e0");
    assert.equal(c.cursorText, "#101010");
    assert.equal(c.selectionBackground, "#e0e0e0");
    assert.equal(c.selectionForeground, "#101010");
    assert.deepEqual(c.palette, GHOSTTY_DEFAULT_PALETTE);
  });
  it("什么都没写就是 Ghostty 默认", () => {
    const c = resolveThemeColors(parseGhosttyTheme(""));
    assert.equal(c.background, GHOSTTY_DEFAULT_BACKGROUND);
    assert.equal(c.foreground, GHOSTTY_DEFAULT_FOREGROUND);
  });
  it("特殊值按最终前后景解析；selection-foreground = cell-foreground → null", () => {
    const c = resolveThemeColors(
      parseGhosttyTheme(
        "background = #000000\nforeground = #ffffff\ncursor-color = cell-background\ncursor-text = cell-foreground\nselection-background = cell-foreground\nselection-foreground = cell-foreground\n"
      )
    );
    assert.equal(c.cursorColor, "#000000");
    assert.equal(c.cursorText, "#ffffff");
    assert.equal(c.selectionBackground, "#ffffff");
    assert.equal(c.selectionForeground, null);
  });
  it("以内置主题为底再覆盖：只换 background，光标沿用底的显式值", () => {
    const base = sourceFromColors(resolveThemeColors(parseGhosttyTheme(MOCHA)));
    const c = resolveThemeColors(parseGhosttyTheme("background = #000000\npalette = 1=#ff0000\n"), base);
    assert.equal(c.background, "#000000");
    assert.equal(c.foreground, "#cdd6f4");
    assert.equal(c.cursorColor, "#f5e0dc");
    assert.equal(c.palette[1], "#ff0000");
    assert.equal(c.palette[2], "#a6e3a1");
  });
  it("16 以上进 extended", () => {
    const c = resolveThemeColors(parseGhosttyTheme("palette = 16=#000000\npalette = 255=#eeeeee\n"));
    assert.deepEqual(c.extended, { 16: "#000000", 255: "#eeeeee" });
  });
});

describe("serializeGhosttyTheme", () => {
  it("解析 → 序列化 → 解析恒等，键序与内置文件一致", () => {
    const colors = resolveThemeColors(parseGhosttyTheme(MOCHA));
    const text = serializeGhosttyTheme(colors);
    assert.equal(text, MOCHA);
    assert.deepEqual(resolveThemeColors(parseGhosttyTheme(text)), colors);
  });
  it("selectionForeground null 写成 cell-foreground，extended 跟在 15 后面", () => {
    const colors: ThemeColors = {
      ...resolveThemeColors(parseGhosttyTheme(MOCHA)),
      selectionForeground: null,
      extended: { 232: "#080808", 16: "#000000" },
    };
    const text = serializeGhosttyTheme(colors);
    assert.match(text, /palette = 15=#bac2de\npalette = 16=#000000\npalette = 232=#080808\nbackground/);
    assert.match(text, /selection-foreground = cell-foreground\n$/);
    assert.deepEqual(resolveThemeColors(parseGhosttyTheme(text)), colors);
  });
});
