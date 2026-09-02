/**
 * 主题偏好：明暗模式 + 浅色槽位 + 深色槽位。
 *
 * 模型照 Ghostty 的 `theme = light:X,dark:Y`：两个槽位各放一套主题，明暗模式
 * （跟随系统 / 浅色 / 深色）决定此刻用哪个槽位。槽位里存的是主题**完整颜色的
 * 副本**而不只是名字——启动时不用等 77KB 的目录 chunk 就能画对界面，内置目录
 * 升级改了颜色也不会在用户没动过设置时悄悄换脸。
 *
 * 一个槽位放什么样的主题不设限：给"浅色"槽位选一套深底主题也行，界面按底色
 * 亮度判深浅（derive.ts），不按槽位。
 *
 * localStorage 的读写都走注入的 storage，纯函数可测；隐私模式读不到就用默认。
 */

import { isHexColor, normalizeHex } from "./color.js";
import { FALCON_DARK, FALCON_LIGHT, type CatalogEntry } from "./catalog.js";
import type { ThemeColors } from "./ghostty.js";

export type ThemeMode = "light" | "dark";
/** 明暗模式偏好 */
export type ThemePref = "system" | ThemeMode;

export interface ThemeChoice {
  name: string;
  /** builtin = 目录里的（Falcon / Ghostty 内置），custom = 用户贴的 Ghostty 文本 */
  kind: "builtin" | "custom";
  colors: ThemeColors;
}

export interface ThemeSettings {
  mode: ThemePref;
  light: ThemeChoice;
  dark: ThemeChoice;
}

export const THEMES_KEY = "falcon.themes";
/** 旧版只存明暗模式的字符串 */
const LEGACY_MODE_KEYS = ["falcon.theme", "mojito.theme"];
/** 旧版终端偏好，themeId 字段是旧的终端配色 id */
const LEGACY_TERM_KEYS = ["falcon.term", "mojito.term"];

export function choiceOf(entry: CatalogEntry): ThemeChoice {
  return { name: entry.name, kind: "builtin", colors: entry.colors };
}

export const DEFAULT_THEME_SETTINGS: ThemeSettings = {
  mode: "system",
  light: choiceOf(FALCON_LIGHT),
  dark: choiceOf(FALCON_DARK),
};

/**
 * 旧版终端配色 id → Ghostty 内置主题名。Campbell 与 Light+ 在 Ghostty 里没有
 * 对应，落回 Falcon 默认（Light+ 本来就是 Falcon Light 的 ANSI）。
 */
export const LEGACY_TERM_THEME_NAMES: Record<string, { slot: ThemeMode; name: string }> = {
  "one-dark": { slot: "dark", name: "Atom One Dark" },
  dracula: { slot: "dark", name: "Dracula" },
  nord: { slot: "dark", name: "Nord" },
  "tokyo-night": { slot: "dark", name: "TokyoNight" },
  "catppuccin-mocha": { slot: "dark", name: "Catppuccin Mocha" },
  "gruvbox-dark": { slot: "dark", name: "Gruvbox Dark" },
  "solarized-dark": { slot: "dark", name: "iTerm2 Solarized Dark" },
  "github-dark": { slot: "dark", name: "GitHub Dark Default" },
  monokai: { slot: "dark", name: "Monokai Classic" },
  "solarized-light": { slot: "light", name: "iTerm2 Solarized Light" },
  "catppuccin-latte": { slot: "light", name: "Catppuccin Latte" },
  "github-light": { slot: "light", name: "GitHub Light Default" },
  "gruvbox-light": { slot: "light", name: "Gruvbox Light" },
  "one-light": { slot: "light", name: "Atom One Light" },
};

