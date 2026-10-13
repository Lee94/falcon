//! 用法：ghostty-bindgen <include 目录> <输出 .rs> <commit>
//!
//! 只放行 `ghostty_*` / `Ghostty*` / `GHOSTTY_*`；枚举生成常量（C 的枚举值可能超出 Rust
//! 枚举能表达的范围，常量最稳）；保留布局断言，交叉编译时逐目标核对结构体布局。
fn main() {
    let mut args = std::env::args().skip(1);
    let inc = args.next().expect("include 目录");
    let out = args.next().expect("输出文件");
    let commit = args.next().expect("commit");
    let bindings = bindgen::Builder::default()
        .header(format!("{inc}/ghostty/vt.h"))
        .clang_arg(format!("-I{inc}"))
        .allowlist_function("ghostty_.*")
        .allowlist_type("Ghostty.*")
        .allowlist_var("GHOSTTY_.*")
        .default_enum_style(bindgen::EnumVariation::Consts)
        .prepend_enum_name(false)
        .layout_tests(true)
        .derive_default(true)
        .generate_comments(false)
        .generate()
        .expect("bindgen 失败");
    let body = bindings.to_string();
    let head = format!(
        "// 由 `cargo xtask ghostty-bindgen` 生成，别手改。\n// 来源：ghostty {commit} 的 include/ghostty/vt.h\n\n"
    );
    std::fs::write(&out, head + &body).expect("写不了输出文件");
}
