//! 给人用的子命令：`attach`（逃生通道：falcon 不可用时 ssh 上去接回自己的会话）、`kill`、`history`。

use std::io::{Read as _, Write as _};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use falcon_proto::sessiond::{ClientMsg, ExportFormat, Hello, HolderMsg, PROTO_MAX, PROTO_MIN, Resize, Role, Welcome, read_frame};
use falcon_proto::terminal_replies::ReplyFilter;

use crate::ipc;
use crate::layout::Layout;

/// 连上并握手
fn handshake(layout: &Layout, sid: &str, role: Role) -> Result<(ipc::Reader, ipc::Writer, Welcome)> {
    let endpoint = layout.read_meta(sid).map(|m| m.endpoint).unwrap_or_else(|| layout.endpoint(sid));
    let stream = ipc::connect(&endpoint).map_err(|e| {
        if layout.read_exit(sid).is_some() {
            anyhow::anyhow!("会话 {sid} 已经结束了")
        } else if layout.read_meta(sid).is_none() {
            anyhow::anyhow!("没有会话 {sid}（用 list 看看）")
        } else {
            anyhow::anyhow!("连不上会话 {sid}：{e}")
        }
    })?;
    let (mut r, mut w) = stream.split()?;
    let hello = Hello { proto_min: PROTO_MIN, proto_max: PROTO_MAX, role, client: format!("falcon-sessiond/{}", falcon_proto::sessiond::FALCON_VERSION) };
    w.write_all(&ClientMsg::Hello(hello).encode())?;
    w.flush()?;
    loop {
        let (k, p) = read_frame(&mut r)?.context("会话关闭了连接")?;
        match HolderMsg::decode(k, p)? {
            HolderMsg::Welcome(wl) => return Ok((r, w, wl)),
            HolderMsg::Error { message } => bail!("{message}"),
            _ => {}
        }
    }
}

/// 断开键：缺省 Ctrl+\（0x1c）。`FALCON_SESSIOND_DETACH_KEY=none` 关掉，`^X` 形式改成别的
fn detach_key() -> Option<u8> {
    match std::env::var("FALCON_SESSIOND_DETACH_KEY").ok().as_deref() {
        Some("none") => None,
        Some(s) if s.len() == 2 && s.starts_with('^') => {
            let c = s.as_bytes()[1].to_ascii_uppercase();
            (b'@'..=b'_').contains(&c).then_some(c - b'@')
        }
        _ => Some(0x1c),
    }
}

