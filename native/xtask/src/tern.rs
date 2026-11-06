//! Tern 的连接程序单独发布，不依赖 GPUI，也不改动已安装的 Falcon 服务。

use anyhow::{Context, Result, bail};

use crate::util::{host_platform, native, root, run as command, version};

pub fn run(args: &[String]) -> Result<()> {
    if !args.is_empty() {
        bail!("tern 不接受参数：{}", args.join(" "));
    }
    command("cargo", ["build", "--release", "-p", "falcon-tern"], Some(&native()))?;
    let exe = if cfg!(windows) { "falcon-tern.exe" } else { "falcon-tern" };
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| native().join("target"));
    let target = if target.is_absolute() { target } else { native().join(target) };
    let built = target.join("release").join(exe);
    let release = root().join("release");
    std::fs::create_dir_all(&release)?;
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    let out = release.join(format!("falcon-tern-v{}-{}{suffix}", version(), host_platform()));
    // 不修改仍可能映射进进程的可执行文件，替换发布产物前先解除旧目录项。
    if out.exists() {
        std::fs::remove_file(&out)?;
    }
    std::fs::copy(&built, &out).with_context(|| format!("复制 {} 失败", built.display()))?;
    println!("Tern 连接程序：{}", out.display());
    println!("Tern 插件目录：{}", root().join("integrations/tern").display());
    Ok(())
}
