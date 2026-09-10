#!/usr/bin/env node
/**
 * 把 falcon 打包成单个可执行文件（Node SEA，Single Executable Application）。
 *
 *   pnpm build:bin                          # 打当前平台
 *   pnpm build:bin --target linux-x64      # 交叉打包（可逗号分隔或传 all）
 *   pnpm build:bin --skip-build            # 跳过 pnpm build（dist 已是最新时）
 *
 * 产物在 release/ 下。工作原理：
 *   1. esbuild 把 server 打成单个 CJS bundle（@lydell/node-pty 保持 external）；
 *   2. 生成 SEA bootstrap：首次运行把 pty.node / spawn-helper / web 静态资源
 *      解压到 <dataDir>/runtime/<hash>/，再以该目录为根 createRequire 执行 bundle，
 *      因此 node-pty 的动态 require 与 spawn-helper 都按真实文件系统解析，无需 patch；
 *   3. 资产（web 产物、node-pty、内置的 meegle CLI 二进制）嵌入 SEA blob，注入到
 *      nodejs.org 官方二进制（postject），macOS 重新 ad-hoc 签名。
 *
 * 不支持 Windows 目标：服务本身依赖 zellij 与 POSIX shell。
 */
import { execFileSync } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";
import esbuild from "esbuild";
import { inject } from "postject";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const BUILD = path.join(ROOT, "build");
const CACHE = path.join(BUILD, "cache");
const RELEASE = path.join(ROOT, "release");

/** 发布产物内嵌的 Node 运行时版本（官方 nodejs.org 分发，ICU 内置、无动态库依赖） */
const NODE_VERSION =
  process.env.FALCON_NODE_VERSION ?? process.env.MOJITO_NODE_VERSION ?? process.version.slice(1);
const SEA_FUSE = "NODE_SEA_FUSE_fce680ab2cc467b6e072b8b5df1996b2";
const KNOWN_TARGETS = ["darwin-arm64", "darwin-x64", "linux-x64", "linux-arm64"];

const serverPkg = readJson(path.join(ROOT, "packages/server/package.json"));
const VERSION = serverPkg.version;

// ---- CLI 参数 ----
const argv = process.argv.slice(2);
let targets = [`${process.platform}-${process.arch}`];
let skipBuild = false;
for (let i = 0; i < argv.length; i++) {
  if (argv[i] === "--target") {
    const v = argv[++i];
    targets = v === "all" ? KNOWN_TARGETS : v.split(",");
  } else if (argv[i] === "--skip-build") {
    skipBuild = true;
  } else {
    console.error(`未知参数: ${argv[i]}`);
    process.exit(1);
  }
}
for (const t of targets) {
  if (!KNOWN_TARGETS.includes(t)) {
    console.error(`不支持的目标: ${t}（可选: ${KNOWN_TARGETS.join(", ")}, all）`);
    process.exit(1);
  }
}

function readJson(p) {
  return JSON.parse(fs.readFileSync(p, "utf8"));
}

function run(cmd, args, opts = {}) {
  execFileSync(cmd, args, { stdio: "inherit", cwd: ROOT, ...opts });
}

/** 递归收集 dir 下全部文件，返回相对路径列表 */
function walk(dir, prefix = "") {
  const out = [];
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    const rel = prefix ? `${prefix}/${e.name}` : e.name;
    if (e.isDirectory()) out.push(...walk(path.join(dir, e.name), rel));
    else if (e.isFile()) out.push(rel);
  }
  return out;
}

async function download(url, dest) {
  if (fs.existsSync(dest)) return;
  console.log(`  下载 ${url}`);
  const res = await fetch(url);
  if (!res.ok) throw new Error(`下载失败 ${res.status}: ${url}`);
  fs.mkdirSync(path.dirname(dest), { recursive: true });
  const tmp = `${dest}.part`;
  fs.writeFileSync(tmp, Buffer.from(await res.arrayBuffer()));
  fs.renameSync(tmp, dest);
}

