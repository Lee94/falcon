/**
 * Ghostty 主题文件格式：解析、补默认值、序列化。纯函数，零 DOM。
 *
 * 主题就是一段 Ghostty 配置，只认这几个键（其余键原样忽略，所以整份
 * ~/.config/ghostty/config 贴进来也能用）：
 *
 *   background / foreground                 = #rrggbb | rrggbb | X11 颜色名
 *   cursor-color / cursor-text              = 同上 | cell-foreground | cell-background
 *   selection-background / -foreground      = 同上 | cell-foreground | cell-background
 *   palette                                 = N=颜色   （N 0–255，可重复出现）
 *   theme                                   = 名字 | light:名字,dark:名字（先以它为底再覆盖）
 *
 * 缺省语义照 Ghostty 文档：cursor-color 缺省用前景色、cursor-text 缺省用背景色；
 * selection-* 缺省是"窗口前景 / 背景互换"（不是格子的）；selection-foreground 写
 * cell-foreground 表示选中文字保留原色，这是 xterm.js 不给 selectionForeground 的
 * 那种效果，所以 ThemeColors 里用 null 表示。
 *
 * 颜色名按 X11 rgb.txt（Ghostty 认的是这张表），与 CSS 名字有四处不一样：
 * green / gray / maroon / purple，这里取 X11 的值；gray0–gray100 按公式生成。
 */

export interface ThemeColors {
  background: string;
  foreground: string;
  cursorColor: string;
  cursorText: string;
  selectionBackground: string;
  /** null = 选中文字保留原字色（Ghostty 的 cell-foreground） */
  selectionForeground: string | null;
  /** ANSI 0–15，全是 #rrggbb */
  palette: string[];
  /** 16–255 的覆盖。Ghostty 允许，内置主题从来不写，用户主题偶尔有 */
  extended?: Record<number, string>;
}

export type CellRef = "cell-foreground" | "cell-background";

/** 一段 Ghostty 文本里写了什么。没写的键就是 undefined，交给 resolveThemeColors 补。 */
export interface GhosttyThemeSource {
  theme?: string;
  background?: string;
  foreground?: string;
  cursorColor?: string | CellRef;
  cursorText?: string | CellRef;
  selectionBackground?: string | CellRef;
  selectionForeground?: string | CellRef;
  palette: Record<number, string>;
  /** 识别出的颜色赋值条数（palette 每条算一条）。0 = 这段文字里根本没有主题 */
  recognized: number;
}

/**
 * Ghostty 自己的默认配色（`ghostty +show-config --default`，1.3.1），
 * 只在用户贴的主题缺键时兜底。
 */
export const GHOSTTY_DEFAULT_BACKGROUND = "#282c34";
export const GHOSTTY_DEFAULT_FOREGROUND = "#ffffff";
export const GHOSTTY_DEFAULT_PALETTE: readonly string[] = [
  "#1d1f21",
  "#cc6666",
  "#b5bd68",
  "#f0c674",
  "#81a2be",
  "#b294bb",
  "#8abeb7",
  "#c5c8c6",
  "#666666",
  "#d54e53",
  "#b9ca4a",
  "#e7c547",
  "#7aa6da",
  "#c397d8",
  "#70c0b1",
  "#eaeaea",
];

