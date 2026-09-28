#!/usr/bin/env node
/**
 * 把 web 的主题数据导出给原生客户端的 falcon-theme crate（Rust），并用 web 的
 * derive.ts 把全部内置主题跑一遍，产出 Rust 侧对拍用的金标准。
 *
 * 何时重跑：
 *   - `pnpm vendor-ghostty-themes` 重新 vendor 了 Ghostty 主题之后（目录变了）；
 *   - web 的 lib/theme/derive.ts / color.ts / pref.ts / catalog.ts 改了规则之后
 *     （金标准变了）。重跑后 Rust 那边 `cargo test -p falcon-theme` 会红在
 *     tests/derive_golden.rs，照着 TS 的改动改 Rust，直到重新全绿。
 *   TS 是真相来源：别手改这里的任何产物，也别为了让 Rust 变绿去改金标准。
 *
 * 跑法（必须经 tsx——要直接 import web 的 TS 源码；拿到的是求值后的真实字符串，
 * 不用自己去抠模板字符串；derive.ts 里 `./color.js` 这种导入 node 自己的类型剥离
 * 解析不到 .ts，tsx 可以）：
 *
 *   pnpm --filter @falcon/server exec tsx ../../native/scripts/export-ghostty-themes.mjs
 *
 * 路径一律按本文件位置算，与 cwd 无关。derive.ts 从 @falcon/shared 取
 * appearanceFromHex，走的是 packages/shared/dist——shared 改过要先 build。
 *
 * 产物（都在 native/crates/falcon-theme/ 下）：
 *   data/ghostty-themes.tsv       与 web 的 GHOSTTY_THEMES_DATA 逐字节相同（外加末尾换行）：
 *                                 每行 `名字 \t 22 个不带 # 的 rrggbb`，顺序
 *                                 bg fg cursor cursorText selBg selFg palette0..15。
 *                                 Rust 用 include_str! 嵌进去，按需解析（见 src/catalog.rs）。
 *   data/ghostty-themes.meta.rs   条数与来源，include! 进 crate。
 *   data/LICENSE.txt              主题数据的 MIT 授权（mbadolato/iTerm2-Color-Schemes），原样复制。
 *   tests/fixtures/derive-golden.tsv
 *                                 Falcon 两套 + 全部 Ghostty 主题经 deriveTheme 得到的每个
 *                                 CSS 变量，Rust 的 css_vars() 必须逐字相同。
 *   tests/fixtures/pref-default.json
 *                                 JSON.stringify(DEFAULT_THEME_SETTINGS)，钉住偏好文件的形状
 *                                 （localStorage `falcon.themes` 与原生的偏好文件要能对照）。
 */

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(HERE, "../..");
const WEB_THEMES = path.join(ROOT, "packages/web/src/assets/themes");
const WEB_THEME_LIB = path.join(ROOT, "packages/web/src/lib/theme");
const CRATE = path.join(ROOT, "native/crates/falcon-theme");
const DATA_DIR = path.join(CRATE, "data");
const FIXTURE_DIR = path.join(CRATE, "tests/fixtures");

const LINE = /^[^\t\n\r]+\t(?:[\da-f]{6} ){21}[\da-f]{6}$/;

async function importTs(file) {
  try {
    return await import(pathToFileURL(file).href);
  } catch (err) {
    throw new Error(
      `导入 ${path.relative(ROOT, file)} 失败（${err.message}）。\n` +
        "这个脚本要经 tsx 跑：pnpm --filter @falcon/server exec tsx ../../native/scripts/export-ghostty-themes.mjs"
    );
  }
}

/** Rust 字符串字面量的转义：JSON.stringify 的 \uXXXX 在 Rust 里不合法 */
function rustString(s) {
  let out = '"';
  for (const ch of s) {
    const cp = ch.codePointAt(0);
    if (ch === "\\" || ch === '"') out += `\\${ch}`;
    else if (cp < 0x20 || cp === 0x7f) out += `\\u{${cp.toString(16)}}`;
    else out += ch;
  }
  return `${out}"`;
}

function write(file, content) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const old = fs.existsSync(file) ? fs.readFileSync(file, "utf8") : null;
  fs.writeFileSync(file, content);
  const tag = old === null ? "新建" : old === content ? "未变" : "更新";
  console.log(`${tag} ${path.relative(ROOT, file)}（${Buffer.byteLength(content)} 字节）`);
}

