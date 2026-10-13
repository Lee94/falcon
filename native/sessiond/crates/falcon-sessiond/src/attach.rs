//! 给人用的子命令：`attach`（逃生通道：falcon 不可用时 ssh 上去接回自己的会话）、`kill`、`history`。

use std::io::{Read as _, Write as _};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use falcon_proto::sessiond::{ClientMsg, ExportFormat, Hello, HolderMsg, PROTO_MAX, PROTO_MIN, Resize, Role, Welcome, read_frame};

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
    out.write_all(b"\x1b[H\x1b[2J\x1b[3J")?;
    out.flush()?;

    // 键盘 → 会话（遇到断开键就停）
    let detached = Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let send = send.clone();
        let detached = detached.clone();
        let key = detach_key();
        std::thread::spawn(move || {
            let mut stdin = std::io::stdin().lock();
            let mut buf = [0u8; 4096];
            loop {
                let n = match stdin.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                let chunk = &buf[..n];
                if let Some(k) = key
                    && let Some(i) = chunk.iter().position(|&b| b == k)
                {
                    if i > 0 {
                        send(ClientMsg::Input(chunk[..i].to_vec()));
                    }
                    detached.store(true, std::sync::atomic::Ordering::SeqCst);
                    break;
                }
                if !send(ClientMsg::Input(chunk.to_vec())) {
                    break;
                }
            }
            // 叫醒主循环：关掉写方向，holder 随后关闭连接
            send(ClientMsg::Ping);
        });
    }
    // 终端尺寸：轮询比装 SIGWINCH 处理器简单，Windows 也一样
    {
        let send = send.clone();
        let detached = detached.clone();
        std::thread::spawn(move || {
            let mut last = (cols, rows);
            while !detached.load(std::sync::atomic::Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(250));
                if let Some(now) = term::size()
                    && now != last
                {
                    last = now;
                    if !send(ClientMsg::Resize(Resize { cols: now.0, rows: now.1 })) {
                        break;
                    }
                }
            }
        });
    }

    let mut synced = false;
    let mut ended: Option<String> = None;
    loop {
        if detached.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }
        let Ok(Some((k, p))) = read_frame(&mut r) else {
            ended = Some("会话关闭了连接".into());
            break;
        };
        let Ok(msg) = HolderMsg::decode(k, p) else { continue };
        match msg {
            HolderMsg::Synced { snapshot, continuation, .. } if !synced => {
                synced = true;
                out.write_all(&snapshot)?;
                out.write_all(&continuation)?;
                out.flush()?;
            }
            // Synced 之前的输出都已含在快照里
            HolderMsg::Output { bytes, .. } if synced => {
                out.write_all(&bytes)?;
                out.flush()?;
            }
            HolderMsg::Pong if detached.load(std::sync::atomic::Ordering::SeqCst) => break,
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
    // 把会话可能开着的模式关掉，别把用户的终端留在鼠标上报 / 备用屏里
    out.write_all(b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1004l\x1b[?2004l\x1b[?1049l\x1b[?25h\x1b[0m\r\n")?;
    out.flush()?;
    drop(raw);
    match ended {
        Some(why) => eprintln!("[falcon-sessiond] {why}"),
        None => eprintln!("[falcon-sessiond] 已断开，会话 {sid} 照常运行（falcon-sessiond attach {sid} 接回）"),
    }
    Ok(0)
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
    // 连不上：按 meta 里的 pid 动手
    let Some(meta) = layout.read_meta(sid) else {
        if layout.read_exit(sid).is_some() {
            return Ok(());
        }
        bail!("没有会话 {sid}");
    };
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
