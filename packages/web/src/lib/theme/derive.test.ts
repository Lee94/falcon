import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { contrast, parseHex, perceptualLightness } from "./color.js";
import { FALCON_DARK, FALCON_LIGHT, parseCatalogData } from "./catalog.js";
import { deriveTheme, extendedAnsi } from "./derive.js";
import { parseGhosttyTheme, resolveThemeColors } from "./ghostty.js";
import { GHOSTTY_THEMES_DATA } from "../../assets/themes/ghostty-themes.js";

const NOCTIS_LUX = resolveThemeColors(
  parseGhosttyTheme(`background = #f6edda
foreground = #005661
selection-background = #d4e8e2
selection-foreground = #005661
cursor-color = #005661
cursor-text = #f6edda
palette = 0=#003b42
palette = 1=#e34e1c
palette = 2=#00b368
palette = 3=#f49725
palette = 4=#0094f0
palette = 5=#ff5792
palette = 6=#00bdd6
palette = 7=#8ca6a6
palette = 8=#004d57
palette = 9=#ff4000
palette = 10=#00d17a
palette = 11=#ff8c00
palette = 12=#0fa3ff
palette = 13=#ff6b9f
palette = 14=#00cbe6
palette = 15=#bbc3c4
`)
);

const UI_TEXT_VARS = ["--muted-foreground", "--destructive", "--success", "--warning"];

/** 语义色的可读性底线：3:1，但主题自己的字色都不到 3:1（C64 这类复古主题）时只要求不比字色差 */
function textFloor(colors: { background: string; foreground: string }): number {
  return Math.min(3, contrast(colors.foreground, colors.background)) - 1e-9;
}

