//! 会话守护进程 falcon-sessiond 的工具链（ADR 0020「构建与分发」，设计文档第 13 节）：
//!
//! - `ghostty`：确保钉死版本的 Zig 与 ghostty 源码（打上 `sessiond/ghostty-patches/` 的补丁）就绪，
//!   按目标编 libghostty-vt 的静态库（`zig build -Demit-lib-vt -Doptimize=ReleaseSafe`），放进
//!   `native/.cache/ghostty/<标识>/<三元组>/`（标识见 [`Pin::build_id`]）；
//! - `ghostty-bindgen`：从同一份头文件重新生成 `ghostty-vt-sys/src/bindings.rs`（只在升 pin 时跑）；
//! - `sessiond`：用 cargo-zigbuild 交叉编 5 个目标，产物在 `native/target-sessiond/<三元组>/release/`。
//!
//! 全程不经任何 build.rs 联网：下载只在这里发生，都按钉死的 sha256 校验。Zig 的包缓存
//! （ghostty 的依赖）也指到 `native/.cache/`，不写进 `~/.cache/zig`。
//!
//! 实测（2026-10-13，macOS arm64，Zig 0.16.0 + ghostty 05dcf88）：静态库每个目标 25–65 秒；
//! 五个目标都**不需要链 libc++**（ghostty 的 C++ 依赖以 no-libcxx 模式编进了静态库）。

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use sha2::Digest as _;

use crate::util::{download, native, run_env};

/// 钉死的 Zig。升级时把四个宿主平台 tarball 的 sha256 一起换掉（来自 ziglang.org/download/index.json）
const ZIG_VERSION: &str = "0.16.0";
const ZIG_TARBALLS: [(&str, &str); 4] = [
    ("aarch64-macos", "b23d70deaa879b5c2d486ed3316f7eaa53e84acf6fc9cc747de152450d401489"),
    ("x86_64-macos", "0387557ed1877bc6a2e1802c8391953baddba76081876301c522f52977b52ba7"),
    ("x86_64-linux", "70e49664a74374b48b51e6f3fdfbf437f6395d42509050588bd49abe52ba3d00"),
    ("aarch64-linux", "ea4b09bfb22ec6f6c6ceac57ab63efb6b46e17ab08d21f69f3a48b38e1534f17"),
];

/// 守护进程的 5 个目标：(Rust 三元组, Zig 目标)。
///
/// macOS 的 Zig 目标带上最低系统版本，与 Rust 的缺省部署目标对齐（arm64 11.0、x64 10.12
/// 太老，ghostty 不支持，取 10.15）——不对齐的话链接器对每个目标文件报一句
/// "built for newer macOS version"。
pub const TARGETS: [(&str, &str); 5] = [
    ("x86_64-unknown-linux-musl", "x86_64-linux-musl"),
    ("aarch64-unknown-linux-musl", "aarch64-linux-musl"),
    ("x86_64-apple-darwin", "x86_64-macos.10.15"),
    ("aarch64-apple-darwin", "aarch64-macos.11.0"),
    ("x86_64-pc-windows-gnu", "x86_64-windows-gnu"),
];

/// x86_64 macOS 的部署目标：与上面 Zig 目标的版本一致
const X64_MACOS_DEPLOYMENT: &str = "10.15";

struct Pin {
    commit: String,
    sha256: String,
    /// 打在源码上的补丁（`sessiond/ghostty-patches/*.patch`），按文件名排好；已与 pin 的
    /// `patches=` 核对过
    patches: Vec<PathBuf>,
    /// 全部补丁按文件名顺序拼起来的 sha256；没有补丁时为 None
    patches_sha256: Option<String>,
}

