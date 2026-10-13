//! 链 libghostty-vt 的静态库。库由 `cargo xtask ghostty` 用钉死版本的 Zig 编好，放在
//! `native/.cache/ghostty/<commit>/<target>/`；这里不联网、不调 Zig，找不到就报错并说该跑什么。
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
    let commit = std::fs::read_to_string(&pin)
        .expect("读不到 ghostty.pin")
        .lines()
        .find_map(|l| l.trim().strip_prefix("commit=").map(str::to_string))
        .expect("ghostty.pin 里没有 commit=");
    println!("cargo:rustc-env=GHOSTTY_COMMIT={commit}");

    let target = std::env::var("TARGET").unwrap();
    let dir = match std::env::var_os("GHOSTTY_VT_LIB_DIR") {
        Some(d) => PathBuf::from(d),
        None => manifest.join("../../../.cache/ghostty").join(&commit).join(&target),
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
