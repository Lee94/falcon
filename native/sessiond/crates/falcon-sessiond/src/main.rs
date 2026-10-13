//! falcon-sessiond：falcon 的会话守护进程（ADR 0020，设计文档 `docs/design/session-daemon.md`）。
//!
//! 每个会话在宿主机上是一对进程：
//! - **holder**（`hold`）：持 PTY、把输出原样转给接入方、存程序状态、探前台命令、看护 VT 进程。
//!   纯 Rust，不碰 libghostty-vt——它活着会话就活着；
//! - **VT 进程**（`vt`）：吃一份输出副本，在 libghostty-vt 里维护终端状态，回答终端查询、
//!   导出接回用的快照。它挂了 holder 重启它并用原始尾巴重建，会话不受影响。
//!
//! 其余子命令：`spawn`（服务端在本机起会话）、`bridge`（服务端经 SSH 接会话：只搬字节）、
//! `attach`（人手的逃生通道）、`list`、`kill`、`history`、`version`。
//!
//! 一个二进制给全部子命令：分发时只有一个文件，按版本命名，旧版不删（升级不杀会话）。

// 交叉编译时链接器是 zig 带的 lld，它对 rustc 缺省传的 `-Wl,-O1` 回一句
// "ignoring deprecated linker optimization setting '1'"，rustc 把链接器的 stderr 报成警告。
// 无害，而且不是我们能改的参数
#![allow(linker_messages)]

mod attach;
mod bridge;
#[cfg(test)]
mod corpus_tests;
mod fg;
mod holder;
mod ipc;
mod layout;
mod log;
mod proc;
mod replay;
mod spawn;
mod vt;
mod vtproto;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use falcon_proto::sessiond::{FALCON_VERSION, PROTO_MAX, PROTO_MIN, SpawnSpec, VersionInfo, valid_sid};

use crate::layout::Layout;

const USAGE: &str = "falcon-sessiond —— falcon 的会话守护进程

用法：
  falcon-sessiond attach <会话> [--force]      接上一个会话（Ctrl+\\ 断开，会话照常运行）
  falcon-sessiond list [--json]                列出会话
  falcon-sessiond kill <会话>...               结束会话
  falcon-sessiond history <会话> [--vt|--plain|--html]   导出会话历史
  falcon-sessiond version [--json]

  <会话> 是 16 位的会话短 id（list 里的 SID）。
  默认的 falcon 根目录是这个二进制所在 bin/ 的上一层；--root <目录> 或 FALCON_SESSIOND_ROOT 可改。

给 falcon 用的：
  spawn  --root <目录> --id <sid> --spec <base64>
  bridge --root <目录> --id <sid> [--create <base64>]
  hold   --root <目录> --id <sid>
  vt     --cols <n> --rows <n> [--scrollback <行>]";

/// 二进制所在 `bin/` 的上一层：`~/.falcon/bin/falcon-sessiond-<版本>` → `~/.falcon`
fn default_root() -> Option<PathBuf> {
    if let Some(r) = std::env::var_os("FALCON_SESSIOND_ROOT").filter(|r| !r.is_empty()) {
        return Some(PathBuf::from(r));
    }
    let exe = std::env::current_exe().ok()?;
    let bin = exe.parent()?;
    (bin.file_name()? == "bin").then(|| bin.parent().map(PathBuf::from)).flatten()
}

struct Args {
    rest: Vec<String>,
}

impl Args {
    fn flag(&mut self, name: &str) -> bool {
        match self.rest.iter().position(|a| a == name) {
            Some(i) => {
                self.rest.remove(i);
                true
            }
            None => false,
        }
    }

    fn value(&mut self, name: &str) -> Result<Option<String>> {
        match self.rest.iter().position(|a| a == name) {
            Some(i) => {
                if i + 1 >= self.rest.len() {
                    bail!("{name} 后面缺值");
                }
                let v = self.rest.remove(i + 1);
                self.rest.remove(i);
                Ok(Some(v))
            }
            None => Ok(None),
        }
    }

    fn root(&mut self) -> Result<Layout> {
        let root = match self.value("--root")? {
            Some(r) => PathBuf::from(r),
            None => default_root().context("不知道 falcon 根目录在哪：用 --root 指定")?,
        };
        Ok(Layout::new(root))
    }

    fn sid(&mut self) -> Result<String> {
        let sid = self.value("--id")?.context("缺 --id")?;
        if !valid_sid(&sid) {
            bail!("会话 id 不合法：{sid}");
        }
        Ok(sid)
    }

