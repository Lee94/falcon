//! Linux 服务端安装文件（只含服务端，没有客户端）：.deb / .rpm / 自解包 install.sh。
//!
//! 输入是 `cargo xtask server` 的发布单文件 `release/falcon-v<版本>-linux-<arch>`；
//! 三种产物都装同一个 /usr/bin/falcon（install.sh 是 /usr/local/bin），再跑
//! `falcon service install` 注册 systemd 服务（root = system 级）——服务本身的
//! 注册逻辑只维护在服务端一处，包脚本不各写一份 unit。
//!
//! ```text
//! cargo xtask linux --target linux-x64              # 出 .deb / .rpm / install.sh
//! cargo xtask linux --target linux-arm64 --deb-only # 只要 .deb
//! ```
//!
//! 产物：
//! - `release/falcon-v<版本>-linux-<arch>.deb`（ar 归档：debian-binary + control.tar.gz +
//!   data.tar.gz，postinst/prerm 只调 `service install/uninstall`）
//! - `release/falcon-v<版本>-linux-<arch>.rpm`（rpm crate，同一对脚本，%preun 按 $1 区分卸载/升级）
//! - `release/falcon-v<版本>-linux-<arch>-install.sh`（脚本头 + 追加的原始二进制，
//!   `__FALCON_BINARY_BELOW__` 标记行之后即 ELF）
//!
//! 升级语义三处一致：已装过 falcon.service 时 `service install` 不带参数 = 沿用原
//! --host / --port / --data-dir（服务端从旧 unit / 任务 XML 读回），否则首装用
//! `--data-dir /var/lib/falcon`。卸载不动 /var/lib/falcon 里的数据与 bin/。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::util::{host_platform, root, version};

const POSTINST: &str = include_str!("../assets/linux/postinst.sh");
const PRERM: &str = include_str!("../assets/linux/prerm.sh");
const PREUN: &str = include_str!("../assets/linux/preun.sh");
const INSTALL_HEAD: &str = include_str!("../assets/linux/install.sh");

/// linux-<arch> 平台名 → dpkg / rpm 的架构名
const DEB_ARCH: [(&str, &str); 2] = [("linux-x64", "amd64"), ("linux-arm64", "arm64")];
const RPM_ARCH: [(&str, &str); 2] = [("linux-x64", "x86_64"), ("linux-arm64", "aarch64")];

pub fn run(args: &[String]) -> Result<()> {
    let mut target = host_platform();
    let mut server: Option<PathBuf> = None;
    let mut deb_only = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--target" => target = it.next().context("--target 后面缺平台名")?.clone(),
            "--server" => server = Some(PathBuf::from(it.next().context("--server 后面缺路径")?)),
            "--deb-only" => deb_only = true,
            other => bail!("未知参数：{other}"),
        }
    }
    if !target.starts_with("linux-") {
        bail!("Linux 安装文件只打 linux-* 目标：{target}");
    }
    let rel = |p: &Path| p.strip_prefix(root()).unwrap_or(p).display().to_string();

    let ver = version();
    let release = root().join("release");
    let bin = server.unwrap_or_else(|| release.join(format!("falcon-v{ver}-{target}")));
    if !bin.is_file() {
        bail!("找不到 {}，先 cargo xtask server --target {target}", rel(&bin));
    }
    std::fs::create_dir_all(&release)?;

    let deb = release.join(format!("falcon-v{ver}-{target}.deb"));
    build_deb(&bin, ver, &target, &deb)?;
    println!("✓ {}（{:.1} MB）", rel(&deb), mb(&deb));

    if deb_only {
        return Ok(());
    }

    let rpm_out = release.join(format!("falcon-v{ver}-{target}.rpm"));
    build_rpm(&bin, ver, &target, &rpm_out)?;
    println!("✓ {}（{:.1} MB）", rel(&rpm_out), mb(&rpm_out));

    let sh = release.join(format!("falcon-v{ver}-{target}-install.sh"));
    build_install_sh(&bin, &sh)?;
    println!("✓ {}（{:.1} MB）", rel(&sh), mb(&sh));
    Ok(())
}

fn mb(path: &Path) -> f64 {
    std::fs::metadata(path).map(|m| m.len() as f64 / 1024.0 / 1024.0).unwrap_or(0.0)
}

