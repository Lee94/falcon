#!/usr/bin/env node
/**
 * 从本机持有的 Berkeley Mono TX-02 zip 抽出 Regular / Bold / Oblique / BoldOblique，
 * 压成 woff2 放到 packages/web/src/assets/fonts/berkeley-mono/。
 *
 *   node scripts/vendor-berkeley-mono.mjs [zip]
 *
 * 默认读仓库根目录的 Berkeley-Mono-TX-02-*-FONToMASS.zip。zip 是 U.S. Graphics
 * 的商业发行物，不进仓库；产物 woff2 作为产品内嵌默认字体进二进制。
 *
 * 只要这四切：界面的 font-medium（500）落到 Regular，font-semibold（600）落到 Bold。
 * Medium / SemiBold 那几份几乎一样大，加进来只是多一倍下载。Oblique 当 italic 挂，
 * 终端 ANSI 斜体和 <em> 才能命中真切，而不是浏览器把 Regular 拉斜。
 */
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const OUT_DIR = path.join(ROOT, "packages/web/src/assets/fonts/berkeley-mono");
const ZIP_PREFIX = "Berkeley Mono TX-02/TX-02 2.002/";

const FACES = [
  "TX-02-Regular.ttf",
  "TX-02-Bold.ttf",
  "TX-02-Oblique.ttf",
  "TX-02-BoldOblique.ttf",
];

function convertTtfToWoff2(ttfPath, woff2Path) {
  const ttf2woff2 = path.join(ROOT, "node_modules/.bin/ttf2woff2");
  const bin = fs.existsSync(ttf2woff2) ? ttf2woff2 : "npx";
  const args = fs.existsSync(ttf2woff2) ? [] : ["--yes", "ttf2woff2"];
  const input = fs.readFileSync(ttfPath);
  const out = execFileSync(bin, args, { input, maxBuffer: 64 * 1024 * 1024 });
  fs.writeFileSync(woff2Path, out);
}

function findZip(explicit) {
  if (explicit) {
    const abs = path.resolve(explicit);
    if (!fs.existsSync(abs)) throw new Error(`找不到 zip：${abs}`);
    return abs;
  }
  const matches = fs
    .readdirSync(ROOT)
    .filter((n) => /^Berkeley-Mono-TX-02-.*\.zip$/i.test(n))
    .map((n) => path.join(ROOT, n));
  if (matches.length === 0) {
    throw new Error(
      "仓库根目录没有 Berkeley-Mono-TX-02-*.zip，把发行包放在那里或把路径当参数传入",
    );
  }
  matches.sort();
  return matches[matches.length - 1];
}

const kb = (n) => `${Math.round(n / 1024)} KB`;

function main() {
  const zipPath = findZip(process.argv[2]);
  fs.mkdirSync(OUT_DIR, { recursive: true });
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "falcon-berkeley-"));
  try {
    console.log(`源 zip ${path.basename(zipPath)}`);
    const entries = FACES.map((f) => ZIP_PREFIX + f);
    execFileSync("unzip", ["-jo", zipPath, ...entries, "-d", tmp]);

    const notice = [
      "Berkeley Mono TX-02 (2.002)",
      "Copyright (c) 2022-2024, U.S. Graphics LLC. All Rights Reserved.",
      "https://usgraphics.com/products/berkeley-mono",
      "",
      "商业字体，只作为本产品内嵌默认字体分发。升级字库时用",
      "  node scripts/vendor-berkeley-mono.mjs <zip>",
      "从官方发行包重新抽出，不要把 zip 提交进仓库。",
      "",
    ].join("\n");
    fs.writeFileSync(path.join(OUT_DIR, "NOTICE.txt"), notice);

    for (const face of FACES) {
      const ttf = path.join(tmp, face);
      if (!fs.existsSync(ttf)) throw new Error(`zip 里找不到 ${ZIP_PREFIX}${face}`);
      const dest = path.join(OUT_DIR, face.replace(/\.ttf$/i, ".woff2"));
      convertTtfToWoff2(ttf, dest);
      console.log(`  ${path.basename(ttf)} ${kb(fs.statSync(ttf).size)} → ${path.basename(dest)} ${kb(fs.statSync(dest).size)}`);
    }
    console.log(`已写入 ${OUT_DIR}`);
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
}

try {
  main();
} catch (err) {
  console.error(err);
  process.exit(1);
}