impl Pin {
    /// 这份 libghostty-vt 的标识：commit，有补丁时再加 `+falcon.<补丁哈希前 12 位>`。
    /// 源码与静态库的缓存目录按它分（改了补丁不会捡到旧库），VT 进程报的 ghostty 版本也是它
    /// （ghostty-vt-sys 的 build.rs 照同一规则拼）——服务端看到存量会话的 VT 进程版本与新部署的
    /// 不同才热升级它（sessions/manager.rs），补丁不进版本号的话旧会话会一直用旧的格式化器
    fn build_id(&self) -> String {
        match &self.patches_sha256 {
            Some(h) => format!("{}+falcon.{}", self.commit, &h[..12]),
            None => self.commit.clone(),
        }
    }
}

fn pin() -> Result<Pin> {
    let path = native().join("sessiond/ghostty.pin");
    let text = std::fs::read_to_string(&path).with_context(|| format!("读不到 {}", path.display()))?;
    let get = |k: &str| text.lines().find_map(|l| l.trim().strip_prefix(k).map(str::to_string));
    let field = |k: &str| get(k).with_context(|| format!("ghostty.pin 缺 {k}"));
    let commit = field("commit=")?;
    let sha256 = field("sha256=")?;
    let pinned = get("patches=").filter(|v| !v.is_empty());

    let dir = native().join("sessiond/ghostty-patches");
    let mut patches: Vec<PathBuf> = match std::fs::read_dir(&dir) {
        Ok(rd) => rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "patch")).collect(),
        Err(_) => Vec::new(),
    };
    patches.sort();
    let patches_sha256 = if patches.is_empty() {
        None
    } else {
        let mut h = sha2::Sha256::new();
        for p in &patches {
            h.update(std::fs::read(p)?);
        }
        Some(hex::encode(h.finalize()))
    };
    if patches_sha256 != pinned {
        bail!(
            "{} 与 ghostty.pin 的 patches= 对不上：补丁改过的话把它换成 `patches={}`（没有补丁就删掉这一行）",
            dir.display(),
            patches_sha256.as_deref().unwrap_or("")
        );
    }
    Ok(Pin { commit, sha256, patches, patches_sha256 })
}

fn cache() -> PathBuf {
    native().join(".cache")
}

fn sha256_hex(path: &Path) -> Result<String> {
    let data = std::fs::read(path)?;
    Ok(hex::encode(sha2::Sha256::digest(&data)))
}

/// 宿主平台在 Zig 下载页上的名字
fn zig_host() -> Result<&'static str> {
    Ok(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-macos",
        ("macos", "x86_64") => "x86_64-macos",
        ("linux", "x86_64") => "x86_64-linux",
        ("linux", "aarch64") => "aarch64-linux",
        (os, arch) => bail!("守护进程只在 macOS / Linux 上构建（{os}-{arch} 不支持）：Windows 产物由它们交叉编出"),
    })
}

/// 确保钉死版本的 Zig 就绪，返回 zig 可执行文件
fn ensure_zig() -> Result<PathBuf> {
    let host = zig_host()?;
    let name = format!("zig-{host}-{ZIG_VERSION}");
    let dir = cache().join("zig").join(&name);
    let zig = dir.join("zig");
    if zig.is_file() {
        return Ok(zig);
    }
    let sha = ZIG_TARBALLS.iter().find(|(h, _)| *h == host).map(|(_, s)| *s).context("没有钉这个平台的 Zig")?;
    let tarball = cache().join("zig").join(format!("{name}.tar.xz"));
    if !tarball.is_file() || sha256_hex(&tarball)? != sha {
        download(&format!("https://ziglang.org/download/{ZIG_VERSION}/{name}.tar.xz"), &tarball)?;
    }
    let got = sha256_hex(&tarball)?;
    if got != sha {
        bail!("{} 校验不过：期望 {sha}，实际 {got}", tarball.display());
    }
    // xz 走系统 tar（macOS 的 bsdtar 与 GNU tar 都认 -J），不为它拉一个 xz 解码库
    let parent = dir.parent().expect("zig 目录");
    crate::util::run("tar", [OsString::from("-xJf"), tarball.clone().into(), "-C".into(), parent.into()], None)?;
    if !zig.is_file() {
        bail!("解开 {} 后没找到 {}", tarball.display(), zig.display());
    }
    Ok(zig)
}

