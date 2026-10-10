//! macOS 安装包（.pkg，原 scripts/build-macos-pkg.mjs）：把 Falcon.app 装进 /Applications，
//! postinstall 以当前登录用户跑 `falcon service install`（用户级 LaunchAgent，不能用 root
//! 的 guid），装过服务的话带上原 plist 里的 --host / --port / --data-dir。
//!
//! App 的可执行文件默认是原生客户端（GPUI，设计见 docs/design/gpui-client.md）：
//! 它启动时自己做 launcher.sh 那两步（service install → 等端口），然后直接连本机服务。
//! `--launcher` 退回旧的 launcher.sh（注册服务后用浏览器打开浏览器版界面）。
//!
//! ```text
//! cargo xtask pkg                 # 没有当前平台的服务端产物就先 cargo xtask server
//! cargo xtask pkg --skip-bin      # 必须已有 release/falcon-v*-darwin-*
//! cargo xtask pkg --launcher      # App 里放 launcher.sh 而不是原生客户端
//! ```
//!
//! 产物 release/Falcon-v<版本>-darwin-<arch>.pkg。无开发者证书，只做 ad-hoc
//! 签名——别人机器上 Gatekeeper 会拦，系统设置里「仍要打开」即可。
//!
//! 必须在 macOS 上跑（pkgbuild / iconutil / codesign）。模板在 native/xtask/assets/macos-pkg/。

use std::path::Path;

use anyhow::{Result, bail};

use crate::util::{self, TempDir, host_platform, native, root, version};

fn build(skip_bin: bool, use_launcher: bool) -> Result<()> {
    if !cfg!(target_os = "macos") {
        bail!("macOS 安装包只能在 macOS 上打（需要 pkgbuild / iconutil / codesign）");
    }
    let templates = native().join("xtask/assets/macos-pkg");
    let release = root().join("release");
    let target = host_platform();
    let ver = version();
    let rel = |p: &Path| p.strip_prefix(root()).unwrap_or(p).display().to_string();

    let server_bin = release.join(format!("falcon-v{ver}-{target}"));
    if !server_bin.is_file() {
        if skip_bin {
            bail!("找不到 {}，先 cargo xtask server", rel(&server_bin));
        }
        println!("== 没有服务端产物，先 cargo xtask server ==");
        crate::server::build(&target, false)?;
    }
    if !server_bin.is_file() {
        bail!("cargo xtask server 之后仍没有 {}", rel(&server_bin));
    }

    let stage = TempDir::new("pkg")?;
    let payload = stage.path().join("payload");
    let app = payload.join("Falcon.app");
    let contents = app.join("Contents");
    let macos_dir = contents.join("MacOS");
    let resources = contents.join("Resources");
    let scripts = stage.path().join("scripts");
    for d in [&macos_dir, &resources, &scripts] {
        std::fs::create_dir_all(d)?;
    }

    // 默认应用图标的 macOS 版式（留边 + 投影，ADR 0018），图形定义在 icons.rs，按每档尺寸现画。
    // 装好的 App 没启动时 Dock / 访达显示它；启动后原生客户端再按服务端的选择换
    let app_icon_svg = crate::icons::macos(&crate::icons::default_icon());
    let iconset = stage.path().join("AppIcon.iconset");
    std::fs::create_dir_all(&iconset)?;
    const SIZES: [(u32, &str); 10] = [
        (16, "icon_16x16.png"),
        (32, "icon_16x16@2x.png"),
        (32, "icon_32x32.png"),
        (64, "icon_32x32@2x.png"),
        (128, "icon_128x128.png"),
        (256, "icon_128x128@2x.png"),
        (256, "icon_256x256.png"),
        (512, "icon_256x256@2x.png"),
        (512, "icon_512x512.png"),
        (1024, "icon_512x512@2x.png"),
    ];
    for (px, name) in SIZES {
        crate::icons::raster(&app_icon_svg, &iconset.join(name), px)?;
    }
    util::run(
        "iconutil",
        ["-c".as_ref(), "icns".as_ref(), "-o".as_ref(), resources.join("AppIcon.icns").as_os_str(), iconset.as_os_str()],
        None,
    )?;

    // 服务端二进制进 Resources：CFBundleExecutable 不能是它本身。
    // 双击 .app 如果直接跑服务端，前台进程就是服务器，退出 App 会把会话全带走；
    // 也和 launchd KeepAlive 抢同一个端口。启动器只负责 install + 打开界面。
    copy_exec(&server_bin, &resources.join("falcon"))?;

    // 读已装服务原参数的 shell 片段：postinstall 与 launcher.sh 都从 App 里 source 它，
    // 所以不论 App 入口是原生客户端还是 launcher.sh 都要带上
    std::fs::copy(templates.join("service-args.sh"), resources.join("service-args.sh"))?;

    let exe = macos_dir.join("Falcon");
    if use_launcher {
        copy_exec(&templates.join("launcher.sh"), &exe)?;
    } else {
        // 原生客户端：release 构建（crates.io 走 native/.cargo/config.toml 里的镜像配置）
        println!("== cargo build --release -p falcon-app ==");
        util::run("cargo", ["build", "--release", "-p", "falcon-app"], Some(&native()))?;
        copy_exec(&native().join("target/release/falcon-app"), &exe)?;
    }

    let plist = std::fs::read_to_string(templates.join("Info.plist"))?.replace("__VERSION__", ver);
    std::fs::write(contents.join("Info.plist"), plist)?;

    // ad-hoc：没有 Developer ID。--deep 把 Resources/falcon 一并签上。
    util::run("codesign", ["--force".as_ref(), "--deep".as_ref(), "--sign".as_ref(), "-".as_ref(), app.as_os_str()], None)?;

    copy_exec(&templates.join("postinstall"), &scripts.join("postinstall"))?;

    std::fs::create_dir_all(&release)?;
    let pkg = release.join(format!("Falcon-v{ver}-{target}.pkg"));
    util::run(
        "pkgbuild",
        [
            "--root".as_ref(),
            payload.as_os_str(),
            "--install-location".as_ref(),
            "/Applications".as_ref(),
            "--scripts".as_ref(),
            scripts.as_os_str(),
            "--identifier".as_ref(),
            "com.falcon.app".as_ref(),
            "--version".as_ref(),
            ver.as_ref(),
            "--ownership".as_ref(),
            "recommended".as_ref(),
            pkg.as_os_str(),
        ] as [&std::ffi::OsStr; 13],
        None,
    )?;

    let mb = std::fs::metadata(&pkg)?.len() as f64 / 1024.0 / 1024.0;
    println!("✓ {}（{mb:.1} MB）", rel(&pkg));
    Ok(())
}

pub fn run(args: &[String]) -> Result<()> {
    let mut skip_bin = false;
    let mut use_launcher = false;
    for a in args {
        match a.as_str() {
            "--skip-bin" => skip_bin = true,
            "--launcher" => use_launcher = true,
            other => bail!("未知参数：{other}"),
        }
    }
    build(skip_bin, use_launcher)
}

/// 拷贝并设成可执行
fn copy_exec(from: &Path, to: &Path) -> Result<()> {
    std::fs::copy(from, to).map_err(|e| anyhow::anyhow!("拷贝 {} 失败：{e}", from.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(to, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}
