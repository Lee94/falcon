#!/usr/bin/env node
/**
 * 比两套协议 fixture 的**形状**（S 线对拍：Node 版生成的 vs Rust 版生成的）。
 *
 *   node native/scripts/compare-fixtures.mjs <基准目录> <对照目录>
 *
 * 只比结构：同名文件里每个对象的键集合与键序、每个值的类型（null / bool / number / string /
 * array / object），数组按元素逐个比（长度不同也报）。id、时间戳、令牌这些值每次都变，不比。
 * 退出码：有差异 1，没有 0。
 */
import fs from "node:fs";
import path from "node:path";

const [baseDir, otherDir] = process.argv.slice(2);
if (!baseDir || !otherDir) {
  console.error("用法: node compare-fixtures.mjs <基准目录> <对照目录>");
  process.exit(2);
}

const kind = (v) => (v === null ? "null" : Array.isArray(v) ? "array" : typeof v);
const diffs = [];

function walk(a, b, at) {
  const ka = kind(a);
  const kb = kind(b);
  if (ka !== kb) {
    diffs.push(`${at}: 类型 ${ka} ≠ ${kb}`);
    return;
  }
  if (ka === "array") {
    if (a.length !== b.length) diffs.push(`${at}: 数组长度 ${a.length} ≠ ${b.length}`);
    for (let i = 0; i < Math.min(a.length, b.length); i++) walk(a[i], b[i], `${at}[${i}]`);
  } else if (ka === "object") {
    const keysA = Object.keys(a);
    const keysB = Object.keys(b);
    const missing = keysA.filter((k) => !(k in b));
    const extra = keysB.filter((k) => !(k in a));
    if (missing.length) diffs.push(`${at}: 缺键 ${missing.join(", ")}`);
    if (extra.length) diffs.push(`${at}: 多键 ${extra.join(", ")}`);
    const common = keysA.filter((k) => k in b);
    const orderB = keysB.filter((k) => k in a);
    if (common.join(",") !== orderB.join(",")) diffs.push(`${at}: 键序 [${common}] ≠ [${orderB}]`);
    for (const k of common) walk(a[k], b[k], `${at}.${k}`);
  }
}

const names = new Set([...fs.readdirSync(baseDir), ...fs.readdirSync(otherDir)].filter((n) => n.endsWith(".json")));
for (const name of [...names].sort()) {
  const pa = path.join(baseDir, name);
  const pb = path.join(otherDir, name);
  if (!fs.existsSync(pa)) {
    diffs.push(`${name}: 基准里没有`);
    continue;
  }
  if (!fs.existsSync(pb)) {
    diffs.push(`${name}: 对照里没有`);
    continue;
  }
  walk(JSON.parse(fs.readFileSync(pa, "utf8")), JSON.parse(fs.readFileSync(pb, "utf8")), name);
}
for (const d of diffs) console.log(d);
console.log(diffs.length ? `\n${diffs.length} 处差异` : "形状一致");
process.exit(diffs.length ? 1 : 0);
