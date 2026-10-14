//! 链 libghostty-vt 的静态库。库由 `cargo xtask ghostty` 用钉死版本的 Zig 编好，放在
//! `native/.cache/ghostty/<标识>/<target>/`（标识 = commit，打了补丁再加补丁哈希）；这里不联网、
//! 不调 Zig，找不到就报错并说该跑什么。
//!
//! 目录里**只能有静态库**：同目录有 `.dylib` 时 macOS 的链接器会挑 dylib，运行时报
//! `Library not loaded: @rpath/libghostty-vt.dylib`（xtask 拷出来的目录本来就只有 `.a`）。
//!
//! 不需要 libc++：ghostty 的 C++ 依赖（simdutf / highway）是以 no-libcxx 模式编的，
//! pkg-config 的 `Libs.private` 为空（2026-10-13 实测五个目标都不链 c++ 也能过）。

use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let pin = manifest.join("../../ghostty.pin");
    println!("cargo:rerun-if-changed={}", pin.display());
    println!("cargo:rerun-if-env-changed=GHOSTTY_VT_LIB_DIR");
    let text = std::fs::read_to_string(&pin).expect("读不到 ghostty.pin");
    let get = |k: &str| text.lines().find_map(|l| l.trim().strip_prefix(k).map(str::to_string));
    let commit = get("commit=").expect("ghostty.pin 里没有 commit=");
    // 标识与 xtask 的 Pin::build_id 同一规则：有补丁时带上补丁哈希。补丁内容与 patches= 是否
    // 一致由 xtask 核对（这里不联网、不读补丁），缓存目录与 VT 进程报的版本都用它
    let build_id = match get("patches=").filter(|v| !v.is_empty()) {
        Some(h) => format!("{commit}+falcon.{}", &h[..12.min(h.len())]),
        None => commit,
    };
    println!("cargo:rustc-env=GHOSTTY_COMMIT={build_id}");

    let target = std::env::var("TARGET").unwrap();
    let dir = match std::env::var_os("GHOSTTY_VT_LIB_DIR") {
        Some(d) => PathBuf::from(d),
        None => manifest.join("../../../.cache/ghostty").join(&build_id).join(&target),
    };
    let lib = dir.join("libghostty-vt.a");
    if !lib.is_file() {
        panic!(
            "找不到 {}：先跑 `cargo xtask ghostty --targets {target}`（或设 GHOSTTY_VT_LIB_DIR 指向放 libghostty-vt.a 的目录）",
            lib.display()
        );
    }
    println!("cargo:rerun-if-changed={}", lib.display());
    println!("cargo:rustc-link-search=native={}", dir.display());
    println!("cargo:rustc-link-lib=static=ghostty-vt");
}
