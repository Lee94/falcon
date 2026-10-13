//! `bridge`：falcon-server 经 SSH 接会话的那一头（ADR 0020 决定四）。
//!
//! 连上 holder 后把 stdin / stdout 与连接双向透传，**不解析帧**——所以和 holder 的版本无关，
//! 新版 falcon 推上来的 bridge 照样能接旧版 holder 的会话。不用 SSH 的 `direct-streamlocal`
//! 转发 socket：Windows 的 OpenSSH 没有它，exec 桥两边都能用。
//!
//! 退出码（falcon-proto 的 `BRIDGE_EXIT_*`）告诉服务端连不上的原因：会话没了 / 正常结束了 /
//! 还在但暂时连不上。stdout 上除了开头的前导标记（`BRIDGE_PREAMBLE`）与透传的字节什么都不写，错误一律走 stderr。

use std::io::{Read as _, Write as _};

use falcon_proto::sessiond::{BRIDGE_EXIT_EXITED, BRIDGE_EXIT_GONE, BRIDGE_EXIT_UNAVAILABLE, BRIDGE_PREAMBLE, SpawnSpec};

use crate::ipc;
use crate::layout::Layout;

pub fn run(layout: &Layout, sid: &str, create: Option<&SpawnSpec>) -> i32 {
    let endpoint = layout.read_meta(sid).map(|m| m.endpoint).unwrap_or_else(|| layout.endpoint(sid));
    let stream = match ipc::connect(&endpoint) {
        Ok(s) => s,
        Err(_) => match create {
            Some(spec) => match crate::spawn::spawn(layout, sid, spec) {
                Ok(_) => {
                    let endpoint = layout.read_meta(sid).map(|m| m.endpoint).unwrap_or_else(|| layout.endpoint(sid));
                    match ipc::connect(&endpoint) {
                        Ok(s) => s,
                        Err(e) => {
                            eprintln!("会话起来了但连不上：{e}");
                            return BRIDGE_EXIT_UNAVAILABLE;
                        }
                    }
                }
                Err(e) => {
                    eprintln!("起不来会话：{e:#}");
                    return 1;
                }
            },
            None => return unreachable_code(layout, sid),
        },
    };
    relay(stream)
}

/// 连不上时为什么连不上
fn unreachable_code(layout: &Layout, sid: &str) -> i32 {
    if layout.holder_alive(sid).is_some() {
        eprintln!("会话还在，但暂时连不上");
        BRIDGE_EXIT_UNAVAILABLE
    } else if layout.read_exit(sid).is_some() {
        BRIDGE_EXIT_EXITED
    } else {
        BRIDGE_EXIT_GONE
    }
}

fn relay(stream: ipc::Stream) -> i32 {
    let (mut from_holder, mut to_holder) = match stream.split() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    // stdin → holder。服务端关了通道（stdin 读到 EOF）就整个退出：Windows 的管道不能只关一半，
    // 直接退出进程最干脆，holder 那边看到连接断开
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if to_holder.write_all(&buf[..n]).and_then(|()| to_holder.flush()).is_err() {
                        break;
                    }
                }
            }
        }
        std::process::exit(0);
    });
    // holder → stdout，每块都 flush：帧不能憋在缓冲里。先写前导标记（见 BRIDGE_PREAMBLE）
    let mut stdout = std::io::stdout().lock();
    if stdout.write_all(BRIDGE_PREAMBLE).and_then(|()| stdout.flush()).is_err() {
        return 0;
    }
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match from_holder.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if stdout.write_all(&buf[..n]).and_then(|()| stdout.flush()).is_err() {
                    break;
                }
            }
        }
    }
    0
}