const X11_NAMES =
  "aliceblue f0f8ff antiquewhite faebd7 aqua 00ffff aquamarine 7fffd4 azure f0ffff beige f5f5dc " +
  "bisque ffe4c4 black 000000 blanchedalmond ffebcd blue 0000ff blueviolet 8a2be2 brown a52a2a " +
  "burlywood deb887 cadetblue 5f9ea0 chartreuse 7fff00 chocolate d2691e coral ff7f50 " +
  "cornflowerblue 6495ed cornsilk fff8dc crimson dc143c cyan 00ffff darkblue 00008b darkcyan 008b8b " +
  "darkgoldenrod b8860b darkgray a9a9a9 darkgreen 006400 darkgrey a9a9a9 darkkhaki bdb76b " +
  "darkmagenta 8b008b darkolivegreen 556b2f darkorange ff8c00 darkorchid 9932cc darkred 8b0000 " +
  "darksalmon e9967a darkseagreen 8fbc8f darkslateblue 483d8b darkslategray 2f4f4f " +
  "darkslategrey 2f4f4f darkturquoise 00ced1 darkviolet 9400d3 deeppink ff1493 deepskyblue 00bfff " +
  "dimgray 696969 dimgrey 696969 dodgerblue 1e90ff firebrick b22222 floralwhite fffaf0 " +
  "forestgreen 228b22 fuchsia ff00ff gainsboro dcdcdc ghostwhite f8f8ff gold ffd700 goldenrod daa520 " +
  "gray bebebe green 00ff00 greenyellow adff2f grey bebebe honeydew f0fff0 hotpink ff69b4 " +
  "indianred cd5c5c indigo 4b0082 ivory fffff0 khaki f0e68c lavender e6e6fa lavenderblush fff0f5 " +
  "lawngreen 7cfc00 lemonchiffon fffacd lightblue add8e6 lightcoral f08080 lightcyan e0ffff " +
  "lightgoldenrodyellow fafad2 lightgray d3d3d3 lightgreen 90ee90 lightgrey d3d3d3 lightpink ffb6c1 " +
  "lightsalmon ffa07a lightseagreen 20b2aa lightskyblue 87cefa lightslategray 778899 " +
  "lightslategrey 778899 lightsteelblue b0c4de lightyellow ffffe0 lime 00ff00 limegreen 32cd32 " +
  "linen faf0e6 magenta ff00ff maroon b03060 mediumaquamarine 66cdaa mediumblue 0000cd " +
  "mediumorchid ba55d3 mediumpurple 9370db mediumseagreen 3cb371 mediumslateblue 7b68ee " +
  "mediumspringgreen 00fa9a mediumturquoise 48d1cc mediumvioletred c71585 midnightblue 191970 " +
  "mintcream f5fffa mistyrose ffe4e1 moccasin ffe4b5 navajowhite ffdead navy 000080 navyblue 000080 " +
  "oldlace fdf5e6 olive 808000 olivedrab 6b8e23 orange ffa500 orangered ff4500 orchid da70d6 " +
  "palegoldenrod eee8aa palegreen 98fb98 paleturquoise afeeee palevioletred db7093 papayawhip ffefd5 " +
  "peachpuff ffdab9 peru cd853f pink ffc0cb plum dda0dd powderblue b0e0e6 purple a020f0 " +
  "rebeccapurple 663399 red ff0000 rosybrown bc8f8f royalblue 4169e1 saddlebrown 8b4513 " +
  "salmon fa8072 sandybrown f4a460 seagreen 2e8b57 seashell fff5ee sienna a0522d silver c0c0c0 " +
  "skyblue 87ceeb slateblue 6a5acd slategray 708090 slategrey 708090 snow fffafa springgreen 00ff7f " +
  "steelblue 4682b4 tan d2b48c teal 008080 thistle d8bfd8 tomato ff6347 turquoise 40e0d0 " +
  "violet ee82ee webgray 808080 webgreen 008000 webgrey 808080 webmaroon 800000 webpurple 800080 " +
  "wheat f5deb3 white ffffff whitesmoke f5f5f5 yellow ffff00 yellowgreen 9acd32";

let namedColors: Map<string, string> | null = null;

function namedColor(name: string): string | undefined {
  if (!namedColors) {
    namedColors = new Map();
    const parts = X11_NAMES.split(" ");
    for (let i = 0; i + 1 < parts.length; i += 2) namedColors.set(parts[i]!, `#${parts[i + 1]}`);
    for (let n = 0; n <= 100; n++) {
      const v = Math.round((n * 255) / 100 - 1e-9)
        .toString(16)
        .padStart(2, "0");
      namedColors.set(`gray${n}`, `#${v}${v}${v}`);
      namedColors.set(`grey${n}`, `#${v}${v}${v}`);
    }
  }
  return namedColors.get(name.toLowerCase().replace(/[\s_-]/g, ""));
}

