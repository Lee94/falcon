//! `cargo xtask android`：构建 Falcon 安卓客户端（纯 Rust NSH + Android SDK 原生打包，零 Gradle / 零 Kotlin）。
//!
//! 流程（ADR 0028）：
//! 1. 使用 Android NDK 的 LLVM clang 交叉编译 `falcon-app`（`--lib`）的 `aarch64-linux-android`
//!    （`arm64-v8a`）与 `x86_64-linux-android`（`x86_64`）动态库，按 `nsh-platform-android` 的
//!    `AppActivity` 约定命名为 `lib/<abi>/libnsh.so`（终端字体已由 `falcon-nsh/build.rs` 内嵌）；
//! 2. 通过 `cargo metadata` 定位 `nsh-platform-android` 的纯 Java 宿主层源码，直接调用 JDK `javac` 与
//!    Android SDK `build-tools` 的 `d8` 编译为 `classes.dex`；
//! 3. 生成 `com.falcon.android` 的 `AndroidManifest.xml`，用 `aapt2` 编译资源并链接基础 APK，经 `jar uf0`
//!    无压缩注入 `classes.dex` 与 `lib/<abi>/libnsh.so`，再用 `zipalign` 页对齐、`apksigner` 签名，
//!    输出 `release/falcon-v<版本>-android.apk`。

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::util::{self, TempDir};