/** 下载并解压 @lydell/node-pty 的平台预编译包，返回解压后的 package 目录 */
async function fetchPtyPlatformPkg(target, version) {
  const name = `node-pty-${target}`;
  const dir = path.join(CACHE, `${name}-${version}`);
  if (!fs.existsSync(path.join(dir, "package", "package.json"))) {
    const tgz = path.join(CACHE, `${name}-${version}.tgz`);
    await download(`https://registry.npmjs.org/@lydell/${name}/-/${name}-${version}.tgz`, tgz);
    fs.mkdirSync(dir, { recursive: true });
    run("tar", ["-xzf", tgz, "-C", dir]);
  }
  return path.join(dir, "package");
}

/** 下载并解压目标平台的官方 Node，返回 node 可执行文件路径 */
async function fetchNodeBinary(target) {
  const name = `node-v${NODE_VERSION}-${target}`;
  const bin = path.join(CACHE, name, "bin", "node");
  if (!fs.existsSync(bin)) {
    const tgz = path.join(CACHE, `${name}.tar.gz`);
    await download(`https://nodejs.org/dist/v${NODE_VERSION}/${name}.tar.gz`, tgz);
    run("tar", ["-xzf", tgz, "-C", CACHE]);
  }
  return bin;
}

// ---- 1. 构建 workspace ----
if (!skipBuild) {
  console.log("== pnpm build ==");
  run("pnpm", ["build"]);
}

