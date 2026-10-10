//! 协议 fixture 的生成与形状比对（原 native/scripts/gen-fixtures.mjs / compare-fixtures.mjs）。
//!
//! # `cargo xtask fixtures`
//!
//! 拉起一个**真实的** falcon 服务端（临时数据目录、空闲端口），建一个本地项目（工作目录
//! 是一个临时 git 仓库：几个文件、两次提交、一处未提交改动、一个未跟踪文件）、SSH
//! 项目、多仓库容器、附属项目、会话……把各接口的真实响应落盘到
//! native/crates/falcon-proto/tests/fixtures/*.json，再由 falcon-proto 的
//! tests/fixtures.rs 逐个反序列化。服务端改了字段名或删了字段，重生 fixture 时 Rust
//! 测试就红——这是服务端实际输出与 falcon-proto 类型之间的契约检查。
//!
//! 何时重跑：
//!   - falcon-proto 改了线上形状（加 / 改 / 删字段、改字面量）；
//!   - 服务端（native/crates/falcon-server/src/api/）改了某个端点的响应；
//!   - falcon-proto 改了类型之后，顺手重跑一次确认两边还对得上。
//! 重跑后 `git diff` 看一眼 fixture 的变化：id、时间戳、令牌每次都会变，那是正常的；
//! 字段的增减才是要关心的。落盘格式与 Node 版的 `JSON.stringify(value, null, 2)` 逐字节
//! 一致（见 `fixtures/js.rs`），换工具不会带出格式噪音。
//!
//! 跑法（native/ 下）：
//!   cargo build -p falcon-server
//!   cargo xtask fixtures
//!   cargo test -p falcon-proto --test fixtures
//!
//! FALCON_FIXTURE_SERVER_BIN 改起别的服务端可执行文件（默认 `$CARGO_TARGET_DIR/debug/falcon-server`，
//! 没设 CARGO_TARGET_DIR 就是 native/target/debug/falcon-server）；FALCON_FIXTURE_OUT 落到
//! 别的目录，再用 compare-fixtures 与仓库里的那套逐个比形状（改服务端前后对拍用）：
//!   FALCON_FIXTURE_OUT=/tmp/fx-new cargo xtask fixtures
//!   cargo xtask compare-fixtures crates/falcon-proto/tests/fixtures /tmp/fx-new
//!
//! 几个坑：
//!   - 数据目录必须是短路径：zellij 的 IPC socket 全路径在 macOS 上限 104 字节，
//!     长路径会让会话建完立刻 exited。所以一律放 /private/tmp/fal-fx-*。
//!   - 全新数据目录第一次建本地会话会去 GitHub 下载 zellij（14MB）。先从
//!     ~/.falcon/bin 或 ~/.mojito/bin 拷一份锁定版本的二进制进去（服务端安装流程会先
//!     检查 binDir 里现成的），找不到才让它下载。也可以用 FALCON_FIXTURE_ZELLIJ 指定。
//!   - 端口只在 4940–4999 里挑，绝不碰 4923（那是日常在用、设了密码的实例）。
//!   - 从 falcon 终端里跑时环境里会带着父实例的 FALCON_* 变量（FALCON_WEB_DIST 之类），
//!     启动子服务端前全部去掉。
//!   - fixture 里不能有机密：飞书项目的登录用户（名字、邮箱、头像）脱敏；飞书的业务数据
//!     （空间、待办）一律不取。路径、用户名保留。
//!
//! # `cargo xtask compare-fixtures <基准目录> <对照目录>`
//!
//! 比两套 fixture 的**形状**（改服务端前后对拍；S 线时是 Node 版生成的 vs Rust 版生成的）。
//! 只比结构：同名文件里每个对象的键集合与键序、每个值的类型（null / boolean / number /
//! string / array / object），数组按元素逐个比（长度不同也报）。id、时间戳、令牌这些值
//! 每次都变，不比。退出码：有差异 1，没有 0。

mod client;
mod js;

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use std::{env, fs};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::json;

use crate::util;
use client::{Frame, Pumped, Ws};
use js::Js;

const TMP: &str = "/private/tmp";
const DATA: &str = "/private/tmp/fal-fx-data";
const REPO: &str = "/private/tmp/fal-fx-repo";
const PLAIN: &str = "/private/tmp/fal-fx-plain";
const PASSWORD: &str = "fixture-pass";

/// 1×1 PNG
const PNG_HEX: &str = concat!(
    "89504e470d0a1a0a0000000d4948445200000001000000010806000000",
    "1f15c4890000000d49444154789c6360f8cfc0f01f0005000201e2a6a1",
    "e90000000049454e44ae426082",
);

fn png() -> Vec<u8> {
    hex::decode(PNG_HEX).expect("PNG 常量是合法十六进制")
}

pub fn run(args: &[String]) -> Result<()> {
    if let Some(arg) = args.first() {
        bail!(
            "fixtures 不带参数（收到 {arg}）：换服务端 / 输出目录 / zellij 用环境变量 \
             FALCON_FIXTURE_SERVER_BIN / FALCON_FIXTURE_OUT / FALCON_FIXTURE_ZELLIJ"
        );
    }
    let server_bin = match env_path("FALCON_FIXTURE_SERVER_BIN") {
        Some(p) => std::path::absolute(p)?,
        None => default_server_bin()?,
    };
    let out_dir = match env_path("FALCON_FIXTURE_OUT") {
        Some(p) => std::path::absolute(p)?,
        None => util::native().join("crates/falcon-proto/tests/fixtures"),
    };
    if !server_bin.exists() {
        bail!("没找到 {}：先 (cd native && cargo build -p falcon-server)", server_bin.display());
    }
    cleanup_dirs();
    let mut server: Option<Child> = None;
    let res = generate(&server_bin, &out_dir, &mut server);
    if res.is_err() {
        // 脚本里是 child.on("exit") 当场报；这里在出错时补一句，免得只看到一个"连不上"
        if let Some(Ok(Some(status))) = server.as_mut().map(Child::try_wait) {
            eprintln!("服务端提前退出：{status}，日志在 {DATA}/server.log");
        }
    }
    stop_server(server);
    cleanup_dirs();
    res
}

/// 环境变量当路径用；空串当没设（脚本里 `process.env.X ? … : …` 的口径）
fn env_path(name: &str) -> Option<OsString> {
    env::var_os(name).filter(|v| !v.is_empty())
}