const TARGETS: &[(&str, &str, &str, &str)] = &[
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

pub fn run(args: &[String]) -> Result<()> {
    let mut jni_only = false;
    for a in args {
        match a.as_str() {
            "--jni-only" => jni_only = true,
            other => bail!("未知参数：{other}"),
        }
    }

    let root = util::root();
    let native_dir = util::native();
    let sdk_dir = find_android_sdk()?;
    let ndk_bin = find_ndk_clang_bin(&sdk_dir)?;

    let stage = TempDir::new("android-nsh")?;
    let apk_extra = stage.path().join("apk-extra");
    std::fs::create_dir_all(&apk_extra)?;

    println!("==> 1/3 交叉编译 falcon-app NSH 动态库（arm64-v8a, x86_64 -> libnsh.so）");
    let tc = rustup_toolchain_bin()?;
    let cargo_bin = tc.join("cargo");
    let rustc_bin = tc.join("rustc");
    let mut path_entries = vec![tc.clone(), ndk_bin.clone()];
    if let Some(orig) = std::env::var_os("PATH") {
        path_entries.extend(std::env::split_paths(&orig));
    }
    let joined_path = std::env::join_paths(path_entries)?;
    let dylib_fallback = tc
        .parent()
        .expect("工具链目录")
        .join("lib")
        .into_os_string();

    let strip_bin = ndk_bin.join(if cfg!(windows) { "llvm-strip.exe" } else { "llvm-strip" });
    for (rust_target, abi, linker_env, clang_name) in TARGETS {
        let target_tools = ndk_target_env(&ndk_bin, rust_target, linker_env, clang_name)?;
        let mut build_env: Vec<(&str, &OsStr)> = vec![
            ("PATH", joined_path.as_os_str()),
            ("RUSTC", rustc_bin.as_os_str()),
            ("DYLD_FALLBACK_LIBRARY_PATH", dylib_fallback.as_os_str()),
        ];
        for (k, v) in &target_tools {
            build_env.push((k.as_str(), v.as_os_str()));
        }
        util::run_env(
            &cargo_bin,
            [
                "build",
                "--release",
                "-p",
                "falcon-app",
                "--lib",
                "--target",
                rust_target,
            ],
            Some(&native_dir),
            &build_env,
        )?;
        let built_so = native_dir
            .join("target")
            .join(rust_target)
            .join("release")
            .join("libfalcon_app.so");
        let abi_dir = apk_extra.join("lib").join(abi);
        std::fs::create_dir_all(&abi_dir)?;
        // nsh-platform-android 的 AppActivity 在 meta-data com.longbridge.nsh.RUST=true 时固定加载 libnsh.so
        let dst_so = abi_dir.join("libnsh.so");
        std::fs::copy(&built_so, &dst_so).with_context(|| {
            format!("复制 {} -> {} 失败", built_so.display(), dst_so.display())
        })?;
        if strip_bin.is_file() {
            let _ = util::run(&strip_bin, [OsStr::new("--strip-unneeded"), dst_so.as_os_str()], None);
        }
        if jni_only {
            let out_jni = native_dir.join("target/android-jni").join(abi);
            std::fs::create_dir_all(&out_jni)?;
            let out_so = out_jni.join("libnsh.so");
            std::fs::copy(&dst_so, &out_so)?;
            println!(
                "  已生成 {}",
                out_so.strip_prefix(&root).unwrap_or(&out_so).display()
            );
        }
    }

    if jni_only {
        return Ok(());
    }

    println!("==> 2/3 编译 nsh-platform-android Java 宿主层（javac + d8，零 Gradle）");
    let android_jar = resolve_android_jar(&sdk_dir)?;
    let build_tools = resolve_build_tools_dir(&sdk_dir)?;
    let java_home = find_java_home();
    let javac_bin = jdk_tool("javac", java_home.as_deref())?;
    let jar_bin = jdk_tool("jar", java_home.as_deref())?;
    let keytool_bin = jdk_tool("keytool", java_home.as_deref())?;

    let nsh_android_dir = util::cargo_package_dir("nsh-platform-android")?;
    let java_src_dir = nsh_android_dir.join("java/com/longbridge/nsh");
    let mut java_files = Vec::new();
    collect_files_with_ext(&java_src_dir, "java", &mut java_files)?;
    if java_files.is_empty() {
        bail!("{} 下没有找到 .java 源文件", java_src_dir.display());
    }

    let classes_dir = stage.path().join("classes");
    std::fs::create_dir_all(&classes_dir)?;
    let mut javac_args: Vec<OsString> = vec![
        "--release".into(),
        "17".into(),
        "-encoding".into(),
        "UTF-8".into(),
        "-cp".into(),
        android_jar.clone().into_os_string(),
        "-d".into(),
        classes_dir.clone().into_os_string(),
    ];
    javac_args.extend(java_files.into_iter().map(PathBuf::into_os_string));
    util::run(&javac_bin, &javac_args, None)?;

    let mut class_files = Vec::new();
    collect_files_with_ext(&classes_dir, "class", &mut class_files)?;
    if class_files.is_empty() {
        bail!("javac 未产出任何 .class 文件");
    }

    // 先打成单一 classes.jar 再交给 d8，避免向脚本包装器传入上百个含 `$` 内部类名的路径参数
    let classes_jar = stage.path().join("classes.jar");
    util::run(
        &jar_bin,
        [
            OsStr::new("cf"),
            classes_jar.as_os_str(),
            OsStr::new("-C"),
            classes_dir.as_os_str(),
            OsStr::new("."),
        ],
        None,
    )?;

    let d8_bin = sdk_tool(&build_tools, "d8")?;
    let d8_args: Vec<OsString> = vec![
        "--release".into(),
        "--min-api".into(),
        "26".into(),
        "--lib".into(),
        android_jar.clone().into_os_string(),
        "--output".into(),
        apk_extra.clone().into_os_string(),
        classes_jar.into_os_string(),
    ];
    let mut java_env: Vec<(&str, &OsStr)> = Vec::new();
    if let Some(ref jh) = java_home {
        java_env.push(("JAVA_HOME", jh.as_os_str()));
    }
    util::run_env(&d8_bin, &d8_args, None, &java_env)?;
    if !apk_extra.join("classes.dex").is_file() {
        bail!("d8 未产出 classes.dex");
    }

    println!("==> 3/3 链接资源并签名 APK（aapt2 + zipalign + apksigner）");
    let ver = util::version();
    let ver_code = version_code_from_semver(ver);
    let manifest_path = stage.path().join("AndroidManifest.xml");
    std::fs::write(&manifest_path, android_manifest_xml(ver_code, ver))?;

    let res_dir = stage.path().join("res");
    let mipmap_dir = res_dir.join("mipmap-xxxhdpi");
    std::fs::create_dir_all(&mipmap_dir)?;
    let icon_src = native_dir.join("crates/falcon-app/assets/icon-512.png");
    std::fs::copy(&icon_src, mipmap_dir.join("ic_launcher.png"))
        .with_context(|| format!("复制应用图标 {} 失败", icon_src.display()))?;

    let aapt2_bin = sdk_tool(&build_tools, "aapt2")?;
    let compiled_res = stage.path().join("compiled-res.zip");
    util::run(
        &aapt2_bin,
        [
            OsStr::new("compile"),
            OsStr::new("--dir"),
            res_dir.as_os_str(),
            OsStr::new("-o"),
            compiled_res.as_os_str(),
        ],
        None,
    )?;

    let unaligned_apk = stage.path().join("unaligned.apk");
    util::run(
        &aapt2_bin,
        [
            OsStr::new("link"),
            OsStr::new("-o"),
            unaligned_apk.as_os_str(),
            OsStr::new("-I"),
            android_jar.as_os_str(),
            OsStr::new("--manifest"),
            manifest_path.as_os_str(),
            compiled_res.as_os_str(),
        ],
        None,
    )?;

    // 以 STORE（0 压缩）把 classes.dex 与 lib/<abi>/libnsh.so 写入 APK，配合 zipalign 16 KB 页对齐支持系统直接 mmap
    util::run(
        &jar_bin,
        [
            OsStr::new("uf0"),
            unaligned_apk.as_os_str(),
            OsStr::new("-C"),
            apk_extra.as_os_str(),
            OsStr::new("classes.dex"),
            OsStr::new("-C"),
            apk_extra.as_os_str(),
            OsStr::new("lib"),
        ],
        None,
    )?;

    let zipalign_bin = sdk_tool(&build_tools, "zipalign")?;
    let aligned_apk = stage.path().join("aligned.apk");
    let mut zipalign_args: Vec<&OsStr> = vec![OsStr::new("-f")];
    for flag in zipalign_page_flags(&build_tools) {
        zipalign_args.push(OsStr::new(flag));
    }
    zipalign_args.extend([
        OsStr::new("4"),
        unaligned_apk.as_os_str(),
        aligned_apk.as_os_str(),
    ]);
    util::run(&zipalign_bin, zipalign_args, None)?;

    let release_dir = root.join("release");
    std::fs::create_dir_all(&release_dir)?;
    let out_apk = release_dir.join(format!("falcon-v{ver}-android.apk"));

    let (ks_path, ks_pass, key_alias, key_pass) = prepare_keystore(stage.path(), &keytool_bin)?;
    let apksigner_bin = sdk_tool(&build_tools, "apksigner")?;
    let ks_pass_arg = format!("pass:{ks_pass}");
    let key_pass_arg = format!("pass:{key_pass}");
    util::run_env(
        &apksigner_bin,
        [
            OsStr::new("sign"),
            OsStr::new("--ks"),
            ks_path.as_os_str(),
            OsStr::new("--ks-key-alias"),
            OsStr::new(&key_alias),
            OsStr::new("--ks-pass"),
            OsStr::new(&ks_pass_arg),
            OsStr::new("--key-pass"),
            OsStr::new(&key_pass_arg),
            OsStr::new("--out"),
            out_apk.as_os_str(),
            aligned_apk.as_os_str(),
        ],
        None,
        &java_env,
    )?;

    let size_mb = std::fs::metadata(&out_apk)?.len() as f64 / (1024.0 * 1024.0);
    println!(
        "完成：{}（{size_mb:.2} MB）",
        out_apk.strip_prefix(&root).unwrap_or(&out_apk).display()
    );
    Ok(())
}

pub(crate) fn version_code_from_semver(ver: &str) -> u32 {
    let core = ver.split('-').next().unwrap_or(ver);
    let mut parts = core.split('.').map(|s| s.parse::<u32>().unwrap_or(0));
    let major = parts.next().unwrap_or(0);
    let minor = parts.next().unwrap_or(0);
    let patch = parts.next().unwrap_or(0);
    (major * 10_000 + minor * 100 + patch).max(1)
}

pub(crate) fn android_manifest_xml(version_code: u32, version_name: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<manifest xmlns:android="http://schemas.android.com/apk/res/android"
    package="com.falcon.android"
    android:versionCode="{version_code}"
    android:versionName="{version_name}">

    <uses-sdk
        android:minSdkVersion="26"
        android:targetSdkVersion="35" />

    <uses-permission android:name="android.permission.INTERNET" />
    <uses-permission android:name="android.permission.POST_NOTIFICATIONS" />

    <application
        android:label="Falcon"
        android:icon="@mipmap/ic_launcher"
        android:allowBackup="true"
        android:supportsRtl="true"
        android:usesCleartextTraffic="true">

        <meta-data
            android:name="com.longbridge.nsh.RUST"
            android:value="true" />

        <provider
            android:name="com.longbridge.nsh.FilesProvider"
            android:authorities="com.falcon.android.files"
            android:exported="false"
            android:grantUriPermissions="true" />

        <activity
            android:name="com.longbridge.nsh.AppActivity"
            android:exported="true"
            android:launchMode="singleTask"
            android:windowSoftInputMode="adjustResize"
            android:configChanges="orientation|screenSize|screenLayout|smallestScreenSize|keyboard|keyboardHidden|uiMode|density|fontScale|locale|layoutDirection">
            <intent-filter>
                <action android:name="android.intent.action.MAIN" />
                <category android:name="android.intent.category.LAUNCHER" />
            </intent-filter>
            <intent-filter>
                <action android:name="android.intent.action.VIEW" />
                <category android:name="android.intent.category.DEFAULT" />
                <category android:name="android.intent.category.BROWSABLE" />
                <data android:scheme="falcon" />
            </intent-filter>
        </activity>
    </application>
</manifest>
"#
    )
}

