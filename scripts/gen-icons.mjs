#!/usr/bin/env node
/**
 * 从 public/favicon.svg 栅格化 Chrome / iOS 安装图标。
 *
 *   node scripts/gen-icons.mjs
 */
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const SRC = path.join(ROOT, "packages/web/public/favicon.svg");
const OUT = path.join(ROOT, "packages/web/public/icons");

// 俯冲猎鹰：两翼对称、身体是向下的菱，跟上一版两片叶子一样是「中间一根 + 左右两瓣」，
// 16px 仍能认出剪影。颜色是两档金，深底跟 PWA theme-color / 深色主题对齐。
const MARK = `
  <g transform="translate(256 240)">
    <path fill="#c9a024" d="M-200-72L-16-20 2 40-96 8Z"/>
    <path fill="#e0b63a" d="M200-72L16-20-2 40 96 8Z"/>
    <path fill="#e4c878" d="M0-6-34 32 0 176Z"/>
    <path fill="#f3d99a" d="M0-6 34 32 0 176Z"/>
    <path fill="#c9a024" d="M0-108-32-26 0 2 32-26Z"/>
    <path fill="#f3d99a" d="M0-50-12-12 0 4 12-12Z"/>
  </g>
`;

function svg(inner) {
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512">${inner}</svg>\n`;
}

function raster(svgText, dest, size) {
  execFileSync("rsvg-convert", ["-w", String(size), "-h", String(size), "-o", dest], {
    input: svgText,
  });
}

fs.mkdirSync(OUT, { recursive: true });

const any = svg(`<rect width="512" height="512" rx="112" fill="#0a0a0a"/>${MARK}`);
const maskable = svg(
  `<rect width="512" height="512" fill="#0a0a0a"/><g transform="translate(256 256) scale(0.72) translate(-256 -256)">${MARK}</g>`
);

fs.writeFileSync(SRC, any);

raster(any, path.join(OUT, "icon-192.png"), 192);
raster(any, path.join(OUT, "icon-512.png"), 512);
raster(maskable, path.join(OUT, "icon-maskable-192.png"), 192);
raster(maskable, path.join(OUT, "icon-maskable-512.png"), 512);
raster(maskable, path.join(OUT, "apple-touch-icon.png"), 180);

console.log(`wrote icons → ${path.relative(ROOT, OUT)}`);
