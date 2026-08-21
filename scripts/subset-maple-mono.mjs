#!/usr/bin/env node
/**
 * 把 vendor 下来的 Maple Mono NL NF CN Regular（6.5MB woff2）切成两片：
 *
 *   MapleMonoNL-NF-CN-Latin.woff2 —— 拉丁 / 标点 / 制表符等终端骨架字形，很小，
 *     首屏只拉它；**刻意剔除 PUA 图标区**：Nerd Font 图标由同页的
 *     SymbolsNerdFontMono-Regular（官方图标字体、全量 vendor）兜底，
 *     Maple 内嵌的那份是重复的。
 *   MapleMonoNL-NF-CN-CJK.woff2 —— 全部 CJK / 假名 / 全角区，几 MB，
 *     由 @font-face 的 unicode-range 按需加载：界面字体走系统栈，
 *     只有终端真的输出中文时浏览器才会拉这一片。
 *
 * Bold 整个不要：终端 ANSI 粗体由浏览器从 Regular 合成，等宽字体的合成粗体
 * 不改变字符步进，却省下再一份 6.3MB 的下载。
 *
 *   node scripts/subset-maple-mono.mjs
 *
 * 输入是 vendor-maple-mono.mjs 落下的 Regular woff2，改动字体版本后先跑那个再跑这个。
 */

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import subsetFont from "subset-font";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const DIR = path.join(ROOT, "packages/web/src/assets/fonts/maple-mono");
const SRC = path.join(DIR, "MapleMonoNL-NF-CN-Regular.woff2");

/** 与 packages/web/src/lib/maple-mono.css 里的 unicode-range 保持一致 */
const LATIN_RANGES = [
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
  [0xfb00, 0xfb4f], // 连字区（NL 版基本没有，保险）
  [0xfffd, 0xfffd], // replacement character
];

const CJK_RANGES = [
  [0x2e80, 0x303f], // CJK 部首 / 康熙部首 / CJK 标点
  [0x3040, 0x30ff], // 平假名 / 片假名
  [0x3100, 0x312f], // 注音
  [0x31c0, 0x31ef], // CJK 笔画
  [0x3200, 0x33ff], // 带圈字符 / CJK 兼容
  [0x3400, 0x4dbf], // 扩展 A
  [0x4e00, 0x9fff], // 基本区
  [0xf900, 0xfaff], // 兼容表意
  [0xfe10, 0xfe1f], // 竖排标点
  [0xfe30, 0xfe4f], // CJK 兼容形式
  [0xff00, 0xffef], // 全角 / 半角形式
  [0x20000, 0x2fa1f], // 扩展 B 及以后（字体里有多少留多少）
];

function textOf(ranges) {
  let out = "";
  for (const [lo, hi] of ranges) {
    for (let cp = lo; cp <= hi; cp++) out += String.fromCodePoint(cp);
  }
  return out;
}

const mb = (n) => `${(n / 1024 / 1024).toFixed(2)}MB`;

const src = fs.readFileSync(SRC);
console.log(`源字体 ${path.basename(SRC)}：${mb(src.length)}`);

for (const [name, ranges] of [
  ["Latin", LATIN_RANGES],
  ["CJK", CJK_RANGES],
]) {
  const out = path.join(DIR, `MapleMonoNL-NF-CN-${name}.woff2`);
  const buf = await subsetFont(src, textOf(ranges), { targetFormat: "woff2" });
  fs.writeFileSync(out, buf);
  console.log(`→ ${path.basename(out)}：${mb(buf.length)}`);
}