fn prepare_keystore(stage: &Path, keytool_bin: &Path) -> Result<(PathBuf, String, String, String)> {
    if let Some(ks) = std::env::var_os("FALCON_ANDROID_KEYSTORE") {
        let path = PathBuf::from(ks);
        if !path.is_file() {
            bail!("FALCON_ANDROID_KEYSTORE 指定的文件不存在：{}", path.display());
        }
        let ks_pass = std::env::var("FALCON_ANDROID_KS_PASS").unwrap_or_else(|_| "android".into());
        let alias = std::env::var("FALCON_ANDROID_KEY_ALIAS").unwrap_or_else(|_| "androiddebugkey".into());
        let key_pass = std::env::var("FALCON_ANDROID_KEY_PASS").unwrap_or_else(|_| ks_pass.clone());
        return Ok((path, ks_pass, alias, key_pass));
    }
    if let Some(home) = std::env::var_os("HOME") {
        let user_debug = PathBuf::from(home).join(".android/debug.keystore");
        if user_debug.is_file() {
            return Ok((
                user_debug,
                "android".into(),
                "androiddebugkey".into(),
                "android".into(),
            ));
        }
    }
    let generated = stage.join("debug.keystore");
    util::run(
        keytool_bin,
        [
            OsStr::new("-genkeypair"),
            OsStr::new("-v"),
            OsStr::new("-storetype"),
            OsStr::new("PKCS12"),
            OsStr::new("-keystore"),
            generated.as_os_str(),
            OsStr::new("-storepass"),
            OsStr::new("android"),
            OsStr::new("-alias"),
            OsStr::new("androiddebugkey"),
            OsStr::new("-keyalg"),
            OsStr::new("RSA"),
            OsStr::new("-keysize"),
            OsStr::new("2048"),
            OsStr::new("-validity"),
            OsStr::new("10000"),
            OsStr::new("-dname"),
            OsStr::new("CN=Android Debug,O=Android,C=US"),
        ],
        None,
    )?;
    Ok((
        generated,
        "android".into(),
        "androiddebugkey".into(),
        "android".into(),
    ))
}

