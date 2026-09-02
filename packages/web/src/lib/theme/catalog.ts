/**
 * 内置主题目录：Falcon 自己的两套默认 + Ghostty 1.3.1 内置的 463 套。
 *
 * Ghostty 那份是 77KB 的生成模块（assets/themes/ghostty-themes.ts），只在打开
 * 主题选择器时才动态 import——偏好里存的是选中主题的完整颜色副本，启动时不需要
 * 目录就能把界面画对。名字与 `ghostty +list-themes` 一字不差，Ghostty 配置里
 * `theme = Catppuccin Mocha` 在这里查同一个名字就能命中。
 */

import { appearanceFromHex, type TermAppearance } from "@falcon/shared";
import type { ThemeColors } from "./ghostty.js";

export { GHOSTTY_THEMES_COUNT, GHOSTTY_THEMES_ORIGIN } from "../../assets/themes/ghostty-themes.meta.js";

export interface CatalogEntry {
  name: string;
  colors: ThemeColors;
  appearance: TermAppearance;
}

export const FALCON_LIGHT_NAME = "Falcon Light";
export const FALCON_DARK_NAME = "Falcon Dark";

/**
 * 默认深色：shadcn neutral 的底 / 字，ANSI 用 xterm.js 默认（Tango，本来就是给
 * 深底配的）。选区是以前的 #ffffff33 预混到底色上——Ghostty 格式没有 alpha。
 */
const FALCON_DARK_COLORS: ThemeColors = {
  background: "#0a0a0a",
  foreground: "#fafafa",
  cursorColor: "#fafafa",
  cursorText: "#0a0a0a",
  selectionBackground: "#3b3b3b",
  selectionForeground: null,
  palette: [
    "#2e3436",
    "#cc0000",
    "#4e9a06",
    "#c4a000",
    "#3465a4",
    "#75507b",
    "#06989a",
    "#d3d7cf",
    "#555753",
    "#ef2929",
    "#8ae234",
    "#fce94f",
    "#729fcf",
    "#ad7fa8",
    "#34e2e2",
    "#eeeeec",
  ],
};

/** 默认浅色：白底近黑字，ANSI 取 VS Code Light+——白底上公认可读的一套 */
const FALCON_LIGHT_COLORS: ThemeColors = {
  background: "#ffffff",
  foreground: "#171717",
  cursorColor: "#171717",
  cursorText: "#ffffff",
  selectionBackground: "#d9d9d9",
  selectionForeground: null,
  palette: [
    "#000000",
    "#cd3131",
    "#00bc00",
    "#949800",
    "#0451a5",
    "#bc05bc",
    "#0598bc",
    "#555555",
    "#666666",
    "#cd3131",
    "#14ce14",
    "#b5ba00",
    "#0451a5",
    "#bc05bc",
    "#0598bc",
    "#a5a5a5",
  ],
};

function entry(name: string, colors: ThemeColors): CatalogEntry {
  return { name, colors, appearance: appearanceFromHex(colors.background) };
}

export const FALCON_LIGHT: CatalogEntry = entry(FALCON_LIGHT_NAME, FALCON_LIGHT_COLORS);
export const FALCON_DARK: CatalogEntry = entry(FALCON_DARK_NAME, FALCON_DARK_COLORS);
export const FALCON_THEMES: readonly CatalogEntry[] = [FALCON_LIGHT, FALCON_DARK];

/**
 * 解析 vendor 脚本的产物：每行 `名字 \t 22 个 rrggbb`，顺序是
 * bg fg cursor cursorText selBg selFg palette0..15（与 scripts/vendor-ghostty-themes.mjs 的 KEYS 一致）。
 * 坏行直接抛：数据是构建期生成的，运行时遇到坏行只可能是产物被手改过。
 */
export function parseCatalogData(data: string): CatalogEntry[] {
  const out: CatalogEntry[] = [];
  for (const line of data.split("\n")) {
    if (!line) continue;
    const tab = line.indexOf("\t");
    if (tab < 0) throw new Error(`主题数据坏行：${line.slice(0, 40)}`);
    const name = line.slice(0, tab);
    const hexes = line.slice(tab + 1).split(" ");
    if (hexes.length !== 22 || hexes.some((h) => !/^[\da-f]{6}$/.test(h))) {
      throw new Error(`主题「${name}」颜色数不对`);
    }
    const c = hexes.map((h) => `#${h}`);
    out.push(
      entry(name, {
        background: c[0]!,
        foreground: c[1]!,
        cursorColor: c[2]!,
        cursorText: c[3]!,
        selectionBackground: c[4]!,
        selectionForeground: c[5]!,
        palette: c.slice(6),
      })
    );
  }
  return out;
}

/** 先精确匹配，再忽略大小写——Ghostty 自己只认精确文件名，宽松一档是给手打的 */
export function findTheme(entries: readonly CatalogEntry[], name: string): CatalogEntry | undefined {
  const exact = entries.find((e) => e.name === name);
  if (exact) return exact;
  const lower = name.trim().toLowerCase();
  return entries.find((e) => e.name.toLowerCase() === lower);
}

let catalogPromise: Promise<CatalogEntry[]> | null = null;
let catalogCache: CatalogEntry[] | null = null;

/** Falcon 两套 + Ghostty 全部。失败（chunk 拉不下来）允许下次重试。 */
export function loadCatalog(): Promise<CatalogEntry[]> {
  catalogPromise ??= import("../../assets/themes/ghostty-themes.js")
    .then((m) => {
      catalogCache = [...FALCON_THEMES, ...parseCatalogData(m.GHOSTTY_THEMES_DATA)];
      return catalogCache;
    })
    .catch((err) => {
      catalogPromise = null;
      throw err;
    });
  return catalogPromise;
}

/** 已经加载过就同步给，没有就 null（选择器首次打开前） */
export function cachedCatalog(): CatalogEntry[] | null {
  return catalogCache;
}
