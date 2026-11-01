//! `falcon-sessiond hc` 网关的端到端测试（ADR 0026）：
//! 在独立的 `/tmp/sdt-hc-*` 目录下启动 `falcon-sessiond hc serve`，
//! 覆盖 Plain 与 Framed 模式下的全量 JSON-RPC 方法与 `terminal.attach` 二进制帧桥接。
#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_falcon-sessiond");

struct HcHarness {
    dir: String,
    sock: String,
    child: Child,
}

impl HcHarness {
    fn start(tag: &str) -> Self {
        let dir = format!("/tmp/sdt-hc-{}-{tag}", std::process::id());
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sock = format!("{dir}/hc.sock");
        let state_dir = format!("{dir}/hc-state");

        let child = Command::new(BIN)
            .args([
                "hc",
                "serve",
                "--root",
                &dir,
                "--socket",
                &sock,
                "--state-dir",
                &state_dir,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("启动 hc serve");

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if UnixStream::connect(&sock).is_ok() {
                return Self { dir, sock, child };
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("hc serve 在 5 秒内未就绪: {sock}");
    }

    fn plain_rpc(&self, id: u64, method: &str, params: Value) -> Value {
        let mut s = UnixStream::connect(&self.sock).expect("连接 hc.sock");
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let mut line = serde_json::to_vec(&req).unwrap();
        line.push(b'\n');
        s.write_all(&line).unwrap();
        let mut reader = BufReader::new(s);
        let mut resp = String::new();
        reader.read_line(&mut resp).unwrap();
        serde_json::from_str(&resp).expect("解析 JSON-RPC 响应")
    }
}

impl Drop for HcHarness {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = Command::new(BIN)
            .args(["list", "--json", "--root", &self.dir])
            .output()
            .map(|o| {
                let list: Vec<falcon_proto::sessiond::ListEntry> =
                    serde_json::from_slice(&o.stdout).unwrap_or_default();
                for e in list.into_iter().filter(|e| e.alive) {
                    let _ = Command::new(BIN)
                        .args(["kill", "--root", &self.dir, &e.sid])
                        .output();
                }
            });
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn encode_frame(channel: u32, kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(9 + payload.len());
    out.extend_from_slice(&channel.to_le_bytes());
    out.push(kind);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    out
}

fn read_framed(s: &mut UnixStream) -> (u32, u8, Vec<u8>) {
    let mut hdr = [0u8; 9];
    s.read_exact(&mut hdr).expect("读 9 字节帧头");
    let ch = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
    let kind = hdr[4];
    let len = u32::from_le_bytes([hdr[5], hdr[6], hdr[7], hdr[8]]) as usize;
    let mut payload = vec![0u8; len];
    if len > 0 {
        s.read_exact(&mut payload).expect("读帧载荷");
    }
    (ch, kind, payload)
}

#[test]
fn plain_rpc_lifecycle_and_docs() {
    let h = HcHarness::start("lifecycle");

    // 1. hello + server.status + rpc.discover + templates.list
    let r = h.plain_rpc(1, "hello", json!({ "generation": 1, "framed": false }));
    assert_eq!(r["result"]["generation"], 1);
    assert_eq!(r["result"]["framed"], false);
    assert!(r["result"]["core"]["id"].as_str().unwrap().len() == 32);

    let st = h.plain_rpc(2, "server.status", json!({}));
    assert_eq!(st["result"]["live"], 0);
    assert_eq!(st["result"]["finished"], 0);

    let disc = h.plain_rpc(3, "rpc.discover", json!({}));
    assert_eq!(disc["result"]["openrpc"], "1.2.6");

    let tmpl = h.plain_rpc(4, "templates.list", json!({}));
    assert!(tmpl["result"]["templates"].as_array().unwrap().is_empty());

    // 2. doc.get / doc.set CAS
    let d0 = h.plain_rpc(5, "doc.get", json!({ "name": "layout" }));
    assert_eq!(d0["result"]["version"], 0);
    assert!(d0["result"]["value"].is_null());

    let d1 = h.plain_rpc(
        6,
        "doc.set",
        json!({ "name": "layout", "value": { "tabs": ["a"] }, "version": 0 }),
    );
    assert_eq!(d1["result"]["version"], 1);

    let conflict = h.plain_rpc(
        7,
        "doc.set",
        json!({ "name": "layout", "value": { "tabs": ["b"] }, "version": 0 }),
    );
    assert_eq!(conflict["error"]["data"]["kind"], "conflict");
    assert_eq!(conflict["error"]["data"]["version"], 1);

    // 3. terminal.run -> terminal.get -> terminal.label -> terminal.keep -> terminal.send -> terminal.read -> terminal.kill -> terminal.rm
    let run_res = h.plain_rpc(
        8,
        "terminal.run",
        json!({
            "command": ["/bin/sh"],
            "name": "worker-1",
            "labels": { "role": "builder" }
        }),
    );
    let term = &run_res["result"];
    assert_eq!(term["address"], "t1");
    assert_eq!(term["name"], "worker-1");
    assert_eq!(term["state"], "live");
    assert_eq!(term["labels"]["role"], "builder");
    assert_eq!(term["labels"]["herdr.space"], "falcon.standalone");

    // 名称冲突检查
    let dup = h.plain_rpc(
        9,
        "terminal.run",
        json!({ "command": ["/bin/sh"], "name": "worker-1" }),
    );
    assert_eq!(dup["error"]["data"]["kind"], "name_taken");

    // terminal.label 修改与删除标签
    let lbl = h.plain_rpc(
        10,
        "terminal.label",
        json!({
            "target": "worker-1",
            "set": { "env": "dev" },
            "unset": ["role"]
        }),
    );
    assert_eq!(lbl["result"]["labels"]["env"], "dev");
    assert!(lbl["result"]["labels"].get("role").is_none());

    // terminal.keep 翻转
    let kp = h.plain_rpc(11, "terminal.keep", json!({ "target": "t1", "keep": true }));
    assert_eq!(kp["result"]["keep"], true);

    // terminal.send --force 发送命令并读取输出
    let sent = h.plain_rpc(
        12,
        "terminal.send",
        json!({
            "target": "t1",
            "text": "echo HC_HELLO_7788",
            "force": true
        }),
    );
    assert_eq!(sent["result"]["turn"], 0);

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut found = false;
    while Instant::now() < deadline {
        let rd = h.plain_rpc(13, "terminal.read", json!({ "target": "t1" }));
        if rd["result"]["text"]
            .as_str()
            .unwrap_or("")
            .contains("HC_HELLO_7788")
        {
            found = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(found, "terminal.read 应当读到刚刚发送的 echo 输出");

    // terminal.kill 终止会话
    let killed = h.plain_rpc(14, "terminal.kill", json!({ "target": "t1" }));
    assert_eq!(killed["result"]["state"], "finished");

    // 退出后仍可 terminal.read 读到快照
    let rd_after = h.plain_rpc(15, "terminal.read", json!({ "target": "t1" }));
    assert!(
        rd_after["result"]["text"]
            .as_str()
            .unwrap_or("")
            .contains("HC_HELLO_7788")
    );

    // terminal.rm 删除已退出的会话
    let rm = h.plain_rpc(16, "terminal.rm", json!({ "target": "t1" }));
    assert_eq!(rm["result"]["removed"], "t1");

    let ls = h.plain_rpc(17, "terminal.list", json!({}));
    assert!(ls["result"]["terminals"].as_array().unwrap().is_empty());
}

#[test]
fn framed_attach_and_events_stream() {
    let h = HcHarness::start("framed");

    // 先创建一个交互式 sh 会话
    let run_res = h.plain_rpc(
        1,
        "terminal.run",
        json!({ "command": ["/bin/sh"], "name": "pane-a" }),
    );
    assert_eq!(run_res["result"]["address"], "t1");

    // 建立 Framed 连接：首行是带 framed: true 的 hello
    let mut s = UnixStream::connect(&h.sock).expect("连接 hc.sock");
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut hello_line =
        serde_json::to_vec(&json!({ "jsonrpc": "2.0", "id": 1, "method": "hello", "params": { "generation": 1, "framed": true } }))
            .unwrap();
    hello_line.push(b'\n');
    s.write_all(&hello_line).unwrap();

    // 首包回复仍然是换行分隔 JSON
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while s.read_exact(&mut byte).is_ok() {
        if byte[0] == b'\n' {
            break;
        }
        buf.push(byte[0]);
    }
    let hello_resp: Value = serde_json::from_slice(&buf).unwrap();
    assert_eq!(hello_resp["result"]["framed"], true);

    // 切入 Framed 模式：订阅事件流 + attach 到 t1
    let sub_req = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "events.subscribe",
        "params": {}
    }))
    .unwrap();
    s.write_all(&encode_frame(0, 0, &sub_req)).unwrap();

    let (ch, kind, payload) = read_framed(&mut s);
    assert_eq!(ch, 0);
    assert_eq!(kind, 0);
    let sub_resp: Value = serde_json::from_slice(&payload).unwrap();
    let ev_channel = sub_resp["result"]["channel"].as_u64().unwrap() as u32;
    assert!(ev_channel >= 1);

    let att_req = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "terminal.attach",
        "params": { "target": "t1", "cols": 80, "rows": 24 }
    }))
    .unwrap();
    s.write_all(&encode_frame(0, 0, &att_req)).unwrap();