/// 确保钉死 commit 的 ghostty 源码就绪（补丁已打上），返回源码目录
fn ensure_ghostty_src(pin: &Pin) -> Result<PathBuf> {
    let base = cache().join("ghostty").join(pin.build_id());
    let src = base.join("src");
    if src.join("build.zig").is_file() {
        return Ok(src);
    }
    let tarball = cache().join("ghostty").join(format!("{}.tar.gz", pin.commit));
    if !tarball.is_file() || sha256_hex(&tarball)? != pin.sha256 {
        download(&format!("https://github.com/ghostty-org/ghostty/archive/{}.tar.gz", pin.commit), &tarball)?;
    }
    let got = sha256_hex(&tarball)?;
    if got != pin.sha256 {
        bail!("{} 校验不过：期望 {}，实际 {got}（GitHub 重新生成了源码包？核对后更新 ghostty.pin）", tarball.display(), pin.sha256);
    }
    let tmp = base.join("src.part");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;
    let file = std::fs::File::open(&tarball)?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(file));
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        // 剥掉顶层的 ghostty-<commit>/
        let rel: PathBuf = path.components().skip(1).collect();
        if rel.as_os_str().is_empty() {
            continue;
        }
        entry.unpack(tmp.join(rel))?;
    }
    // 补丁打在解到一半的目录上，全部打完才改名成 src：中途失败不会留下半打补丁的源码
    for p in &pin.patches {
        println!("== ghostty 补丁：{} ==", p.file_name().unwrap_or_default().to_string_lossy());
        crate::util::run("patch", [OsString::from("-p1"), "--batch".into(), "--forward".into(), "-i".into(), p.into()], Some(&tmp))?;
    }
    std::fs::rename(&tmp, &src)?;
    Ok(src)
}

/// `src` 是 ghostty 源码目录。ghostty 的 build.zig 用 `git describe` 取版本号，而 `.cache` 在
/// falcon 自己的仓库里：不拦的话 git 一路往上找到 falcon 的仓库和它的 tag，ghostty 当成
/// "打了 tag 的发布版" 去核对版本号，直接 panic（实测）。挡在源码目录的上一层，git 找不到
/// 仓库，ghostty 就退回 `x.y.z-dev`
fn zig_env(zig: &Path, src: &Path) -> Vec<(&'static str, OsString)> {
    vec![
        ("ZIG_GLOBAL_CACHE_DIR", cache().join("zig-global").into_os_string()),
        ("PATH", prepend_path(&[zig.parent().expect("zig 目录")])),
        ("GIT_CEILING_DIRECTORIES", src.parent().expect("源码目录的上一层").as_os_str().to_owned()),
    ]
}

fn prepend_path(dirs: &[&Path]) -> OsString {
    let mut parts: Vec<PathBuf> = dirs.iter().map(|d| d.to_path_buf()).collect();
    if let Some(p) = std::env::var_os("PATH") {
        parts.extend(std::env::split_paths(&p));
    }
    std::env::join_paths(parts).expect("PATH")
}

/// 某个目标的静态库目录（里面只有 libghostty-vt.a），ghostty-vt-sys 的 build.rs 认这个位置
pub fn lib_dir(build_id: &str, triple: &str) -> PathBuf {
    cache().join("ghostty").join(build_id).join(triple)
}

