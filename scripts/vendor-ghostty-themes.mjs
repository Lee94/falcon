#!/usr/bin/env node
/**
 * 把 Ghostty 内置主题（iTerm2-Color-Schemes 的 ghostty/ 目录）压成一个 TS 模块，
 * 放到 packages/web/src/assets/themes/ghostty-themes.ts。
 *
 *   node scripts/vendor-ghostty-themes.mjs              # 优先用本机 Ghostty.app 里的主题
 *   node scripts/vendor-ghostty-themes.mjs --from <dir> # 指定主题目录
 *   node scripts/vendor-ghostty-themes.mjs --github     # 从 GitHub 稀疏克隆
 *
 * 本机 Ghostty.app 的主题目录就是"用户装的那个 Ghostty 认的主题"，名字与
 * `ghostty +list-themes` 一字不差，优先用它；没装 Ghostty 才去 GitHub 拉。
 * 产物每行一个主题：`名字 \t bg fg cursor cursorText selBg selFg p0..p15`，
 * 22 个不带 # 的 rrggbb 用空格隔开——比 JSON 小一半，解析在 lib/theme/catalog.ts。
 */

import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const OUT_DIR = path.join(ROOT, "packages/web/src/assets/themes");
const OUT_TS = path.join(OUT_DIR, "ghostty-themes.ts");
/** 条数与来源单独一个小模块：设置页要显示，但不该为此背上 77KB 的数据 */
const OUT_META = path.join(OUT_DIR, "ghostty-themes.meta.ts");
const OUT_LICENSE = path.join(OUT_DIR, "LICENSE.txt");
const APP_THEMES = "/Applications/Ghostty.app/Contents/Resources/ghostty/themes";
const APP_PLIST = "/Applications/Ghostty.app/Contents/Info.plist";
const REPO = "https://github.com/mbadolato/iTerm2-Color-Schemes";
const LICENSE_URL = "https://raw.githubusercontent.com/mbadolato/iTerm2-Color-Schemes/master/LICENSE";

/** 键的顺序就是产物里 22 个颜色的顺序，lib/theme/catalog.ts 按同一顺序解析 */
const KEYS = [
  "background",
  "foreground",
  "cursor-color",
  "cursor-text",
  "selection-background",
  "selection-foreground",
];

function parseArgs(argv) {
  const opts = { from: null, github: false };
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === "--from") opts.from = argv[++i];
    else if (argv[i] === "--github") opts.github = true;
    else throw new Error(`未知参数 ${argv[i]}`);
  }
  return opts;
}

function ghosttyVersion() {
  try {
    return execFileSync("/usr/libexec/PlistBuddy", ["-c", "Print CFBundleShortVersionString", APP_PLIST], {
      encoding: "utf8",
    }).trim();
  } catch {
    return null;
  }
}

function sparseClone() {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "falcon-ghostty-themes-"));
  console.log(`稀疏克隆 ${REPO} 到 ${tmp}`);
  execFileSync("git", ["clone", "--depth", "1", "--filter=blob:none", "--sparse", REPO, tmp], {
    stdio: "inherit",
  });
  execFileSync("git", ["-C", tmp, "sparse-checkout", "set", "ghostty"], { stdio: "inherit" });
  const commit = execFileSync("git", ["-C", tmp, "rev-parse", "--short", "HEAD"], {
    encoding: "utf8",
  }).trim();
  return { dir: path.join(tmp, "ghostty"), commit };
}

/**
 * 只认内置主题这种规整写法（每个键都有、全是 #rrggbb）。用户手写主题的宽松解析
 * （不带 #、X11 颜色名、cell-foreground 之类特殊值）在 lib/theme/ghostty.ts。
 */
