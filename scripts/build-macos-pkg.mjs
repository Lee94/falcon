#!/usr/bin/env node
/**
 * 打一份 macOS 安装包（.pkg）：把 Falcon.app 装进 /Applications，postinstall
 * 以当前登录用户跑 `falcon service install`（用户级 LaunchAgent，不能用 root
 * 的 guid），装过服务的话带上原 plist 里的 --host / --port / --data-dir。
 *
 * App 的可执行文件默认是原生客户端（native/，GPUI，设计见 docs/design/gpui-client.md）：
 * 它启动时自己做 launcher.sh 那两步（service install → 等端口），然后直接连本机服务。
 * `--launcher` 退回旧的 launcher.sh（注册服务后用浏览器打开 web 界面）。
 *
 *   pnpm build:pkg                 # 没有当前平台 SEA 就先 pnpm build:bin
 *   pnpm build:pkg --skip-bin      # 必须已有 release/falcon-v*-darwin-*
 *   pnpm build:pkg --launcher      # App 里放 launcher.sh 而不是原生客户端
 *
 * 产物 release/Falcon-v<版本>-darwin-<arch>.pkg。无开发者证书，只做 ad-hoc
 * 签名——别人机器上 Gatekeeper 会拦，系统设置里「仍要打开」即可。
 *
 * 必须在 macOS 上跑（pkgbuild / iconutil / codesign）。
 */
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { DEFAULT_ID, ICONS, macos as macosIconSvg, raster } from "./app-icons.mjs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const RELEASE = path.join(ROOT, "release");
const TEMPLATES = path.join(ROOT, "scripts", "macos-pkg");

const serverPkg = JSON.parse(
  fs.readFileSync(path.join(ROOT, "packages/server/package.json"), "utf8")
);
const VERSION = serverPkg.version;

const argv = process.argv.slice(2);
let skipBin = false;
let useLauncher = false;
for (const a of argv) {
  if (a === "--skip-bin") skipBin = true;
  else if (a === "--launcher") useLauncher = true;
  else {
    console.error(`未知参数: ${a}`);
    process.exit(1);
  }
}

if (process.platform !== "darwin") {
  console.error("macOS 安装包只能在 macOS 上打（需要 pkgbuild / iconutil / codesign）");
  process.exit(1);
}

function run(cmd, args, opts = {}) {
  execFileSync(cmd, args, { stdio: "inherit", cwd: ROOT, ...opts });
}

const target = `${process.platform}-${process.arch}`;
const seaBin = path.join(RELEASE, `falcon-v${VERSION}-${target}`);
if (!fs.existsSync(seaBin)) {
  if (skipBin) {
    console.error(`找不到 ${path.relative(ROOT, seaBin)}，先 pnpm build:bin`);
    process.exit(1);
  }
  console.log("== 没有 SEA 产物，先 pnpm build:bin ==");
  run("pnpm", ["build:bin"]);
}
if (!fs.existsSync(seaBin)) {
  console.error(`build:bin 之后仍没有 ${path.relative(ROOT, seaBin)}`);
  process.exit(1);
}

const stage = fs.mkdtempSync(path.join(os.tmpdir(), "falcon-pkg-"));
const app = path.join(stage, "payload", "Falcon.app");
const contents = path.join(app, "Contents");
const macos = path.join(contents, "MacOS");
const resources = path.join(contents, "Resources");
const scripts = path.join(stage, "scripts");

try {
  fs.mkdirSync(macos, { recursive: true });
  fs.mkdirSync(resources, { recursive: true });
  fs.mkdirSync(scripts, { recursive: true });

  // 默认应用图标的 macOS 版式（留边 + 投影，ADR 0018），图形定义在 app-icons.mjs，按每档尺寸现画。
  // 装好的 App 没启动时 Dock / 访达显示它；启动后原生客户端再按服务端的选择换
  const appIconSvg = macosIconSvg(ICONS[DEFAULT_ID]);
  const iconset = path.join(stage, "AppIcon.iconset");
  fs.mkdirSync(iconset);
  const sizes = [
    [16, "icon_16x16.png"],
    [32, "icon_16x16@2x.png"],
    [32, "icon_32x32.png"],
    [64, "icon_32x32@2x.png"],
    [128, "icon_128x128.png"],
    [256, "icon_128x128@2x.png"],
    [256, "icon_256x256.png"],
    [512, "icon_256x256@2x.png"],
    [512, "icon_512x512.png"],
    [1024, "icon_512x512@2x.png"],
  ];
  for (const [px, name] of sizes) {
    raster(appIconSvg, path.join(iconset, name), px);
  }
  run("iconutil", ["-c", "icns", "-o", path.join(resources, "AppIcon.icns"), iconset]);

  // SEA 二进制进 Resources：CFBundleExecutable 不能是它本身。
  // 双击 .app 如果直接跑 SEA，前台进程就是服务器，退出 App 会把会话全带走；
  // 也和 launchd KeepAlive 抢同一个端口。启动器只负责 install + 打开浏览器。
  const bundled = path.join(resources, "falcon");
  fs.copyFileSync(seaBin, bundled);
  fs.chmodSync(bundled, 0o755);

  // 读已装服务原参数的 shell 片段：postinstall 与 launcher.sh 都从 App 里 source 它，
  // 所以不论 App 入口是原生客户端还是 launcher.sh 都要带上
  fs.copyFileSync(
    path.join(TEMPLATES, "service-args.sh"),
    path.join(resources, "service-args.sh")
  );

  const exe = path.join(macos, "Falcon");
  if (useLauncher) {
    fs.copyFileSync(path.join(TEMPLATES, "launcher.sh"), exe);
  } else {
    // 原生客户端：release 构建（crates.io 走 native/.cargo/config.toml 里的镜像配置）
    console.log("== cargo build --release -p falcon-app ==");
    run("cargo", ["build", "--release", "-p", "falcon-app"], { cwd: path.join(ROOT, "native") });
    fs.copyFileSync(path.join(ROOT, "native/target/release/falcon-app"), exe);
  }
  fs.chmodSync(exe, 0o755);

  const plist = fs
    .readFileSync(path.join(TEMPLATES, "Info.plist"), "utf8")
    .replaceAll("__VERSION__", VERSION);
  fs.writeFileSync(path.join(contents, "Info.plist"), plist);

  // ad-hoc：没有 Developer ID。--deep 把 Resources/falcon 一并签上。
  run("codesign", ["--force", "--deep", "--sign", "-", app]);

  fs.copyFileSync(path.join(TEMPLATES, "postinstall"), path.join(scripts, "postinstall"));
  fs.chmodSync(path.join(scripts, "postinstall"), 0o755);

  fs.mkdirSync(RELEASE, { recursive: true });
  const pkg = path.join(RELEASE, `Falcon-v${VERSION}-${target}.pkg`);
  run("pkgbuild", [
    "--root",
    path.join(stage, "payload"),
    "--install-location",
    "/Applications",
    "--scripts",
    scripts,
    "--identifier",
    "com.falcon.app",
    "--version",
    VERSION,
    "--ownership",
    "recommended",
    pkg,
  ]);

  const mb = (fs.statSync(pkg).size / 1024 / 1024).toFixed(1);
  console.log(`✓ ${path.relative(ROOT, pkg)}（${mb} MB）`);
} finally {
  fs.rmSync(stage, { recursive: true, force: true });
}