/// `cargo build -p falcon-server` 的产物：认 CARGO_TARGET_DIR（`cargo xtask` 原样继承
/// 调用者的环境），没设就是 native/target
fn default_server_bin() -> Result<PathBuf> {
    let target = match env_path("CARGO_TARGET_DIR") {
        Some(dir) => std::path::absolute(dir)?,
        None => util::native().join("target"),
    };
    Ok(target.join("debug/falcon-server"))
}

fn generate(server_bin: &Path, out_dir: &Path, server: &mut Option<Child>) -> Result<()> {
    prepare_repo()?;
    prepare_data_dir()?;
    let port = free_port(4940, 4999)?;
    let child = server.insert(start_server(server_bin, port)?);
    let mut api = Api { port, cookie: None };
    wait_ready(&api, child)?;
    fs::create_dir_all(out_dir)?;
    for entry in fs::read_dir(out_dir)? {
        let path = entry?.path();
        if path.file_name().is_some_and(|n| n.to_string_lossy().ends_with(".json")) {
            fs::remove_file(&path).with_context(|| format!("删不掉旧 fixture {}", path.display()))?;
        }
    }
    let mut out = Out { dir: out_dir.to_path_buf(), written: 0 };
    capture(&mut api, &mut out)?;
    let shown = out_dir.strip_prefix(util::root()).unwrap_or(out_dir);
    println!("写了 {} 个 fixture 到 {}/", out.written, shown.display());
    Ok(())
}

