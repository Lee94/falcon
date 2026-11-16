//! `cargo xtask android`：构建 Falcon 安卓客户端（Rust JNI 动态库 + 字体解包 + Gradle APK 打包）。
//!
//! 流程：
//! 1. 将 `native/assets/fonts/berkeley-mono/*.woff2` 解压为 TTF 写入 `android/app/src/main/assets/fonts/`
//!    （不将商业 TTF 提交入库，仅在构建期解包）；
//! 2. 使用 Android NDK 的 LLVM clang 交叉编译 `falcon-android` 的 JNI 动态库，拷入
//!    `android/app/src/main/jniLibs/<abi>/`。默认只编真机主流架构 `arm64-v8a`（`aarch64-linux-android`）；
//!    模拟器联调传 `--abis x86_64` 或 `--abis all`。没选中的 ABI 目录先删掉，免得本地早先编的
//!    旧动态库被 Gradle 一起打进 APK；
//! 3. 调用 Gradle 打出可安装的 APK，并复制到 `release/falcon-v<版本>-android.apk`。

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::util;

/// (Rust 三元组, ABI, 链接器环境变量, NDK clang)
type AndroidTarget = (&'static str, &'static str, &'static str, &'static str);

const FONTS: &[(&str, &str)] = &[
    ("berkeley-mono/TX-02-Regular.woff2", "TX-02-Regular.ttf"),
    ("berkeley-mono/TX-02-Bold.woff2", "TX-02-Bold.ttf"),
    ("berkeley-mono/TX-02-Oblique.woff2", "TX-02-Oblique.ttf"),
    ("berkeley-mono/TX-02-BoldOblique.woff2", "TX-02-BoldOblique.ttf"),
];

const ALL_TARGETS: &[AndroidTarget] = &[
    (
        "aarch64-linux-android",
        "arm64-v8a",
        "CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER",
        "aarch64-linux-android26-clang",
    ),
    (
        "x86_64-linux-android",
        "x86_64",
        "CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER",
        "x86_64-linux-android26-clang",
    ),
];

/// `--abis` 的取值：缺省只要 arm64-v8a；`all` 是全部；否则逗号分隔的 ABI 名或 Rust 三元组
fn resolve_targets(abis_arg: Option<&str>) -> Result<Vec<AndroidTarget>> {
    let Some(raw) = abis_arg else {
        return Ok(vec![ALL_TARGETS[0]]);
    };
    if raw == "all" {
        return Ok(ALL_TARGETS.to_vec());
    }
    let mut out: Vec<AndroidTarget> = Vec::new();
    for item in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let Some(&entry) = ALL_TARGETS.iter().find(|(triple, abi, _, _)| *abi == item || *triple == item) else {
            bail!("不支持的 Android ABI：{item}（可选：arm64-v8a, x86_64, all）");
        };
        if !out.iter().any(|(_, abi, _, _)| *abi == entry.1) {
            out.push(entry);
        }
    }
    if out.is_empty() {
        bail!("--abis 不能为空（可选：arm64-v8a, x86_64, all）");
    }
    Ok(out)
}

