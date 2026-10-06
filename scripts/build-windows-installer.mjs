#!/usr/bin/env node
/**
 * 打一份 Windows 安装包（Inno Setup 的 setup.exe）：只装原生客户端（native/，GPUI）。
 *
 * 与 macOS pkg 不同，这里没有捆绑服务端——SEA 不支持 Windows 目标（服务本身依赖 Zellij
 * 与 POSIX shell，见 README），装好后在客户端里连别处的 falcon 服务。
 *
 *   pnpm build:win                 # cargo build --release，再编安装包
 *   pnpm build:win --skip-cargo    # 只用已有的 native/target/release/falcon-app.exe
 *
 * 产物 release/Falcon-v<版本>-win32-x64-setup.exe。没有代码签名证书，别人机器上
 * SmartScreen 会拦，「更多信息 → 仍要运行」即可。
 *
 * 必须在 Windows 上跑：exe 的图标与版本信息由 falcon-app 的 build.rs 经 rc.exe 编进去
 * （Windows SDK，随 VS 生成工具装）；安装包要 Inno Setup 6 的 ISCC.exe——
 * `winget install JRSoftware.InnoSetup --scope user`，或用 FALCON_ISCC 指到它。
 */
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const NATIVE = path.join(ROOT, "native");
const RELEASE = path.join(ROOT, "release");
const ISS = path.join(ROOT, "scripts", "windows-installer", "Falcon.iss");

const argv = process.argv.slice(2);
let skipCargo = false;
for (const a of argv) {
  if (a === "--skip-cargo") skipCargo = true;
  else {
    console.error(`未知参数: ${a}`);
    process.exit(1);
  }
}

if (process.platform !== "win32") {
  console.error("Windows 安装包只能在 Windows 上打（需要 rc.exe 与 Inno Setup）");
  process.exit(1);
}

// 版本跟原生客户端走（exe 版本信息里也是这个），不跟服务端：安装包里只有客户端
const cargoToml = fs.readFileSync(path.join(NATIVE, "Cargo.toml"), "utf8");
const VERSION = cargoToml.match(/\[workspace\.package\][^[]*?\nversion\s*=\s*"([^"]+)"/)?.[1];
if (!VERSION) {
  console.error("读不出 native/Cargo.toml 的 [workspace.package] version");
  process.exit(1);
}

function findIscc() {
  const candidates = [
    process.env.FALCON_ISCC,
    process.env.LOCALAPPDATA && path.join(process.env.LOCALAPPDATA, "Programs", "Inno Setup 6", "ISCC.exe"),
    process.env["ProgramFiles(x86)"] && path.join(process.env["ProgramFiles(x86)"], "Inno Setup 6", "ISCC.exe"),
    process.env.ProgramFiles && path.join(process.env.ProgramFiles, "Inno Setup 6", "ISCC.exe"),
  ].filter(Boolean);
  for (const c of candidates) if (fs.existsSync(c)) return c;
  // PATH 兜底
  try {
    const out = execFileSync("where", ["ISCC.exe"], { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] });
    const first = out.split(/\r?\n/).find(Boolean);
    if (first) return first;
  } catch {}
  return null;
}

const iscc = findIscc();
if (!iscc) {
  console.error("找不到 Inno Setup 6 的 ISCC.exe：winget install JRSoftware.InnoSetup --scope user，或设 FALCON_ISCC");
  process.exit(1);
}

if (!skipCargo) {
  // crates.io 走 native/.cargo/config.toml 里的镜像配置
  console.log("== cargo build --release -p falcon-app ==");
  execFileSync("cargo", ["build", "--release", "-p", "falcon-app"], { stdio: "inherit", cwd: NATIVE });
}
const exe = path.join(NATIVE, "target", "release", "falcon-app.exe");
if (!fs.existsSync(exe)) {
  console.error(`找不到 ${path.relative(ROOT, exe)}，先去掉 --skip-cargo 跑一遍`);
  process.exit(1);
}

// 安装包自己的图标用 build.rs 编进 exe 的同一份 .ico（在 OUT_DIR 里，目录名带哈希；
// 换过构建配置会留下好几个，取最新的那个）
const buildDir = path.join(NATIVE, "target", "release", "build");
const icon = fs
  .readdirSync(buildDir)
  .filter((d) => d.startsWith("falcon-app-"))
  .map((d) => path.join(buildDir, d, "out", "falcon.ico"))
  .filter((p) => fs.existsSync(p))
  .sort((a, b) => fs.statSync(b).mtimeMs - fs.statSync(a).mtimeMs)[0];
if (!icon) {
  console.error("target/release/build/falcon-app-*/out/ 里没有 falcon.ico，build.rs 没走 Windows 资源那一段？");
  process.exit(1);
}

fs.mkdirSync(RELEASE, { recursive: true });
const base = `Falcon-v${VERSION}-${process.platform}-${process.arch}-setup`;
console.log(`== ISCC ${path.relative(ROOT, ISS)} ==`);
execFileSync(
  iscc,
  [
    "/Qp",
    `/DAppVersion=${VERSION}`,
    `/DSourceExe=${exe}`,
    `/DSourceIcon=${icon}`,
    `/DOutputDir=${RELEASE}`,
    `/DOutputBaseFilename=${base}`,
    ISS,
  ],
  { stdio: "inherit", cwd: ROOT }
);

const out = path.join(RELEASE, `${base}.exe`);
const mb = (fs.statSync(out).size / 1024 / 1024).toFixed(1);
console.log(`✓ ${path.relative(ROOT, out)}（${mb} MB）`);