    fn positional_sid(&mut self) -> Result<String> {
        let i = self.rest.iter().position(|a| !a.starts_with("--")).context("缺会话 id")?;
        let sid = self.rest.remove(i);
        if !valid_sid(&sid) {
            bail!("会话 id 不合法：{sid}（用 list 看 SID）");
        }
        Ok(sid)
    }

    fn done(self) -> Result<()> {
        if let Some(a) = self.rest.first() {
            bail!("多余的参数：{a}");
        }
        Ok(())
    }
}

fn main() {
    let mut argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.is_empty() || matches!(argv[0].as_str(), "-h" | "--help" | "help") {
        println!("{USAGE}");
        std::process::exit(if argv.is_empty() { 2 } else { 0 });
    }
    let cmd = argv.remove(0);
    let code = match run(&cmd, Args { rest: argv }) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("falcon-sessiond {cmd}: {e:#}");
            1
        }
    };
    std::process::exit(code);
}

fn run(cmd: &str, mut a: Args) -> Result<i32> {
    match cmd {
        "version" => {
            let json = a.flag("--json");
            a.done()?;
            let info = VersionInfo {
                version: FALCON_VERSION.into(),
                proto_min: PROTO_MIN,
                proto_max: PROTO_MAX,
                ghostty: ghostty_vt::GHOSTTY_COMMIT.into(),
                target: env!("FALCON_SESSIOND_TARGET").into(),
            };
            if json {
                println!("{}", serde_json::to_string(&info)?);
            } else {
                println!("falcon-sessiond {}（{}，ghostty {}，协议 {}–{}）", info.version, info.target, &info.ghostty[..7], PROTO_MIN, PROTO_MAX);
            }
            Ok(0)
        }
        "hold" => {
            let layout = a.root()?;
            let sid = a.sid()?;
            a.done()?;
            holder::run(layout, sid)?;
            Ok(0)
        }
        "vt" => {
            let cols: u16 = a.value("--cols")?.context("缺 --cols")?.parse()?;
            let rows: u16 = a.value("--rows")?.context("缺 --rows")?.parse()?;
            let scrollback: usize = a.value("--scrollback")?.map(|s| s.parse()).transpose()?.unwrap_or(10_000);
            a.done()?;
            vt::run(cols, rows, scrollback)?;
            Ok(0)
        }
        "spawn" => {
            let layout = a.root()?;
            let sid = a.sid()?;
            let spec = SpawnSpec::decode(&a.value("--spec")?.context("缺 --spec")?)?;
            a.done()?;
            let res = spawn::spawn(&layout, &sid, &spec);
            let out = match res {
                Ok(r) => r,
                Err(e) => falcon_proto::sessiond::SpawnResult { ok: false, pid: 0, existing: false, error: Some(format!("{e:#}")) },
            };
            println!("{}", serde_json::to_string(&out)?);
            Ok(if out.ok { 0 } else { 1 })
        }
        "bridge" => {
            let layout = a.root()?;
            let sid = a.sid()?;
            let create = a.value("--create")?.map(|s| SpawnSpec::decode(&s)).transpose()?;
            a.done()?;
            Ok(bridge::run(&layout, &sid, create.as_ref()))
        }
        "attach" => {
            let force = a.flag("--force");
            let layout = a.root()?;
            let sid = a.positional_sid()?;
            a.done()?;
            attach::run(&layout, &sid, force)
        }
        "list" => {
            let json = a.flag("--json");
            let layout = a.root()?;
            a.done()?;
            layout::list(&layout, json)?;
            Ok(0)
        }
        "kill" => {
            let layout = a.root()?;
            let mut sids = Vec::new();
            while !a.rest.is_empty() {
                sids.push(a.positional_sid()?);
            }
            if sids.is_empty() {
                bail!("缺会话 id");
            }
            let mut code = 0;
            for sid in sids {
                if let Err(e) = attach::kill(&layout, &sid) {
                    eprintln!("{sid}: {e:#}");
                    code = 1;
                }
            }
            Ok(code)
        }
        "history" => {
            let format = if a.flag("--html") {
                falcon_proto::sessiond::ExportFormat::Html
            } else if a.flag("--plain") {
                falcon_proto::sessiond::ExportFormat::Plain
            } else {
                a.flag("--vt");
                falcon_proto::sessiond::ExportFormat::Vt
            };
            let layout = a.root()?;
            let sid = a.positional_sid()?;
            a.done()?;
            attach::history(&layout, &sid, format)?;
            Ok(0)
        }
        other => bail!("未知子命令：{other}\n\n{USAGE}"),
    }
}