// ---- 2. esbuild：server → 单个 CJS bundle（平台无关，只做一次）----
console.log("== esbuild bundle ==");
fs.mkdirSync(BUILD, { recursive: true });
const bundlePath = path.join(BUILD, "bundle.cjs");
await esbuild.build({
  entryPoints: [path.join(ROOT, "packages/server/dist/index.js")],
  bundle: true,
  platform: "node",
  format: "cjs",
  target: "node24",
  outfile: bundlePath,
  // node-pty 留在磁盘上解析（原生扩展 + spawn-helper）；其余是有纯 JS 回退的可选原生依赖
  external: ["@lydell/node-pty", "cpu-features", "*.node", "bufferutil", "utf-8-validate"],
  define: { "import.meta.url": "__importMetaUrl" },
  banner: { js: 'const __importMetaUrl = require("node:url").pathToFileURL(__filename).href;' },
  logLevel: "warning",
});
const bundleSrc = fs.readFileSync(bundlePath, "utf8").replace(/^#!.*\n/, "");

// node-pty 主包（纯 JS 部分，平台无关）的真实目录
const serverRequire = createRequire(path.join(ROOT, "packages/server/package.json"));
const ptyPkgDir = path.dirname(
  fs.realpathSync(serverRequire.resolve("@lydell/node-pty/package.json"))
);
const ptyVersion = readJson(path.join(ptyPkgDir, "package.json")).version;
// 飞书项目面板的数据源：npm 包自带六个平台的静态二进制，按目标各取一个
const meeglePkgDir = path.dirname(
  fs.realpathSync(serverRequire.resolve("@lark-project/meegle/package.json"))
);
const webDist = path.join(ROOT, "packages/web/dist");
if (!fs.existsSync(path.join(webDist, "index.html"))) {
  console.error("packages/web/dist 缺少 index.html，请先构建 web");
  process.exit(1);
}

// ---- 3. 逐目标产出 ----
fs.mkdirSync(RELEASE, { recursive: true });
// 生成 blob 也用官方 Node：发行版（如 Homebrew）可能在编译时禁用了 SEA
const hostNode = await fetchNodeBinary(`${process.platform}-${process.arch}`);
for (const target of targets) {
  console.log(`== 打包 ${target} ==`);
  const outDir = path.join(BUILD, target);
  fs.rmSync(outDir, { recursive: true, force: true });
  fs.mkdirSync(outDir, { recursive: true });

  // 3a. 资产清单：runtime 目录相对路径 -> 源文件
  const ptyPlatformDir = await fetchPtyPlatformPkg(target, ptyVersion);
  /** @type {{key: string, src: string, mode: number}[]} */
  const assets = [];
  const addTree = (srcDir, destPrefix) => {
    for (const rel of walk(srcDir)) {
      const src = path.join(srcDir, rel);
      assets.push({
        key: `${destPrefix}/${rel}`,
        src,
        mode: fs.statSync(src).mode & 0o777,
      });
    }
  };
  addTree(ptyPkgDir, "node_modules/@lydell/node-pty");
  addTree(ptyPlatformDir, `node_modules/@lydell/node-pty-${target}`);
  addTree(webDist, "web");
  const meegleBin = path.join(meeglePkgDir, "bin", `meegle-${target}`);
  if (!fs.existsSync(meegleBin)) {
    console.error(`@lark-project/meegle 没有 ${target} 的二进制: ${meegleBin}`);
    process.exit(1);
  }
  assets.push({ key: "bin/meegle", src: meegleBin, mode: 0o755 });

  const hasher = crypto.createHash("sha256");
  hasher.update(`${VERSION}\0${target}\0`);
  for (const a of assets.sort((x, y) => (x.key < y.key ? -1 : 1))) {
    hasher.update(a.key);
    hasher.update(String(a.mode));
    hasher.update(fs.readFileSync(a.src));
  }
  hasher.update(bundleSrc);
  const hash = hasher.digest("hex").slice(0, 16);

  // 3b. SEA 主脚本 = bootstrap（解压资产 + createRequire）+ 包裹的 bundle
  const manifest = {
    version: VERSION,
    hash,
    files: assets.map((a) => ({ key: a.key, mode: a.mode })),
  };
  const prelude = `"use strict";
// falcon SEA bootstrap —— 由 scripts/build-binary.mjs 生成，勿手改
const MANIFEST = ${JSON.stringify(manifest)};
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { createRequire } = require("node:module");

let seaMode = false;
try { seaMode = require("node:sea").isSea(); } catch {}

let anchor; // bundle 以此文件为基准解析磁盘上的依赖
if (seaMode) {
  const sea = require("node:sea");
  // data dir 的解析须与 packages/server/src/config.ts parseArgs / defaultDataDir 一致
  let dataDir = process.env.FALCON_DATA_DIR ?? process.env.MOJITO_DATA_DIR;
  if (!dataDir) {
    const next = path.join(os.homedir(), ".falcon");
    const prev = path.join(os.homedir(), ".mojito");
    dataDir = (!fs.existsSync(next) && fs.existsSync(prev)) ? prev : next;
  }
  const i = process.argv.indexOf("--data-dir");
  if (i !== -1 && process.argv[i + 1]) dataDir = process.argv[i + 1];

  const runtimeRoot = path.join(dataDir, "runtime");
  const runtimeDir = path.join(runtimeRoot, MANIFEST.hash);
  if (!fs.existsSync(path.join(runtimeDir, ".complete"))) {
    const tmp = runtimeDir + ".tmp-" + process.pid;
    for (const f of MANIFEST.files) {
      const dest = path.join(tmp, f.key);
      fs.mkdirSync(path.dirname(dest), { recursive: true });
      fs.writeFileSync(dest, Buffer.from(sea.getAsset(f.key)), { mode: f.mode });
    }
    fs.writeFileSync(path.join(tmp, ".complete"), "");
    try {
      fs.renameSync(tmp, runtimeDir);
    } catch (err) {
      // 并发启动时另一进程可能已解压完成
      if (!fs.existsSync(path.join(runtimeDir, ".complete"))) throw err;
      fs.rmSync(tmp, { recursive: true, force: true });
    }
  }
  // 清理旧版本残留（runtime 下全部内容都是本 bootstrap 解压的）
  for (const e of fs.readdirSync(runtimeRoot)) {
    if (e !== MANIFEST.hash) {
      fs.rmSync(path.join(runtimeRoot, e), { recursive: true, force: true });
    }
  }
  // 这个 env 会随 PTY 传给会话里的 shell：从 falcon 终端里再启一个 falcon 时，
  // 子实例会继承父实例的 FALCON_WEB_DIST。父实例升级后旧 runtime 已被上面的
  // 清理删掉，继承值指向不存在的目录，web UI 会静默 404。显式 override 仍然
  // 尊重，但指向的目录必须真有 index.html，否则视为陈旧继承、换成自己的。
  const inheritedWebDist = process.env.FALCON_WEB_DIST ?? process.env.MOJITO_WEB_DIST;
  if (!inheritedWebDist || !fs.existsSync(path.join(inheritedWebDist, "index.html"))) {
    process.env.FALCON_WEB_DIST = path.join(runtimeDir, "web");
  }
  // 内置的 meegle CLI 同样从 runtime 目录指入（bundle 里没有 node_modules 可解析，
  // 见 packages/server/src/meegle/bin.ts）；继承自父实例的陈旧路径同理换成自己的
  const inheritedMeegle = process.env.FALCON_MEEGLE_BIN;
  if (!inheritedMeegle || !fs.existsSync(inheritedMeegle)) {
    process.env.FALCON_MEEGLE_BIN = path.join(runtimeDir, "bin", "meegle");
  }
  anchor = path.join(runtimeDir, "sea-loader.cjs");
} else {
  // 未注入 SEA 时直接 node 运行本文件：按仓库布局解析，用于打包产物的快速自检
  anchor = ${JSON.stringify(path.join(ROOT, "packages/server/dist/index.js"))};
}

const __seaRequire = createRequire(anchor);
const __seaModule = { exports: {} };
(function (require, module, exports, __filename, __dirname) {
`;
  const postlude = `
})(__seaRequire, __seaModule, __seaModule.exports, anchor, path.dirname(anchor));
`;
  const seaMain = path.join(outDir, "sea-main.cjs");
  fs.writeFileSync(seaMain, prelude + bundleSrc + postlude);

  // 3c. 生成 SEA blob
  const blobPath = path.join(outDir, "sea.blob");
  const seaConfig = {
    main: seaMain,
    output: blobPath,
    disableExperimentalSEAWarning: true,
    assets: Object.fromEntries(assets.map((a) => [a.key, a.src])),
  };
  const configPath = path.join(outDir, "sea-config.json");
  fs.writeFileSync(configPath, JSON.stringify(seaConfig, null, 2));
  run(hostNode, ["--experimental-sea-config", configPath]);

  // 3d. 官方 Node 二进制 + postject 注入
  const nodeBin = await fetchNodeBinary(target);
  const outBin = path.join(RELEASE, `falcon-v${VERSION}-${target}`);
  fs.copyFileSync(nodeBin, outBin);
  fs.chmodSync(outBin, 0o755);
  const isDarwin = target.startsWith("darwin");
  if (isDarwin && process.platform === "darwin") {
    run("codesign", ["--remove-signature", outBin]);
  }
  console.log("  注入 SEA blob…");
  await inject(outBin, "NODE_SEA_BLOB", fs.readFileSync(blobPath), {
    sentinelFuse: SEA_FUSE,
    ...(isDarwin ? { machoSegmentName: "NODE_SEA" } : {}),
  });
  if (isDarwin && process.platform === "darwin") {
    run("codesign", ["--sign", "-", outBin]);
  }
  const mb = (fs.statSync(outBin).size / 1024 / 1024).toFixed(1);
  console.log(`  ✓ ${path.relative(ROOT, outBin)}（${mb} MB）`);
}

console.log("完成。");
