//! `cargo xtask tailnet`：编客户端内置 Tailscale 节点的动态库 libfalcon_tailnet（ADR 0030 决定七）。
//!
//! Go 模块在 `native/tailnet/`，`go build -buildmode=c-shared` 出一份只暴露 C ABI 的动态库，
//! 客户端的 `falcon-tailnet` 用 dlopen 加载。Go 工具链跟 Zig 一样由这里钉哈希下载到
//! `native/.cache/go/`，不要求系统装 Go；Go 的模块与编译缓存也放在 `native/.cache/go/` 下。
//!
//! c-shared 要 cgo，各目标的 C 工具链：
//!
//! - macOS：系统 clang（`-arch` 指定架构）。只编本机架构，`cargo xtask pkg` 也只打本机架构；
//! - Windows：`zig cc -target x86_64-windows-gnu`（Zig 由 `sessiond.rs` 管）；在 Windows 上编
//!   时改用 PATH 上的 mingw gcc。出来的 DLL 只有 C ABI，MSVC 构建的客户端照样能加载；
//! - Android：NDK 的 clang（与 `cargo xtask android` 找的是同一套）；
//! - Linux：系统 cc。Linux 只发服务端，这个目标只给开发用。
//!
//! 产物：`native/target-tailnet/<Rust 三元组>/<库文件名>`，与 `falcon-tailnet` 开发构建的
//! 查找位置一致。

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::sessiond::{cache, sha256_hex};
use crate::util::{download, native, run_env};

/// 钉死的 Go。tailscale.com 的 go.mod 要求多新，这里就得多新（v1.104.2 要 1.27.3）。
/// 升级时把四个宿主平台归档的 sha256 一起换掉（来自 https://go.dev/dl/?mode=json）
const GO_VERSION: &str = "1.27.4";
const GO_ARCHIVES: [(&str, &str); 4] = [
    ("darwin-arm64.tar.gz", "91a44a2b8a716600ad94a4f143e156116620db8828086a6e3602291be9bd0bb4"),
    ("linux-amd64.tar.gz", "7226736c2339719cda28fbf3a54d71aa63bcefe376e98cef00034268dab24840"),
    ("linux-arm64.tar.gz", "3392582988a273a87ab5043ee1b2c9c17a2c3c00580bf9326c47a2e0aaaeac92"),
    ("windows-amd64.zip", "178ab98950ac69ab25069307dc6ade79f3816e414cce95b4da166a115cde1e7e"),
];

#[derive(Clone, Copy)]
enum Cc {
    /// 系统 clang 加 `-arch <架构>`
    Apple(&'static str),
    /// `zig cc -target <Zig 目标>`；Windows 宿主上换成 mingw gcc
    Zig(&'static str),
    /// NDK 的 clang 驱动（带 API 级别的那个文件名）
    Ndk(&'static str),
    /// 系统 cc（只用于本机目标）
    Host,
}

#[derive(Clone, Copy)]
struct Target {
    /// 消费这份库的 Rust 三元组，也是产物目录名
    triple: &'static str,
    goos: &'static str,
    goarch: &'static str,
    cc: Cc,
}

const TARGETS: [Target; 7] = [
    Target { triple: "aarch64-apple-darwin", goos: "darwin", goarch: "arm64", cc: Cc::Apple("arm64") },
    Target { triple: "x86_64-apple-darwin", goos: "darwin", goarch: "amd64", cc: Cc::Apple("x86_64") },
    // 客户端是 MSVC 构建；DLL 用 mingw 工具链编，只暴露 C ABI（ADR 0030 决定一）
    Target { triple: "x86_64-pc-windows-msvc", goos: "windows", goarch: "amd64", cc: Cc::Zig("x86_64-windows-gnu") },
    Target { triple: "aarch64-linux-android", goos: "android", goarch: "arm64", cc: Cc::Ndk("aarch64-linux-android29-clang") },
    Target { triple: "x86_64-linux-android", goos: "android", goarch: "amd64", cc: Cc::Ndk("x86_64-linux-android29-clang") },
    Target { triple: "x86_64-unknown-linux-gnu", goos: "linux", goarch: "amd64", cc: Cc::Host },
    Target { triple: "aarch64-unknown-linux-gnu", goos: "linux", goarch: "arm64", cc: Cc::Host },
];

/// 某个目标的库文件名（与 `libloading::library_filename("falcon_tailnet")` 一致）
fn lib_name(goos: &str) -> &'static str {
    match goos {
        "darwin" => "libfalcon_tailnet.dylib",
        "windows" => "falcon_tailnet.dll",
        _ => "libfalcon_tailnet.so",
    }
}

fn go_dir() -> PathBuf {
    cache().join("go")
}

fn go_host() -> Result<&'static str> {
    Ok(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "darwin-arm64.tar.gz",
        ("linux", "x86_64") => "linux-amd64.tar.gz",
        ("linux", "aarch64") => "linux-arm64.tar.gz",
        ("windows", "x86_64") => "windows-amd64.zip",
        (os, arch) => bail!("没有钉 {os}-{arch} 的 Go 工具链"),
    })
}

