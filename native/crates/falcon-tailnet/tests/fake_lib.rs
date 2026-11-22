//! 用假库（tests/fake_tailnet.c）测加载、接口版本核对、JSON 往返与字符串释放。
//!
//! 每个测试编一份独立文件名的假库：同一个文件 dlopen 两次拿到的是同一份，状态会串。
//! 编库用系统 cc（`CC` 环境变量可换）；Windows 上没有 cc 时跳过。

use std::ffi::{CStr, c_char, c_int};
use std::path::{Path, PathBuf};
use std::process::Command;

use falcon_tailnet::{InterfaceList, LIB_STEM, LoadError, NetInterface, StartConfig, State, Tailnet};

/// 编一份假库，返回路径；编不了（Windows 上没有 cc）返回 None
fn build_fake(tag: &str, defines: &[&str]) -> Option<PathBuf> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("falcon-tailnet-fake").join(tag);
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join(libloading::library_filename(LIB_STEM));
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fake_tailnet.c");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
    let mut cmd = Command::new(&cc);
    cmd.args(["-shared", "-fPIC", "-O0", "-o"]).arg(&out).arg(&src);
    for d in defines {
        cmd.arg(format!("-D{d}"));
    }
    match cmd.status() {
        Ok(s) if s.success() => Some(out),
        Ok(s) => panic!("{cc} 编假库失败：{s}"),
        Err(e) if cfg!(windows) => {
            eprintln!("跳过：没有 C 编译器（{cc}：{e}）");
            None
        }
        Err(e) => panic!("找不到 C 编译器 {cc}：{e}"),
    }
}

/// 读假库的 fake_* 探针。库已经被 Tailnet::open 加载，这里再 dlopen 一次拿到同一份
fn probe_str(path: &Path, sym: &[u8]) -> String {
    unsafe {
        let lib = libloading::Library::new(path).unwrap();
        let f: libloading::Symbol<unsafe extern "C" fn() -> *const c_char> = lib.get(sym).unwrap();
        CStr::from_ptr(f()).to_string_lossy().into_owned()
    }
}

fn probe_frees(path: &Path) -> c_int {
    unsafe {
        let lib = libloading::Library::new(path).unwrap();
        let f: libloading::Symbol<unsafe extern "C" fn() -> c_int> = lib.get(b"fake_frees\0").unwrap();
        f()
    }
}

#[test]
fn full_lifecycle() {
    let Some(path) = build_fake("ok", &[]) else { return };
    let tn = Tailnet::open(&path).expect("加载假库");
    assert_eq!(tn.path(), path);

    // 没起：stopped、没有代理
    let st = tn.status().unwrap();
    assert_eq!(st.state, State::Stopped);
    assert!(tn.proxy().unwrap().is_none());

    // start：配置以 camelCase JSON 交过去，None 字段不出现
    let cfg = StartConfig {
        state_dir: "/tmp/falcon-ts".into(),
        hostname: "falcon-test".into(),
        control_url: Some("https://hs.example.com".into()),
        auth_key: None,
    };
    tn.start(&cfg).unwrap();
    let sent: serde_json::Value = serde_json::from_str(&probe_str(&path, b"fake_last_config\0")).unwrap();
    assert_eq!(
        sent,
        serde_json::json!({"stateDir": "/tmp/falcon-ts", "hostname": "falcon-test", "controlUrl": "https://hs.example.com"})
    );

    // running：self、peers 按 Go 给的顺序，未知字段忽略
    let st = tn.status().unwrap();
    assert_eq!(st.state, State::Running);
    assert_eq!(st.self_node.as_ref().unwrap().ipv4.as_deref(), Some("100.64.0.9"));
    let names: Vec<&str> = st.peers.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["work", "nas"]);
    assert_eq!(st.peers[0].owner.as_deref(), Some("Ada"));
    assert!(st.has_peer("work.tail.ts.net") && st.has_peer("nas") && !st.has_peer("printer"));

    let proxy = tn.proxy().unwrap().expect("running 时有代理");
    assert_eq!(proxy.socks5h_url(), "socks5h://tsnet:0123456789abcdef@127.0.0.1:41641");

    // 启动失败：Go 的 {"error"} 原样变成 Error::Node
    let mut bad = cfg.clone();
    bad.hostname = "fail".into();
    assert_eq!(tn.start(&bad).unwrap_err().to_string(), "control plane unreachable");

    // set_interfaces
    let list = InterfaceList {
        interfaces: vec![NetInterface { name: "wlan0".into(), index: 3, mtu: 1500, up: true, addrs: vec!["192.168.1.5/24".into()] }],
        dns: vec!["192.168.1.1".into()],
    };
    tn.set_interfaces(&list).unwrap();
    let sent: InterfaceList = serde_json::from_str(&probe_str(&path, b"fake_last_interfaces\0")).unwrap();
    assert_eq!(sent, list);

    // stop
    tn.start(&cfg).unwrap();
    tn.stop();
    assert_eq!(tn.status().unwrap().state, State::Stopped);

    // logout：控制面报错也照样清掉状态目录，错误交回调用方
    let state_dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("falcon-tailnet-fake/state");
    std::fs::create_dir_all(&state_dir).unwrap();
    std::fs::write(state_dir.join("tailscaled.state"), b"{}").unwrap();
    tn.start(&cfg).unwrap();
    assert_eq!(tn.logout(&state_dir).unwrap_err().to_string(), "offline");
    assert!(!state_dir.exists());
    assert_eq!(tn.status().unwrap().state, State::Stopped);
    // 状态目录本来就没有也不报 IO 错误
    assert_eq!(tn.logout(&state_dir).unwrap_err().to_string(), "offline");

    // 每个非 NULL 返回值都交还了 ftn_free：
    // status×4 + proxy×1（没起时那次是 NULL）+ start 失败×1 + logout×2 = 8
    assert_eq!(probe_frees(&path), 8);
}

#[test]
fn rejects_wrong_abi_version() {
    let Some(path) = build_fake("abi99", &["FAKE_ABI=99"]) else { return };
    match Tailnet::open(&path) {
        Err(LoadError::AbiMismatch { found: 99, expected: 1, .. }) => {}
        Err(other) => panic!("期望 AbiMismatch，得到 {other}"),
        Ok(_) => panic!("期望 AbiMismatch，却加载成功"),
    }
}

#[test]
fn reports_missing_symbol() {
    let Some(path) = build_fake("nosym", &["FAKE_NO_SET_INTERFACES"]) else { return };
    match Tailnet::open(&path) {
        Err(LoadError::Load { message, .. }) => assert!(message.contains("ftn_set_interfaces"), "{message}"),
        Err(other) => panic!("期望 Load 错误，得到 {other}"),
        Ok(_) => panic!("期望缺符号，却加载成功"),
    }
}

#[test]
fn missing_file_is_load_error() {
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join("falcon-tailnet-fake/none").join(libloading::library_filename(LIB_STEM));
    assert!(matches!(Tailnet::open(&path), Err(LoadError::Load { .. })));
}
