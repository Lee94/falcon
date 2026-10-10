#!/usr/bin/env node
/**
 * 编译滚动位置插件（native/zellij-plugin，ADR 0019），产物拷到
 * native/crates/falcon-server/assets/falcon-scroll.wasm——这份是提交进仓库的，服务端
 * 编译时 include_bytes! 进二进制，平时的构建、build:bin 与 CI 都直接用它，只有改了
 * 插件或升了 zellij 才需要跑这个脚本。
 *
 *   pnpm build:zellij-plugin
 *
 * 需要 rustup 管的工具链带 wasm32-wasip1 target（Homebrew 的 rust 没有这个 target）：
 *
 *   brew install rustup        # keg-only，不会盖住 PATH 里的 Homebrew rust
 *   rustup toolchain install stable --profile minimal --target wasm32-wasip1
 *
 * 实测坑：直接调工具链里的 cargo 时它会去 PATH 上找 rustc（找到 Homebrew 那个，报
 * "can't find crate for core"），rust-lld 也找不到 libLLVM.dylib——平时是 rustup 的
 * 代理替你设好的。这里显式把 RUSTC、PATH 和动态库路径都指进工具链。
 */

import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const CRATE = path.join(ROOT, "native/zellij-plugin");
const OUT = path.join(ROOT, "native/crates/falcon-server/assets/falcon-scroll.wasm");

// 插件 API 跟着 zellij 版本走，两边必须同步升级
const zellijVersion = fs
  .readFileSync(path.join(ROOT, "native/crates/falcon-server/src/zellij/version.rs"), "utf8")
  .match(/pub const ZELLIJ_VERSION: &str = "([^"]+)"/)[1];
const tileVersion = fs
  .readFileSync(path.join(CRATE, "Cargo.toml"), "utf8")
  .match(/zellij-tile = "=([^"]+)"/)[1];
if (tileVersion !== zellijVersion) {
  console.error(
    `zellij-tile（${tileVersion}）与锁定的 zellij（${zellijVersion}）版本不一致，` +
      `先改 ${path.relative(ROOT, CRATE)}/Cargo.toml`
  );
  process.exit(1);
}

function findRustup() {
  for (const cand of ["rustup", "/opt/homebrew/opt/rustup/bin/rustup", "/usr/local/opt/rustup/bin/rustup"]) {
    try {
      execFileSync(cand, ["--version"], { stdio: "ignore" });
      return cand;
    } catch {
      // 下一个
    }
  }
  console.error("找不到 rustup（见本脚本顶部的安装说明）");
  process.exit(1);
}

const rustup = findRustup();
const rustc = execFileSync(rustup, ["which", "--toolchain", "stable", "rustc"], {
  encoding: "utf8",
}).trim();
const toolchain = path.dirname(path.dirname(rustc));
if (!fs.existsSync(path.join(toolchain, "lib/rustlib/wasm32-wasip1"))) {
  console.error(
    `工具链 ${toolchain} 没有 wasm32-wasip1 target：\n` +
      `  ${rustup} target add wasm32-wasip1 --toolchain stable`
  );
  process.exit(1);
}

const lib = path.join(toolchain, "lib");
execFileSync(path.join(toolchain, "bin/cargo"), ["build", "--release"], {
  cwd: CRATE,
  stdio: "inherit",
  env: {
    ...process.env,
    PATH: `${path.join(toolchain, "bin")}${path.delimiter}${process.env.PATH}`,
    RUSTC: rustc,
    DYLD_FALLBACK_LIBRARY_PATH: lib,
    LD_LIBRARY_PATH: lib,
  },
});

fs.mkdirSync(path.dirname(OUT), { recursive: true });
fs.copyFileSync(path.join(CRATE, "target/wasm32-wasip1/release/falcon-scroll.wasm"), OUT);
console.log(`已更新 ${path.relative(ROOT, OUT)}（${fs.statSync(OUT).size} 字节）`);