/// 确保钉死版本的 Go 就绪，返回 go 可执行文件
fn ensure_go() -> Result<PathBuf> {
    let host = go_host()?;
    let name = format!("go{GO_VERSION}.{host}");
    let root = go_dir().join(format!("go{GO_VERSION}-{}", host.split('.').next().expect("平台名")));
    let go = root.join("go").join("bin").join(if cfg!(windows) { "go.exe" } else { "go" });
    if go.is_file() {
        return Ok(go);
    }
    let sha = GO_ARCHIVES.iter().find(|(h, _)| *h == host).map(|(_, s)| *s).expect("在表里");
    let archive = go_dir().join(&name);
    if !archive.is_file() || sha256_hex(&archive)? != sha {
        download(&format!("https://go.dev/dl/{name}"), &archive)?;
    }
    let got = sha256_hex(&archive)?;
    if got != sha {
        bail!("{} 校验不过：期望 {sha}，实际 {got}", archive.display());
    }
    // 解到临时目录再改名：中途失败不会留下一个看起来能用的半截工具链
    let staging = root.with_extension("part");
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    std::fs::create_dir_all(&staging)?;
    // macOS 的 bsdtar、GNU tar 与 Windows 自带的 tar.exe（也是 bsdtar）都能直接解 .tar.gz / .zip
    crate::util::run("tar", [OsString::from("-xf"), archive.clone().into(), "-C".into(), staging.clone().into()], None)?;
    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }
    std::fs::rename(&staging, &root)?;
    if !go.is_file() {
        bail!("解开 {} 后没找到 {}", archive.display(), go.display());
    }
    Ok(go)
}

/// 所有 go 命令共用的环境：缓存都放进 native/.cache/go，不碰 ~/go 与 ~/.cache；不许 Go 自己
/// 去下别的工具链（GOTOOLCHAIN=local），也不吃用户全局的 GOFLAGS / go.work
fn go_env() -> Vec<(&'static str, OsString)> {
    vec![
        ("GOTOOLCHAIN", "local".into()),
        ("GOFLAGS", "".into()),
        ("GOWORK", "off".into()),
        ("GOPATH", go_dir().join("path").into_os_string()),
        ("GOMODCACHE", go_dir().join("mod").into_os_string()),
        ("GOCACHE", go_dir().join("build").into_os_string()),
        ("GOENV", "off".into()),
    ]
}