function parseTheme(name, text) {
  const values = new Map();
  const palette = new Array(16).fill(null);
  for (const raw of text.split("\n")) {
    const line = raw.trim();
    if (!line || line.startsWith("#")) continue;
    const eq = line.indexOf("=");
    if (eq < 0) continue;
    const key = line.slice(0, eq).trim();
    const value = line.slice(eq + 1).trim();
    if (key === "palette") {
      const m = /^(\d+)\s*=\s*#?([0-9a-fA-F]{6})$/.exec(value);
      if (!m) throw new Error(`${name}: 看不懂的 palette 行「${raw}」`);
      const idx = Number(m[1]);
      if (idx > 15) continue;
      palette[idx] = m[2].toLowerCase();
    } else if (KEYS.includes(key)) {
      const m = /^#?([0-9a-fA-F]{6})$/.exec(value);
      if (!m) throw new Error(`${name}: ${key} 不是 #rrggbb：「${value}」`);
      values.set(key, m[1].toLowerCase());
    }
  }
  const out = [];
  for (const key of KEYS) {
    const v = values.get(key);
    if (!v) throw new Error(`${name}: 缺 ${key}`);
    out.push(v);
  }
  for (let i = 0; i < 16; i++) {
    if (!palette[i]) throw new Error(`${name}: 缺 palette ${i}`);
    out.push(palette[i]);
  }
  return out;
}

async function fetchLicense() {
  try {
    const res = await fetch(LICENSE_URL, { headers: { "user-agent": "falcon-vendor" } });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    fs.writeFileSync(OUT_LICENSE, await res.text());
    console.log(`写入 ${path.relative(ROOT, OUT_LICENSE)}`);
  } catch (err) {
    if (fs.existsSync(OUT_LICENSE)) {
      console.warn(`LICENSE 下载失败（${err.message}），沿用已有文件`);
    } else {
      throw new Error(`LICENSE 下载失败且本地没有副本：${err.message}`);
    }
  }
}

async function main() {
  const opts = parseArgs(process.argv.slice(2));
  let dir = opts.from;
  let origin;
  if (!dir && !opts.github && fs.existsSync(APP_THEMES)) {
    dir = APP_THEMES;
    origin = `Ghostty.app ${ghosttyVersion() ?? "?"}`;
  } else if (!dir) {
    const cloned = sparseClone();
    dir = cloned.dir;
    origin = `${REPO} @ ${cloned.commit}`;
  } else {
    origin = dir;
  }

  const names = fs
    .readdirSync(dir)
    .filter((f) => !f.startsWith(".") && fs.statSync(path.join(dir, f)).isFile())
    .sort((a, b) => a.localeCompare(b, "en"));
  const lines = [];
  for (const name of names) {
    if (/[\t\n\r`\\]|\$\{/.test(name)) throw new Error(`主题名含不能进模板字符串的字符：${name}`);
    const colors = parseTheme(name, fs.readFileSync(path.join(dir, name), "utf8"));
    lines.push(`${name}\t${colors.join(" ")}`);
  }

  fs.mkdirSync(OUT_DIR, { recursive: true });
  const header = [
    "/**",
    " * 由 scripts/vendor-ghostty-themes.mjs 生成，勿手改。",
    ` * 来源：${origin}（mbadolato/iTerm2-Color-Schemes，MIT，见同目录 LICENSE.txt）。`,
    " *",
    " * 每行一个主题：名字 \\t bg fg cursor cursorText selBg selFg palette0..15，",
    " * 22 个不带 # 的 rrggbb 以空格分隔。解析见 lib/theme/catalog.ts。",
    " */",
    "",
  ].join("\n");
  fs.writeFileSync(OUT_TS, header + `export const GHOSTTY_THEMES_DATA = \`${lines.join("\n")}\`;\n`);
  fs.writeFileSync(
    OUT_META,
    `/** 由 scripts/vendor-ghostty-themes.mjs 生成，勿手改。 */\n` +
      `export const GHOSTTY_THEMES_ORIGIN = ${JSON.stringify(origin)};\n` +
      `export const GHOSTTY_THEMES_COUNT = ${lines.length};\n`
  );
  console.log(`写入 ${path.relative(ROOT, OUT_TS)}：${lines.length} 个主题，来源 ${origin}`);
  await fetchLicense();
}

main().catch((err) => {
  console.error(err.message);
  process.exit(1);
});