describe("deriveTheme", () => {
  it("深浅按底色判，不按槽位", () => {
    assert.equal(deriveTheme(FALCON_DARK.colors).appearance, "dark");
    assert.equal(deriveTheme(FALCON_LIGHT.colors).appearance, "light");
    assert.equal(deriveTheme(NOCTIS_LUX).appearance, "light");
  });

  it("Falcon 默认复现 shadcn neutral 的灰阶", () => {
    const light = deriveTheme(FALCON_LIGHT.colors).vars;
    const near = (hex: string, l: number) => Math.abs(perceptualLightness(hex) - l) < 0.012;
    assert.ok(near(light["--muted"]!, 0.97), light["--muted"]);
    assert.ok(near(light["--muted-foreground"]!, 0.556), light["--muted-foreground"]);
    assert.ok(near(light["--primary"]!, 0.205), light["--primary"]);
    assert.equal(light["--card"], "#ffffff");
    const dark = deriveTheme(FALCON_DARK.colors).vars;
    assert.ok(near(dark["--card"]!, 0.205), dark["--card"]);
    assert.ok(near(dark["--popover"]!, 0.269), dark["--popover"]);
    assert.ok(near(dark["--muted-foreground"]!, 0.708), dark["--muted-foreground"]);
    assert.ok(near(dark["--accent"]!, 0.371), dark["--accent"]);
    assert.equal(dark["--border"], "#fafafa1a");
  });

  it("语义色来自 ANSI，且在底色上至少 3:1", () => {
    const t = deriveTheme(FALCON_DARK.colors);
    // Tango 的普通红 #cc0000 在 #0a0a0a 上不到 4.5，亮红 #ef2929 够
    assert.equal(t.vars["--destructive"], "#ef2929");
    for (const key of UI_TEXT_VARS) {
      assert.ok(contrast(t.vars[key]!, t.colors.background) >= 3, `${key}=${t.vars[key]}`);
    }
    const lux = deriveTheme(NOCTIS_LUX);
    for (const key of UI_TEXT_VARS) {
      assert.ok(contrast(lux.vars[key]!, lux.colors.background) >= 3, `${key}=${lux.vars[key]}`);
    }
  });

  it("语义色的字色在语义色上选底 / 字里对比更高的", () => {
    const light = deriveTheme(FALCON_LIGHT.colors).vars;
    assert.equal(light["--destructive-foreground"], "#ffffff");
    const mocha = deriveTheme(
      parseCatalogData(GHOSTTY_THEMES_DATA).find((e) => e.name === "Catppuccin Mocha")!.colors
    ).vars;
    // 粉红 #f38ba8 上深底色更清楚
    assert.equal(mocha["--destructive-foreground"], "#1e1e2e");
  });

  it("每个 var 都是合法颜色或百分比，全部主题无一例外", () => {
    const entries = [FALCON_LIGHT, FALCON_DARK, ...parseCatalogData(GHOSTTY_THEMES_DATA)];
    for (const e of entries) {
      const t = deriveTheme(e.colors);
      for (const [k, v] of Object.entries(t.vars)) {
        const ok = parseHex(v) !== undefined || /^\d+%$/.test(v) || v === "currentcolor";
        assert.ok(ok, `${e.name} ${k}=${v}`);
      }
      for (const key of UI_TEXT_VARS) {
        assert.ok(contrast(t.vars[key]!, e.colors.background) >= textFloor(e.colors), `${e.name} ${key}=${t.vars[key]}`);
      }
      assert.equal(t.appearance, e.appearance);
    }
  });

  it("xterm ITheme 逐字段映射；选区保留原字色时 selectionForeground 缺省", () => {
    const t = deriveTheme(FALCON_DARK.colors);
    assert.equal(t.xterm.background, "#0a0a0a");
    assert.equal(t.xterm.cursor, "#fafafa");
    assert.equal(t.xterm.cursorAccent, "#0a0a0a");
    assert.equal(t.xterm.selectionBackground, "#3b3b3b");
    assert.equal(t.xterm.selectionForeground, undefined);
    assert.equal(t.xterm.black, "#2e3436");
    assert.equal(t.xterm.brightWhite, "#eeeeec");
    assert.equal(t.xterm.extendedAnsi, undefined);
    const lux = deriveTheme(NOCTIS_LUX);
    assert.equal(lux.xterm.selectionForeground, "#005661");
  });

  it("hint 就是深浅 + 底字", () => {
    assert.deepEqual(deriveTheme(FALCON_LIGHT.colors).hint, {
      appearance: "light",
      background: "#ffffff",
      foreground: "#171717",
    });
  });

  it("shiki 变量齐全", () => {
    const v = deriveTheme(FALCON_DARK.colors).vars;
    for (const name of [
      "comment",
      "keyword",
      "string",
      "string-expression",
      "constant",
      "function",
      "parameter",
      "punctuation",
      "link",
      "inserted",
      "deleted",
      "changed",
    ]) {
      assert.ok(v[`--shiki-token-${name}`], name);
    }
    assert.equal(v["--shiki-ansi-bright-white"], "#eeeeec");
  });
});

describe("extendedAnsi", () => {
  it("xterm 256 色标准立方与灰阶，与 Ghostty 默认一致", () => {
    const x = extendedAnsi({});
    assert.equal(x.length, 240);
    assert.equal(x[0], "#000000");
    assert.equal(x[1], "#00005f");
    assert.equal(x[21 - 16], "#0000ff");
    assert.equal(x[196 - 16], "#ff0000");
    assert.equal(x[231 - 16], "#ffffff");
    assert.equal(x[232 - 16], "#080808");
    assert.equal(x[255 - 16], "#eeeeee");
  });
  it("覆盖落到对应下标", () => {
    const x = extendedAnsi({ 16: "#123456", 255: "#abcdef", 3: "#000000" });
    assert.equal(x[0], "#123456");
    assert.equal(x[239], "#abcdef");
  });
  it("带 extended 的主题给 xterm 整份 extendedAnsi", () => {
    const t = deriveTheme({ ...FALCON_DARK.colors, extended: { 16: "#123456" } });
    assert.equal(t.xterm.extendedAnsi?.length, 240);
    assert.equal(t.xterm.extendedAnsi?.[0], "#123456");
  });
});
