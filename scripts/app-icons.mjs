/**
 * 内置应用图标的图形定义与各种形状（ADR 0018）。gen-icons.mjs 用它出 web / 原生的图标文件，
 * build-macos-pkg.mjs 用它现画安装包的 AppIcon。
 *
 * 每个图标 = 底色 `bg`（SVG fill，可以是渐变引用）+ 画在 1024 网格上的 `mark`（+ 可选 `defs`）。
 * id 列表与 packages/shared/src/appIcon.ts 的 APP_ICON_IDS 一一对应（web 与原生各有测试对账）。
 */
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));

/** 默认图标。必须与 shared 的 DEFAULT_APP_ICON 相同（web 的 appIcon.test.ts 对账） */
export const DEFAULT_ID = "emberwing";

// ---- 橙翼（设计稿「闪电鸟 | 品牌视觉完整设计稿」v02）----
// 标志原图是 1254 见方的深色底栅格插画（设计稿自己说明没有矢量稿与 32px 简化版），
// 原样存在 scripts/app-icons/emberwing.png。图标规范：深炭黑容器里，原方图居中缩至 0.80 S。
// 容器取 #0F0F0F 而不是稿里的 #101010：原图外圈实测均值 rgb(15.5,15.2,14.9)，
// 用 #101010 在把暗部拉亮后能看出一圈方块边，#0F0F0F 与它差不到半个色阶。
function emberwing() {
  const png = fs.readFileSync(path.join(HERE, "app-icons/emberwing.png")).toString("base64");
  const side = 1024 * 0.8;
  const at = (1024 - side) / 2;
  return {
    bg: "#0F0F0F",
    mark: `<image href="data:image/png;base64,${png}" x="${at}" y="${at}" width="${side}" height="${side}"/>`,
  };
}

// ---- 电翼（设计稿「闪电鸟：电翼」，1024 网格）----
// 鸟举起一道闪电当翅膀。头是半径 118.7 的正圆，喙上缘与它相切；背线由尾尖向头圆作切线，
// 腹线是一段二次曲线；眼睛半径 23.7。数值照设计稿原样，别手调。
const INDIGO = "#1D1A45";
const VOLT = "#FFD60A";
const IVORY = "#FFF6DD";
const NIGHT = "#0F0D26";
const WING = "M351.8 751.9L560.8 751.9L411.2 501.2L536.5 501.2L197 182.1L325.7 432.8L189.9 432.8Z";
// 单色版的翅膀：翅根在背线处留出 14 的缝，否则同色的翅膀和鸟身会粘成一团
const WING_MONO = "M411.2 501.2L536.5 501.2L197 182.1L325.7 432.8L189.9 432.8L332.9 714.6L465.4 592Z";
const BODY =
  "M200.2 856.4L577.3 507.6A118.7 118.7 0 0 1 710.3 488.2L834.1 549.1L776.3 586.4A118.7 118.7 0 0 1 682.6 710.8Q539.8 872.9 200.2 856.4Z";
const eye = (fill) => `<circle cx="677.2" cy="566.4" r="23.7" fill="${fill}"/>`;

const voltwing = (bg) => ({
  bg,
  mark: `<path d="${WING}" fill="${VOLT}"/><path d="${BODY}" fill="${IVORY}"/>${eye(bg)}`,
});
const voltwingMono = (bg, ink) => ({
  bg,
  mark: `<path d="${WING_MONO}" fill="${ink}"/><path d="${BODY}" fill="${ink}"/>${eye(bg)}`,
});

// ---- 雷鸟（512 网格画的，放大一倍进 1024）----
// 正面展翅：三叉冠羽、怒目、橙色尖喙，翅膀与尾巴是锯齿状的闪电羽。只定义右半边（x ≥ 256）
// 再镜像；整只先涂亮面，再裁出左半涂暗面（左暗右亮的折纸感）。
function thunderbird() {
  const WING_R = [[300, 246], [466, 76], [410, 160], [484, 156], [404, 222], [454, 250], [360, 274], [390, 306], [294, 300]];
  const BODY_R = [[256, 212], [298, 244], [294, 300], [330, 400], [284, 372], [256, 454]];
  const CREST_R = [[256, 30], [272, 122], [314, 66], [298, 148], [256, 148]];
  const HEAD_R = [[256, 116], [302, 140], [308, 188], [286, 220], [256, 232]];
  const EYE_R = [[262, 174], [302, 160], [292, 190], [268, 188]];
  const pts = (half) => half.map((p) => p.join(",")).join(" ");
  const mirror = (half) => half.map(([x, y]) => [512 - x, y]);
  const both = (half) => `<polygon points="${pts(half)}"/><polygon points="${pts(mirror(half))}"/>`;
  const silhouette = [WING_R, BODY_R, CREST_R, HEAD_R].map(both).join("");
  // 同色描边盖住相邻多边形之间的抗锯齿拼缝，否则放大后能看到一道道细线
  const sil = (color) =>
    `<g fill="${color}" stroke="${color}" stroke-width="2" stroke-linejoin="round">${silhouette}</g>`;
  return {
    bg: "url(#tb-glow)",
    defs:
      `<radialGradient id="tb-glow" cx="512" cy="440" r="500" gradientUnits="userSpaceOnUse">` +
      `<stop offset="0" stop-color="#3b37b0"/><stop offset="0.55" stop-color="#1c1a58"/><stop offset="1" stop-color="#0c0b24"/>` +
      `</radialGradient>` +
      `<clipPath id="tb-left"><rect width="256" height="512"/></clipPath>`,
    // 翼展原本占满 89% 画布，缩一点留出呼吸感，并把鸟（y 30–454）挪到垂直居中
    mark:
      `<g transform="scale(2) translate(256 256) scale(0.92) translate(-256 -242)">` +
      sil("#ffd21f") +
      `<g clip-path="url(#tb-left)">${sil("#f2a900")}</g>` +
      `<g fill="#0a0a0a">${both(EYE_R)}</g>` +
      `<polygon fill="#ff7a1a" points="234,196 278,196 256,250"/>` +
      `<polygon fill="#e2560a" points="256,196 278,196 256,250"/>` +
      `</g>`,
  };
}

