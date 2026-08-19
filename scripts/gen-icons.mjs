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

const MARK = `
  <g transform="translate(256 272)">
    <path fill="#2bb86a" d="M-6-8C-18-128-168-138-186-18-198 58-108 122-6 146-28 48-24-40-6-8Z"/>
    <path fill="#3ddc84" d="M6-8C18-128 168-138 186-18 198 58 108 122 6 146 28 48 24-40 6-8Z"/>
    <path d="M0-122V136" fill="none" stroke="#14532d" stroke-width="16" stroke-linecap="round"/>
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