fn collect_files_with_ext(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("读取目录 {} 失败", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_files_with_ext(&path, ext, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some(ext) {
            out.push(path);
        }
    }
    out.sort();
    Ok(())
}

fn resolve_android_jar(sdk_dir: &Path) -> Result<PathBuf> {
    let preferred = sdk_dir.join("platforms/android-35/android.jar");
    if preferred.is_file() {
        return Ok(preferred);
    }
    let platforms_root = sdk_dir.join("platforms");
    if platforms_root.is_dir() {
        let mut candidates: Vec<(u64, PathBuf)> = std::fs::read_dir(&platforms_root)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter_map(|p| {
                let api = p
                    .file_name()?
                    .to_str()?
                    .strip_prefix("android-")?
                    .parse::<u64>()
                    .ok()?;
                let jar = p.join("android.jar");
                (api >= 34 && jar.is_file()).then_some((api, jar))
            })
            .collect();
        candidates.sort_by_key(|(api, _)| *api);
        if let Some((_, jar)) = candidates.pop() {
            return Ok(jar);
        }
    }
    bail!(
        "{} 下未找到 platforms/android-35/android.jar（请通过 sdkmanager --install \"platforms;android-35\" 安装）",
        sdk_dir.display()
    )
}

fn resolve_build_tools_dir(sdk_dir: &Path) -> Result<PathBuf> {
    let bt_root = sdk_dir.join("build-tools");
    if !bt_root.is_dir() {
        bail!(
            "{} 下没有 build-tools 目录（请通过 sdkmanager --install \"build-tools;35.0.0\" 安装）",
            sdk_dir.display()
        );
    }
    let mut versions: Vec<PathBuf> = std::fs::read_dir(&bt_root)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    versions.sort_by_key(|p| ndk_version_key(p));
    versions.pop().context("build-tools 目录下没有已安装版本")
}

fn sdk_tool(build_tools_dir: &Path, name: &str) -> Result<PathBuf> {
    let candidates = if cfg!(windows) {
        vec![format!("{name}.bat"), format!("{name}.exe"), name.to_string()]
    } else {
        vec![name.to_string()]
    };
    for cand in candidates {
        let p = build_tools_dir.join(&cand);
        if p.is_file() {
            return Ok(p);
        }
    }
    bail!("{} 下找不到工具 {name}", build_tools_dir.display())
}

fn jdk_tool(name: &str, java_home: Option<&Path>) -> Result<PathBuf> {
    let exe = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    if let Some(jh) = java_home {
        let p = jh.join("bin").join(&exe);
        if p.is_file() {
            return Ok(p);
        }
    }
    util::require_tool(&exe, "请安装 JDK 17+ 或设置 JAVA_HOME")
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
        .split(['.', '-'])
        .map(|seg| seg.parse::<u64>().unwrap_or(0))
        .collect()
}

fn rustup_toolchain_bin() -> Result<PathBuf> {
    let out = Command::new("rustup")
        .args(["which", "--toolchain", "stable", "cargo"])
        .output()
        .context("起不来 rustup（装了吗？）")?;
    if !out.status.success() {
        bail!(
            "rustup which cargo 失败：{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let cargo = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    Ok(cargo.parent().context("cargo 的目录")?.to_path_buf())
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

/// 为指定 Android 目标构造带三元组后缀的 Cargo / `cc-rs` 工具链环境变量。
///
/// NDK r23+ 只提供带 API 等级的包装器（如 `aarch64-linux-android26-clang`），不再带无后缀的
/// `aarch64-linux-android-clang`；而 `falcon-app` 在 Android 上会经 `cc-rs` 编译 `ring` 与 `onig_sys`。
/// 这里只设 `CC_<target>` / `AR_<target>` 等带目标后缀的变量，绝不写全局 `CC` / `AR`，以免污染宿主机
/// `build.rs` 与 proc-macro 的本地编译。
fn ndk_target_env(
    ndk_bin: &Path,
    rust_target: &str,
    linker_env: &str,
    clang_name: &str,
) -> Result<Vec<(String, OsString)>> {
    let clang = ndk_tool(ndk_bin, clang_name)?;
    let clangxx = ndk_tool(ndk_bin, &format!("{clang_name}++"))?;
    let ar = ndk_tool(ndk_bin, "llvm-ar")?;
    let ranlib = ndk_tool(ndk_bin, "llvm-ranlib")?;

    let lower = rust_target.replace('-', "_");
    let upper = lower.to_ascii_uppercase();
    Ok(vec![
        (linker_env.to_string(), clang.clone().into_os_string()),
        (format!("CARGO_TARGET_{upper}_AR"), ar.clone().into_os_string()),
        (format!("CC_{lower}"), clang.into_os_string()),
        (format!("CXX_{lower}"), clangxx.into_os_string()),
        (format!("AR_{lower}"), ar.into_os_string()),
        (format!("RANLIB_{lower}"), ranlib.into_os_string()),
    ])
}

fn ndk_tool(ndk_bin: &Path, name: &str) -> Result<PathBuf> {
    let candidates = if cfg!(windows) {
        vec![format!("{name}.cmd"), format!("{name}.exe"), name.to_string()]
    } else {
        vec![name.to_string()]
    };
    for cand in candidates {
        let p = ndk_bin.join(&cand);
        if p.is_file() {
            return Ok(p);
        }
    }
    bail!("找不到 NDK 工具：{}", ndk_bin.join(name).display())
}

/// build-tools 35+ 支持 `-P 16`（把未压缩 `.so` 按 16 KiB 边界对齐，兼顾 16 KB 与 4 KB 页设备）；
/// 旧版 build-tools 不认 `-P`，退回 `-p`。
fn zipalign_page_flags(build_tools_dir: &Path) -> &'static [&'static str] {
    if ndk_version_key(build_tools_dir).first().copied().unwrap_or(0) >= 35 {
        &["-P", "16"]
    } else {
        &["-p"]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zipalign_uses_16kb_page_flag_on_build_tools_35_plus() {
        assert_eq!(zipalign_page_flags(Path::new("/sdk/build-tools/35.0.0")), &["-P", "16"]);
        assert_eq!(zipalign_page_flags(Path::new("/sdk/build-tools/36.0.0")), &["-P", "16"]);
        assert_eq!(zipalign_page_flags(Path::new("/sdk/build-tools/34.0.0")), &["-p"]);
    }

    #[test]
    fn ndk_selection_prefers_env_and_sorts_versions_numerically() {
        let tmp = util::TempDir::new("android-ndk-test").unwrap();
        let sdk = tmp.path();
        for ver in ["9.1.0", "27.2.12479018", "26.3.11579264"] {
            std::fs::create_dir_all(
                sdk.join(format!("ndk/{ver}/toolchains/llvm/prebuilt/linux-x86_64/bin")),
            )
            .unwrap();
        }
        let picked = resolve_ndk_clang_bin(sdk, None).unwrap();
        assert_eq!(
            picked,
            sdk.join("ndk/27.2.12479018/toolchains/llvm/prebuilt/linux-x86_64/bin")
        );

        let pinned = sdk.join("ndk/26.3.11579264");
        let picked_env = resolve_ndk_clang_bin(sdk, Some(&pinned)).unwrap();
        assert_eq!(
            picked_env,
            pinned.join("toolchains/llvm/prebuilt/linux-x86_64/bin")
        );
    }

    #[test]
    fn ndk_target_env_wires_cc_rs_and_cargo_cross_tools_without_clobbering_host_cc() {
        let tmp = util::TempDir::new("android-ndk-env").unwrap();
        let bin = tmp.path();
        for tool in [
            "aarch64-linux-android26-clang",
            "aarch64-linux-android26-clang++",
            "x86_64-linux-android26-clang",
            "x86_64-linux-android26-clang++",
            "llvm-ar",
            "llvm-ranlib",
        ] {
            std::fs::write(bin.join(tool), b"").unwrap();
        }

        for (rust_target, _abi, linker_env, clang_name) in TARGETS {
            let envs = ndk_target_env(bin, rust_target, linker_env, clang_name).unwrap();
            let keys: Vec<&str> = envs.iter().map(|(k, _)| k.as_str()).collect();
            let lower = rust_target.replace('-', "_");
            let upper = lower.to_ascii_uppercase();
            assert!(keys.contains(linker_env));
            assert!(keys.contains(&format!("CARGO_TARGET_{upper}_AR").as_str()));
            assert!(keys.contains(&format!("CC_{lower}").as_str()));
            assert!(keys.contains(&format!("CXX_{lower}").as_str()));
            assert!(keys.contains(&format!("AR_{lower}").as_str()));
            assert!(keys.contains(&format!("RANLIB_{lower}").as_str()));
            assert!(!keys.contains(&"CC"));
            assert!(!keys.contains(&"AR"));
        }
    }

    #[test]
    fn generated_manifest_targets_nsh_rust_activity_and_falcon_package() {
        let code = version_code_from_semver("0.3.28");
        assert_eq!(code, 328);
        let xml = android_manifest_xml(code, "0.3.28");
        assert!(xml.contains(r#"package="com.falcon.android""#));
        assert!(xml.contains(r#"android:name="com.longbridge.nsh.RUST""#));
        assert!(xml.contains(r#"android:name="com.longbridge.nsh.AppActivity""#));
        assert!(xml.contains(r#"android:authorities="com.falcon.android.files""#));
        assert!(xml.contains(r#"<data android:scheme="falcon" />"#));
    }
}