fn capture(api: &mut Api, out: &mut Out) -> Result<()> {
    // ---- 认证（先不设密码：绑回环、没密码时不需要认证）/ 系统 / 目录 ----
    out.save("auth-status", &api.get("/api/auth/status")?)?;
    out.save("system", &api.get("/api/system")?)?;
    out.save("askpass-pending", &api.get("/api/askpass/pending")?)?;
    out.save("fs-validate-ok", &api.call("POST", "/api/fs/validate", json!({ "path": REPO }))?)?;
    out.save(
        "fs-validate-missing",
        &api.call("POST", "/api/fs/validate", json!({ "path": format!("{TMP}/fal-fx-nope") }))?,
    )?;
    out.save("fs-list", &api.get(&format!("/api/fs/list?{}", qs(&[("path", REPO)])))?)?;
    out.save("shells", &api.get("/api/shells")?)?;

    // ---- 远端主机：只落库、不真连（127.0.0.1:1 连不上，正好拿到 ok:false 的探测）----
    let host = api.call(
        "POST",
        "/api/hosts",
        json!({
            "name": "fixture-host",
            "host": "127.0.0.1",
            "port": 1,
            "username": "fixture",
            "authMethod": "password",
            "secret": "not-a-real-password",
        }),
    )?;
    out.save("host-create", &host)?;
    let host_id = id(&host)?;
    out.save(
        "host-update",
        &api.call(
            "PUT",
            &format!("/api/hosts/{host_id}"),
            json!({
                "name": "fixture-host-renamed",
                "host": "127.0.0.1",
                "port": 1,
                "username": "fixture",
                "authMethod": "password",
            }),
        )?,
    )?;
    out.save("host-test", &api.bare("POST", &format!("/api/hosts/{host_id}/test"))?)?;
    out.save(
        "host-test-draft",
        &api.call(
            "POST",
            "/api/hosts/test",
            json!({
                "name": "draft",
                "host": "127.0.0.1",
                "port": 1,
                "username": "fixture",
                "authMethod": "password",
                "hostId": host_id,
            }),
        )?,
    )?;

    // ---- 项目 ----
    let local = api.call(
        "POST",
        "/api/projects",
        json!({ "name": "fixture", "type": "local", "workingDir": REPO, "defaultWorktreeBranch": "main" }),
    )?;
    out.save("project-create-local", &local)?;
    let local_id = id(&local)?;
    let ssh = api.call(
        "POST",
        "/api/projects",
        json!({ "name": "fixture-ssh", "type": "ssh", "hostId": host_id, "workingDir": "/srv/app" }),
    )?;
    out.save("project-create-ssh", &ssh)?;
    let ssh_id = id(&ssh)?;
    let multi =
        api.call("POST", "/api/projects", json!({ "name": "fixture-multi", "type": "local", "repos": [REPO] }))?;
    out.save("project-create-multi", &multi)?;
    let multi_id = id(&multi)?;
    let plain =
        api.call("POST", "/api/projects", json!({ "name": "fixture-plain", "type": "local", "workingDir": PLAIN }))?;
    let plain_id = id(&plain)?;
    out.save(
        "project-update",
        &api.call(
            "PUT",
            &format!("/api/projects/{plain_id}"),
            json!({ "name": "fixture-plain-renamed", "type": "local", "workingDir": PLAIN, "shell": "/bin/sh" }),
        )?,
    )?;
    out.save("hosts", &api.get("/api/hosts")?)?;

    // ---- 派生前探测 ----
    out.save("repo-info", &api.get(&format!("/api/projects/{local_id}/repo"))?)?;
    out.save("repos-multi", &api.get(&format!("/api/projects/{multi_id}/repos"))?)?;

    // ---- Git 面板（在动文件之前取，仓库状态最干净）----
    out.save("git-snapshot", &api.get(&format!("/api/projects/{local_id}/git"))?)?;
    out.save("git-snapshot-multi", &api.get(&format!("/api/projects/{multi_id}/git?{}", qs(&[("repo", REPO)])))?)?;
    out.save("git-snapshot-unavailable", &api.get(&format!("/api/projects/{plain_id}/git"))?)?;
    out.save("git-changes", &api.get(&format!("/api/projects/{local_id}/git/changes"))?)?;
    out.save(
        "git-changes-batch",
        &api.call("POST", "/api/git/changes", json!({ "ids": [local_id, plain_id, "gone"] }))?,
    )?;
    out.save("git-diff", &api.get(&format!("/api/projects/{local_id}/git/diff?{}", qs(&[("path", "src/main.rs")])))?)?;
    out.save(
        "git-diff-untracked",
        &api.get(&format!("/api/projects/{local_id}/git/diff?{}", qs(&[("path", "notes.txt"), ("untracked", "1")])))?,
    )?;
    out.save(
        "git-diff-unavailable",
        &api.get(&format!("/api/projects/{plain_id}/git/diff?{}", qs(&[("path", "x")])))?,
    )?;
    out.save("git-working", &api.get(&format!("/api/projects/{local_id}/git/working"))?)?;
    out.save("git-working-unavailable", &api.get(&format!("/api/projects/{plain_id}/git/working"))?)?;
    let log = api.get(&format!("/api/projects/{local_id}/git/log"))?;
    out.save("git-log", &log)?;
    out.save("git-log-unavailable", &api.get(&format!("/api/projects/{plain_id}/git/log"))?)?;
    out.save("git-refs", &api.get(&format!("/api/projects/{local_id}/git/refs"))?)?;
    out.save("git-refs-unavailable", &api.get(&format!("/api/projects/{plain_id}/git/refs"))?)?;
    let sha = log
        .get("commits")
        .and_then(|c| c.at(0))
        .ok_or_else(|| anyhow!("git-log 里没有提交：{}", log.compact()))?
        .text("sha")
        .map_err(|e| anyhow!(e))?;
    out.save("git-commit-detail", &api.get(&format!("/api/projects/{local_id}/git/commit?{}", qs(&[("sha", &sha)])))?)?;
    out.save(
        "git-commit-diff",
        &api.get(&format!("/api/projects/{local_id}/git/commit/diff?{}", qs(&[("sha", &sha), ("path", "README.md")])))?,
    )?;
    out.save(
        "git-op-fetch",
        &api.call("POST", &format!("/api/projects/{local_id}/git/op"), json!({ "op": "fetch" }))?,
    )?;
    out.save("git-push", &api.bare("POST", &format!("/api/projects/{local_id}/git/push"))?)?;

    // ---- 文件面板（在 git 之后：会往仓库里写未跟踪文件）----
    out.save("files", &api.get(&format!("/api/projects/{local_id}/files"))?)?;
    out.save("files-subdir", &api.get(&format!("/api/projects/{local_id}/files?{}", qs(&[("path", "src")])))?)?;
    out.save("files-index", &api.get(&format!("/api/projects/{local_id}/files/index"))?)?;
    let text = api.get(&format!("/api/projects/{local_id}/file?{}", qs(&[("path", "README.md")])))?;
    out.save("file-text", &text)?;
    out.save(
        "file-image",
        &api.get(&format!("/api/projects/{local_id}/file?{}", qs(&[("path", "assets/logo.png")])))?,
    )?;
    out.save(
        "file-binary",
        &api.get(&format!("/api/projects/{local_id}/file?{}", qs(&[("path", "assets/blob.bin")])))?,
    )?;
    // 原始字节路由不是 JSON，只确认 rawBase 拼出来的地址取得到字节
    let raw_base = text.text("rawBase").map_err(|e| anyhow!(e))?;
    let raw = client::request(api.port, "GET", &format!("{raw_base}assets/logo.png"), &[], None)?;
    if raw.status != 200 || raw.body != png() {
        bail!("原始字节路由不对：{}", raw.status);
    }
    out.save(
        "mkdir",
        &api.call(
            "POST",
            &format!("/api/projects/{local_id}/mkdir"),
            json!({ "path": "uploads", "recursive": false }),
        )?,
    )?;
    let upload = format!("/api/projects/{local_id}/upload?{}", qs(&[("path", "uploads"), ("name", "hello.txt")]));
    out.save("upload", &api.request("PUT", &upload, Body::Bytes(b"hello falcon\n", OCTET_STREAM), &[200])?.json)?;
    out.save("error-conflict", &api.request("PUT", &upload, Body::Bytes(b"again\n", OCTET_STREAM), &[409])?.json)?;
    out.save(
        "rename",
        &api.call(
            "POST",
            &format!("/api/projects/{local_id}/rename"),
            json!({ "path": "uploads/hello.txt", "name": "hi.txt" }),
        )?,
    )?;
    out.save(
        "remove",
        &api.call(
            "POST",
            &format!("/api/projects/{local_id}/remove"),
            json!({ "paths": ["uploads/hi.txt", "../outside.txt"] }),
        )?,
    )?;

    // ---- 中转（挂机器，ADR 0016）：enabled:false，只落库不起隧道、不连主机 ----
    let fwd = api.call(
        "POST",
        "/api/forwards",
        json!({
            "hostId": host_id,
            "name": "pg",
            "kind": "local",
            "bindPort": 45999,
            "destPort": 5432,
            "enabled": false,
        }),
    )?;
    out.save("forward-create", &fwd)?;
    let fwd_id = id(&fwd)?;
    out.save("forward-update", &api.call("PATCH", &format!("/api/forwards/{fwd_id}"), json!({ "name": "postgres" }))?)?;
    // 本机的发布没有 hostId，挂主机的有
    let share = api.call("POST", "/api/shares", json!({ "name": "vite", "destPort": 5173, "enabled": false }))?;
    out.save("share-create", &share)?;
    let share_id = id(&share)?;
    let host_share = api.call(
        "POST",
        "/api/shares",
        json!({ "hostId": host_id, "name": "api", "destPort": 8000, "enabled": false }),
    )?;
    out.save("share-create-host", &host_share)?;
    out.save("share-update", &api.call("PATCH", &format!("/api/shares/{share_id}"), json!({ "name": "web" }))?)?;
    out.save("relays", &api.get("/api/relays")?)?;
    out.save("forward-delete", &api.bare("DELETE", &format!("/api/forwards/{fwd_id}"))?)?;
    out.save("share-delete", &api.bare("DELETE", &format!("/api/shares/{share_id}"))?)?;
    api.bare("DELETE", &format!("/api/shares/{}", id(&host_share)?))?;

    // ---- 宿主机 Zellij（仅 SSH 项目；只读写库，不连远端）----
    out.save("host-zellij-status", &api.get(&format!("/api/projects/{ssh_id}/host"))?)?;
    out.save(
        "host-authorization",
        &api.call("POST", &format!("/api/projects/{ssh_id}/host"), json!({ "authorized": false }))?,
    )?;
    out.save("host-zellij-status-denied", &api.get(&format!("/api/projects/{ssh_id}/host"))?)?;

    // ---- 附属项目：派生 → 预检 → 存档 → 恢复 → 删除（会在仓库旁边建一棵 worktree）----
    let wt = api.call(
        "POST",
        &format!("/api/projects/{local_id}/worktrees"),
        json!({ "mode": "new-branch", "branch": "feat/fixture" }),
    )?;
    out.save("worktree-create", &wt)?;
    let wt_id = id(&wt)?;
    out.save("worktree-status", &api.get(&format!("/api/projects/{wt_id}/worktree"))?)?;
    out.save("project-archive", &api.bare("POST", &format!("/api/projects/{wt_id}/archive"))?)?;
    out.save("project-restore", &api.bare("POST", &format!("/api/projects/{wt_id}/restore"))?)?;
    out.save("project-delete", &api.bare("DELETE", &format!("/api/projects/{wt_id}?force=true"))?)?;
    out.save("projects", &api.get("/api/projects")?)?;

    // ---- 会话 ----
    let session = api.call(
        "POST",
        &format!("/api/projects/{local_id}/sessions"),
        json!({ "appearance": "dark", "background": "#0a0a0a", "foreground": "#fafafa" }),
    )?;
    out.save("session-create", &session)?;
    let session_id = id(&session)?;
    let (ws_messages, busy) = capture_session_ws(api, &session_id)?;
    out.save("ws-session-messages", &Js::Arr(ws_messages))?;
    out.save("session-foreground-busy", &busy)?;
    out.save("session-foreground", &api.get(&format!("/api/sessions/{session_id}/foreground"))?)?;
    out.save(
        "session-rename",
        &api.call("PATCH", &format!("/api/sessions/{session_id}"), json!({ "name": "fixture shell" }))?,
    )?;
    out.save("sessions", &api.get("/api/sessions")?)?;
    out.save("system-after-session", &api.get("/api/system")?)?;
    out.save(
        "paste-image",
        &api.request(
            "POST",
            &format!("/api/sessions/{session_id}/paste-image"),
            Body::Bytes(&png(), "image/png"),
            &[200],
        )?
        .json,
    )?;
    out.save("session-reattach", &api.bare("POST", &format!("/api/sessions/{session_id}/reattach"))?)?;
    out.save("session-terminate", &api.bare("POST", &format!("/api/sessions/{session_id}/terminate"))?)?;
    // Terminate 直接删行：之后既不在列表里，也清除不了（清除只认 dead 的行）
    out.save("sessions-after-terminate", &api.get("/api/sessions")?)?;
    out.save(
        "error-session-clear",
        &api.request("DELETE", &format!("/api/sessions/{session_id}"), Body::None, &[409])?.json,
    )?;
    out.save("ws-session-missing", &Js::Arr(ws_collect(api, &format!("/ws/sessions/{session_id}"), Until::Ms(800))?))?;

    // shell 自己退出 = 已丢失（dead）：这才是能"清除记录"的那种会话
    let doomed = api.call("POST", &format!("/api/projects/{local_id}/sessions"), json!({}))?;
    let doomed_id = id(&doomed)?;
    out.save("ws-session-exit", &Js::Arr(capture_session_exit(api, &doomed_id)?))?;
    out.save("sessions-with-dead", &api.get("/api/sessions")?)?;
    out.save("session-reattach-dead", &api.bare("POST", &format!("/api/sessions/{doomed_id}/reattach"))?)?;
    out.save("ws-session-dead", &Js::Arr(ws_collect(api, &format!("/ws/sessions/{doomed_id}"), Until::Ms(800))?))?;
    out.save("session-clear", &api.bare("DELETE", &format!("/api/sessions/{doomed_id}"))?)?;

    // ---- Zellij 安装通道：本地项目已经装好，直接 done；项目不存在是 failed ----
    out.save("ws-install-local", &Js::Arr(ws_collect(api, &format!("/ws/install/{local_id}"), Until::Close)?))?;
    out.save("ws-install-missing", &Js::Arr(ws_collect(api, "/ws/install/no-such-project", Until::Close)?))?;

    // ---- 飞书项目：状态脱敏；业务数据（空间、待办）一律不取；固定列表是 falcon 自己的库 ----
    let mut meegle = api.get("/api/meegle/status")?;
    if meegle.get("user").is_some_and(Js::truthy) {
        meegle.set(
            "user",
            Js::obj([
                ("key", Js::str("user_redacted")),
                ("name", Js::str("已脱敏")),
                ("email", Js::str("redacted@example.com")),
            ]),
        );
    }
    out.save("meegle-status", &meegle)?;
    let flag = |k: &str| meegle.get(k).is_some_and(Js::truthy);
    if !flag("installed") || !flag("authenticated") {
        // 没装 / 没登录时业务接口是 409 + reason，正好是 app 要认的错误体
        out.save("error-meegle-unavailable", &api.request("GET", "/api/meegle/spaces", Body::None, &[409])?.json)?;
    }
    let pin = api.call(
        "POST",
        "/api/meegle/pins",
        json!({
            "kind": "workitem",
            "spaceKey": "fixture_space",
            "spaceName": "Fixture",
            "targetId": "1001",
            "typeKey": "story",
            "label": "登录页",
            "url": "https://project.feishu.cn/fixture/story/detail/1001",
        }),
    )?;
    out.save("meegle-pin-create", &pin)?;
    let pin_id = id(&pin)?;
    api.call(
        "POST",
        "/api/meegle/pins",
        json!({ "kind": "view", "spaceKey": "fixture_space", "targetId": "view_1", "label": "view_1" }),
    )?;
    out.save(
        "meegle-pin-rename",
        &api.call("PATCH", &format!("/api/meegle/pins/{pin_id}"), json!({ "label": "登录页（改）" }))?,
    )?;
    out.save("meegle-pins", &api.get("/api/meegle/pins")?)?;
    out.save("meegle-pin-delete", &api.bare("DELETE", &format!("/api/meegle/pins/{pin_id}"))?)?;

    // ---- 错误体 ----
    out.save("error-not-found", &api.request("GET", "/api/projects/no-such-project/git", Body::None, &[404])?.json)?;
    out.save(
        "error-bad-request",
        &api.request(
            "GET",
            &format!("/api/projects/{local_id}/git/commit?{}", qs(&[("sha", "zz")])),
            Body::None,
            &[400],
        )?
        .json,
    )?;

    // ---- 最后才设密码：之后所有请求都要 cookie ----
    out.save("auth-password", &api.call("POST", "/api/auth/password", json!({ "next": PASSWORD }))?)?;
    out.save("auth-status-locked", &api.get("/api/auth/status")?)?;
    out.save("error-unauthorized", &api.request("GET", "/api/projects", Body::None, &[401])?.json)?;
    out.save(
        "error-login",
        &api.request("POST", "/api/auth/login", Body::Json(json!({ "password": "wrong-password" })), &[401])?.json,
    )?;
    let login = api.request("POST", "/api/auth/login", Body::Json(json!({ "password": PASSWORD })), &[200])?;
    api.cookie = Some(login.token.context("登录响应里没有 falcon_token")?);
    out.save("auth-login", &login.json)?;
    out.save("auth-status-authenticated", &api.get("/api/auth/status")?)?;

    // 收尾：删项目（DELETE 的响应已经有 project-delete 了，这里只清理）
    for p in [&local_id, &ssh_id, &multi_id, &plain_id] {
        api.bare("DELETE", &format!("/api/projects/{p}?force=true"))?;
    }
    api.bare("DELETE", &format!("/api/hosts/{host_id}"))?;
    out.save("auth-logout", &api.bare("POST", "/api/auth/logout")?)?;
    Ok(())
}

