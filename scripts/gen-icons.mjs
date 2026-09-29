#!/usr/bin/env node
/**
 * 生成全部内置应用图标（ADR 0018）。图形定义在 scripts/app-icons.mjs，这里只管出文件：
 *
 *   packages/web/public/icons/<id>/icon-{192,512}.png      圆角方块：标签页图标、设置里的预览、PWA purpose=any
 *   packages/web/public/icons/<id>/maskable-{192,512}.png  PWA purpose=maskable
 *   packages/web/public/icons/<id>/apple-touch-icon.png    iOS 主屏幕（满版方块，系统自己裁角）
 *   native/crates/falcon-app/assets/app-icons/<id>.png     原生客户端运行时的 Dock 图标（macOS 版式）
 *
 * 标签页图标用 PNG 不用 SVG：默认图标是栅格插画，塞进 SVG 就是一个 1MB 多的 favicon。
 * 安装包的 AppIcon.icns 由 build-macos-pkg.mjs 直接从 app-icons.mjs 现画，不在这里出。
 *
 *   node scripts/gen-icons.mjs
 */
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { ICONS, macos, maskable, raster, rounded, square } from "./app-icons.mjs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const WEB_OUT = path.join(ROOT, "packages/web/public/icons");
const NATIVE_OUT = path.join(ROOT, "native/crates/falcon-app/assets/app-icons");

// 从头出：删掉的图标不留旧文件（旧版平铺的 icons/icon-192.png 等也一并清掉）
fs.rmSync(WEB_OUT, { recursive: true, force: true });
fs.rmSync(NATIVE_OUT, { recursive: true, force: true });
fs.rmSync(path.join(ROOT, "packages/web/public/favicon.svg"), { force: true });
fs.mkdirSync(NATIVE_OUT, { recursive: true });

for (const [id, icon] of Object.entries(ICONS)) {
  const dir = path.join(WEB_OUT, id);
  fs.mkdirSync(dir, { recursive: true });
  const any = rounded(icon);
  const mask = maskable(icon);
  raster(any, path.join(dir, "icon-192.png"), 192);
  raster(any, path.join(dir, "icon-512.png"), 512);
  raster(mask, path.join(dir, "maskable-192.png"), 192);
  raster(mask, path.join(dir, "maskable-512.png"), 512);
  raster(square(icon), path.join(dir, "apple-touch-icon.png"), 180);
  raster(macos(icon), path.join(NATIVE_OUT, `${id}.png`), 512);
}

console.log(`wrote ${Object.keys(ICONS).length} icons → ${path.relative(ROOT, WEB_OUT)}, ${path.relative(ROOT, NATIVE_OUT)}`);