export interface StorageLike {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

function hexOrNull(v: unknown): string | null {
  return typeof v === "string" && isHexColor(v) ? normalizeHex(v) : null;
}

/** 颜色副本必须整套合法，缺一个就整个不认——半套主题画出来比默认更糟 */
export function sanitizeThemeColors(raw: unknown): ThemeColors | undefined {
  if (!raw || typeof raw !== "object") return undefined;
  const r = raw as Record<string, unknown>;
  const background = hexOrNull(r.background);
  const foreground = hexOrNull(r.foreground);
  const cursorColor = hexOrNull(r.cursorColor);
  const cursorText = hexOrNull(r.cursorText);
  const selectionBackground = hexOrNull(r.selectionBackground);
  if (!background || !foreground || !cursorColor || !cursorText || !selectionBackground) return undefined;
  const selectionForeground = r.selectionForeground === null ? null : hexOrNull(r.selectionForeground);
  if (selectionForeground === undefined) return undefined;
  if (!Array.isArray(r.palette) || r.palette.length !== 16) return undefined;
  const palette: string[] = [];
  for (const c of r.palette) {
    const h = hexOrNull(c);
    if (!h) return undefined;
    palette.push(h);
  }
  const colors: ThemeColors = {
    background,
    foreground,
    cursorColor,
    cursorText,
    selectionBackground,
    selectionForeground,
    palette,
  };
  if (r.extended && typeof r.extended === "object") {
    const extended: Record<number, string> = {};
    for (const [k, v] of Object.entries(r.extended as Record<string, unknown>)) {
      const idx = Number(k);
      const h = hexOrNull(v);
      if (Number.isInteger(idx) && idx >= 16 && idx <= 255 && h) extended[idx] = h;
    }
    if (Object.keys(extended).length > 0) colors.extended = extended;
  }
  return colors;
}

export function sanitizeThemeChoice(raw: unknown, fallback: ThemeChoice): ThemeChoice {
  if (!raw || typeof raw !== "object") return fallback;
  const r = raw as Record<string, unknown>;
  const colors = sanitizeThemeColors(r.colors);
  if (!colors) return fallback;
  const name = typeof r.name === "string" && r.name.trim() ? r.name.trim().slice(0, 80) : fallback.name;
  return { name, kind: r.kind === "custom" ? "custom" : "builtin", colors };
}

export function sanitizeThemePref(raw: unknown): ThemePref {
  return raw === "light" || raw === "dark" || raw === "system" ? raw : "system";
}

export function sanitizeThemeSettings(raw: unknown): ThemeSettings {
  if (!raw || typeof raw !== "object") return { ...DEFAULT_THEME_SETTINGS };
  const r = raw as Record<string, unknown>;
  return {
    mode: sanitizeThemePref(r.mode),
    light: sanitizeThemeChoice(r.light, DEFAULT_THEME_SETTINGS.light),
    dark: sanitizeThemeChoice(r.dark, DEFAULT_THEME_SETTINGS.dark),
  };
}

export interface LoadedThemeSettings {
  settings: ThemeSettings;
  /** 旧版终端配色能对上 Ghostty 主题时给出，由调用方加载目录后异步补进槽位 */
  legacyTerm?: { slot: ThemeMode; name: string };
}

function readJson(storage: StorageLike, key: string): unknown {
  const raw = storage.getItem(key);
  if (!raw) return undefined;
  try {
    return JSON.parse(raw) as unknown;
  } catch {
    return undefined;
  }
}

/**
 * 读偏好；没有新格式时从旧的两个 key 迁移（明暗模式直接带过来，终端配色
 * 需要目录里的颜色，只返回线索）。
 */
export function loadThemeSettings(storage: StorageLike | undefined): LoadedThemeSettings {
  if (!storage) return { settings: { ...DEFAULT_THEME_SETTINGS } };
  try {
    const current = readJson(storage, THEMES_KEY);
    if (current) return { settings: sanitizeThemeSettings(current) };

    const settings: ThemeSettings = { ...DEFAULT_THEME_SETTINGS };
    for (const key of LEGACY_MODE_KEYS) {
      const mode = storage.getItem(key);
      if (mode === "light" || mode === "dark" || mode === "system") {
        settings.mode = mode;
        break;
      }
    }
    let legacyTerm: LoadedThemeSettings["legacyTerm"];
    for (const key of LEGACY_TERM_KEYS) {
      const term = readJson(storage, key);
      const id = term && typeof term === "object" ? (term as { themeId?: unknown }).themeId : undefined;
      if (typeof id === "string" && LEGACY_TERM_THEME_NAMES[id]) {
        legacyTerm = LEGACY_TERM_THEME_NAMES[id];
        break;
      }
    }
    return legacyTerm ? { settings, legacyTerm } : { settings };
  } catch {
    return { settings: { ...DEFAULT_THEME_SETTINGS } };
  }
}

export function saveThemeSettings(storage: StorageLike | undefined, settings: ThemeSettings): void {
  try {
    storage?.setItem(THEMES_KEY, JSON.stringify(settings));
  } catch {
    // 写不进去就只在本次页面生效
  }
}

export function resolveThemeMode(pref: ThemePref, systemDark: boolean): ThemeMode {
  if (pref !== "system") return pref;
  return systemDark ? "dark" : "light";
}

export function isDefaultThemeSettings(s: ThemeSettings): boolean {
  return (
    s.light.kind === "builtin" &&
    s.light.name === DEFAULT_THEME_SETTINGS.light.name &&
    s.dark.kind === "builtin" &&
    s.dark.name === DEFAULT_THEME_SETTINGS.dark.name
  );
}
