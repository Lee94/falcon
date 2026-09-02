import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { FALCON_DARK, FALCON_LIGHT } from "./catalog.js";
import {
  DEFAULT_THEME_SETTINGS,
  THEMES_KEY,
  choiceOf,
  isDefaultThemeSettings,
  loadThemeSettings,
  resolveThemeMode,
  sanitizeThemeChoice,
  sanitizeThemeColors,
  sanitizeThemeSettings,
  saveThemeSettings,
  type StorageLike,
} from "./pref.js";

function memStorage(init: Record<string, string> = {}): StorageLike & { data: Record<string, string> } {
  const data = { ...init };
  return {
    data,
    getItem: (k) => (k in data ? data[k]! : null),
    setItem: (k, v) => {
      data[k] = v;
    },
  };
}

describe("sanitizeThemeColors", () => {
  it("整套合法才认，规范成小写六位", () => {
    const c = sanitizeThemeColors({ ...FALCON_DARK.colors, background: "#0A0A0A" });
    assert.equal(c?.background, "#0a0a0a");
    assert.equal(c?.selectionForeground, null);
  });
  it("缺任何一项 / 调色板不是 16 个 / 非法色 → undefined", () => {
    assert.equal(sanitizeThemeColors({ ...FALCON_DARK.colors, cursorText: undefined }), undefined);
    assert.equal(sanitizeThemeColors({ ...FALCON_DARK.colors, palette: ["#000"] }), undefined);
    assert.equal(sanitizeThemeColors({ ...FALCON_DARK.colors, foreground: "red" }), undefined);
    assert.equal(sanitizeThemeColors(null), undefined);
  });
  it("extended 只收 16–255 的合法项，空了就不带", () => {
    const c = sanitizeThemeColors({ ...FALCON_DARK.colors, extended: { 16: "#123456", 3: "#000000", 300: "#ffffff", 20: "x" } });
    assert.deepEqual(c?.extended, { 16: "#123456" });
    assert.equal(sanitizeThemeColors({ ...FALCON_DARK.colors, extended: { 3: "#000000" } })?.extended, undefined);
  });
});

describe("sanitizeThemeChoice / sanitizeThemeSettings", () => {
  it("坏的 choice 回 fallback；名字截 80", () => {
    const fb = choiceOf(FALCON_LIGHT);
    assert.equal(sanitizeThemeChoice({ name: "x", colors: {} }, fb), fb);
    const long = sanitizeThemeChoice({ name: "a".repeat(100), kind: "custom", colors: FALCON_DARK.colors }, fb);
    assert.equal(long.name.length, 80);
    assert.equal(long.kind, "custom");
    assert.equal(sanitizeThemeChoice({ name: "y", kind: "weird", colors: FALCON_DARK.colors }, fb).kind, "builtin");
  });
  it("settings 缺字段逐项回默认", () => {
    const s = sanitizeThemeSettings({ mode: "dark", light: null });
    assert.equal(s.mode, "dark");
    assert.equal(s.light.name, DEFAULT_THEME_SETTINGS.light.name);
    assert.equal(s.dark.name, DEFAULT_THEME_SETTINGS.dark.name);
    assert.equal(sanitizeThemeSettings("junk").mode, "system");
  });
});

describe("loadThemeSettings", () => {
  it("没有 storage 用默认", () => {
    assert.deepEqual(loadThemeSettings(undefined).settings, DEFAULT_THEME_SETTINGS);
  });
  it("新格式直接读", () => {
    const st = memStorage();
    saveThemeSettings(st, { mode: "light", light: choiceOf(FALCON_LIGHT), dark: { ...choiceOf(FALCON_DARK), name: "X", kind: "custom" } });
    const { settings, legacyTerm } = loadThemeSettings(st);
    assert.equal(settings.mode, "light");
    assert.equal(settings.dark.name, "X");
    assert.equal(settings.dark.kind, "custom");
    assert.equal(legacyTerm, undefined);
  });
  it("旧格式：明暗模式带过来，终端配色给出目录线索", () => {
    const st = memStorage({
      "falcon.theme": "dark",
      "falcon.term": JSON.stringify({ fontId: "maple", themeId: "catppuccin-mocha" }),
    });
    const { settings, legacyTerm } = loadThemeSettings(st);
    assert.equal(settings.mode, "dark");
    assert.equal(settings.dark.name, DEFAULT_THEME_SETTINGS.dark.name);
    assert.deepEqual(legacyTerm, { slot: "dark", name: "Catppuccin Mocha" });
  });
  it("旧格式 mojito.* 也认；跟随界面 / 没对应的 id 没有线索", () => {
    const st = memStorage({ "mojito.theme": "light", "mojito.term": JSON.stringify({ themeId: "match" }) });
    const { settings, legacyTerm } = loadThemeSettings(st);
    assert.equal(settings.mode, "light");
    assert.equal(legacyTerm, undefined);
    assert.equal(loadThemeSettings(memStorage({ "falcon.term": JSON.stringify({ themeId: "campbell" }) })).legacyTerm, undefined);
  });
  it("坏 JSON 用默认", () => {
    assert.equal(loadThemeSettings(memStorage({ [THEMES_KEY]: "{oops" })).settings.mode, "system");
  });
  it("storage 抛异常也不炸", () => {
    const st: StorageLike = {
      getItem: () => {
        throw new Error("SecurityError");
      },
      setItem: () => {
        throw new Error("SecurityError");
      },
    };
    assert.equal(loadThemeSettings(st).settings.mode, "system");
    saveThemeSettings(st, DEFAULT_THEME_SETTINGS);
  });
});

describe("resolveThemeMode / isDefaultThemeSettings", () => {
  it("system 跟系统，其余固定", () => {
    assert.equal(resolveThemeMode("system", true), "dark");
    assert.equal(resolveThemeMode("system", false), "light");
    assert.equal(resolveThemeMode("light", true), "light");
    assert.equal(resolveThemeMode("dark", false), "dark");
  });
  it("默认判定只看两个槽位，不看明暗模式", () => {
    assert.ok(isDefaultThemeSettings({ ...DEFAULT_THEME_SETTINGS, mode: "dark" }));
    assert.ok(!isDefaultThemeSettings({ ...DEFAULT_THEME_SETTINGS, dark: { ...choiceOf(FALCON_DARK), name: "Dracula" } }));
  });
});
