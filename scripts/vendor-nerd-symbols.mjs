#!/usr/bin/env node
/**
 * 抽出 Symbols Nerd Font Mono，压成 woff2 放到
 * packages/web/src/assets/fonts/nerd-symbols/。
 *
 *   node scripts/vendor-nerd-symbols.mjs
 */

import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { pipeline } from "node:stream/promises";
import { createWriteStream } from "node:fs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const OUT_DIR = path.join(ROOT, "packages/web/src/assets/fonts/nerd-symbols");
const VERSION = "v3.5.0";
const ZIP_URL = `https://github.com/ryanoasis/nerd-fonts/releases/download/${VERSION}/NerdFontsSymbolsOnly.zip`;

async function download(url, dest) {
  const res = await fetch(url, { headers: { "user-agent": "falcon-vendor" } });
  if (!res.ok || !res.body) throw new Error(`下载失败 ${url}: HTTP ${res.status}`);
  await pipeline(res.body, createWriteStream(dest));
}

function convertTtfToWoff2(ttfPath, woff2Path) {
  const ttf2woff2 = path.join(ROOT, "node_modules/.bin/ttf2woff2");
  const bin = fs.existsSync(ttf2woff2) ? ttf2woff2 : "npx";
  const args = fs.existsSync(ttf2woff2) ? [] : ["--yes", "ttf2woff2"];
  const input = fs.readFileSync(ttfPath);
  const out = execFileSync(bin, args, { input, maxBuffer: 64 * 1024 * 1024 });
  fs.writeFileSync(woff2Path, out);
}

async function main() {
  fs.mkdirSync(OUT_DIR, { recursive: true });
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "falcon-nerd-"));
  const zipPath = path.join(tmp, "nf.zip");
  try {
    console.log(`下载 ${ZIP_URL}`);
    await download(ZIP_URL, zipPath);
    execFileSync("unzip", ["-jo", zipPath, "SymbolsNerdFontMono-Regular.ttf", "LICENSE", "-d", tmp]);
    const license = path.join(tmp, "LICENSE");
    if (fs.existsSync(license)) fs.copyFileSync(license, path.join(OUT_DIR, "LICENSE.txt"));
    const ttf = path.join(tmp, "SymbolsNerdFontMono-Regular.ttf");
    const dest = path.join(OUT_DIR, "SymbolsNerdFontMono-Regular.woff2");
    console.log("压缩 SymbolsNerdFontMono-Regular.ttf");
    convertTtfToWoff2(ttf, dest);
    console.log(`  ${Math.round(fs.statSync(dest).size / 1024)} KB`);
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