// ---- .deb（GNU ar 归档 + 两个 gzip tar 成员；不依赖 dpkg 工具，哪里都能打）----

fn build_deb(bin: &Path, ver: &str, target: &str, out: &Path) -> Result<()> {
    let deb_arch = DEB_ARCH
        .iter()
        .find(|(p, _)| *p == target)
        .map(|(_, a)| *a)
        .with_context(|| format!("没有 {target} 对应的 deb 架构名"))?;
    let size_kb = (std::fs::metadata(bin)?.len() / 1024).max(1);
    let control = format!(
        "Package: falcon-server\nVersion: {ver}\nArchitecture: {deb_arch}\nMaintainer: Falcon\nInstalled-Size: {size_kb}\nSection: net\nPriority: optional\nDepends: libc6\nDescription: falcon terminal server\n 终端会话服务端，装好即注册 systemd 服务并随系统自启。\n"
    );
    let bin_bytes = std::fs::read(bin)?;
    let control_tar = tar_gz(&[
        ("control", control.as_bytes(), 0o644),
        ("postinst", POSTINST.as_bytes(), 0o755),
        ("prerm", PRERM.as_bytes(), 0o755),
    ])?;
    let data_tar = tar_gz(&[("./usr/bin/falcon", &bin_bytes, 0o755)])?;
    let deb = ar(&[
        ("debian-binary", b"2.0\n".as_slice()),
        ("control.tar.gz", &control_tar),
        ("data.tar.gz", &data_tar),
    ]);
    let _ = std::fs::remove_file(out);
    std::fs::write(out, deb)?;
    Ok(())
}

/// gzip 的 tar 成员（文件名、内容、权限位）
fn tar_gz(entries: &[(&str, &[u8], u32)]) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    let enc = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::best());
    let mut b = tar::Builder::new(enc);
    for (name, data, mode) in entries {
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(*mode);
        h.set_uid(0);
        h.set_gid(0);
        h.set_mtime(0);
        h.set_cksum();
        b.append_data(&mut h, name, *data)?;
    }
    b.into_inner()?.finish()?;
    Ok(buf)
}

/// GNU ar：!<arch>\n + 成员（名字带尾斜杠，头 60 字节，奇数长度补 \n）
fn ar(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = b"!<arch>\n".to_vec();
    for (name, data) in members {
        let header = format!("{name:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n", 0, 0, 0, 100644, data.len());
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(data);
        if data.len() % 2 == 1 {
            out.push(b'\n');
        }
    }
    out
}

// ---- .rpm（rpm crate 纯 Rust 打包，脚本与 deb 同一套）----

fn build_rpm(bin: &Path, ver: &str, target: &str, out: &Path) -> Result<()> {
    let rpm_arch = RPM_ARCH
        .iter()
        .find(|(p, _)| *p == target)
        .map(|(_, a)| *a)
        .with_context(|| format!("没有 {target} 对应的 rpm 架构名"))?;
    let mut builder = rpm::PackageBuilder::new("falcon-server", ver, "MIT", rpm_arch, "falcon terminal server");
    builder.using_config(rpm::BuildConfig::v4().compression(rpm::CompressionType::Gzip));
    builder
        .release("1")
        .description("falcon terminal server（终端会话服务端），装好即注册 systemd 服务并随系统自启。")
        .requires(rpm::Dependency::any("glibc"))
        .post_install_script(rpm::Scriptlet::new(POSTINST))
        .pre_uninstall_script(rpm::Scriptlet::new(PREUN))
        .with_file(bin, rpm::FileOptions::new("/usr/bin/falcon").permissions(0o755))
        .map_err(anyhow::Error::from)?;
    let pkg = builder.build().map_err(anyhow::Error::from)?;
    let _ = std::fs::remove_file(out);
    pkg.write_file(out).map_err(anyhow::Error::from)?;
    Ok(())
}

// ---- install.sh（自解包：脚本头 + 标记行 + 原始二进制）----

fn build_install_sh(bin: &Path, out: &Path) -> Result<()> {
    let head = format!("{INSTALL_HEAD}__FALCON_BINARY_BELOW__\n");
    let mut content = head.into_bytes();
    content.extend_from_slice(&std::fs::read(bin)?);
    let _ = std::fs::remove_file(out);
    std::fs::write(out, content)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(out, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}
