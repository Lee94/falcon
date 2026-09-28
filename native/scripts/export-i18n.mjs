#!/usr/bin/env node
/**
 * 从 web 的 packages/web/src/i18n.ts 导出原生客户端的文案资源：
 * native/crates/falcon-app/locales/zh-CN.json（rust-i18n 的单语言文件格式）。
 *
 *   node native/scripts/export-i18n.mjs
 *
 * key 与 web 完全同名（sidebar.newTerminal …），两边对照、改文案都只看 web 那一份；
 * 原生独有的文案写在 falcon-app/i18n-native/*.json（按界面区域一个文件，一律挂在 `native.<区域>`
 * 下面；不能放 locales/ 里，rust-i18n 会把它们当成语言），脚本按文件名顺序合并进来（同名 key
 * 以原生为准，并在控制台列出，避免悄悄覆盖）。
 *
 * 插值语法不同：i18next 是 {{name}}，rust-i18n 是 %{name}，这里统一改写。
 * web 改了 i18n.ts 之后重跑本脚本。
 */
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const SRC = path.join(ROOT, "packages/web/src/i18n.ts");
const OUT_DIR = path.join(ROOT, "native/crates/falcon-app/locales");
const NATIVE_DIR = path.join(ROOT, "native/crates/falcon-app/i18n-native");
const OUT = path.join(OUT_DIR, "zh-CN.json");

const text = fs.readFileSync(SRC, "utf8");
const start = text.indexOf("const zh = {");
const end = text.indexOf("\n};", start);
if (start < 0 || end < 0) throw new Error("i18n.ts 里找不到 `const zh = { ... };`，脚本需要跟着改");
// 对象字面量本身就是合法 JS（只有字符串与嵌套对象），直接求值
const zh = new Function(`${text.slice(start, end + 3)}\nreturn zh;`)();

function convert(node) {
  if (typeof node === "string") return node.replace(/\{\{\s*(\w+)\s*\}\}/g, "%{$1}");
  const out = {};
  for (const [k, v] of Object.entries(node)) out[k] = convert(v);
  return out;
}

function merge(base, extra, prefix = "") {
  for (const [k, v] of Object.entries(extra)) {
    const key = prefix ? `${prefix}.${k}` : k;
    if (v && typeof v === "object") {
      if (typeof base[k] === "string") throw new Error(`${key} 在 web 里是字符串，原生里是对象`);
      base[k] = base[k] ?? {};
      merge(base[k], v, key);
    } else {
      if (base[k] !== undefined) console.warn(`原生覆盖了 web 的文案：${key}`);
      base[k] = v;
    }
  }
}

const result = convert(zh.translation);
if (fs.existsSync(NATIVE_DIR)) {
  for (const f of fs.readdirSync(NATIVE_DIR).filter((f) => f.endsWith(".json")).sort()) {
    merge(result, convert(JSON.parse(fs.readFileSync(path.join(NATIVE_DIR, f), "utf8"))));
  }
}
fs.mkdirSync(OUT_DIR, { recursive: true });
fs.writeFileSync(OUT, JSON.stringify(result, null, 2) + "\n");
const count = (n) => (typeof n === "string" ? 1 : Object.values(n).reduce((a, v) => a + count(v), 0));
console.log(`写出 ${path.relative(ROOT, OUT)}：${count(result)} 条`);