/// 编一个目标的静态库（已有就跳过），返回 zig 的安装前缀（include 在里面）
fn build_lib(zig: &Path, src: &Path, pin: &Pin, triple: &str, zig_target: &str, force: bool) -> Result<PathBuf> {
    let prefix = cache().join("ghostty").join(pin.build_id()).join(format!("out-{triple}"));
    let dest_dir = lib_dir(&pin.build_id(), triple);
    let dest = dest_dir.join("libghostty-vt.a");
    if dest.is_file() && prefix.join("include/ghostty/vt.h").is_file() && !force {
        return Ok(prefix);
    }
    println!("== libghostty-vt：{triple}（zig -Dtarget={zig_target}）==");
    let env = zig_env(zig, src);
    let env_ref: Vec<(&str, &std::ffi::OsStr)> = env.iter().map(|(k, v)| (*k, v.as_os_str())).collect();
    run_env(
        zig,
        [
            OsString::from("build"),
            "-Demit-lib-vt".into(),
            "-Doptimize=ReleaseSafe".into(),
            format!("-Dtarget={zig_target}").into(),
            "--prefix".into(),
            prefix.clone().into(),
        ],
        Some(src),
        &env_ref,
    )?;
    // Windows 上 zig 出的静态库叫 ghostty-vt-static.lib（ghostty-vt.lib 是 DLL 的导入库）
    let built = ["lib/libghostty-vt.a", "lib/ghostty-vt-static.lib"]
        .iter()
        .map(|p| prefix.join(p))
        .find(|p| p.is_file())
        .with_context(|| format!("{} 下没有静态库", prefix.display()))?;
    std::fs::create_dir_all(&dest_dir)?;
    std::fs::copy(&built, &dest)?;
    Ok(prefix)
}

/// `--targets host|all|<三元组>[,<三元组>…]`
fn parse_targets(args: &[String]) -> Result<(Vec<(&'static str, &'static str)>, Vec<String>)> {
    let mut sel: Option<String> = None;
    let mut rest = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--targets" {
            sel = Some(it.next().context("--targets 后面缺值")?.clone());
        } else {
            rest.push(a.clone());
        }
    }
    let sel = sel.unwrap_or_else(|| "host".into());
    let targets = match sel.as_str() {
        "all" => TARGETS.to_vec(),
        "host" => vec![host_target()?],
        list => list
            .split(',')
            .map(|t| TARGETS.iter().find(|(r, _)| *r == t).copied().with_context(|| format!("不认识的目标 {t}")))
            .collect::<Result<_>>()?,
    };
    Ok((targets, rest))
}

/// 本机对应的那个目标（Linux 上是 musl：守护进程一律静态链接）
pub fn host_target() -> Result<(&'static str, &'static str)> {
    let triple = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-musl",
        ("linux", "aarch64") => "aarch64-unknown-linux-musl",
        (os, arch) => bail!("{os}-{arch} 上不构建守护进程"),
    };
    Ok(*TARGETS.iter().find(|(r, _)| *r == triple).expect("在表里"))
}

/// `cargo xtask ghostty [--targets …] [--force]`
pub fn ghostty(args: &[String]) -> Result<()> {
    let (targets, rest) = parse_targets(args)?;
    let force = rest.iter().any(|a| a == "--force");
    if let Some(a) = rest.iter().find(|a| *a != "--force") {
        bail!("未知参数：{a}");
    }
    let pin = pin()?;
    let zig = ensure_zig()?;
    let src = ensure_ghostty_src(&pin)?;
    for (triple, zt) in targets {
        build_lib(&zig, &src, &pin, triple, zt, force)?;
        println!("libghostty-vt（{triple}）：{}", lib_dir(&pin.build_id(), triple).display());
    }
    Ok(())
}