/** @type {Record<string, {bg: string, mark: string, defs?: string}>} 顺序即设置里的排列顺序 */
export const ICONS = {
  emberwing: emberwing(),
  voltwing: voltwing(INDIGO),
  "voltwing-spark": voltwingMono(VOLT, INDIGO),
  "voltwing-night": voltwing(NIGHT),
  "voltwing-ivory": voltwingMono(IVORY, INDIGO),
  // 鸟字：「鸟」字里的竖折折钩本来就是一道闪电，把它点亮；那一点正好是眼睛（电翼稿方向 B）
  glyph: {
    bg: INDIGO,
    mark:
      `<path d="M592.3 186.4L469.6 272.4L498.9 297L456.1 297L445.7 370.7L640.1 370.7L622.9 493.6L696.6 493.6L724.2 297L530.5 297L623.1 229.4ZM223.1 714.8L561.1 714.8L571.4 641L233.5 641Z" fill="${IVORY}"/>` +
      `<circle cx="528.7" cy="419.8" r="44.3" fill="${IVORY}"/>` +
      `<path d="M441.6 297L367.9 297L333.3 542.7L328.2 579.6L323 616.4L660.9 616.4L539.4 825.3L765.3 616.4L775.7 542.7L407.1 542.7Z" fill="${VOLT}"/>`,
  },
  // 一闪：整只鸟就是一个闪电符号，只加了喙和眼（电翼稿方向 C）
  flash: {
    bg: INDIGO,
    mark:
      `<path d="M399.8 223.6L596.4 223.6L753.7 276.1L609.5 315.4L530.8 472.7L701.2 472.7L308 839.7L426 557.9L255.6 557.9Z" fill="${VOLT}"/>` +
      `<circle cx="543.9" cy="269.5" r="22.5" fill="${INDIGO}"/>`,
  },
  thunderbird: thunderbird(),
};

const svg = (icon, body, extraDefs = "") =>
  `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024">` +
  (icon.defs || extraDefs ? `<defs>${icon.defs ?? ""}${extraDefs}</defs>` : "") +
  body +
  `</svg>\n`;
const scaled = (k, inner) => `<g transform="translate(512 512) scale(${k}) translate(-512 -512)">${inner}</g>`;

/** 圆角方块，圆角 229（1024 网格）：标签页图标、设置里的预览、PWA purpose=any */
export const rounded = (icon) => svg(icon, `<rect width="1024" height="1024" rx="229" fill="${icon.bg}"/>${icon.mark}`);
/** 满版方块：iOS 主屏幕自己裁超椭圆，给它圆角反而露出四个角的底色 */
export const square = (icon) => svg(icon, `<rect width="1024" height="1024" fill="${icon.bg}"/>${icon.mark}`);
/**
 * maskable：安全区是直径 80% 的圆（r = 409.6）。图形缩到 0.84 才完整落进去——
 * 最紧的圆形遮罩也切不到翼尖和尾尖。
 */
export const maskable = (icon) =>
  svg(icon, `<rect width="1024" height="1024" fill="${icon.bg}"/>${scaled(0.84, icon.mark)}`);
/**
 * macOS 版式（Big Sur 起的图标网格）：824 见方的圆角块居中、四周留透明边、带一点投影。
 * 满版的图标放进 Dock 会比别的应用大一圈。原生 falcon-core 的 MAC_INSET / MAC_RADIUS 是同一组数
 */
export const macos = (icon) =>
  svg(
    icon,
    `<rect x="100" y="100" width="824" height="824" rx="185" fill="${icon.bg}" filter="url(#mac-shadow)"/>` +
      scaled(824 / 1024, icon.mark),
    `<filter id="mac-shadow" x="-10%" y="-10%" width="120%" height="125%"><feDropShadow dx="0" dy="10" stdDeviation="12" flood-color="#000" flood-opacity="0.3"/></filter>`
  );

/** 用 rsvg-convert 光栅化成 size 见方的 PNG */
export function raster(svgText, dest, size) {
  execFileSync("rsvg-convert", ["-w", String(size), "-h", String(size), "-o", dest], {
    input: svgText,
    maxBuffer: 64 * 1024 * 1024,
  });
}
