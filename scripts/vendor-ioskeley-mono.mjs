#!/usr/bin/env node
/**
 * 从 IoskeleyMono 官方 Release 抽 IoskeleyMonoTerm Nerd Font Mono 的 Regular，
 * 子集化后压成 woff2 放到 packages/web/src/assets/fonts/ioskeley-mono/。
 *
 *   node scripts/vendor-ioskeley-mono.mjs
 *
 * IoskeleyMono = Iosevka 的自定义 build（Ioskeley = Iosevka + Berkeley），
 * 照着 Berkeley Mono 的骨架调的；Term 变体把符号窄化成 1 格，正是终端要的。
 * https://github.com/ahatem/IoskeleyMono —— SIL OFL 1.1，全文见同目录 LICENSE.txt
 *
 * 上游 zip 一份 4.7MB，18037 个码位，其中 10523 个是 Nerd Font 的 PUA 图标。
 * 图标区**整个剔除**：termFontStack 里 Symbols Nerd Font Mono 排在它前面，
 * 这份内嵌图标从来不会被用到（和 subset-maple-mono.mjs 同样的理由）。
 * 剩下按下面的范围表切，只留终端真正会画的字形。
 *
 * 只要 Regular：Bold / Italic 由浏览器合成，等宽字体的合成粗体不改变字符步进，
 * 省掉再来 19 个字重的下载。CJK 上游本来就没有，落到 stack 后面的 Maple。
 */

import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { pipeline } from "node:stream/promises";
import { createWriteStream } from "node:fs";
import subsetFont from "subset-font";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const OUT_DIR = path.join(ROOT, "packages/web/src/assets/fonts/ioskeley-mono");
const VERSION = "v2.1.0";
const ZIP_URL = `https://github.com/ahatem/IoskeleyMono/releases/download/${VERSION}/IoskeleyMono-Term-NerdFont.zip`;
const LICENSE_URL = `https://raw.githubusercontent.com/ahatem/IoskeleyMono/${VERSION}/LICENSE`;
const ENTRY = "Normal/IoskeleyMonoTermNerdFontMono-Regular.ttf";
const OUT_FILE = "IoskeleyMonoTermNFM-Latin.woff2";

/**
 * 保留范围。与 subset-maple-mono.mjs 的 LATIN_RANGES 同一套取舍——终端真会画的
 * 骨架字形。PUA 不在表里就是刻意的。
 *
 * css 里**不写 unicode-range**：只有一片，没有按需分片可言；表里没覆盖到的字符
 * 浏览器按 cmap 落空后自动回退到 stack 下一位（Maple / 系统 CJK / emoji）。
 */
const RANGES = [
  [0x0000, 0x024f], // Basic Latin + Latin-1 + Extended-A/B
  [0x0250, 0x02af], // IPA（man 页偶尔用）
  [0x0370, 0x03ff], // 希腊（数学 / 日志常见）
  [0x0400, 0x04ff], // 西里尔
  [0x1e00, 0x1eff], // Latin Extended Additional
  [0x2000, 0x206f], // 通用标点
  [0x2070, 0x209f], // 上下标
  [0x20a0, 0x20cf], // 货币
  [0x2100, 0x218f], // 字母式符号 + 数字形式
  [0x2190, 0x21ff], // 箭头
  [0x2200, 0x22ff], // 数学运算符
  [0x2300, 0x23ff], // 杂项技术（⌘ ⏎ 等）
  [0x2400, 0x243f], // 控制图片
  [0x2500, 0x257f], // 制表符（TUI 边框）
  [0x2580, 0x259f], // 方块元素（进度条）
  [0x25a0, 0x25ff], // 几何图形
  [0x2600, 0x27bf], // 杂项符号 + 装饰符号
  [0x2b00, 0x2bff], // 杂项符号与箭头
  [0xfffd, 0xfffd], // replacement character
];

function textOf(ranges) {
  let out = "";
  for (const [lo, hi] of ranges) {
    for (let cp = lo; cp <= hi; cp++) out += String.fromCodePoint(cp);
  }
  return out;
}

async function download(url, dest) {
  const res = await fetch(url, { headers: { "user-agent": "falcon-vendor" } });
  if (!res.ok || !res.body) throw new Error(`下载失败 ${url}: HTTP ${res.status}`);
  await pipeline(res.body, createWriteStream(dest));
}

const kb = (n) => `${Math.round(n / 1024)} KB`;

async function main() {
  fs.mkdirSync(OUT_DIR, { recursive: true });
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "falcon-ioskeley-"));
  const zipPath = path.join(tmp, "ioskeley.zip");
  try {
    console.log(`下载 ${ZIP_URL}`);
    await download(ZIP_URL, zipPath);
    console.log("下载 LICENSE");
    await download(LICENSE_URL, path.join(OUT_DIR, "LICENSE.txt"));

    execFileSync("unzip", ["-jo", zipPath, ENTRY, "-d", tmp]);
    const ttf = path.join(tmp, path.basename(ENTRY));
    if (!fs.existsSync(ttf)) throw new Error(`zip 里找不到 ${ENTRY}`);
    console.log(`源字体 ${path.basename(ttf)}：${kb(fs.statSync(ttf).size)}`);

    const buf = await subsetFont(fs.readFileSync(ttf), textOf(RANGES), { targetFormat: "woff2" });
    const dest = path.join(OUT_DIR, OUT_FILE);
    fs.writeFileSync(dest, buf);
    console.log(`→ ${OUT_FILE}：${kb(buf.length)}`);
    console.log(`已写入 ${OUT_DIR}`);
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
