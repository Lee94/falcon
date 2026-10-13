//! `version` 报出编这个二进制的目标三元组：服务端据此核对推到宿主机上的是不是对的那份
fn main() {
    println!("cargo:rustc-env=FALCON_SESSIOND_TARGET={}", std::env::var("TARGET").unwrap());
}
