/**
 * 应用图标（ADR 0018）：内置几套可选，也能上传一张自定义图片。
 *
 * 选择是**服务端级**的：存在 settings 表里，连这台服务端的所有浏览器（标签页图标、PWA 清单、
 * iOS 主屏幕）与原生客户端（Dock）看到的是同一个图标。
 *
 * 内置图标的图形在 scripts/app-icons.mjs（gen-icons.mjs 出文件），产物按 id 分目录放在 web 的 public/icons/<id>/ 下
 * （原生另有一份 macOS 版式的 PNG 嵌进二进制）。这里只管 id、地址与清单。
 */

/** 内置图标，顺序即设置里的排列顺序。与 scripts/app-icons.mjs 的 ICONS 一一对应 */
export const APP_ICON_IDS = [
  "emberwing",
  "voltwing",
  "voltwing-spark",
  "voltwing-night",
  "voltwing-ivory",
  "glyph",
  "flash",
  "thunderbird",
] as const;

export type BuiltinAppIcon = (typeof APP_ICON_IDS)[number];
export type AppIconChoice = BuiltinAppIcon | "custom";

/** 必须与 scripts/app-icons.mjs 的 DEFAULT_ID 相同（安装包的 AppIcon 用它） */
export const DEFAULT_APP_ICON: BuiltinAppIcon = "emberwing";

/** 自定义图标由客户端（web 画布 / 原生 image crate）规整成这个边长的正方形 PNG 再上传 */
export const APP_ICON_CUSTOM_SIZE = 512;

export interface AppIconState {
  selected: AppIconChoice;
  /** 已上传的自定义图标的版本（内容哈希前缀），null = 没有。地址里带着它，换了图就换了地址 */
  custom: string | null;
}

export function isBuiltinAppIcon(value: unknown): value is BuiltinAppIcon {
  return typeof value === "string" && (APP_ICON_IDS as readonly string[]).includes(value);
}

export function isAppIconChoice(value: unknown): value is AppIconChoice {
  return value === "custom" || isBuiltinAppIcon(value);
}

/**
 * 存储值 → 生效的选择。认不出的 id（降级到没有这套图标的旧版本、手改过库）
 * 与选了自定义却没有图的，一律回默认——图标永远得有一个能用的。
 */
export function resolveAppIcon(stored: string | undefined, custom: string | null): AppIconChoice {
  if (stored === "custom") return custom ? "custom" : DEFAULT_APP_ICON;
  return isBuiltinAppIcon(stored) ? stored : DEFAULT_APP_ICON;
}

export type BuiltinIconFile =
  | "icon-192.png"
  | "icon-512.png"
  | "maskable-192.png"
  | "maskable-512.png"
  | "apple-touch-icon.png";

export function builtinIconUrl(id: BuiltinAppIcon, file: BuiltinIconFile): string {
  return `/icons/${id}/${file}`;
}

export function customIconUrl(version: string): string {
  return `/api/app-icon/custom.png?v=${encodeURIComponent(version)}`;
}

/**
 * 当前选择落到页面上的几个地址：标签页图标、iOS 主屏幕。
 * 标签页图标用 192 的 PNG 而不是 SVG：默认图标是栅格插画，做成 SVG 就是 1MB 多的 favicon
 */
export interface AppIconLinks {
  favicon: string;
  appleTouch: string;
}

export function appIconLinks(state: AppIconState): AppIconLinks {
  if (state.selected === "custom" && state.custom) {
    const url = customIconUrl(state.custom);
    return { favicon: url, appleTouch: url };
  }
  const id = state.selected === "custom" ? DEFAULT_APP_ICON : state.selected;
  return {
    favicon: builtinIconUrl(id, "icon-192.png"),
    appleTouch: builtinIconUrl(id, "apple-touch-icon.png"),
  };
}

export interface ManifestIcon {
  src: string;
  sizes: string;
  type: string;
  purpose: "any" | "maskable";
}

/**
 * PWA 清单（以前是 public/manifest.webmanifest 静态文件）。图标跟着选择走，所以由服务端现出。
 *
 * 自定义图标只有一张 512 的 any：Chrome 的安装条件是「至少一张 ≥144px 的 any 图标」，
 * 够了；用户随手传的图不是照 maskable 安全区画的，不冒充 maskable。
 */
export function webManifest(state: AppIconState) {
  let icons: ManifestIcon[];
  if (state.selected === "custom" && state.custom) {
    icons = [
      {
        src: customIconUrl(state.custom),
        sizes: `${APP_ICON_CUSTOM_SIZE}x${APP_ICON_CUSTOM_SIZE}`,
        type: "image/png",
        purpose: "any",
      },
    ];
  } else {
    const id = state.selected === "custom" ? DEFAULT_APP_ICON : state.selected;
    icons = [
      { src: builtinIconUrl(id, "icon-192.png"), sizes: "192x192", type: "image/png", purpose: "any" },
      { src: builtinIconUrl(id, "icon-512.png"), sizes: "512x512", type: "image/png", purpose: "any" },
      { src: builtinIconUrl(id, "maskable-192.png"), sizes: "192x192", type: "image/png", purpose: "maskable" },
      { src: builtinIconUrl(id, "maskable-512.png"), sizes: "512x512", type: "image/png", purpose: "maskable" },
    ];
  }
  return {
    id: "/",
    name: "Falcon",
    short_name: "Falcon",
    description: "持久化终端工作台",
    start_url: "/",
    scope: "/",
    display: "standalone",
    background_color: "#0a0a0a",
    theme_color: "#0a0a0a",
    lang: "zh-CN",
    categories: ["developer", "utilities"],
    icons,
  };
}