const HEX6 = /^#?([\da-f]{6})$/i;
const HEX3 = /^#?([\da-f]{3})$/i;

/** 一个 Ghostty 颜色值 → 小写 #rrggbb；认不出返回 undefined。 */
export function parseGhosttyColor(value: string): string | undefined {
  const v = unquote(value);
  const m6 = HEX6.exec(v);
  if (m6) return `#${m6[1]!.toLowerCase()}`;
  // Ghostty 不认 3 位缩写，但用户从 CSS 那边搬来的主题偶尔这么写，宽容一点
  const m3 = HEX3.exec(v);
  if (m3) {
    const h = m3[1]!.toLowerCase();
    return `#${h[0]}${h[0]}${h[1]}${h[1]}${h[2]}${h[2]}`;
  }
  return namedColor(v);
}

function isCellRef(v: string): v is CellRef {
  return v === "cell-foreground" || v === "cell-background";
}

function unquote(s: string): string {
  const t = s.trim();
  return t.length >= 2 && t.startsWith('"') && t.endsWith('"') ? t.slice(1, -1).trim() : t;
}

const COLOR_KEYS: Record<
  string,
  "background" | "foreground" | "cursorColor" | "cursorText" | "selectionBackground" | "selectionForeground"
> = {
  background: "background",
  foreground: "foreground",
  "cursor-color": "cursorColor",
  "cursor-text": "cursorText",
  "selection-background": "selectionBackground",
  "selection-foreground": "selectionForeground",
};

const CELL_REF_KEYS = new Set(["cursorColor", "cursorText", "selectionBackground", "selectionForeground"]);

/**
 * 解析一段 Ghostty 配置。认不出的行静默跳过（这是"贴整份 config 也能用"的代价），
 * 空值（`key =`）按 Ghostty 语义视为恢复默认，即从结果里去掉。
 */
export function parseGhosttyTheme(text: string): GhosttyThemeSource {
  const src: GhosttyThemeSource = { palette: {}, recognized: 0 };
  for (const raw of text.split(/\r?\n/)) {
    const line = raw.trim();
    if (!line || line.startsWith("#")) continue;
    const eq = line.indexOf("=");
    if (eq < 0) continue;
    const key = line.slice(0, eq).trim().toLowerCase();
    const value = unquote(line.slice(eq + 1));
    if (key === "theme") {
      src.theme = value || undefined;
      continue;
    }
    if (key === "palette") {
      const m = /^(\d{1,3})\s*=\s*(.+)$/.exec(value);
      if (!m) continue;
      const idx = Number(m[1]);
      if (idx > 255) continue;
      const color = parseGhosttyColor(m[2]!);
      if (!color) continue;
      src.palette[idx] = color;
      src.recognized++;
      continue;
    }
    const field = COLOR_KEYS[key];
    if (!field) continue;
    if (!value) {
      delete src[field];
      continue;
    }
    const lowered = value.toLowerCase();
    if (CELL_REF_KEYS.has(field) && isCellRef(lowered)) {
      src[field] = lowered;
      src.recognized++;
      continue;
    }
    const color = parseGhosttyColor(value);
    if (!color) continue;
    src[field] = color;
    src.recognized++;
  }
  return src;
}

/** `theme = X` 的值：单个名字，或 `light:X,dark:Y`（顺序、空格、只写一边都允许） */
export function parseThemeSetting(value: string): { light?: string; dark?: string; single?: string } {
  const v = unquote(value);
  if (!v.includes(":")) return v ? { single: v } : {};
  const out: { light?: string; dark?: string } = {};
  for (const part of v.split(",")) {
    const colon = part.indexOf(":");
    if (colon < 0) continue;
    const k = part.slice(0, colon).trim().toLowerCase();
    const name = part.slice(colon + 1).trim();
    if (!name) continue;
    if (k === "light") out.light = name;
    else if (k === "dark") out.dark = name;
  }
  return out;
}

