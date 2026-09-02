import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { FALCON_DARK, FALCON_LIGHT, FALCON_THEMES, findTheme, parseCatalogData } from "./catalog.js";
import { parseGhosttyTheme, resolveThemeColors, serializeGhosttyTheme } from "./ghostty.js";
import { GHOSTTY_THEMES_DATA } from "../../assets/themes/ghostty-themes.js";
import { GHOSTTY_THEMES_COUNT } from "../../assets/themes/ghostty-themes.meta.js";

describe("Ghostty 内置目录", () => {
  const entries = parseCatalogData(GHOSTTY_THEMES_DATA);

  it("条数与生成脚本记录一致，名字唯一且不与 Falcon 撞", () => {
    assert.equal(entries.length, GHOSTTY_THEMES_COUNT);
    const names = new Set(entries.map((e) => e.name));
    assert.equal(names.size, entries.length);
    for (const f of FALCON_THEMES) assert.ok(!names.has(f.name), f.name);
  });

  it("用户 Ghostty 配置里常见的名字都在", () => {
    for (const name of ["Catppuccin Mocha", "Catppuccin Latte", "Dracula", "Nord", "TokyoNight", "Gruvbox Dark"]) {
      assert.ok(findTheme(entries, name), name);
    }
  });

  it("Catppuccin Mocha 与 Ghostty 自带文件逐色一致", () => {
    const mocha = findTheme(entries, "Catppuccin Mocha")!;
    assert.equal(mocha.appearance, "dark");
    assert.equal(mocha.colors.background, "#1e1e2e");
    assert.equal(mocha.colors.foreground, "#cdd6f4");
    assert.equal(mocha.colors.cursorColor, "#f5e0dc");
    assert.equal(mocha.colors.selectionBackground, "#585b70");
    assert.equal(mocha.colors.palette[0], "#45475a");
    assert.equal(mocha.colors.palette[15], "#bac2de");
  });

  it("每条都能序列化成合法 Ghostty 主题再读回来", () => {
    for (const e of entries) {
      const back = resolveThemeColors(parseGhosttyTheme(serializeGhosttyTheme(e.colors)));
      assert.deepEqual(back, e.colors, e.name);
    }
  });

  it("findTheme 精确优先，其次忽略大小写", () => {
    assert.equal(findTheme(entries, "dracula")?.name, "Dracula");
    assert.equal(findTheme(entries, "  DRACULA ")?.name, "Dracula");
    assert.equal(findTheme(entries, "nope"), undefined);
  });

  it("坏数据直接抛", () => {
    assert.throws(() => parseCatalogData("Bad\t000000"));
    assert.throws(() => parseCatalogData("no tab"));
  });
});

describe("Falcon 默认主题", () => {
  it("深浅各一套，与旧版跟随界面的底 / 字一致", () => {
    assert.equal(FALCON_DARK.appearance, "dark");
    assert.equal(FALCON_LIGHT.appearance, "light");
    assert.equal(FALCON_DARK.colors.background, "#0a0a0a");
    assert.equal(FALCON_DARK.colors.foreground, "#fafafa");
    assert.equal(FALCON_LIGHT.colors.background, "#ffffff");
    assert.equal(FALCON_LIGHT.colors.foreground, "#171717");
    assert.equal(FALCON_LIGHT.colors.palette.length, 16);
  });
});