/// `cargo xtask ghostty-bindgen`：从钉死 commit 的头文件重新生成绑定
pub fn bindgen(args: &[String]) -> Result<()> {
    if let Some(a) = args.first() {
        bail!("未知参数：{a}");
    }
    let pin = pin()?;
    let zig = ensure_zig()?;
    let src = ensure_ghostty_src(&pin)?;
    let (triple, zt) = host_target()?;
    let prefix = build_lib(&zig, &src, &pin, triple, zt, false)?;
    let out = native().join("sessiond/crates/ghostty-vt-sys/src/bindings.rs");
    let mut env: Vec<(&str, OsString)> = Vec::new();
    // macOS 上 bindgen 要的 libclang 在 CommandLineTools 里；别处按 bindgen 自己的规矩找
    if std::env::var_os("LIBCLANG_PATH").is_none() && Path::new("/Library/Developer/CommandLineTools/usr/lib").is_dir() {
        env.push(("LIBCLANG_PATH", "/Library/Developer/CommandLineTools/usr/lib".into()));
    }
    let env_ref: Vec<(&str, &std::ffi::OsStr)> = env.iter().map(|(k, v)| (*k, v.as_os_str())).collect();
    run_env(
        "cargo",
        [
            OsString::from("run"),
            "--release".into(),
            "--manifest-path".into(),
            native().join("sessiond/tools/bindgen/Cargo.toml").into(),
            "--target-dir".into(),
            native().join("target-sessiond/bindgen").into(),
            "--".into(),
            prefix.join("include").into(),
            out.clone().into(),
            pin.commit.clone().into(),
        ],
        None,
        &env_ref,
    )?;
    println!("已重新生成 {}（git diff 就是这次的 ABI 变化）", out.display());
    Ok(())
}

/// rustup 的 stable 工具链目录。这台机器 PATH 上排前面的是 Homebrew 的 rustc，它没有
/// musl / Windows 的标准库（同 build-web.sh 里的处理）
fn rustup_toolchain_bin() -> Result<PathBuf> {
    let out = Command::new("rustup").args(["which", "--toolchain", "stable", "cargo"]).output().context("起不来 rustup（装了吗？）")?;
    if !out.status.success() {
        bail!("rustup which cargo 失败：{}", String::from_utf8_lossy(&out.stderr));
    }
    let cargo = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    Ok(cargo.parent().context("cargo 的目录")?.to_path_buf())
}

fn cargo_home_bin() -> Option<PathBuf> {
    std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo")))
        .map(|p| p.join("bin"))
}

/// 守护进程某个目标的产物路径
pub fn artifact(triple: &str) -> PathBuf {
    let exe = if triple.contains("windows") { "falcon-sessiond.exe" } else { "falcon-sessiond" };
    native().join("target-sessiond").join(triple).join("release").join(exe)
}

/// `cargo xtask sessiond [--targets …] [--check] [--test]`
pub fn run(args: &[String]) -> Result<()> {
    let (targets, rest) = parse_targets(args)?;
    let mut check = false;
    let mut test = false;
    for a in &rest {
        match a.as_str() {
            "--check" => check = true,
            "--test" => test = true,
            other => bail!("未知参数：{other}"),
        }
    }
    build(&targets, check, test)
}

