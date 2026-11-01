//! `falcon-sessiond hc` 子命令入口：
//! - `hc serve`：单实例长驻网关，拿住 `<state_dir>/core.lock` 的 `flock(LOCK_EX | LOCK_NB)`，
//!   写入监听的 socket 路径，对外服务 Plain 与 Framed 双模式连接；
//! - `hc connect [--] [ignored...]`：供 `hc machine add --via` 或 SSH 远端使用，
//!   若网关没在跑先后台拉起，然后在 stdin/stdout 与网关 socket 之间双向搬运字节；
//! - `hc status [--json]`：探测网关是否在跑、返回版本/PID/会话数/socket 路径；
//! - `hc config [--synthesize-labels <true|false>]`：读写 `core.json` 里的 `synthesize_labels` 开关。

#[cfg(unix)]
pub mod attach;
#[cfg(unix)]
pub mod mirror;
#[cfg(unix)]
pub mod ops;
pub mod paint;
pub mod queryfilter;
pub mod registry;
pub mod rpc;
pub mod wire;

#[cfg(not(unix))]
pub fn run(_layout: &crate::layout::Layout, _sub: &str, _rest: Vec<String>) -> anyhow::Result<i32> {
    anyhow::bail!("falcon-sessiond hc 网关第一期仅支持 Unix (macOS / Linux)");
}

#[cfg(not(unix))]
pub fn maybe_autostart(_layout: &crate::layout::Layout) {}

#[cfg(unix)]
pub use unix_impl::{maybe_autostart, run};

#[cfg(unix)]
mod unix_impl {
    use std::collections::BTreeMap;
    use std::io::{Read as _, Write as _};
    use std::os::unix::io::AsRawFd as _;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicU32;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use anyhow::{Context, Result, bail};
    use serde::{Deserialize, Serialize};
    use serde_json::{Value, json};
    use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
    use tokio::net::{TcpListener, UnixListener};
    use tokio::sync::mpsc;

    use crate::hc::attach::decode_viewer_frame;
    use crate::hc::mirror::{EventFilter, GatewayState, SharedGateway, background_loop};
    use crate::hc::ops::{ConnContext, dispatch};
    use crate::hc::registry::Registry;
    use crate::hc::rpc::{RpcError, RpcRequest, err_response, ok_response};
    use crate::hc::wire::{Frame, FramedDecoder, kind as wkind};
    use crate::layout::{Layout, private_dir};

    #[derive(Debug, Clone)]
    pub struct HcPaths {
        pub session: String,
        pub state_dir: PathBuf,
        pub socket_path: PathBuf,
        pub lock_path: PathBuf,
    }

    #[derive(Serialize, Deserialize, Debug, Clone)]
    pub struct HcGatewayStatus {
        pub running: bool,
        pub session: String,
        pub socket_path: String,
        pub core_id: String,
        pub core_name: String,
        pub synthesize_labels: bool,
        pub pid: Option<u32>,
        pub version: Option<String>,
        pub live: usize,
        pub finished: usize,
    }

