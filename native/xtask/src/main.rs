//! `cargo xtask <命令>`：仓库的构建 / 打包 / 资源生成任务。取代原来 `scripts/*.mjs` 与
//! `native/scripts/*.mjs` 那批 Node 脚本（仓库里不再有 JS / Node 依赖）。
//!
//! 在 `native/` 下跑（`.cargo/config.toml` 里有 `xtask` 别名）；别处可以
//! `cargo run --manifest-path native/Cargo.toml -p xtask -- <命令>`。
//!
//! 外部工具照旧是外部工具（都不是 JS）：打包用 macOS 的 pkgbuild / iconutil / codesign、
//! Windows 的 Inno Setup；图标用 rsvg-convert（librsvg）；字体用 HarfBuzz 的 hb-subset 与
//! Google 的 woff2_compress；下载走 curl。缺哪个就报哪个，并说怎么装。

mod fixtures;
mod fonts;
mod icons;
mod meegle;
mod pkg;
mod server;
mod themes;
mod util;
mod win;
mod zellij;

const USAGE: &str = "用法：cargo xtask <命令> [参数]

发布与打包
  server [--target <平台>] [--skip-web]   服务端发布单文件（内嵌浏览器版与 meegle CLI），产物 release/falcon-v<版本>-<平台>
  web                                    浏览器版客户端（native/scripts/build-web.sh），产物 native/target-wasm/dist
  pkg [--skip-bin] [--launcher]          macOS 安装包（Falcon.app = 原生客户端 + Resources 里的服务端）
  win [--skip-cargo]                     Windows 安装包（Inno Setup，只有原生客户端）
  meegle [--target <平台>]               取锁定版本的 meegle CLI 到 native/.cache/meegle/（server 会自动调）

Zellij
  zellij-update <版本|--latest>          改锁定的 Zellij 版本（校验五个 target 的产物齐全）
  zellij-plugin                          重编滚动位置插件，产物提交在 falcon-server/assets/

资源（产物都已进仓库，只在要换时跑）
  icons                                  生成全部内置应用图标（浏览器版 native/web/icons 与原生 Dock 图标）
  vendor-fonts <berkeley|ioskeley|maple|nerd> [参数]
  vendor-themes [--from <目录> | --github]

协议 fixture
  fixtures                               起真服务端，把各接口的响应落盘到 falcon-proto/tests/fixtures/
  compare-fixtures <基准目录> <对照目录>  比两套 fixture 的形状

平台名：darwin-arm64 / darwin-x64 / linux-x64 / linux-arm64（与 release 产物的后缀一致）。";

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || matches!(args[0].as_str(), "-h" | "--help" | "help") {
        println!("{USAGE}");
        std::process::exit(if args.is_empty() { 1 } else { 0 });
    }
    let cmd = args.remove(0);
    let res = match cmd.as_str() {
        "server" => server::run(&args),
        "web" => server::build_web(),
        "pkg" => pkg::run(&args),
        "win" => win::run(&args),
        "meegle" => meegle::run(&args),
        "zellij-update" => zellij::update(&args),
        "zellij-plugin" => zellij::build_plugin(&args),
        "icons" => icons::run(&args),
        "vendor-fonts" => fonts::run(&args),
        "vendor-themes" => themes::run(&args),
        "fixtures" => fixtures::run(&args),
        "compare-fixtures" => fixtures::compare(&args),
        other => {
            eprintln!("未知命令：{other}\n\n{USAGE}");
            std::process::exit(1);
        }
    };
    if let Err(e) = res {
        eprintln!("xtask {cmd} 失败：{e:#}");
        std::process::exit(1);
    }
}