pub fn run(layout: &Layout, sid: &str, force: bool) -> Result<i32> {
    if !force
        && let Ok(inside) = std::env::var("FALCON_SESSIOND_ID")
        && !inside.is_empty()
    {
        bail!("你正在 falcon 会话 {inside} 里，在会话里再 attach 会套娃；确定要这么做就加 --force");
    }
    let native = std::env::var("TERM_PROGRAM").is_ok_and(|program| program.eq_ignore_ascii_case("tern"));
    let filter = native.then(|| Arc::new(Mutex::new(ReplyFilter::default())));
    let _signals = term::DetachSignals::install(native)?;
    let (mut r, w, welcome) = handshake(layout, sid, Role::Attach)?;
    let w = Arc::new(Mutex::new(w));
    let send = {
        let w = w.clone();
        move |msg: ClientMsg| {
            let mut w = w.lock().unwrap_or_else(|e| e.into_inner());
            w.write_all(&msg.encode()).and_then(|()| w.flush()).is_ok()
        }
    };
    let (cols, rows) = term::size().unwrap_or((welcome.cols, welcome.rows));
    send(ClientMsg::Resize(Resize { cols, rows }));
    send(ClientMsg::Sync { req: 1 });

    let raw = term::RawMode::enable()?;
    let mut out = std::io::stdout().lock();
    // 清屏连同滚动历史：接下来的快照按"一个全新的终端"来画
    write_output(&mut out, &filter, b"\x1b[H\x1b[2J\x1b[3J")?;
    out.flush()?;

    let detached = Arc::new(AtomicBool::new(false));
    // 键盘与 TSP 回包保留；普通终端查询由 holder 的 VT 回答，不能再送一次。
    {
        let send = send.clone();
        let detached = detached.clone();
        let filter = filter.clone();
        let key = detach_key();
        std::thread::spawn(move || {
            let mut stdin = std::io::stdin().lock();
            let mut buf = [0u8; 4096];
            while !detached.load(Ordering::SeqCst) {
                let n = match stdin.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                if detached.load(Ordering::SeqCst) {
                    break;
                }
                let chunk = &buf[..n];
                if let Some(k) = key
                    && let Some(i) = chunk.iter().position(|&b| b == k)
                {
                    forward_input(&filter, &chunk[..i], true, &send);
                    break;
                }
                if !forward_input(&filter, chunk, false, &send) {
                    break;
                }
            }
            if !detached.load(Ordering::SeqCst) {
                forward_input(&filter, &[], true, &send);
            }
            // EOF 也是 detach；Ping 的 Pong 叫醒阻塞在 read_frame 的主循环。
            detached.store(true, Ordering::SeqCst);
            send(ClientMsg::Ping);
        });
    }
    // 同一个定时线程负责 Escape 的 30ms 截止、信号唤醒与 250ms 尺寸节流。
    let timer = {
        let send = send.clone();
        let detached = detached.clone();
        let filter = filter.clone();
        std::thread::spawn(move || {
            let mut last = (cols, rows);
            let mut size_checked = Instant::now();
            loop {
                std::thread::sleep(Duration::from_millis(if native { 30 } else { 250 }));
                if native && term::detach_requested() {
                    detached.store(true, Ordering::SeqCst);
                }
                if detached.load(Ordering::SeqCst) {
                    send(ClientMsg::Ping);
                    break;
                }
                if !forward_input(&filter, &[], true, &send) {
                    detached.store(true, Ordering::SeqCst);
                    send(ClientMsg::Ping);
                    break;
                }
                if size_checked.elapsed() >= Duration::from_millis(250) {
                    size_checked = Instant::now();
                    if let Some(now) = term::size()
                        && now != last
                    {
                        last = now;
                        if !send(ClientMsg::Resize(Resize { cols: now.0, rows: now.1 })) {
                            detached.store(true, Ordering::SeqCst);
                            send(ClientMsg::Ping);
                            break;
                        }
                    }
                }
            }
        })
    };

    let refresh_result = Arc::new(Mutex::new(None));
    let mut refresh_thread = None;
    let mut synced = false;
    let mut ended: Option<String> = None;
    let display_result = (|| -> Result<()> {
        loop {
            if detached.load(Ordering::SeqCst) || (native && term::detach_requested()) {
                break;
            }
            report_refresh_result(&refresh_result);
            let Ok(Some((k, p))) = read_frame(&mut r) else {
                ended = Some("会话关闭了连接".into());
                break;
            };
            // 输入线程或信号已经请求离开时，不再把排队的 Output 写给 Tern。
            if detached.load(Ordering::SeqCst) || (native && term::detach_requested()) {
                break;
            }
            let Ok(msg) = HolderMsg::decode(k, p) else { continue };
            match msg {
                HolderMsg::Synced { snapshot, continuation, .. } if !synced => {
                    synced = true;
                    write_output(&mut out, &filter, &snapshot)?;
                    write_output(&mut out, &filter, &continuation)?;
                    out.flush()?;
                    if native && !detached.load(Ordering::SeqCst) && !term::detach_requested()
                        && !falcon_proto::tsp::snapshot_parts_have_live_surface(&[&snapshot, &continuation])
                    {
                        let layout = layout.clone();
                        let sid = sid.to_owned();
                        let result = refresh_result.clone();
                        let send = send.clone();
                        // 重探会等 TSP hello；主循环必须继续转发输出和输入才能完成握手。
                        refresh_thread = Some(std::thread::spawn(move || {
                            *result.lock().unwrap_or_else(|e| e.into_inner()) = Some(crate::reprobe::run(&layout, &sid));
                            send(ClientMsg::Ping);
                        }));
                    }
                }
                // Synced 之前的输出都已含在快照里。
                HolderMsg::Output { bytes, .. } if synced => {
                    write_output(&mut out, &filter, &bytes)?;
                    out.flush()?;
                }
                HolderMsg::Exit(s) => {
                    ended = Some(match (s.code, s.signal) {
                        (Some(c), _) => format!("会话结束了（退出码 {c}）"),
                        (None, Some(sig)) => format!("会话结束了（信号 {sig}）"),
                        _ => "会话结束了".into(),
                    });
                    break;
                }
                _ => {}
            }
        }
        Ok(())
    })();
    detached.store(true, Ordering::SeqCst);
    let _ = timer.join();
    // 先停止输出并恢复外层终端，再让仍在运行的同一个 OMP 回到 ANSI。
    let reset_result = out.write_all(b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1004l\x1b[?2004l\x1b[?1049l\x1b[?25h\x1b[0m\r\n")
        .and_then(|()| out.flush());
    drop(out);
    drop(raw);
    if let Some(refresh) = refresh_thread
        && refresh.join().is_err()
    {
        eprintln!("[falcon-sessiond] 切入 Tern 时的 OMP 重探线程异常退出");
    }
    report_refresh_result(&refresh_result);
    if native
        && let Err(error) = crate::reprobe::run(layout, sid)
    {
        eprintln!("[falcon-sessiond] OMP 切回 ANSI 失败：{error:#}");
    }
    display_result?;
    reset_result?;
    match ended {
        Some(why) => eprintln!("[falcon-sessiond] {why}"),
        None => eprintln!("[falcon-sessiond] 已断开，会话 {sid} 照常运行（falcon-sessiond attach {sid} 接回）"),
    }
    Ok(0)
}

fn write_output(out: &mut impl std::io::Write, filter: &Option<Arc<Mutex<ReplyFilter>>>, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(filter) = filter {
        filter.lock().unwrap_or_else(|e| e.into_inner()).observe_output(bytes);
    }
    out.write_all(bytes)
}

fn forward_input(filter: &Option<Arc<Mutex<ReplyFilter>>>, bytes: &[u8], flush: bool, send: &impl Fn(ClientMsg) -> bool) -> bool {
    if let Some(filter) = filter {
        // 保持 push/flush 与发送的顺序一致，避免定时线程把后半段抢先发出去。
        let mut filter = filter.lock().unwrap_or_else(|e| e.into_inner());
        let output = if bytes.is_empty() { Vec::new() } else { filter.push(bytes) };
        if !output.is_empty() && !send(ClientMsg::Input(output)) {
            return false;
        }
        if flush && filter.has_pending() {
            let pending = filter.flush_pending();
            if !pending.is_empty() {
                return send(ClientMsg::Input(pending));
            }
        }
        true
    } else {
        bytes.is_empty() || send(ClientMsg::Input(bytes.to_vec()))
    }
}

fn report_refresh_result(result: &Mutex<Option<Result<bool>>>) {
    if let Some(Err(error)) = result.lock().unwrap_or_else(|e| e.into_inner()).take() {
        eprintln!("[falcon-sessiond] OMP 切入 Tern 失败：{error:#}");
    }
}

pub fn kill(layout: &Layout, sid: &str) -> Result<()> {
    if let Ok((mut r, mut w, _)) = handshake(layout, sid, Role::Attach) {
        w.write_all(&ClientMsg::Terminate.encode())?;
        w.flush()?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            match read_frame(&mut r) {
                Ok(Some((k, p))) => {
                    if matches!(HolderMsg::decode(k, p), Ok(HolderMsg::Exit(_))) {
                        return Ok(());
                    }
                }
                _ => return Ok(()),
            }
        }
        bail!("会话 5 秒内没有结束");
    }
    // 连不上：按 meta 里的 pid 动手（shell_pid == 0 是 falcon-server 远端代理，绝不能对 pgid 0 发信号或杀 server）
    let Some(meta) = layout.read_meta(sid) else {
        if layout.read_exit(sid).is_some() {
            return Ok(());
        }
        bail!("没有会话 {sid}");
    };
    if meta.shell_pid != 0 {
        crate::proc::kill_session(meta.shell_pid, false);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && crate::proc::alive(meta.shell_pid) {
            std::thread::sleep(Duration::from_millis(50));
        }
        if crate::proc::alive(meta.shell_pid) {
            crate::proc::kill_session(meta.shell_pid, true);
        }
        if crate::proc::is_sessiond(meta.pid) {
            crate::proc::kill(meta.pid);
        }
    }
    let _ = std::fs::remove_file(layout.meta_path(sid));
    #[cfg(unix)]
    let _ = std::fs::remove_file(&meta.endpoint);
    Ok(())
}

pub fn history(layout: &Layout, sid: &str, format: ExportFormat) -> Result<()> {
    let (mut r, mut w, _) = handshake(layout, sid, Role::Attach)?;
    w.write_all(&ClientMsg::Export { req: 1, format }.encode())?;
    w.flush()?;
    loop {
        let (k, p) = read_frame(&mut r)?.context("会话关闭了连接")?;
        if let HolderMsg::Exported { bytes, .. } = HolderMsg::decode(k, p)? {
            if let Some(error) = bytes.strip_prefix(falcon_proto::sessiond::EXPORT_ERROR_PREFIX) {
                bail!("原生界面导出失败：{}", String::from_utf8_lossy(error));
            }
            let mut out = std::io::stdout().lock();
            out.write_all(&bytes)?;
            out.flush()?;
            return Ok(());
        }
    }
}

/// 终端的 raw 模式与尺寸
mod term {
    #[cfg(unix)]
    static DETACH_SIGNAL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    #[cfg(unix)]
    extern "C" fn request_detach(_: libc::c_int) {
        DETACH_SIGNAL.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn detach_requested() -> bool {
        #[cfg(unix)]
        { DETACH_SIGNAL.load(std::sync::atomic::Ordering::SeqCst) }
        #[cfg(not(unix))]
        { false }
    }

    pub struct DetachSignals {
        #[cfg(unix)]
        saved: Option<[libc::sigaction; 2]>,
    }

    impl DetachSignals {
        pub fn install(native: bool) -> anyhow::Result<Self> {
            #[cfg(unix)]
            {
                if !native {
                    return Ok(Self { saved: None });
                }
                DETACH_SIGNAL.store(false, std::sync::atomic::Ordering::SeqCst);
                unsafe {
                    let mut action: libc::sigaction = std::mem::zeroed();
                    action.sa_sigaction = request_detach as *const () as libc::sighandler_t;
                    action.sa_flags = libc::SA_RESTART;
                    libc::sigemptyset(&mut action.sa_mask);
                    let mut saved: [libc::sigaction; 2] = std::mem::zeroed();
                    if libc::sigaction(libc::SIGHUP, &action, &mut saved[0]) != 0 {
                        anyhow::bail!("安装 SIGHUP 处理器失败：{}", std::io::Error::last_os_error());
                    }
                    if libc::sigaction(libc::SIGTERM, &action, &mut saved[1]) != 0 {
                        let error = std::io::Error::last_os_error();
                        libc::sigaction(libc::SIGHUP, &saved[0], std::ptr::null_mut());
                        anyhow::bail!("安装 SIGTERM 处理器失败：{error}");
                    }
                    Ok(Self { saved: Some(saved) })
                }
            }
            #[cfg(not(unix))]
            {
                let _ = native;
                Ok(Self {})
            }
        }
    }

    #[cfg(unix)]
    impl Drop for DetachSignals {
        fn drop(&mut self) {
            if let Some(saved) = &self.saved {
                unsafe {
                    libc::sigaction(libc::SIGHUP, &saved[0], std::ptr::null_mut());
                    libc::sigaction(libc::SIGTERM, &saved[1], std::ptr::null_mut());
                }
            }
        }
    }

    #[cfg(unix)]
    pub fn size() -> Option<(u16, u16)> {
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        let r = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) };
        (r == 0 && ws.ws_col > 0 && ws.ws_row > 0).then_some((ws.ws_col, ws.ws_row))
    }

    #[cfg(unix)]
    pub struct RawMode(Option<libc::termios>);

    #[cfg(unix)]
    impl RawMode {
        pub fn enable() -> anyhow::Result<Self> {
            unsafe {
                if libc::isatty(libc::STDIN_FILENO) == 0 {
                    return Ok(RawMode(None));
                }
                let mut t: libc::termios = std::mem::zeroed();
                if libc::tcgetattr(libc::STDIN_FILENO, &mut t) != 0 {
                    anyhow::bail!("读不到终端设置：{}", std::io::Error::last_os_error());
                }
                let saved = t;
                libc::cfmakeraw(&mut t);
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &t);
                Ok(RawMode(Some(saved)))
            }
        }
    }

    #[cfg(unix)]
    impl Drop for RawMode {
        fn drop(&mut self) {
            if let Some(t) = &self.0 {
                unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, t) };
            }
        }
    }

    #[cfg(windows)]
    use windows_sys::Win32::System::Console::{
        CONSOLE_MODE, CONSOLE_SCREEN_BUFFER_INFO, DISABLE_NEWLINE_AUTO_RETURN, ENABLE_PROCESSED_OUTPUT,
        ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, GetConsoleScreenBufferInfo, GetStdHandle,
        STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleCP, SetConsoleMode, SetConsoleOutputCP,
    };

    #[cfg(windows)]
    pub fn size() -> Option<(u16, u16)> {
        unsafe {
            let mut info: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
            if GetConsoleScreenBufferInfo(GetStdHandle(STD_OUTPUT_HANDLE), &mut info) == 0 {
                return None;
            }
            let w = info.srWindow.Right - info.srWindow.Left + 1;
            let h = info.srWindow.Bottom - info.srWindow.Top + 1;
            (w > 0 && h > 0).then_some((w as u16, h as u16))
        }
    }

    #[cfg(windows)]
    pub struct RawMode {
        input: CONSOLE_MODE,
        output: CONSOLE_MODE,
    }

    #[cfg(windows)]
    impl RawMode {
        pub fn enable() -> anyhow::Result<Self> {
            unsafe {
                let hin = GetStdHandle(STD_INPUT_HANDLE);
                let hout = GetStdHandle(STD_OUTPUT_HANDLE);
                let mut input: CONSOLE_MODE = 0;
                let mut output: CONSOLE_MODE = 0;
                GetConsoleMode(hin, &mut input);
                GetConsoleMode(hout, &mut output);
                // 输入：只留 VT 输入（不回显、不按行、不吃 Ctrl+C）；输出：开 VT 解析
                SetConsoleMode(hin, ENABLE_VIRTUAL_TERMINAL_INPUT);
                SetConsoleMode(hout, ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING | DISABLE_NEWLINE_AUTO_RETURN);
                SetConsoleCP(65001);
                SetConsoleOutputCP(65001);
                Ok(RawMode { input, output })
            }
        }
    }

    #[cfg(windows)]
    impl Drop for RawMode {
        fn drop(&mut self) {
            unsafe {
                SetConsoleMode(GetStdHandle(STD_INPUT_HANDLE), self.input);
                SetConsoleMode(GetStdHandle(STD_OUTPUT_HANDLE), self.output);
            }
        }
    }
}
