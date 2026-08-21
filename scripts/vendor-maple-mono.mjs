#!/usr/bin/env node
/**
 * 从 maple-font 官方 Release 抽出 Maple Mono NL NF CN 的 Regular / Bold，
 * 压成 woff2 放到 packages/web/src/assets/fonts/maple-mono/。
 *
 *   node scripts/vendor-maple-mono.mjs
 *
 * 只抽两个字重：终端几乎只用 Regular，Bold 给 ANSI 粗体。完整 CN 包上百 MB，
 * 不能整包提交。
 */

import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { pipeline } from "node:stream/promises";
import { createWriteStream } from "node:fs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const OUT_DIR = path.join(ROOT, "packages/web/src/assets/fonts/maple-mono");
const VERSION = "v7.9";
const ZIP_URL = `https://github.com/subframe7536/maple-font/releases/download/${VERSION}/MapleMonoNL-NF-CN-unhinted.zip`;
const OFL_URL = `https://raw.githubusercontent.com/subframe7536/maple-font/${VERSION}/OFL.txt`;

// 只要 Regular：Bold 由浏览器合成（等宽字体的合成粗体不改变字符步进），
// 省一份 6.3MB 的下载。Regular 本身也不直接进页面——vendor 完还要跑
// scripts/subset-maple-mono.mjs 切成 Latin / CJK 两片，CSS 引用的是那两片。
const WANTED = [
  { match: /MapleMonoNL-NF-CN-Regular\.ttf$/i, out: "MapleMonoNL-NF-CN-Regular.woff2" },
];

async function download(url, dest) {
  const res = await fetch(url, { headers: { "user-agent": "mojito-vendor" } });
  if (!res.ok || !res.body) throw new Error(`下载失败 ${url}: HTTP ${res.status}`);
  await pipeline(res.body, createWriteStream(dest));
}

function convertTtfToWoff2(ttfPath, woff2Path) {
  const ttf2woff2 = path.join(ROOT, "node_modules/.bin/ttf2woff2");
  const bin = fs.existsSync(ttf2woff2) ? ttf2woff2 : "npx";
  const args = fs.existsSync(ttf2woff2)
    ? []
    : ["--yes", "ttf2woff2"];
  const input = fs.readFileSync(ttfPath);
  const out = execFileSync(bin, args, { input, maxBuffer: 64 * 1024 * 1024 });
  fs.writeFileSync(woff2Path, out);
}

async function main() {
  fs.mkdirSync(OUT_DIR, { recursive: true });
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "mojito-maple-"));
  const zipPath = path.join(tmp, "maple.zip");
  try {
    console.log(`下载 ${ZIP_URL}`);
    await download(ZIP_URL, zipPath);
    console.log(`下载 OFL`);
    await download(OFL_URL, path.join(OUT_DIR, "OFL.txt"));

    const listing = execFileSync("unzip", ["-l", zipPath], { encoding: "utf8" });
    const names = listing
      .split("\n")
      .map((line) => line.trim().split(/\s+/).pop() ?? "")
      .filter(Boolean);

    for (const item of WANTED) {
      const entry = names.find((n) => item.match.test(n.replaceAll("\\", "/")));
      if (!entry) throw new Error(`zip 里找不到 ${item.match}`);
      const extracted = path.join(tmp, path.basename(entry));
      execFileSync("unzip", ["-jo", zipPath, entry, "-d", tmp]);
      const ttf = fs.existsSync(extracted)
        ? extracted
        : fs.readdirSync(tmp).map((f) => path.join(tmp, f)).find((f) => item.match.test(f));
      if (!ttf || !fs.existsSync(ttf)) throw new Error(`解压后找不到 ${entry}`);
      const dest = path.join(OUT_DIR, item.out);
      console.log(`压缩 ${path.basename(ttf)} → ${item.out}`);
      convertTtfToWoff2(ttf, dest);
      const kb = Math.round(fs.statSync(dest).size / 1024);
      console.log(`  ${kb} KB`);
    }
    console.log(`已写入 ${OUT_DIR}`);
    console.log("切子集");
    execFileSync("node", [path.join(ROOT, "scripts/subset-maple-mono.mjs")], {
      stdio: "inherit",
    });
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
