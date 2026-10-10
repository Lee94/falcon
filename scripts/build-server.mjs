#!/usr/bin/env node
/**
 * 把服务端（native/crates/falcon-server）打成单个可执行文件。
 *
 *   pnpm build:bin                        # 打当前平台
 *   pnpm build:bin --skip-web             # web 产物已是最新时跳过 web 构建
 *   pnpm build:bin --target linux-x64     # 交叉编译：需要先 rustup target add 对应三元组并配好链接器
 *
 * 产物 release/falcon-v<版本>-<平台>，build-macos-pkg.mjs 把它放进 Falcon.app 的 Resources，
 * `falcon service install` 再把它拷到 <dataDir>/bin/falcon。不需要首次运行的解压步骤：
 *   - 浏览器版客户端（native/scripts/build-web.sh 的产物，rust-embed）与 Zellij 滚动插件
 *     直接从二进制里服务；
 *   - 内置的 meegle CLI 编进二进制，首次用到时释放到 <dataDir>/bin/meegle-<内容哈希>。
 *
 * 浏览器版要 rustup 的 wasm32-unknown-unknown target 与同版本的 wasm-bindgen-cli（见 build-web.sh）。
 */
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const NATIVE = path.join(ROOT, "native");
const RELEASE = path.join(ROOT, "release");

/** 平台名 → Rust 目标三元组。与 SEA 的 KNOWN_TARGETS 同一组（Windows 不做本机服务） */
const TRIPLES = {
  "darwin-arm64": "aarch64-apple-darwin",
  "darwin-x64": "x86_64-apple-darwin",
  "linux-x64": "x86_64-unknown-linux-gnu",
  "linux-arm64": "aarch64-unknown-linux-gnu",
};

// 发布版本号：仓库根 package.json（与 native/Cargo.toml 的工作区版本一起改）
const VERSION = readJson(path.join(ROOT, "package.json")).version;
const HOST = `${process.platform}-${process.arch}`;

const argv = process.argv.slice(2);
let target = HOST;
let skipWeb = false;
for (let i = 0; i < argv.length; i++) {
  if (argv[i] === "--target") target = argv[++i];
  else if (argv[i] === "--skip-web") skipWeb = true;
  else {
    console.error(`未知参数: ${argv[i]}`);
    process.exit(1);
  }
}
if (!TRIPLES[target]) {
  console.error(`不支持的目标: ${target}（可选: ${Object.keys(TRIPLES).join(", ")}）`);
  process.exit(1);
}

function readJson(p) {
  return JSON.parse(fs.readFileSync(p, "utf8"));
}

function run(cmd, args, opts = {}) {
  execFileSync(cmd, args, { stdio: "inherit", cwd: ROOT, ...opts });
}

// ---- 1. 浏览器版客户端（GPUI 编到 wasm）----
const webDist = path.join(NATIVE, "target-wasm/dist");
if (!skipWeb) {
  console.log("== 构建浏览器版（native/scripts/build-web.sh） ==");
  run(path.join(NATIVE, "scripts/build-web.sh"), []);
}
if (!fs.existsSync(path.join(webDist, "index.html"))) {
  console.error("native/target-wasm/dist 缺少 index.html，先跑 native/scripts/build-web.sh");
  process.exit(1);
}

// ---- 2. 本平台的 meegle CLI（npm 包自带六个平台的静态二进制，根 package.json 锁定版本）----
const rootRequire = createRequire(path.join(ROOT, "package.json"));
const meeglePkgDir = path.dirname(
  fs.realpathSync(rootRequire.resolve("@lark-project/meegle/package.json"))
);
const meegleBin = path.join(meeglePkgDir, "bin", `meegle-${target}`);
if (!fs.existsSync(meegleBin)) {
  console.error(`@lark-project/meegle 没有 ${target} 的二进制: ${meegleBin}`);
  process.exit(1);
}

// ---- 3. cargo ----
// rustup 的工具链排在前面：PATH 上的 Homebrew rustc 没有交叉目标的标准库（build-web.sh 同理）
// 发布产物不带调试信息（工作区的 release profile 为原生客户端留着 debuginfo）。改 profile 会让
// 依赖全部重编，所以单独一个 target 目录，不跟 falcon-app 的 release 缓存互相冲掉
const TARGET_DIR = path.join(NATIVE, "target-server");
const cargoArgs = [
  "build",
  "--release",
  "-p",
  "falcon-server",
  "--features",
  "embed-web,embed-meegle",
  "--config",
  "profile.release.debug=false",
  "--config",
  'profile.release.strip="symbols"',
];
if (target !== HOST) cargoArgs.push("--target", TRIPLES[target]);
console.log(`== cargo ${cargoArgs.join(" ")} ==`);
run("cargo", cargoArgs, {
  cwd: NATIVE,
  env: {
    ...process.env,
    FALCON_EMBED_WEB_DIR: webDist,
    FALCON_EMBED_MEEGLE_BIN: meegleBin,
    CARGO_TARGET_DIR: TARGET_DIR,
  },
});

const built = path.join(
  TARGET_DIR,
  ...(target !== HOST ? [TRIPLES[target]] : []),
  "release",
  "falcon-server"
);
fs.mkdirSync(RELEASE, { recursive: true });
const out = path.join(RELEASE, `falcon-v${VERSION}-${target}`);
// 先删再拷：覆盖一个正在跑的同名文件时，macOS 会因改了已映射的签名文件把它 SIGKILL
fs.rmSync(out, { force: true });
fs.copyFileSync(built, out);
fs.chmodSync(out, 0o755);
const mb = (fs.statSync(out).size / 1024 / 1024).toFixed(1);
console.log(`== 完成：${path.relative(ROOT, out)}（${mb} MB）==`);