    // 读取直到拿到 id=3 的控制响应，同时收集 attach 通道上的首帧
    let mut att_channel = 0u32;
    let mut got_first_paint = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let (c, k, p) = read_framed(&mut s);
        if c == 0 && k == 0 {
            let v: Value = serde_json::from_slice(&p).unwrap();
            if v["id"] == 3 {
                att_channel = v["result"]["channel"].as_u64().unwrap() as u32;
            }
        } else if att_channel != 0 && c == att_channel && k == 0 {
            got_first_paint.extend_from_slice(&p);
            break;
        }
    }
    assert!(att_channel >= 1);
    assert!(
        got_first_paint.windows(4).any(|w| w == b"\x1b[0m"),
        "attach 首帧应包含重置序列"
    );

    // 通过 attach 通道直接写入命令，并验证实时输出与信用归还
    s.write_all(&encode_frame(att_channel, 0, b"echo FRAMED_LIVE_99\n"))
        .unwrap();
    let mut live_out = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let (c, k, p) = read_framed(&mut s);
        if c == att_channel && k == 0 {
            live_out.extend_from_slice(&p);
            // 归还信用
            let credit = (p.len() as u32).to_le_bytes();
            s.write_all(&encode_frame(att_channel, 1, &credit)).unwrap();
            if String::from_utf8_lossy(&live_out).contains("FRAMED_LIVE_99") {
                break;
            }
        }
    }
    assert!(
        String::from_utf8_lossy(&live_out).contains("FRAMED_LIVE_99"),
        "attach 通道应收到实时 PTY 输出"
    );
}