/// 响应里的 `id`（拼下一个请求的 URL 用）
fn id(v: &Js) -> Result<String> {
    v.text("id").map_err(|e| anyhow!(e))
}

// ---------------- WebSocket ----------------

/// 会话 WS 的二进制帧：1 字节类型头 + 终端字节。0x02 是回放（TERM_FRAME_REPLAY）
const TERM_FRAME_REPLAY: u8 = 0x02;

/// 连上会话，报尺寸与外观，敲一条 `sleep` 让前台命令变一变，收集控制消息。
/// 返回（控制消息, sleep 跑着时取的前台命令）
fn capture_session_ws(api: &Api, id: &str) -> Result<(Vec<Js>, Js)> {
    let mut ws = api.ws(&format!("/ws/sessions/{id}"))?;
    let mut text = Vec::new();
    let mut replay = 0usize;
    let mut output = String::new();
    let mut on = |frame: Frame| -> Result<bool> {
        match frame {
            Frame::Text(s) => text.push(parse_msg(&s)?),
            Frame::Binary(bytes) => {
                if bytes.first() == Some(&TERM_FRAME_REPLAY) {
                    replay += 1;
                }
                // 逐帧按 UTF-8 解（与脚本的 Buffer.toString 一样，帧边界上切开的字符成 U+FFFD）
                output.push_str(&String::from_utf8_lossy(bytes.get(1..).unwrap_or_default()));
            }
        }
        Ok(false)
    };
    ws.send(r##"{"type":"appearance","appearance":"dark","background":"#0a0a0a","foreground":"#fafafa"}"##)?;
    ws.send(r#"{"type":"resize","cols":100,"rows":30}"#)?;
    ws.hold(Duration::from_millis(1500), &mut on)?;
    ws.send(r#"{"type":"input","data":"sleep 4\r"}"#)?;
    ws.hold(Duration::from_millis(2000), &mut on)?;
    let busy = api.get(&format!("/api/sessions/{id}/foreground"))?;
    ws.hold(Duration::from_millis(5000), &mut on)?;
    ws.close();
    if replay == 0 {
        bail!("会话 WS 没收到回放帧");
    }
    let plain = strip_ansi(&output);
    if !plain.contains("sleep 4") {
        let tail: String = {
            let chars: Vec<char> = plain.chars().collect();
            chars[chars.len().saturating_sub(600)..].iter().collect()
        };
        bail!("会话 WS 没收到输入回显：{}\n控制消息：{}", Js::Str(tail).compact(), Js::Arr(text).compact());
    }
    Ok((text, busy))
}

/// 连上会话敲 `exit`，收集控制消息直到服务端推 dead。
fn capture_session_exit(api: &Api, id: &str) -> Result<Vec<Js>> {
    let mut ws = api.ws(&format!("/ws/sessions/{id}"))?;
    let mut text = Vec::new();
    // dead 可能在敲 exit 之前的那 1.5s 里就到了（脚本里 promise 早已 resolve，await 立刻过）
    let dead = std::cell::Cell::new(false);
    let mut on = |frame: Frame| -> Result<bool> {
        if let Frame::Text(s) = frame {
            let msg = parse_msg(&s)?;
            if msg.get("type") == Some(&Js::str("state")) && msg.get("state") == Some(&Js::str("dead")) {
                dead.set(true);
            }
            text.push(msg);
        }
        Ok(dead.get())
    };
    ws.send(r#"{"type":"resize","cols":100,"rows":30}"#)?;
    ws.hold(Duration::from_millis(1500), &mut on)?;
    ws.send(r#"{"type":"input","data":"exit\r"}"#)?;
    if !dead.get() {
        ws.pump(Instant::now() + Duration::from_secs(15), &mut on)?;
    }
    if !dead.get() {
        bail!("exit 之后 15s 没等到 dead：{}", Js::Arr(text).compact());
    }
    // 服务端在 dead 后面紧跟一条 `title: null`（mark_dead 里连着广播）。脚本里这两帧落在
    // 同一个 TCP 包里，dead 的回调还没轮到 close()，title 就已经派发进来了——提交的
    // fixture 里有它。这里留一小段余量把紧跟着的帧收完，比赌同一个包更稳
    ws.pump(Instant::now() + Duration::from_millis(300), &mut |frame| {
        if let Frame::Text(s) = frame {
            text.push(parse_msg(&s)?);
        }
        Ok(false)
    })?;
    ws.close();
    Ok(text)
}

enum Until {
    /// 收这么多毫秒就关
    Ms(u64),
    /// 等服务端关（最多 30s）
    Close,
}

/// 连一条 WS，收集文本帧：到时间就关，或者等服务端关。
fn ws_collect(api: &Api, path: &str, until: Until) -> Result<Vec<Js>> {
    let mut ws = api.ws(path)?;
    let mut text = Vec::new();
    let mut on = |frame: Frame| -> Result<bool> {
        if let Frame::Text(s) = frame {
            text.push(parse_msg(&s)?);
        }
        Ok(false)
    };
    match until {
        Until::Close => {
            if ws.pump(Instant::now() + Duration::from_secs(30), &mut on)? != Pumped::Closed {
                bail!("{path} 30s 没关");
            }
        }
        Until::Ms(ms) => {
            ws.hold(Duration::from_millis(ms), &mut on)?;
            ws.close();
            // 脚本里是 `await closed`（不设上限）；这里给 10s，服务端不回 close 也不至于挂死
            ws.wait_closed(Instant::now() + Duration::from_secs(10));
        }
    }
    Ok(text)
}

fn parse_msg(s: &str) -> Result<Js> {
    Js::parse(s).map_err(|e| anyhow!("WS 文本帧不是 JSON（{e}）：{s}"))
}

/// 粗略去掉 ANSI 转义：zellij 重绘时会在字符之间插光标移动与 SGR。
///
/// 照脚本里的三条正则依次各扫一遍（顺序有关：先去 CSI，再去 OSC，再去字符集切换）：
/// `\x1b\[[0-9;?]*[ -\/]*[@-~]`、`\x1b\][^\x07\x1b]*(\x07|\x1b\\)`、`\x1b[()][A-Z0-9]`。
/// 三条的字符类彼此不相交，贪婪匹配不需要回溯，手写扫描与正则结果一致。
fn strip_ansi(s: &str) -> String {
    let csi = strip_pass(s, |c, i| {
        if c.get(i + 1) != Some(&'[') {
            return None;
        }
        let mut j = i + 2;
        while c.get(j).is_some_and(|&ch| ch.is_ascii_digit() || ch == ';' || ch == '?') {
            j += 1;
        }
        while c.get(j).is_some_and(|&ch| (' '..='/').contains(&ch)) {
            j += 1;
        }
        c.get(j).is_some_and(|&ch| ('@'..='~').contains(&ch)).then_some(j + 1)
    });
    let osc = strip_pass(&csi, |c, i| {
        if c.get(i + 1) != Some(&']') {
            return None;
        }
        let mut j = i + 2;
        while c.get(j).is_some_and(|&ch| ch != '\x07' && ch != '\x1b') {
            j += 1;
        }
        match (c.get(j), c.get(j + 1)) {
            (Some('\x07'), _) => Some(j + 1),
            (Some('\x1b'), Some('\\')) => Some(j + 2),
            _ => None,
        }
    });
    strip_pass(&osc, |c, i| {
        let charset = matches!(c.get(i + 1), Some('(' | ')'))
            && c.get(i + 2).is_some_and(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit());
        charset.then_some(i + 3)
    })
}

/// 从左到右扫，遇到 ESC 就问 `matcher` 能不能从这里匹配、匹配到哪（不含），能就跳过整段
fn strip_pass(s: &str, matcher: impl Fn(&[char], usize) -> Option<usize>) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\x1b'
            && let Some(end) = matcher(&chars, i)
        {
            i = end;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

// ---------------- HTTP ----------------

const OCTET_STREAM: &str = "application/octet-stream";

enum Body<'a> {
    None,
    Json(serde_json::Value),
    /// 原始字节 + Content-Type
    Bytes(&'a [u8], &'a str),
}

struct Reply {
    json: Js,
    /// Set-Cookie 里的 falcon_token（只有登录响应有）
    token: Option<String>,
}

struct Api {
    port: u16,
    /// 设了密码、登录之后才有
    cookie: Option<String>,
}

impl Api {
    /// GET，期望 200
    fn get(&self, path: &str) -> Result<Js> {
        Ok(self.request("GET", path, Body::None, &[200])?.json)
    }

    /// 带 JSON 请求体，期望 200
    fn call(&self, method: &str, path: &str, body: serde_json::Value) -> Result<Js> {
        Ok(self.request(method, path, Body::Json(body), &[200])?.json)
    }

    /// 不带请求体，期望 200
    fn bare(&self, method: &str, path: &str) -> Result<Js> {
        Ok(self.request(method, path, Body::None, &[200])?.json)
    }

    /// 脚本里的 `api()`：状态码不在 `expect` 里就报错；响应不是 JSON 时 `json` 为 Undefined
    /// （由 save 报"响应不是 JSON"）
    fn request(&self, method: &str, path: &str, body: Body, expect: &[u16]) -> Result<Reply> {
        let cookie = self.cookie.as_ref().map(|c| format!("falcon_token={c}"));
        let mut headers: Vec<(&str, &str)> = Vec::new();
        if let Some(c) = &cookie {
            headers.push(("Cookie", c.as_str()));
        }
        let json_body;
        let payload: Option<&[u8]> = match body {
            Body::None => None,
            Body::Json(v) => {
                headers.push(("Content-Type", "application/json"));
                json_body = serde_json::to_vec(&v)?;
                Some(json_body.as_slice())
            }
            Body::Bytes(bytes, content_type) => {
                headers.push(("Content-Type", content_type));
                Some(bytes)
            }
        };
        let res = client::request(self.port, method, path, &headers, payload)?;
        let raw = String::from_utf8_lossy(&res.body);
        if !expect.contains(&res.status) {
            let expected: Vec<String> = expect.iter().map(u16::to_string).collect();
            let head: String = raw.chars().take(500).collect();
            bail!("{method} {path} → {}（期望 {}）：{head}", res.status, expected.join("/"));
        }
        let json = Js::parse(&raw).unwrap_or(Js::Undefined);
        let token = res.header("set-cookie").and_then(|v| cookie_value(&v, "falcon_token"));
        Ok(Reply { json, token })
    }

    fn ws(&self, path: &str) -> Result<Ws> {
        Ws::connect(self.port, path)
    }
}

/// 脚本里的 `/falcon_token=([^;]+)/.exec(set-cookie)`：第一处 `name=` 后面到分号为止，
/// 空值不算（正则要求至少一个字符，会接着往后找）
fn cookie_value(header: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=");
    let mut rest = header;
    while let Some(pos) = rest.find(&needle) {
        rest = &rest[pos + needle.len()..];
        let value = rest.split(';').next().unwrap_or("");
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

/// `new URLSearchParams(o).toString()`：application/x-www-form-urlencoded，
/// 空格写 `+`，只留字母数字与 `*-._`，其余按 UTF-8 字节 `%XX`（大写）
fn qs(pairs: &[(&str, &str)]) -> String {
    let enc = |s: &str| {
        let mut out = String::new();
        for b in s.bytes() {
            match b {
                b' ' => out.push('+'),
                b'*' | b'-' | b'.' | b'_' | b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z' => out.push(b as char),
                _ => out.push_str(&format!("%{b:02X}")),
            }
        }
        out
    };
    pairs.iter().map(|(k, v)| format!("{}={}", enc(k), enc(v))).collect::<Vec<_>>().join("&")
}

struct Out {
    dir: PathBuf,
    written: usize,
}

impl Out {
    /// `JSON.stringify(value, null, 2)` + 换行
    fn save(&mut self, name: &str, value: &Js) -> Result<()> {
        if *value == Js::Undefined {
            bail!("{name}：响应不是 JSON");
        }
        fs::write(self.dir.join(format!("{name}.json")), format!("{}\n", value.pretty()))?;
        self.written += 1;
        Ok(())
    }
}

// ---------------- 环境 ----------------

const GIT_ENV: [(&str, &str); 4] = [
    ("GIT_AUTHOR_NAME", "Falcon Fixture"),
    ("GIT_AUTHOR_EMAIL", "fixture@example.com"),
    ("GIT_COMMITTER_NAME", "Falcon Fixture"),
    ("GIT_COMMITTER_EMAIL", "fixture@example.com"),
];

fn git(args: &[&str]) -> Result<()> {
    // 本机的全局配置可能要求签名、挂着钩子：一律关掉，fixture 不该依赖它们
    let mut argv =
        vec!["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false", "-c", "core.hooksPath=/dev/null", "-C", REPO];
    argv.extend_from_slice(args);
    let env: Vec<(&str, &OsStr)> = GIT_ENV.iter().map(|(k, v)| (*k, OsStr::new(v))).collect();
    util::run_env("git", argv, None, &env)
}

fn prepare_repo() -> Result<()> {
    let repo = Path::new(REPO);
    fs::create_dir_all(repo.join("src"))?;
    fs::create_dir_all(repo.join("assets"))?;
    fs::create_dir_all(PLAIN)?;
    fs::write(Path::new(PLAIN).join("readme.txt"), "not a git repo\n")?;
    git(&["init", "-q", "-b", "main"])?;
    git(&["config", "user.name", "Falcon Fixture"])?;
    git(&["config", "user.email", "fixture@example.com"])?;
    fs::write(repo.join("README.md"), "# fixture\n\n协议 fixture 用的临时仓库。\n")?;
    fs::write(repo.join("src/main.rs"), "fn main() {\n    println!(\"hello\");\n}\n")?;
    fs::write(repo.join("assets/logo.png"), png())?;
    fs::write(repo.join("assets/blob.bin"), [0u8, 1, 2, 3, 0, 255, 254, 0, 7])?;
    git(&["add", "-A"])?;
    commit_at("2026-09-01T10:00:00+08:00", "feat: 初始提交")?;
    git(&["tag", "v0.1.0"])?;
    {
        use std::io::Write as _;
        let mut readme = fs::OpenOptions::new().append(true).open(repo.join("README.md"))?;
        readme.write_all("\n第二段说明。\n".as_bytes())?;
    }
    git(&["add", "-A"])?;
    commit_at("2026-09-02T10:00:00+08:00", "docs: 补充说明")?;
    // 一处未提交改动 + 一个未跟踪文件
    fs::write(repo.join("src/main.rs"), "fn main() {\n    println!(\"hello, falcon\");\n}\n")?;
    fs::write(repo.join("notes.txt"), "未跟踪的笔记\n")?;
    Ok(())
}

/// 固定提交时间：提交 sha 与 git-log 里的时间每次都一样
fn commit_at(date: &str, message: &str) -> Result<()> {
    let mut env: Vec<(&str, &OsStr)> = GIT_ENV.iter().map(|(k, v)| (*k, OsStr::new(v))).collect();
    env.push(("GIT_AUTHOR_DATE", OsStr::new(date)));
    env.push(("GIT_COMMITTER_DATE", OsStr::new(date)));
    util::run_env(
        "git",
        ["-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null", "-C", REPO, "commit", "-q", "-m", message],
        None,
        &env,
    )
}

/// 把锁定版本的 zellij 拷进数据目录，省掉首次建会话时的下载。
/// 源文件只读不写（~/.falcon 是日常实例的数据目录）
fn prepare_data_dir() -> Result<()> {
    let bin_dir = Path::new(DATA).join("bin");
    fs::create_dir_all(&bin_dir)?;
    let version_rs = fs::read_to_string(util::native().join("crates/falcon-server/src/zellij/version.rs"))?;
    let marker = "ZELLIJ_VERSION: &str = \"";
    let Some(version) = version_rs.split_once(marker).and_then(|(_, rest)| rest.split_once('"')).map(|(v, _)| v) else {
        eprintln!("version.rs 里没找到 ZELLIJ_VERSION，服务端会自己下载 zellij（慢）");
        return Ok(());
    };
    let name = format!("zellij-{version}");
    let home = env::var_os("HOME").map(PathBuf::from);
    let mut candidates: Vec<PathBuf> = Vec::new();
    candidates.extend(env_path("FALCON_FIXTURE_ZELLIJ").map(PathBuf::from));
    if let Some(home) = &home {
        candidates.push(home.join(".falcon/bin").join(&name));
        candidates.push(home.join(".mojito/bin").join(&name));
    }
    let Some(found) = candidates.iter().find(|c| c.exists()) else {
        eprintln!("没找到现成的 {name}，服务端会自己下载（慢）");
        return Ok(());
    };
    let dest = bin_dir.join(&name);
    // macOS 上 fs::copy 先试 clonefile（APFS 写时复制），与脚本的 COPYFILE_FICLONE 一样
    fs::copy(found, &dest).with_context(|| format!("拷不了 {}", found.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

fn start_server(bin: &Path, port: u16) -> Result<Child> {
    let log = fs::OpenOptions::new().create(true).append(true).open(Path::new(DATA).join("server.log"))?;
    let mut cmd = Command::new(bin);
    cmd.args(["--host", "127.0.0.1", "--port", &port.to_string(), "--data-dir", DATA]);
    cmd.env_clear();
    for (k, v) in env::vars_os() {
        let inherited = k.to_str().is_none_or(|k| !k.starts_with("FALCON_") && !k.starts_with("MOJITO_"));
        if inherited {
            cmd.env(k, v);
        }
    }
    if env::var_os("LANG").is_none() {
        cmd.env("LANG", "en_US.UTF-8");
    }
    cmd.stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log);
    cmd.spawn().with_context(|| format!("起不来 {}", bin.display()))
}

fn wait_ready(api: &Api, child: &mut Child) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait()? {
            bail!("服务端提前退出：{status}，日志在 {DATA}/server.log");
        }
        // 还没起来时连接被拒，照常重试
        if let Ok(res) = client::request(api.port, "GET", "/api/auth/status", &[], None)
            && (200..300).contains(&res.status)
        {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    bail!("服务端 20s 没起来，日志在 {DATA}/server.log")
}

/// 先 SIGTERM 让服务端自己收尾，5s 不走再 SIGKILL
fn stop_server(child: Option<Child>) {
    let Some(mut child) = child else { return };
    if !matches!(child.try_wait(), Ok(None)) {
        return;
    }
    let _ = Command::new("kill").args(["-TERM", &child.id().to_string()]).status();
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if !matches!(child.try_wait(), Ok(None)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn cleanup_dirs() {
    // 中途失败时会话没来得及 Terminate，zellij server 是独立进程、不随后端退出：
    // 按二进制路径把数据目录里起的那些收掉（只匹配我们自己的数据目录）。
    // 没有匹配的进程时 pkill 退出码是 1，不算错
    let _ = Command::new("pkill")
        .args(["-f", &format!("{DATA}/bin/zellij")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    // 只删自己建的：数据目录、临时仓库、非仓库目录、派生在仓库旁边的 worktree
    let Ok(entries) = fs::read_dir(TMP) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "fal-fx-data" || name == "fal-fx-plain" || name == "fal-fx-repo" || name.starts_with("fal-fx-repo-")
        {
            let path = entry.path();
            let res = if path.is_dir() { fs::remove_dir_all(&path) } else { fs::remove_file(&path) };
            if let Err(e) = res {
                eprintln!("删不掉 {}：{e}", path.display());
            }
        }
    }
}

fn free_port(from: u16, to: u16) -> Result<u16> {
    // 与 Node 的 listen 一样带 SO_REUSEADDR（std 在 Unix 上默认设），绑得上就是空的
    (from..=to)
        .find(|&p| std::net::TcpListener::bind(("127.0.0.1", p)).is_ok())
        .ok_or_else(|| anyhow!("{from}–{to} 没有空闲端口"))
}

// ---------------- compare-fixtures ----------------

pub fn compare(args: &[String]) -> Result<()> {
    let (base_dir, other_dir) = match args {
        [a, b, ..] if !a.is_empty() && !b.is_empty() => (Path::new(a), Path::new(b)),
        _ => {
            eprintln!("用法：cargo xtask compare-fixtures <基准目录> <对照目录>");
            std::process::exit(2);
        }
    };
    let mut names = BTreeSet::new();
    for dir in [base_dir, other_dir] {
        for entry in fs::read_dir(dir).with_context(|| format!("读不了 {}", dir.display()))? {
            let name = entry?.file_name().to_string_lossy().into_owned();
            if name.ends_with(".json") {
                names.insert(name);
            }
        }
    }
    let mut diffs = Vec::new();
    for name in &names {
        let (pa, pb) = (base_dir.join(name), other_dir.join(name));
        if !pa.exists() {
            diffs.push(format!("{name}: 基准里没有"));
            continue;
        }
        if !pb.exists() {
            diffs.push(format!("{name}: 对照里没有"));
            continue;
        }
        walk(&read_json(&pa)?, &read_json(&pb)?, name, &mut diffs);
    }
    for d in &diffs {
        println!("{d}");
    }
    if diffs.is_empty() {
        println!("形状一致");
        Ok(())
    } else {
        println!("\n{} 处差异", diffs.len());
        std::process::exit(1);
    }
}

fn read_json(path: &Path) -> Result<Js> {
    let src = fs::read_to_string(path).with_context(|| format!("读不了 {}", path.display()))?;
    Js::parse(&src).map_err(|e| anyhow!("{}：{e}", path.display()))
}

fn walk(a: &Js, b: &Js, at: &str, diffs: &mut Vec<String>) {
    let (ka, kb) = (a.kind(), b.kind());
    if ka != kb {
        diffs.push(format!("{at}: 类型 {ka} ≠ {kb}"));
        return;
    }
    match (a, b) {
        (Js::Arr(xs), Js::Arr(ys)) => {
            if xs.len() != ys.len() {
                diffs.push(format!("{at}: 数组长度 {} ≠ {}", xs.len(), ys.len()));
            }
            for (i, (x, y)) in xs.iter().zip(ys).enumerate() {
                walk(x, y, &format!("{at}[{i}]"), diffs);
            }
        }
        (Js::Obj(xs), Js::Obj(ys)) => {
            let has = |pairs: &[(String, Js)], key: &str| pairs.iter().any(|(k, _)| k == key);
            let missing: Vec<&str> = xs.iter().map(|(k, _)| k.as_str()).filter(|k| !has(ys, k)).collect();
            let extra: Vec<&str> = ys.iter().map(|(k, _)| k.as_str()).filter(|k| !has(xs, k)).collect();
            if !missing.is_empty() {
                diffs.push(format!("{at}: 缺键 {}", missing.join(", ")));
            }
            if !extra.is_empty() {
                diffs.push(format!("{at}: 多键 {}", extra.join(", ")));
            }
            let common: Vec<&str> = xs.iter().map(|(k, _)| k.as_str()).filter(|k| has(ys, k)).collect();
            let order_b: Vec<&str> = ys.iter().map(|(k, _)| k.as_str()).filter(|k| has(xs, k)).collect();
            if common.join(",") != order_b.join(",") {
                diffs.push(format!("{at}: 键序 [{}] ≠ [{}]", common.join(","), order_b.join(",")));
            }
            for k in common {
                if let (Some(x), Some(y)) = (a.get(k), b.get(k)) {
                    walk(x, y, &format!("{at}.{k}"), diffs);
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qs_matches_url_search_params() {
        assert_eq!(qs(&[("path", "/private/tmp/fal-fx-repo")]), "path=%2Fprivate%2Ftmp%2Ffal-fx-repo");
        assert_eq!(qs(&[("path", "notes.txt"), ("untracked", "1")]), "path=notes.txt&untracked=1");
        assert_eq!(qs(&[("q", "a b*~'中")]), "q=a+b*%7E%27%E4%B8%AD");
    }

    #[test]
    fn strip_ansi_matches_the_three_regexes() {
        let s = "\x1b[1;32mgreen\x1b[0m \x1b[?25l\x1b]0;title\x07x\x1b]8;;u\x1b\\y\x1b(Bz\x1b[ q";
        assert_eq!(strip_ansi(s), "green xyz");
        // 不成形的序列原样留下
        assert_eq!(strip_ansi("a\x1b[12"), "a\x1b[12");
        assert_eq!(strip_ansi("s\x1b[Ble\x1b[Cep 4"), "sleep 4");
    }

    #[test]
    fn cookie_value_like_the_regex() {
        let h = "falcon_token=abc.def; Path=/; HttpOnly";
        assert_eq!(cookie_value(h, "falcon_token").as_deref(), Some("abc.def"));
        assert_eq!(cookie_value("falcon_token=; Max-Age=0, falcon_token=x", "falcon_token").as_deref(), Some("x"));
        assert_eq!(cookie_value("other=1", "falcon_token"), None);
    }

    #[test]
    fn walk_reports_like_compare_fixtures() {
        let a = Js::parse(r#"{"a":1,"b":[1,2],"c":{"x":null},"d":"s"}"#).unwrap();
        let b = Js::parse(r#"{"c":{"x":"y"},"a":"1","b":[1],"e":true}"#).unwrap();
        let mut diffs = Vec::new();
        walk(&a, &b, "f.json", &mut diffs);
        assert_eq!(
            diffs,
            [
                "f.json: 缺键 d",
                "f.json: 多键 e",
                "f.json: 键序 [a,b,c] ≠ [c,a,b]",
                "f.json.a: 类型 number ≠ string",
                "f.json.b: 数组长度 2 ≠ 1",
                "f.json.c.x: 类型 null ≠ string",
            ]
        );
    }
}