async function main() {
  const { GHOSTTY_THEMES_DATA } = await importTs(path.join(WEB_THEMES, "ghostty-themes.ts"));
  const { GHOSTTY_THEMES_COUNT, GHOSTTY_THEMES_ORIGIN } = await importTs(
    path.join(WEB_THEMES, "ghostty-themes.meta.ts")
  );

  // 先在这边把数据核一遍：Rust 那边的解析是"坏行直接报错"，这里挡住就不会产出坏文件
  const lines = GHOSTTY_THEMES_DATA.split("\n");
  const names = new Set();
  for (const line of lines) {
    if (!LINE.test(line)) throw new Error(`主题数据坏行：${line.slice(0, 60)}`);
    const name = line.slice(0, line.indexOf("\t"));
    if (names.has(name)) throw new Error(`主题名重复：${name}`);
    names.add(name);
  }
  if (lines.length !== GHOSTTY_THEMES_COUNT) {
    throw new Error(`条数 ${lines.length} 与 meta 记录的 ${GHOSTTY_THEMES_COUNT} 不一致`);
  }

  write(path.join(DATA_DIR, "ghostty-themes.tsv"), `${GHOSTTY_THEMES_DATA}\n`);
  write(
    path.join(DATA_DIR, "ghostty-themes.meta.rs"),
    [
      "// 由 native/scripts/export-ghostty-themes.mjs 生成，勿手改。",
      "// 值来自 packages/web/src/assets/themes/ghostty-themes.meta.ts。",
      "",
      "/// 内置 Ghostty 主题的来源（设置页显示用）",
      `pub const GHOSTTY_THEMES_ORIGIN: &str = ${rustString(GHOSTTY_THEMES_ORIGIN)};`,
      "/// 内置 Ghostty 主题条数；测试按它核对 data/ghostty-themes.tsv 的行数",
      `pub const GHOSTTY_THEMES_COUNT: usize = ${GHOSTTY_THEMES_COUNT};`,
      "",
    ].join("\n")
  );
  fs.copyFileSync(path.join(WEB_THEMES, "LICENSE.txt"), path.join(DATA_DIR, "LICENSE.txt"));
  console.log(`复制 ${path.relative(ROOT, path.join(DATA_DIR, "LICENSE.txt"))}`);

  // ---- 金标准：web 的 deriveTheme 跑全部主题 ----
  const { deriveTheme } = await importTs(path.join(WEB_THEME_LIB, "derive.ts"));
  const { FALCON_THEMES, parseCatalogData } = await importTs(path.join(WEB_THEME_LIB, "catalog.ts"));
  const { DEFAULT_THEME_SETTINGS } = await importTs(path.join(WEB_THEME_LIB, "pref.ts"));

  const entries = [...FALCON_THEMES, ...parseCatalogData(GHOSTTY_THEMES_DATA)];
  const keys = Object.keys(deriveTheme(entries[0].colors).vars);
  const golden = [
    "# 由 native/scripts/export-ghostty-themes.mjs 生成，勿手改。",
    "# web 的 deriveTheme 对每套内置主题算出的 CSS 变量：",
    "# `@keys` 行是变量名顺序（即 derive.ts 里 vars 的插入顺序）；其余每行",
    "# `名字 \\t 深浅 \\t 值…`，值以空格分隔，#rrggbb[aa] 去掉 #，其他（34% / currentcolor）原样。",
    `@keys\t${keys.join(" ")}`,
  ];
  for (const e of entries) {
    const t = deriveTheme(e.colors);
    const got = Object.keys(t.vars);
    if (got.join(" ") !== keys.join(" ")) throw new Error(`${e.name} 的变量顺序与第一套不同`);
    const values = keys.map((k) => {
      const v = t.vars[k];
      if (/\s/.test(v)) throw new Error(`${e.name} ${k}=${v} 含空白，金标准格式装不下`);
      return v.startsWith("#") ? v.slice(1) : v;
    });
    golden.push(`${e.name}\t${t.appearance}\t${values.join(" ")}`);
  }
  write(path.join(FIXTURE_DIR, "derive-golden.tsv"), `${golden.join("\n")}\n`);
  write(path.join(FIXTURE_DIR, "pref-default.json"), `${JSON.stringify(DEFAULT_THEME_SETTINGS)}\n`);

  console.log(`导出 ${lines.length} 套 Ghostty 主题（来源 ${GHOSTTY_THEMES_ORIGIN}），金标准 ${entries.length} 套 × ${keys.length} 个变量`);
}

main().catch((err) => {
  console.error(err.message);
  process.exit(1);
});