    /// 推导默认 `HC_SESSION` 名：
    /// - 显式传了 `--session` 或环境变量 `HC_SESSION`（且不是 `default`）则用它；
    /// - 默认数据目录 `~/.falcon` 下用 `"falcon"`；
    /// - 自定义数据目录用 `"falcon-<8位哈希>"`，避免多实例抢同一个 socket。
    pub fn resolve_paths(layout: &Layout, explicit_session: Option<&str>) -> HcPaths {
        let session = explicit_session
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .or_else(|| {
                std::env::var("HC_SESSION")
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty() && s != "default")
            })
            .unwrap_or_else(|| default_session_name(layout));

        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| layout.root.clone());
        let state_base = std::env::var_os("XDG_STATE_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local").join("state"));
        let state_dir = state_base.join("herdr-core").join(&session);

        let run_dir = std::env::var_os("XDG_RUNTIME_DIR")
            .filter(|v| !v.is_empty())
            .map(|p| PathBuf::from(p).join("herdr-core"))
            .or_else(|| {
                let uid = unsafe { libc::getuid() };
                let d = PathBuf::from(format!("/run/user/{uid}/herdr-core"));
                d.is_dir().then_some(d)
            })
            .unwrap_or_else(|| state_dir.clone());

        let socket_path = std::env::var_os("HC_SOCKET")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let candidate = run_dir.join(format!("{session}.sock"));
                // Unix sun_path 长度保护（macOS 104 字节）
                if candidate.to_string_lossy().len() > 95 {
                    let uid = unsafe { libc::getuid() };
                    PathBuf::from(format!("/tmp/hc-{uid}-{session}.sock"))
                } else {
                    candidate
                }
            });

        let lock_path = state_dir.join("core.lock");
        HcPaths {
            session,
            state_dir,
            socket_path,
            lock_path,
        }
    }

    fn default_session_name(layout: &Layout) -> String {
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            let default_falcon = std::path::absolute(home.join(".falcon")).unwrap_or_else(|_| home.join(".falcon"));
            if layout.root == default_falcon {
                return "falcon".into();
            }
        }
        let h = falcon_proto::sessiond::short_hash(&layout.root.to_string_lossy());
        format!("falcon-{}", &h[..8])
    }

    pub fn run(layout: &Layout, sub: &str, mut rest: Vec<String>) -> Result<i32> {
        layout.ensure_dirs()?;
        let session = take_opt(&mut rest, "--session")?;
        let sock_override = take_opt(&mut rest, "--socket")?;
        let state_override = take_opt(&mut rest, "--state-dir")?;
        let tcp_bind = take_opt(&mut rest, "--tcp")?;
        let daemon = take_flag(&mut rest, "--daemon");
        if take_flag(&mut rest, "--no-synth-space") {
            let mut reg = Registry::load(layout.clone())?;
            reg.identity.synthesize_labels = false;
            reg.save_identity();
        }
        let mut paths = resolve_paths(layout, session.as_deref());
        if let Some(sd) = state_override {
            let p = PathBuf::from(sd);
            paths.lock_path = p.join("core.lock");
            paths.state_dir = p;
        }
        if let Some(sk) = sock_override {
            paths.socket_path = PathBuf::from(sk);
        }
        match sub {
            "serve" => {
                if daemon {
                    let _ = ensure_running(layout, &paths)?;
                    return Ok(0);
                }
                serve(layout.clone(), paths, tcp_bind.as_deref())?;
                Ok(0)
            }
            "connect" => {
                // `hc machine add --via '... hc connect --'` 会把远端 shell 行拼在 `--` 后面，全部忽略
                connect(layout, &paths)?;
                Ok(0)
            }
            "status" => {
                let json_out = take_flag(&mut rest, "--json");
                let start_if_stopped = take_flag(&mut rest, "--ensure");
                if let Some(synth) = take_opt(&mut rest, "--synthesize-labels")? {
                    let val = matches!(synth.as_str(), "1" | "true" | "yes" | "on");
                    let mut reg = Registry::load(layout.clone())?;
                    reg.identity.synthesize_labels = val;
                    reg.save_identity();
                }
                if start_if_stopped {
                    let _ = ensure_running(layout, &paths);
                }
                let st = query_status(layout, &paths)?;
                if json_out {
                    println!("{}", serde_json::to_string(&st)?);
                } else if st.running {
                    println!(
                        "hc 网关运行中（session={}，pid={}，socket={}，live={}，finished={}）",
                        st.session,
                        st.pid.unwrap_or(0),
                        st.socket_path,
                        st.live,
                        st.finished
                    );
                } else {
                    println!("hc 网关未运行（session={}，socket={}）", st.session, st.socket_path);
                }
                Ok(0)
            }
            other => bail!("未知的 hc 子命令：{other}（可选：serve | connect | status）"),
        }
    }

    fn take_flag(args: &mut Vec<String>, flag: &str) -> bool {
        if let Some(pos) = args.iter().position(|a| a == flag) {
            args.remove(pos);
            true
        } else {
            false
        }
    }

    fn take_opt(args: &mut Vec<String>, name: &str) -> Result<Option<String>> {
        if let Some(pos) = args.iter().position(|a| a == name) {
            if pos + 1 >= args.len() {
                bail!("{name} 后面缺值");
            }
            let v = args.remove(pos + 1);
            args.remove(pos);
            Ok(Some(v))
        } else {
            Ok(None)
        }
    }

    fn try_flock_exclusive(file: &std::fs::File) -> bool {
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
    }

    pub fn query_status(layout: &Layout, paths: &HcPaths) -> Result<HcGatewayStatus> {
        let reg = Registry::load(layout.clone())?;
        let probe = probe_running_gateway(&paths.socket_path);
        match probe {
            Some(v) => Ok(HcGatewayStatus {
                running: true,
                session: paths.session.clone(),
                socket_path: paths.socket_path.to_string_lossy().into_owned(),
                core_id: v["core"]["id"]
                    .as_str()
                    .unwrap_or(&reg.identity.core_id)
                    .to_string(),
                core_name: v["core"]["name"]
                    .as_str()
                    .unwrap_or(&reg.identity.name)
                    .to_string(),
                synthesize_labels: reg.identity.synthesize_labels,
                pid: v["pid"].as_u64().map(|p| p as u32),
                version: v["version"].as_str().map(String::from),
                live: v["live"].as_u64().unwrap_or(0) as usize,
                finished: v["finished"].as_u64().unwrap_or(0) as usize,
            }),
            None => Ok(HcGatewayStatus {
                running: false,
                session: paths.session.clone(),
                socket_path: paths.socket_path.to_string_lossy().into_owned(),
                core_id: reg.identity.core_id,
                core_name: reg.identity.name,
                synthesize_labels: reg.identity.synthesize_labels,
                pid: None,
                version: None,
                live: 0,
                finished: 0,
            }),
        }
    }

    fn probe_running_gateway(socket: &Path) -> Option<Value> {
        use std::io::BufRead as _;
        let mut s = std::os::unix::net::UnixStream::connect(socket).ok()?;
        let _ = s.set_read_timeout(Some(Duration::from_millis(1500)));
        let _ = s.set_write_timeout(Some(Duration::from_millis(1500)));
        s.write_all(br#"{"jsonrpc":"2.0","id":1,"method":"server.status","params":{}}"#)
            .ok()?;
        s.write_all(b"\n").ok()?;
        let mut reader = std::io::BufReader::new(s);
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        let resp: Value = serde_json::from_str(&line).ok()?;
        resp.get("result").cloned()
    }

    /// 在 `spawn` / `bridge` / `list` 时顺手确保默认 `~/.falcon` 下的 `hc` 网关已在后台运行。
    /// 可通过 `FALCON_NO_HC=1` 禁用；非默认根目录（如临时测试目录）除非设了 `FALCON_AUTOSTART_HC=1`
    /// 否则不自动拉起后台进程。
    pub fn maybe_autostart(layout: &Layout) {
        if std::env::var_os("FALCON_NO_HC")
            .is_some_and(|v| matches!(v.to_str(), Some("1" | "true" | "yes")))
        {
            return;
        }
        let force = std::env::var_os("FALCON_AUTOSTART_HC")
            .is_some_and(|v| matches!(v.to_str(), Some("1" | "true" | "yes")));
        if !force {
            let is_default = std::env::var_os("HOME")
                .map(PathBuf::from)
                .is_some_and(|h| {
                    let def = std::path::absolute(h.join(".falcon")).unwrap_or_else(|_| h.join(".falcon"));
                    layout.root == def
                });
            if !is_default {
                return;
            }
        }
        let paths = resolve_paths(layout, None);
        if std::os::unix::net::UnixStream::connect(&paths.socket_path).is_ok() {
            return;
        }
        let _ = layout.ensure_dirs();
        let _ = spawn_gateway_background(layout, &paths);
    }

    /// 确保网关在后台运行，返回已连接的 `UnixStream`
    pub fn ensure_running(layout: &Layout, paths: &HcPaths) -> Result<std::os::unix::net::UnixStream> {
        if let Ok(s) = std::os::unix::net::UnixStream::connect(&paths.socket_path) {
            return Ok(s);
        }
        spawn_gateway_background(layout, paths)?;
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(8) {
            if let Ok(s) = std::os::unix::net::UnixStream::connect(&paths.socket_path) {
                return Ok(s);
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        bail!(
            "falcon-sessiond hc serve 未能在 8 秒内就绪（socket: {}）",
            paths.socket_path.display()
        )
    }

    fn spawn_gateway_background(layout: &Layout, paths: &HcPaths) -> Result<()> {
        use std::os::unix::process::CommandExt as _;
        let exe = std::env::current_exe()?;
        let log_file = layout.log_dir().join(format!("hc-{}.log", paths.session));
        let out_f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_file)
            .unwrap_or_else(|_| std::fs::File::open("/dev/null").unwrap());
        let err_f = out_f
            .try_clone()
            .unwrap_or_else(|_| std::fs::File::open("/dev/null").unwrap());

        let mut cmd = std::process::Command::new(exe);
        cmd.args([
            "hc",
            "serve",
            "--root",
            &layout.root.to_string_lossy(),
            "--session",
            &paths.session,
        ])
        .env("HC_SESSION", &paths.session)
        .env("HC_SOCKET", &paths.socket_path)
        .stdin(std::process::Stdio::null())
        .stdout(out_f)
        .stderr(err_f)
        .current_dir(&layout.root);

        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        cmd.spawn().context("拉起后台 hc serve 失败")?;
        Ok(())
    }

    /// `falcon-sessiond hc connect`：确保网关运行，并在 stdin/stdout 与网关 socket 间原样桥接字节
    pub fn connect(layout: &Layout, paths: &HcPaths) -> Result<()> {
        let sock_r = ensure_running(layout, paths)?;
        let mut sock_w = sock_r.try_clone()?;

        // stdin -> socket 写线程
        let t_in = std::thread::spawn(move || {
            let mut stdin = std::io::stdin().lock();
            let mut buf = [0u8; 16 * 1024];
            loop {
                match stdin.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if sock_w.write_all(&buf[..n]).is_err() || sock_w.flush().is_err() {
                            break;
                        }
                    }
                }
            }
            let _ = sock_w.shutdown(std::net::Shutdown::Write);
        });

        // socket -> stdout 主线程
        let mut sock_r = sock_r;
        let mut stdout = std::io::stdout().lock();
        let mut buf = [0u8; 16 * 1024];
        loop {
            match sock_r.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if stdout.write_all(&buf[..n]).is_err() || stdout.flush().is_err() {
                        break;
                    }
                }
            }
        }
        drop(stdout);
        let _ = sock_r.shutdown(std::net::Shutdown::Both);
        let _ = t_in.join();
        Ok(())
    }

    pub fn serve(layout: Layout, paths: HcPaths, tcp_bind: Option<&str>) -> Result<()> {
        std::fs::create_dir_all(&paths.state_dir)
            .with_context(|| format!("建不了目录 {}", paths.state_dir.display()))?;
        private_dir(&paths.state_dir)?;
        if let Some(parent) = paths.socket_path.parent() {
            std::fs::create_dir_all(parent)?;
            let _ = private_dir(parent);
        }

        let mut lock_file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&paths.lock_path)
            .with_context(|| format!("打不开 {}", paths.lock_path.display()))?;

        if !try_flock_exclusive(&lock_file) {
            // 已经有进程拿住锁了：检查是不是我们自己的网关；如果是真 hc 占住了，报错提示
            if let Some(status) = probe_running_gateway(&paths.socket_path) {
                if status["installed_by"].as_str() == Some("falcon") {
                    return Ok(());
                }
                bail!(
                    "会话 {:?} 已被原生 herdr-core 占用（socket: {}）。请先执行 `HC_SESSION={} hc server stop`",
                    paths.session,
                    paths.socket_path.display(),
                    paths.session
                );
            }
            return Ok(());
        }

        // 写入当前监听的 socket 路径（不带换行，与 herdr-core 的 core.lock 格式严格一致）
        lock_file.set_len(0)?;
        lock_file.write_all(paths.socket_path.as_os_str().as_encoded_bytes())?;
        lock_file.flush()?;

        let _ = std::fs::remove_file(&paths.socket_path);
        let gw = GatewayState::new(layout)?;

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("构建 tokio runtime 失败")?;

        let sock_path = paths.socket_path.clone();
        let tcp_addr = tcp_bind.map(str::to_string).or_else(|| {
            std::env::var("FALCON_HC_TCP")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        });
        let res = rt.block_on(async move {
            let listener = UnixListener::bind(&sock_path)
                .with_context(|| format!("监听 {} 失败", sock_path.display()))?;
            let tcp_listener = match tcp_addr.as_deref() {
                Some(addr) => Some(
                    TcpListener::bind(addr)
                        .await
                        .with_context(|| format!("监听 TCP {addr} 失败"))?,
                ),
                None => None,
            };
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let _ = std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o600));
            }
            tokio::spawn(background_loop(gw.clone()));

            let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
            let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;

            loop {
                tokio::select! {
                    acc = listener.accept() => {
                        match acc {
                            Ok((stream, _)) => {
                                let gw_c = gw.clone();
                                let (r, w) = stream.into_split();
                                tokio::spawn(async move {
                                    let _ = handle_connection(gw_c, r, w).await;
                                });
                            }
                            Err(e) => {
                                eprintln!("hc serve accept error: {e}");
                            }
                        }
                    }
                    Some(acc) = async {
                        match &tcp_listener {
                            Some(tl) => Some(tl.accept().await),
                            None => None,
                        }
                    } => {
                        match acc {
                            Ok((stream, _)) => {
                                let _ = stream.set_nodelay(true);
                                let gw_c = gw.clone();
                                let (r, w) = stream.into_split();
                                tokio::spawn(async move {
                                    let _ = handle_connection(gw_c, r, w).await;
                                });
                            }
                            Err(e) => {
                                eprintln!("hc serve tcp accept error: {e}");
                            }
                        }
                    }
                    _ = sigterm.recv() => break,
                    _ = sigint.recv() => break,
                }
            }
            Ok::<(), anyhow::Error>(())
        });

        let _ = std::fs::remove_file(&paths.socket_path);
        drop(lock_file);
        res
    }

    async fn handle_connection<R, W>(gw: SharedGateway, read_half: R, mut write_half: W) -> std::io::Result<()>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let mut reader = BufReader::new(read_half);
        let ctx = Arc::new(ConnContext {
            framed: false,
            next_channel: Arc::new(AtomicU32::new(1)),
            frame_out_tx: None,
            channels: Arc::new(Mutex::new(BTreeMap::new())),
            plain_event_filter: Arc::new(Mutex::new(None)),
        });

        let mut line = String::new();
        loop {
            line.clear();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                return Ok(());
            }
            if line.trim().is_empty() {
                continue;
            }
            let (reply_bytes, switch_framed) = handle_rpc_bytes(&gw, &ctx, line.as_bytes()).await;
            if let Some(mut bytes) = reply_bytes {
                bytes.push(b'\n');
                write_half.write_all(&bytes).await?;
                write_half.flush().await?;
            }
            let plain_filter = ctx.plain_event_filter.lock().unwrap_or_else(|p| p.into_inner()).take();
            if let Some(filter) = plain_filter {
                return run_plain_event_stream(gw, filter, reader, write_half).await;
            }
            if switch_framed {
                break;
            }
        }

        // 切换到 Framed 模式：把 BufReader 里尚未消费的预读字节转交给 FramedDecoder
        let leftover = reader.buffer().to_vec();
        let mut raw_read = reader.into_inner();
        let mut dec = FramedDecoder::with_leftover(leftover);

        let (frame_out_tx, mut frame_out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let framed_ctx = Arc::new(ConnContext {
            framed: true,
            next_channel: ctx.next_channel.clone(),
            frame_out_tx: Some(frame_out_tx.clone()),
            channels: ctx.channels.clone(),
            plain_event_filter: ctx.plain_event_filter.clone(),
        });

        let writer_task = tokio::spawn(async move {
            while let Some(frame_bytes) = frame_out_rx.recv().await {
                if write_half.write_all(&frame_bytes).await.is_err() {
                    break;
                }
            }
        });

        let mut read_buf = [0u8; 16 * 1024];
        'read_loop: loop {
            while let Some(frame) = match dec.next_frame() {
                Ok(f) => f,
                Err(_) => break 'read_loop,
            } {
                if frame.channel == 0 {
                    if frame.kind == wkind::DATA {
                        let gw_c = gw.clone();
                        let ctx_c = framed_ctx.clone();
                        let out_tx = frame_out_tx.clone();
                        let payload = frame.payload;
                        tokio::spawn(async move {
                            // 忽略对端发来的无 method 响应或 $/cancelRequest 通知
                            if let Ok(v) = serde_json::from_slice::<Value>(&payload) {
                                if v.get("method").is_none() || v["method"] == "$/cancelRequest" {
                                    return;
                                }
                            }
                            let (reply, _) = handle_rpc_bytes(&gw_c, &ctx_c, &payload).await;
                            if let Some(bytes) = reply {
                                let _ = out_tx.send(Frame::control_json(&bytes));
                            }
                        });
                    }
                } else {
                    let ch_tx = framed_ctx
                        .channels
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .get(&frame.channel)
                        .cloned();
                    if let Some(tx) = ch_tx
                        && let Some(vf) = decode_viewer_frame(frame.kind, frame.payload)
                    {
                        let is_close = matches!(vf, crate::hc::attach::ViewerFrame::Close);
                        let _ = tx.send(vf);
                        if is_close {
                            framed_ctx
                                .channels
                                .lock()
                                .unwrap_or_else(|p| p.into_inner())
                                .remove(&frame.channel);
                        }
                    }
                }
            }

            let n = raw_read.read(&mut read_buf).await?;
            if n == 0 {
                break;
            }
            dec.push(&read_buf[..n]);
        }

        // 断开时清理所有通道
        framed_ctx.channels.lock().unwrap_or_else(|p| p.into_inner()).clear();
        writer_task.abort();
        Ok(())
    }

    async fn handle_rpc_bytes(
        gw: &SharedGateway,
        ctx: &ConnContext,
        raw: &[u8],
    ) -> (Option<Vec<u8>>, bool) {
        let val: Value = match serde_json::from_slice(raw) {
            Ok(v) => v,
            Err(e) => {
                return (
                    Some(err_response(&Value::Null, RpcError::parse_error(e.to_string()))),
                    false,
                );
            }
        };
        let has_id = val.get("id").is_some();
        let req: RpcRequest = match serde_json::from_value(val.clone()) {
            Ok(r) => r,
            Err(_) => {
                let id = val.get("id").cloned().unwrap_or(Value::Null);
                return (
                    Some(err_response(&id, RpcError::invalid_request("missing method"))),
                    false,
                );
            }
        };
        let is_hello = req.method == "hello";
        let res = dispatch(gw, ctx, &req.method, req.params).await;
        let switch_framed = is_hello && res.as_ref().is_ok_and(|v| v["framed"] == true);
        if !has_id {
            return (None, switch_framed);
        }
        let bytes = match res {
            Ok(v) => ok_response(&req.id, v),
            Err(e) => err_response(&req.id, e),
        };
        (Some(bytes), switch_framed)
    }

    async fn run_plain_event_stream<R, W>(
        gw: SharedGateway,
        filter: EventFilter,
        mut reader: BufReader<R>,
        mut writer: W,
    ) -> std::io::Result<()>
    where
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let mut ev_rx = {
            let mut st = gw.lock().unwrap_or_else(|p| p.into_inner());
            st.subscriber_count += 1;
            st.event_tx.subscribe()
        };
        let heartbeat_ms = std::env::var("HC_HEARTBEAT_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(15_000);
        let mut hb_tick = tokio::time::interval(Duration::from_millis(heartbeat_ms.max(50)));
        hb_tick.tick().await;
        let mut ignored = String::new();

        let res = loop {
            let line = tokio::select! {
                ev = ev_rx.recv() => {
                    match ev {
                        Ok(ev) => {
                            let now_match = filter.matches(&ev.record);
                            let before_match = ev.was.as_ref().is_some_and(|w| filter.matches(w));
                            if !now_match && !before_match {
                                continue;
                            }
                            let mut params = ev.params;
                            if now_match && ev.was.is_some() && !before_match {
                                params["data"]["record"] = ev.record;
                            }
                            format!("{}\n", json!({
                                "jsonrpc": "2.0",
                                "method": "event",
                                "params": params,
                            }))
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            let snap = gw.lock().unwrap_or_else(|p| p.into_inner()).snapshot(&filter);
                            format!("{}\n", json!({
                                "jsonrpc": "2.0",
                                "method": "missed",
                                "params": snap,
                            }))
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break Ok(()),
                    }
                }
                _ = hb_tick.tick() => {
                    let seq = gw.lock().unwrap_or_else(|p| p.into_inner()).seq;
                    format!("{}\n", json!({
                        "jsonrpc": "2.0",
                        "method": "heartbeat",
                        "params": { "seq": seq },
                    }))
                }
                n = reader.read_line(&mut ignored) => {
                    ignored.clear();
                    match n {
                        Ok(0) | Err(_) => break Ok(()),
                        Ok(_) => continue,
                    }
                }
            };
            if let Err(e) = writer.write_all(line.as_bytes()).await {
                break Err(e);
            }
            let _ = writer.flush().await;
        };

        let mut st = gw.lock().unwrap_or_else(|p| p.into_inner());
        st.subscriber_count = st.subscriber_count.saturating_sub(1);
        res
    }
}