/** 把一套完整颜色当作"底"：所有键都算显式写了，供 `theme = X` 再覆盖 */
export function sourceFromColors(colors: ThemeColors): GhosttyThemeSource {
  const palette: Record<number, string> = {};
  colors.palette.forEach((c, i) => {
    palette[i] = c;
  });
  for (const [k, v] of Object.entries(colors.extended ?? {})) palette[Number(k)] = v;
  return {
    background: colors.background,
    foreground: colors.foreground,
    cursorColor: colors.cursorColor,
    cursorText: colors.cursorText,
    selectionBackground: colors.selectionBackground,
    selectionForeground: colors.selectionForeground ?? "cell-foreground",
    palette,
    recognized: 6 + Object.keys(palette).length,
  };
}

function resolveRef(v: string | CellRef | undefined, fg: string, bg: string, fallback: string): string {
  if (v === undefined) return fallback;
  if (v === "cell-foreground") return fg;
  if (v === "cell-background") return bg;
  return v;
}

/**
 * 补齐成一套完整颜色。src 覆盖 base，base 缺省是 Ghostty 自己的默认值。
 * 缺省推导用的是**最终**的前景 / 背景：只写了 background / foreground 的主题，
 * 光标与选区要跟着新的字底走，而不是继承 base 的。
 */
export function resolveThemeColors(src: GhosttyThemeSource, base?: GhosttyThemeSource): ThemeColors {
  const merged: GhosttyThemeSource = {
    ...base,
    palette: { ...base?.palette, ...src.palette },
    recognized: (base?.recognized ?? 0) + src.recognized,
  };
  for (const field of ["background", "foreground", "cursorColor", "cursorText", "selectionBackground", "selectionForeground"] as const) {
    if (src[field] !== undefined) merged[field] = src[field];
  }
  const bg = merged.background ?? GHOSTTY_DEFAULT_BACKGROUND;
  const fg = merged.foreground ?? GHOSTTY_DEFAULT_FOREGROUND;
  const palette = GHOSTTY_DEFAULT_PALETTE.map((c, i) => merged.palette[i] ?? c);
  let extended: Record<number, string> | undefined;
  for (const [k, v] of Object.entries(merged.palette)) {
    const idx = Number(k);
    if (idx < 16) continue;
    (extended ??= {})[idx] = v;
  }
  const selectionForeground =
    merged.selectionForeground === "cell-foreground"
      ? null
      : resolveRef(merged.selectionForeground, fg, bg, bg);
  const colors: ThemeColors = {
    background: bg,
    foreground: fg,
    cursorColor: resolveRef(merged.cursorColor, fg, bg, fg),
    cursorText: resolveRef(merged.cursorText, fg, bg, bg),
    selectionBackground: resolveRef(merged.selectionBackground, fg, bg, fg),
    selectionForeground,
    palette,
  };
  if (extended) colors.extended = extended;
  return colors;
}

/**
 * 回写成 Ghostty 主题文件，键序与 Ghostty 内置主题一致（palette 在前）。
 * 解析 → 序列化 → 解析是恒等的，测试钉住这一点。
 */
export function serializeGhosttyTheme(colors: ThemeColors): string {
  const lines: string[] = [];
  colors.palette.forEach((c, i) => lines.push(`palette = ${i}=${c}`));
  for (const idx of Object.keys(colors.extended ?? {})
    .map(Number)
    .sort((a, b) => a - b)) {
    lines.push(`palette = ${idx}=${colors.extended![idx]}`);
  }
  lines.push(`background = ${colors.background}`);
  lines.push(`foreground = ${colors.foreground}`);
  lines.push(`cursor-color = ${colors.cursorColor}`);
  lines.push(`cursor-text = ${colors.cursorText}`);
  lines.push(`selection-background = ${colors.selectionBackground}`);
  lines.push(`selection-foreground = ${colors.selectionForeground ?? "cell-foreground"}`);
  return `${lines.join("\n")}\n`;
}