fn run_go(go: &Path, args: &[&str], extra: &[(&'static str, OsString)]) -> Result<()> {
    let mut env = go_env();
    env.extend(extra.iter().cloned());
    let env: Vec<(&str, &std::ffi::OsStr)> = env.iter().map(|(k, v)| (*k, v.as_os_str())).collect();
    run_env(go, args, Some(&native().join("tailnet")), &env)
}

/// cgo 认的 CC 字段按空白切分、支持引号，路径里有空格时要包起来
fn quote(p: &Path) -> String {
    format!("\"{}\"", p.display())
}

/// 一个目标的 CC 与额外环境
fn cc_env(t: &Target) -> Result<Vec<(&'static str, OsString)>> {
    let mut env = Vec::new();
    match t.cc {
        Cc::Apple(arch) => {
            if std::env::consts::OS != "macos" {
                bail!("{} 只能在 macOS 上编", t.triple);
            }
            // Go 1.27 自己的目标文件按 macOS 13 编（tailscale 的 certstore 也用到 12 起才有的 API），
            // 库的部署目标跟着定 13.0。客户端本身仍是 11.0：更老的系统上 dlopen 会失败，
            // falcon-tailnet 报「加载失败」，其余功能不受影响
            env.push(("CC", format!("clang -arch {arch}").into()));
            env.push(("MACOSX_DEPLOYMENT_TARGET", "13.0".into()));
            env.push(("CGO_CFLAGS", "-O2 -mmacosx-version-min=13.0".into()));
            env.push(("CGO_LDFLAGS", "-mmacosx-version-min=13.0".into()));
        }
        Cc::Zig(zig_target) => {
            if cfg!(windows) {
                let gcc = crate::util::require_tool("gcc.exe", "装 mingw-w64（例如 MSYS2 的 mingw-w64-x86_64-gcc）并放进 PATH")?;
                env.push(("CC", quote(&gcc).into()));
            } else {
                let zig = crate::sessiond::ensure_zig()?;
                env.push(("CC", format!("{} cc -target {zig_target}", quote(&zig)).into()));
                env.push(("ZIG_GLOBAL_CACHE_DIR", cache().join("zig-global").into_os_string()));
                env.push(("ZIG_LOCAL_CACHE_DIR", cache().join("zig-local").into_os_string()));
            }
        }
        Cc::Ndk(clang) => {
            let sdk = crate::android::find_android_sdk()?;
            let bin = crate::android::find_ndk_clang_bin(&sdk)?;
            let clang = bin.join(clang);
            if !clang.is_file() {
                bail!("找不到 NDK 编译器：{}", clang.display());
            }
            env.push(("CC", quote(&clang).into()));
        }
        Cc::Host => {
            let host = host_target()?;
            if host.triple != t.triple {
                bail!("{} 用系统 cc 编，只能在同平台同架构的机器上编（本机是 {}）", t.triple, host.triple);
            }
        }
    }
    Ok(env)
}

/// 本机对应的那个目标
fn host_target() -> Result<Target> {
    let triple = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        (os, arch) => bail!("{os}-{arch} 上没有对应的 tailnet 目标"),
    };
    Ok(*TARGETS.iter().find(|t| t.triple == triple).expect("在表里"))
}

/// 编一个目标，返回产物路径
fn build(go: &Path, t: &Target) -> Result<PathBuf> {
    let out = native().join("target-tailnet").join(t.triple).join(lib_name(t.goos));
    std::fs::create_dir_all(out.parent().expect("产物目录"))?;
    let mut env = cc_env(t)?;
    env.push(("CGO_ENABLED", "1".into()));
    env.push(("GOOS", t.goos.into()));
    env.push(("GOARCH", t.goarch.into()));
    // -s -w 去掉符号表与 DWARF，体积小一截。安卓 15 起有 16 KB 页的设备，Play 要求 .so 按
    // 16 KB 对齐（NDK r28 起缺省如此，旧 NDK 要显式给）
    let ldflags = if t.goos == "android" {
        "-ldflags=-s -w -extldflags=-Wl,-z,max-page-size=16384"
    } else {
        "-ldflags=-s -w"
    };
    let out_str = out.to_str().context("产物路径不是 UTF-8")?;
    println!("==> libfalcon_tailnet（{}）", t.triple);
    run_go(go, &["build", "-buildmode=c-shared", "-trimpath", ldflags, "-o", out_str, "."], &env)?;
    // c-shared 顺带生成的 C 头文件：Rust 侧手写签名，用不上
    let header = out.with_extension("h");
    if header.is_file() {
        std::fs::remove_file(&header)?;
    }
    let size = std::fs::metadata(&out)?.len();
    println!("    {}（{:.1} MB）", out.display(), size as f64 / 1_048_576.0);
    Ok(out)
}

/// `--targets host|all|android|<三元组,…>`；没给就是本机
fn parse_targets(args: &[String]) -> Result<(Vec<Target>, Vec<String>)> {
    let mut targets = None;
    let mut rest = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--targets" {
            let v = it.next().context("--targets 后面要跟目标")?;
            let mut list = Vec::new();
            for item in v.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                match item {
                    "host" => list.push(host_target()?),
                    "all" => list.extend(TARGETS),
                    "android" => list.extend(TARGETS.iter().filter(|t| t.goos == "android")),
                    triple => list.push(
                        *TARGETS.iter().find(|t| t.triple == triple).with_context(|| {
                            let known: Vec<&str> = TARGETS.iter().map(|t| t.triple).collect();
                            format!("未知目标 {triple}（可选：host / all / android / {}）", known.join(" / "))
                        })?,
                    ),
                }
            }
            targets = Some(list);
        } else {
            rest.push(a.clone());
        }
    }
    let targets = match targets {
        Some(t) => t,
        None => vec![host_target()?],
    };
    Ok((targets, rest))
}

/// `cargo xtask tailnet [--targets …] [--check] [--tidy]`
pub fn run(args: &[String]) -> Result<()> {
    let (targets, rest) = parse_targets(args)?;
    let mut check = false;
    let mut tidy = false;
    for a in &rest {
        match a.as_str() {
            "--check" => check = true,
            "--tidy" => tidy = true,
            other => bail!("未知参数：{other}"),
        }
    }
    let go = ensure_go()?;
    if tidy {
        // 升 tailscale.com 时跑：重写 go.mod / go.sum。平时构建只读 go.sum，不联网改它
        println!("==> go mod tidy");
        run_go(&go, &["mod", "tidy"], &[])?;
        return Ok(());
    }
    if check {
        // 本机目标上 go vet（要 cgo，C 工具链与本机构建同一套）
        let host = host_target()?;
        let mut env = cc_env(&host)?;
        env.push(("CGO_ENABLED", "1".into()));
        println!("==> go vet（{}）", host.triple);
        run_go(&go, &["vet", "./..."], &env)?;
        return Ok(());
    }
    for t in &targets {
        build(&go, t)?;
    }
    Ok(())
}