/// 编守护进程（服务端发布构建也走这里）
pub fn build(targets: &[(&'static str, &'static str)], check: bool, test: bool) -> Result<()> {
    let pin = pin()?;
    let zig = ensure_zig()?;
    let src = ensure_ghostty_src(&pin)?;
    for (triple, zt) in targets {
        build_lib(&zig, &src, &pin, triple, zt, false)?;
    }
    let (host_triple, _) = host_target()?;
    let tc = rustup_toolchain_bin()?;
    let mut path_dirs: Vec<&Path> = vec![tc.as_path(), zig.parent().expect("zig 目录")];
    let cargo_bin = cargo_home_bin();
    if let Some(b) = &cargo_bin {
        path_dirs.push(b);
    }
    let path = prepend_path(&path_dirs);
    let manifest = native().join("sessiond/Cargo.toml");
    let target_dir = native().join("target-sessiond");
    let mut env: Vec<(&str, OsString)> = vec![
        ("PATH", path),
        ("ZIG_GLOBAL_CACHE_DIR", cache().join("zig-global").into_os_string()),
        ("MACOSX_DEPLOYMENT_TARGET_x86_64_apple_darwin", X64_MACOS_DEPLOYMENT.into()),
        // 绕过 rustup 代理时 rust-lld / 构建脚本找不到 libLLVM（同 build-web.sh）
        ("DYLD_FALLBACK_LIBRARY_PATH", tc.parent().expect("工具链目录").join("lib").into_os_string()),
    ];
    let env_for = |env: &mut Vec<(&str, OsString)>, triple: &str| {
        env.retain(|(k, _)| *k != "MACOSX_DEPLOYMENT_TARGET");
        if triple == "x86_64-apple-darwin" {
            env.push(("MACOSX_DEPLOYMENT_TARGET", X64_MACOS_DEPLOYMENT.into()));
        }
    };

    if check {
        // 零警告：每个目标都 check 一遍（cfg(windows) 的代码只有交叉 check 才看得到），
        // 单独的 target 目录，不让 -D warnings 冲掉正常构建的缓存
        for (triple, _) in targets {
            env_for(&mut env, triple);
            let mut e = env.clone();
            e.push(("RUSTFLAGS", "-D warnings".into()));
            let er: Vec<(&str, &std::ffi::OsStr)> = e.iter().map(|(k, v)| (*k, v.as_os_str())).collect();
            println!("== cargo check（{triple}，-D warnings）==");
            run_env(
                "cargo",
                [
                    OsString::from("check"),
                    "--manifest-path".into(),
                    manifest.clone().into(),
                    "--workspace".into(),
                    "--all-targets".into(),
                    "--target".into(),
                    (*triple).into(),
                    "--target-dir".into(),
                    target_dir.join("check").into(),
                ],
                None,
                &er,
            )?;
        }
    }
    if test {
        env_for(&mut env, host_triple);
        let er: Vec<(&str, &std::ffi::OsStr)> = env.iter().map(|(k, v)| (*k, v.as_os_str())).collect();
        println!("== cargo test（{host_triple}）==");
        run_env(
            "cargo",
            [
                OsString::from("test"),
                "--manifest-path".into(),
                manifest.clone().into(),
                "--workspace".into(),
                "--target".into(),
                host_triple.into(),
                "--target-dir".into(),
                target_dir.clone().into(),
            ],
            None,
            &er,
        )?;
    }
    if check || test {
        return Ok(());
    }

    for (triple, _) in targets {
        env_for(&mut env, triple);
        let er: Vec<(&str, &std::ffi::OsStr)> = env.iter().map(|(k, v)| (*k, v.as_os_str())).collect();
        // 本机目标用系统链接器；其余交给 cargo-zigbuild（zig cc 当链接器，带各平台的 libc）
        let sub = if *triple == host_triple && cfg!(target_os = "macos") { "build" } else { "zigbuild" };
        if sub == "zigbuild" && !cargo_bin.as_ref().is_some_and(|b| b.join("cargo-zigbuild").is_file()) && crate::util::which("cargo-zigbuild").is_none() {
            bail!("找不到 cargo-zigbuild：cargo install cargo-zigbuild --locked");
        }
        println!("== cargo {sub} --release --target {triple} ==");
        run_env(
            "cargo",
            [
                OsString::from(sub),
                "--release".into(),
                "--manifest-path".into(),
                manifest.clone().into(),
                "-p".into(),
                "falcon-sessiond".into(),
                "--target".into(),
                (*triple).into(),
                "--target-dir".into(),
                target_dir.clone().into(),
            ],
            None,
            &er,
        )?;
        let out = artifact(triple);
        let size = std::fs::metadata(&out).with_context(|| format!("没有产物 {}", out.display()))?.len();
        println!("falcon-sessiond（{triple}）：{}（{:.1} MB）", out.display(), size as f64 / 1e6);
    }
    Ok(())
}