pub fn run(args: &[String]) -> Result<()> {
    let mut jni_only = false;
    let mut abis_arg: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--jni-only" => jni_only = true,
            "--abis" => abis_arg = Some(it.next().context("--abis 后面缺 ABI 名称")?.clone()),
            other => bail!("未知参数：{other}"),
        }
    }
    let targets = resolve_targets(abis_arg.as_deref())?;
    let root = util::root();
    let native_dir = util::native();
    let android_dir = root.join("android");

    println!("==> 1/3 解压终端内嵌字体到 android/app/src/main/assets/fonts/");
    unpack_fonts(&native_dir, &android_dir)?;

    let sdk_dir = find_android_sdk()?;
    let ndk_bin = find_ndk_clang_bin(&sdk_dir)?;

    let abi_names: Vec<&str> = targets.iter().map(|(_, abi, _, _)| *abi).collect();
    println!("==> 2/3 交叉编译 falcon-android JNI 动态库（{}）", abi_names.join(", "));
    let jni_libs = android_dir.join("app/src/main/jniLibs");
    for (_, abi, _, _) in ALL_TARGETS {
        if !abi_names.contains(abi) {
            let stale = jni_libs.join(abi);
            if stale.exists() {
                std::fs::remove_dir_all(&stale).with_context(|| format!("删除 {} 失败", stale.display()))?;
            }
        }
    }
    // 设了 RUSTUP_TOOLCHAIN 就用它（CI 借此固定 Rust 版本），否则用 stable
    let tc = util::rustup_toolchain_bin()?;
    let cargo_bin = tc.join("cargo");
    let rustc_bin = tc.join("rustc");
    let mut path_entries = vec![tc.clone()];
    if let Some(orig) = std::env::var_os("PATH") {
        path_entries.extend(std::env::split_paths(&orig));
    }
    let joined_path = std::env::join_paths(path_entries)?;
    let dylib_fallback = tc
        .parent()
        .expect("工具链目录")
        .join("lib")
        .into_os_string();

    let strip_bin = ndk_bin.join("llvm-strip");
    for (rust_target, abi, linker_env, clang_name) in &targets {
        let clang_path = ndk_bin.join(clang_name);
        if !clang_path.is_file() {
            bail!("找不到 NDK 编译器：{}", clang_path.display());
        }
        util::run_env(
            &cargo_bin,
            ["build", "--release", "-p", "falcon-android", "--target", rust_target],
            Some(&native_dir),
            &[
                ("PATH", joined_path.as_os_str()),
                ("RUSTC", rustc_bin.as_os_str()),
                ("DYLD_FALLBACK_LIBRARY_PATH", dylib_fallback.as_os_str()),
                (linker_env, clang_path.as_os_str()),
            ],
        )?;
        let built_so = native_dir
            .join("target")
            .join(rust_target)
            .join("release")
            .join("libfalcon_android.so");
        let dst_dir = jni_libs.join(abi);
        std::fs::create_dir_all(&dst_dir)?;
        let dst_so = dst_dir.join("libfalcon_android.so");
        std::fs::copy(&built_so, &dst_so).with_context(|| {
            format!("复制 {} -> {} 失败", built_so.display(), dst_so.display())
        })?;
        if strip_bin.is_file() {
            let _ = util::run(&strip_bin, [OsStr::new("--strip-unneeded"), dst_so.as_os_str()], None);
        }
        println!("  已生成 {}", dst_so.strip_prefix(&root).unwrap_or(&dst_so).display());
    }

    if jni_only {
        return Ok(());
    }

    println!("==> 3/3 运行 Gradle 构建安卓安装包（assembleRelease）");
    let local_props = format!("sdk.dir={}\n", sdk_dir.display());
    std::fs::write(android_dir.join("local.properties"), local_props)?;

    let java_home = find_java_home();
    let gradlew = android_dir.join("gradlew");
    let mut envs: Vec<(&str, &OsStr)> = vec![("ANDROID_HOME", sdk_dir.as_os_str())];
    if let Some(ref jh) = java_home {
        envs.push(("JAVA_HOME", jh.as_os_str()));
    }
    util::run_env(
        &gradlew,
        ["assembleRelease", "--no-daemon"],
        Some(&android_dir),
        &envs,
    )?;

    let built_apk = android_dir.join("app/build/outputs/apk/release/app-release.apk");
    if !built_apk.is_file() {
        bail!("找不到构建出的 APK：{}", built_apk.display());
    }
    let release_dir = root.join("release");
    std::fs::create_dir_all(&release_dir)?;
    let out_apk = release_dir.join(format!("falcon-v{}-android.apk", util::version()));
    std::fs::copy(&built_apk, &out_apk)?;
    let size_mb = std::fs::metadata(&out_apk)?.len() as f64 / (1024.0 * 1024.0);
    println!(
        "完成：{}（{size_mb:.2} MB）",
        out_apk.strip_prefix(&root).unwrap_or(&out_apk).display()
    );
    Ok(())
}

fn unpack_fonts(native_dir: &Path, android_dir: &Path) -> Result<()> {
    let fonts_src = native_dir.join("assets/fonts");
    let fonts_dst = android_dir.join("app/src/main/assets/fonts");
    std::fs::create_dir_all(&fonts_dst)?;
    for (rel_src, dst_name) in FONTS {
        let src_path = fonts_src.join(rel_src);
        let dst_path = fonts_dst.join(dst_name);
        let data = std::fs::read(&src_path)
            .with_context(|| format!("读取字体 {} 失败", src_path.display()))?;
        let mut buf = bytes::Bytes::from(data);
        let ttf = woff2::convert_woff2_to_ttf(&mut buf)
            .map_err(|e| anyhow::anyhow!("解压 {} 失败：{e:?}", src_path.display()))?;
        std::fs::write(&dst_path, ttf)?;
    }
    Ok(())
}

fn find_android_sdk() -> Result<PathBuf> {
    for env_key in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        if let Some(val) = std::env::var_os(env_key) {
            let p = PathBuf::from(val);
            if p.is_dir() {
                return Ok(p);
            }
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let mac_sdk = PathBuf::from(&home).join("Library/Android/sdk");
        if mac_sdk.is_dir() {
            return Ok(mac_sdk);
        }
        let linux_sdk = PathBuf::from(&home).join("Android/Sdk");
        if linux_sdk.is_dir() {
            return Ok(linux_sdk);
        }
    }
    let cache_sdk = PathBuf::from("/Volumes/faye/Cache/android-sdk");
    if cache_sdk.is_dir() {
        return Ok(cache_sdk);
    }
    bail!("未找到 Android SDK，请设置 ANDROID_HOME 或将 SDK 安装在 ~/Library/Android/sdk")
}

fn find_ndk_clang_bin(sdk_dir: &Path) -> Result<PathBuf> {
    let env_ndk = ["ANDROID_NDK_HOME", "ANDROID_NDK_ROOT"]
        .into_iter()
        .find_map(|key| std::env::var_os(key).map(PathBuf::from));
    resolve_ndk_clang_bin(sdk_dir, env_ndk.as_deref())
}

fn resolve_ndk_clang_bin(sdk_dir: &Path, env_ndk: Option<&Path>) -> Result<PathBuf> {
    // GitHub Actions 等 CI 会预装多个 NDK（含非 LTS 预览版），并用 ANDROID_NDK_HOME 指到默认 LTS 版本；
    // 优先认环境变量，未设时再按数字版本号取 sdk/ndk/ 下最高的一版（避免字典序把 9.x 排在 27.x 后面）。
    let ndk_dir = match env_ndk.filter(|p| p.is_dir()) {
        Some(dir) => dir.to_path_buf(),
        None => {
            let ndk_root = sdk_dir.join("ndk");
            if !ndk_root.is_dir() {
                bail!("{} 下没有 ndk 目录", sdk_dir.display());
            }
            let mut versions: Vec<PathBuf> = std::fs::read_dir(&ndk_root)?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.is_dir())
                .collect();
            versions.sort_by_key(|p| ndk_version_key(p));
            versions.pop().context("ndk 目录下没有已安装的 NDK 版本")?
        }
    };
    let prebuilt = ndk_dir.join("toolchains/llvm/prebuilt");
    let mut hosts: Vec<PathBuf> = std::fs::read_dir(&prebuilt)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    hosts.sort();
    let host_dir = hosts.pop().context("找不到 NDK llvm prebuilt 目录")?;
    Ok(host_dir.join("bin"))
}

fn ndk_version_key(path: &Path) -> Vec<u64> {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .split('.')
        .map(|seg| seg.parse::<u64>().unwrap_or(0))
        .collect()
}

fn find_java_home() -> Option<PathBuf> {
    if let Some(jh) = std::env::var_os("JAVA_HOME") {
        let p = PathBuf::from(jh);
        if p.is_dir() {
            return Some(p);
        }
    }
    for candidate in [
        "/opt/homebrew/opt/openjdk@21/libexec/openjdk.jdk/Contents/Home",
        "/opt/homebrew/opt/openjdk@17/libexec/openjdk.jdk/Contents/Home",
        "/opt/homebrew/opt/openjdk/libexec/openjdk.jdk/Contents/Home",
    ] {
        let p = PathBuf::from(candidate);
        if p.is_dir() {
            return Some(p);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ndk_selection_prefers_env_and_sorts_versions_numerically() {
        let tmp = util::TempDir::new("android-ndk-test").unwrap();
        let sdk = tmp.path();
        for ver in ["9.1.0", "27.2.12479018", "26.3.11579264"] {
            std::fs::create_dir_all(sdk.join(format!("ndk/{ver}/toolchains/llvm/prebuilt/linux-x86_64/bin"))).unwrap();
        }
        // 未传环境变量时按数字版本选 27.2，不被字典序的 9.1 盖过
        let picked = resolve_ndk_clang_bin(sdk, None).unwrap();
        assert_eq!(
            picked,
            sdk.join("ndk/27.2.12479018/toolchains/llvm/prebuilt/linux-x86_64/bin")
        );

        // 传入 ANDROID_NDK_HOME 时优先采用指定目录（如 runner 默认 LTS NDK）
        let pinned = sdk.join("ndk/26.3.11579264");
        let picked_env = resolve_ndk_clang_bin(sdk, Some(&pinned)).unwrap();
        assert_eq!(
            picked_env,
            pinned.join("toolchains/llvm/prebuilt/linux-x86_64/bin")
        );
    }

    #[test]
    fn default_targets_selects_only_arm64_v8a_and_supports_all_override() {
        let abis = |arg: Option<&str>| -> Vec<&str> {
            resolve_targets(arg).unwrap().into_iter().map(|(_, abi, _, _)| abi).collect()
        };
        assert_eq!(abis(None), vec!["arm64-v8a"]);
        assert_eq!(abis(Some("all")), vec!["arm64-v8a", "x86_64"]);
        assert_eq!(abis(Some("x86_64")), vec!["x86_64"]);
        // 三元组与 ABI 名都认，重复的只留一个
        assert_eq!(abis(Some("aarch64-linux-android,arm64-v8a")), vec!["arm64-v8a"]);
        assert!(resolve_targets(Some("armeabi-v7a")).is_err());
        assert!(resolve_targets(Some(",")).is_err());
    }
}
