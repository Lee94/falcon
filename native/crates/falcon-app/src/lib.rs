//! `falcon-app`：基于 Native Shell (NSH) `0.5.1` 的六端统一宿主启动器（ADR 0028）。
//!
//! 同一套纯 Rust [`falcon_nsh::FalconApp`] 组件树与 `falcon-core` / `falcon-term` / `falcon-theme` /
//! `falcon-client` 下层引擎在此按目标平台装配：
//! - **macOS**：`nsh-platform-macos`
//! - **Windows**：`nsh-platform-windows`
//! - **Linux Desktop**：`nsh-platform-linux`（GTK 4 + Pango，由 `linux-desktop` feature 开启）
//! - **Android**：`nsh-platform-android`（`cdylib` JNI 导出 `ui_sets!` + `root!` + `main!`）
//! - **Web**：`nsh-platform-web`（`wasm32` 纯 Rust 导出 `rust_app!`，主路径零 JS 虚拟机开销）
//! - **TUI**：`nsh-platform-tui`（`--tui` / `FALCON_TUI=1`，或未开启 `linux-desktop` 时的终端工作台）

#[cfg(not(target_arch = "wasm32"))]
pub mod host;

use falcon_nsh::{FalconLaunchOptions, falcon_root_with_options};
use nsh_core::element::{Registry, Root};

/// 初始化 `falcon-platform` 平台实例并返回 Falcon 所需的 NSH 组件集（`nsh/base` + `falcon/ui`）。
pub fn init_ui_sets() -> Vec<Registry> {
    #[cfg(target_os = "android")]
    host::install_once();
    let _ = falcon_platform::shared();
    vec![nsh_base::registry(), falcon_nsh::registry()]
}

/// 按给定启动配置初始化平台层并创建 Falcon NSH `Root`。
pub fn build_root(options: FalconLaunchOptions) -> Root {
    #[cfg(not(target_arch = "wasm32"))]
    if !options.offline_demo {
        host::install_once();
    }
    let _ = falcon_platform::shared();
    falcon_root_with_options(options)
}

// ── Android JNI 宿主导出 ───────────────────────────────────────────────
#[cfg(target_os = "android")]
nsh_platform_android::ui_sets!(init_ui_sets());

#[cfg(target_os = "android")]
nsh_platform_android::root!(build_root(FalconLaunchOptions::default()));

#[cfg(target_os = "android")]
nsh_platform_android::main!(build_root(FalconLaunchOptions::default()));

// ── Web (WASM) 纯 Rust 宿主导出 ────────────────────────────────────────
#[cfg(target_arch = "wasm32")]
nsh_platform_web::rust_app!(init_ui_sets(), build_root(FalconLaunchOptions::default()));

/// 解析后的命令行 / 环境变量启动参数。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CliArgs {
    /// 强制进入终端网格（TUI）模式。
    pub force_tui: bool,
    /// 启动离线演示工作台（不尝试连接本地服务端）。
    pub offline_demo: bool,
    /// 覆盖连接的服务端根地址（例如 `http://127.0.0.1:4923`）。
    pub server_url: Option<String>,
    /// 打印帮助后退出。
    pub show_help: bool,
}

impl CliArgs {
    /// 从迭代器与环境变量解析启动配置。
    pub fn parse_from<I, S>(args: I) -> anyhow::Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut out = Self {
            force_tui: env_truthy("FALCON_TUI"),
            offline_demo: env_truthy("FALCON_OFFLINE_DEMO"),
            server_url: std::env::var("FALCON_SERVER_URL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            show_help: false,
        };

        let mut iter = args.into_iter();
        while let Some(raw) = iter.next() {
            match raw.as_ref() {
                "-h" | "--help" => out.show_help = true,
                "--tui" => out.force_tui = true,
                "--offline-demo" => out.offline_demo = true,
                "--server" => {
                    let val = iter
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--server requires a <url> argument"))?;
                    out.server_url = Some(val.as_ref().to_string());
                }
                arg if arg.starts_with("--server=") => {
                    out.server_url = Some(arg["--server=".len()..].to_string());
                }
                _ => {}
            }
        }
        Ok(out)
    }

    pub fn into_launch_options(self) -> FalconLaunchOptions {
        FalconLaunchOptions {
            offline_demo: self.offline_demo,
            server_url: self.server_url,
            initial_theme: None,
        }
    }
}

fn env_truthy(name: &str) -> bool {
    std::env::var(name)
        .map(|v| matches!(v.trim(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

pub const CLI_USAGE: &str = "\
Falcon Unified Client (Native Shell 0.5.1)

Usage:
  falcon-app [OPTIONS]

Options:
  --tui               Launch in terminal UI (nsh-platform-tui) mode
  --server <URL>      Connect to a specific falcon-server URL (or set FALCON_SERVER_URL)
  --offline-demo      Start with deterministic offline workspace data
  -h, --help          Print this help message
";

/// 桌面与 TUI 统一命令行入口。
#[cfg(not(target_arch = "wasm32"))]
pub fn run_cli() -> anyhow::Result<()> {
    let cli = CliArgs::parse_from(std::env::args().skip(1))?;
    if cli.show_help {
        print!("{CLI_USAGE}");
        return Ok(());
    }

    host::install_once();

    let force_tui = cli.force_tui;
    let launch_opts = cli.into_launch_options();

    #[cfg(feature = "tui")]
    if force_tui {
        return tui_host::run_tui(launch_opts);
    }

    #[cfg(all(
        not(feature = "tui"),
        not(any(
            target_os = "macos",
            target_os = "windows",
            all(target_os = "linux", feature = "linux-desktop")
        ))
    ))]
    if force_tui {
        anyhow::bail!("TUI backend is not enabled in this build (enable feature `tui`)");
    }

    run_native_desktop_or_fallback(launch_opts)
}

#[cfg(target_arch = "wasm32")]
pub fn run_cli() -> anyhow::Result<()> {
    Ok(())
}

#[cfg(target_os = "macos")]
fn run_native_desktop_or_fallback(launch_opts: FalconLaunchOptions) -> anyhow::Result<()> {
    use nsh_platform_macos::{AppOptions, AppSource, run_app, set_ui_sets};
    set_ui_sets(init_ui_sets());
    let mut opts = AppOptions::default();
    opts.app_id = "com.falcon.app".into();
    opts.title = "Falcon".into();
    opts.size = (1360, 860);
    // macOS 关掉最后一扇窗口时保留引擎与菜单栏状态项（自动化运行除外）
    opts.keep_alive_on_close = std::env::var_os("FALCON_AUTOMATE").is_none();
    opts.limits = nsh_engine::Limits::standalone();
    let _ = run_app(AppSource::Rust(build_root(launch_opts)), opts);
    Ok(())
}

#[cfg(target_os = "windows")]
fn run_native_desktop_or_fallback(launch_opts: FalconLaunchOptions) -> anyhow::Result<()> {
    use nsh_platform_windows::{AppOptions, AppSource, run_app, set_ui_sets};
    set_ui_sets(init_ui_sets());
    let mut opts = AppOptions::default();
    opts.app_id = "com.falcon.app".into();
    opts.title = "Falcon".into();
    opts.size = (1360, 860);
    opts.schemes = vec!["falcon".into()];
    opts.limits = nsh_engine::Limits::standalone();
    let _ = run_app(AppSource::Rust(build_root(launch_opts)), opts);
    Ok(())
}

#[cfg(all(target_os = "linux", feature = "linux-desktop"))]
fn run_native_desktop_or_fallback(launch_opts: FalconLaunchOptions) -> anyhow::Result<()> {
    use nsh_platform_linux::{AppOptions, AppSource, run_app, set_ui_sets};
    set_ui_sets(init_ui_sets());
    let mut opts = AppOptions::default();
    opts.app_id = "com.falcon.app".into();
    opts.title = "Falcon".into();
    opts.size = (1360, 860);
    opts.schemes = vec!["falcon".into()];
    opts.limits = nsh_engine::Limits::standalone();
    let _ = run_app(AppSource::Rust(build_root(launch_opts)), opts);
    Ok(())
}

#[cfg(all(
    not(target_arch = "wasm32"),
    not(target_os = "macos"),
    not(target_os = "windows"),
    not(all(target_os = "linux", feature = "linux-desktop"))
))]
fn run_native_desktop_or_fallback(launch_opts: FalconLaunchOptions) -> anyhow::Result<()> {
    #[cfg(feature = "tui")]
    {
        tui_host::run_tui(launch_opts)
    }
    #[cfg(not(feature = "tui"))]
    {
        let _ = launch_opts;
        anyhow::bail!(
            "No desktop or TUI backend enabled; build with `--features tui` or `--features linux-desktop`"
        )
    }
}

// ── TUI 宿主后端（nsh-platform-tui + 零额外依赖 ANSI 终端驱动） ────────────
#[cfg(all(feature = "tui", not(target_arch = "wasm32")))]
pub mod tui_host {
    use std::io::{self, Write};
    use std::time::Duration;

    use falcon_nsh::FalconLaunchOptions;
    use nsh_platform_tui::{
        AppOptions, AppView, ColorDepth, Input, Modifiers, MouseButton, MouseInput, MouseKind,
        RunEvent, TerminalCapabilities, run_app,
    };
    use ratatui_core::backend::{Backend, ClearType, WindowSize};
    use ratatui_core::buffer::Cell;
    use ratatui_core::layout::{Position, Size};
    use ratatui_core::style::{Color, Modifier};
    use ratatui_core::terminal::Terminal;

    use crate::{build_root, init_ui_sets};

    /// 在当前宿主终端启动 `nsh-platform-tui` 全屏交互工作台。
    pub fn run_tui(launch_opts: FalconLaunchOptions) -> anyhow::Result<()> {
        let mut opts = AppOptions::default();
        opts.ui_sets = init_ui_sets();
        opts.limits = nsh_engine::Limits::standalone();
        let mut cap = TerminalCapabilities::default();
        cap.colors = if std::env::var_os("NO_COLOR").is_some() {
            ColorDepth::Monochrome
        } else {
            ColorDepth::TrueColor
        };
        opts.capabilities = cap;

        let mut view = AppView::new(opts);
        view.load(build_root(launch_opts))?;

        let _raw_guard = RawTerminalGuard::enter()?;
        let mut transport = StdinTransport::new()?;
        let backend = AnsiBackend::new(io::BufWriter::with_capacity(64 * 1024, io::stdout()));
        let mut terminal = Terminal::new(backend)?;
        terminal.clear()?;

        run_app(&mut terminal, &mut view, |wait| transport.poll(wait))?;
        Ok(())
    }

    /// 兼容任意 `Write` 输出流的 ANSI 24-bit `ratatui_core::backend::Backend` 实现。
    pub struct AnsiBackend<W: Write> {
        writer: W,
        cursor: Position,
        fixed_size: Option<Size>,
    }

    impl<W: Write> AnsiBackend<W> {
        pub fn new(writer: W) -> Self {
            Self {
                writer,
                cursor: Position::new(0, 0),
                fixed_size: None,
            }
        }

        pub fn with_fixed_size(writer: W, width: u16, height: u16) -> Self {
            Self {
                writer,
                cursor: Position::new(0, 0),
                fixed_size: Some(Size::new(width, height)),
            }
        }

        pub fn inner(&self) -> &W {
            &self.writer
        }

        pub fn into_inner(self) -> W {
            self.writer
        }
    }

    impl<W: Write> Backend for AnsiBackend<W> {
        type Error = io::Error;

        fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
        where
            I: Iterator<Item = (u16, u16, &'a Cell)>,
        {
            let mut last_pos: Option<(u16, u16)> = None;
            let mut last_fg = Color::Reset;
            let mut last_bg = Color::Reset;
            let mut last_mod = Modifier::empty();

            write!(self.writer, "\x1b[0m")?;
            for (x, y, cell) in content {
                if last_pos != Some((x, y)) {
                    write!(self.writer, "\x1b[{};{}H", y.saturating_add(1), x.saturating_add(1))?;
                }
                if cell.modifier != last_mod {
                    write!(self.writer, "\x1b[0m")?;
                    last_fg = Color::Reset;
                    last_bg = Color::Reset;
                    write_modifiers(&mut self.writer, cell.modifier)?;
                    last_mod = cell.modifier;
                }
                if cell.fg != last_fg {
                    write_color(&mut self.writer, cell.fg, true)?;
                    last_fg = cell.fg;
                }
                if cell.bg != last_bg {
                    write_color(&mut self.writer, cell.bg, false)?;
                    last_bg = cell.bg;
                }
                let sym = cell.symbol();
                self.writer.write_all(if sym.is_empty() { b" " } else { sym.as_bytes() })?;
                last_pos = Some((x.saturating_add(1), y));
            }
            write!(self.writer, "\x1b[0m")
        }

        fn append_lines(&mut self, n: u16) -> io::Result<()> {
            for _ in 0..n {
                self.writer.write_all(b"\r\n")?;
            }
            Ok(())
        }

        fn hide_cursor(&mut self) -> io::Result<()> {
            self.writer.write_all(b"\x1b[?25l")
        }

        fn show_cursor(&mut self) -> io::Result<()> {
            self.writer.write_all(b"\x1b[?25h")
        }

        fn get_cursor_position(&mut self) -> io::Result<Position> {
            Ok(self.cursor)
        }

        fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
            let pos = position.into();
            self.cursor = pos;
            write!(
                self.writer,
                "\x1b[{};{}H",
                pos.y.saturating_add(1),
                pos.x.saturating_add(1)
            )
        }

        fn clear(&mut self) -> io::Result<()> {
            self.writer.write_all(b"\x1b[2J\x1b[H")
        }

        fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
            let seq: &[u8] = match clear_type {
                ClearType::All => b"\x1b[2J",
                ClearType::AfterCursor => b"\x1b[J",
                ClearType::BeforeCursor => b"\x1b[1J",
                ClearType::CurrentLine => b"\x1b[2K",
                ClearType::UntilNewLine => b"\x1b[K",
            };
            self.writer.write_all(seq)
        }

        fn size(&self) -> io::Result<Size> {
            if let Some(sz) = self.fixed_size {
                return Ok(sz);
            }
            Ok(query_terminal_size())
        }

        fn window_size(&mut self) -> io::Result<WindowSize> {
            let cols_rows = self.size()?;
            Ok(WindowSize {
                columns_rows: cols_rows,
                pixels: Size::new(cols_rows.width.saturating_mul(8), cols_rows.height.saturating_mul(16)),
            })
        }

        fn flush(&mut self) -> io::Result<()> {
            self.writer.flush()
        }
    }

    fn write_modifiers<W: Write>(w: &mut W, m: Modifier) -> io::Result<()> {
        if m.contains(Modifier::BOLD) {
            w.write_all(b"\x1b[1m")?;
        }
        if m.contains(Modifier::DIM) {
            w.write_all(b"\x1b[2m")?;
        }
        if m.contains(Modifier::ITALIC) {
            w.write_all(b"\x1b[3m")?;
        }
        if m.contains(Modifier::UNDERLINED) {
            w.write_all(b"\x1b[4m")?;
        }
        if m.contains(Modifier::SLOW_BLINK) || m.contains(Modifier::RAPID_BLINK) {
            w.write_all(b"\x1b[5m")?;
        }
        if m.contains(Modifier::REVERSED) {
            w.write_all(b"\x1b[7m")?;
        }
        if m.contains(Modifier::HIDDEN) {
            w.write_all(b"\x1b[8m")?;
        }
        if m.contains(Modifier::CROSSED_OUT) {
            w.write_all(b"\x1b[9m")?;
        }
        Ok(())
    }

    fn write_color<W: Write>(w: &mut W, c: Color, fg: bool) -> io::Result<()> {
        match c {
            Color::Reset => w.write_all(if fg { b"\x1b[39m" } else { b"\x1b[49m" }),
            Color::Black => w.write_all(if fg { b"\x1b[30m" } else { b"\x1b[40m" }),
            Color::Red => w.write_all(if fg { b"\x1b[31m" } else { b"\x1b[41m" }),
            Color::Green => w.write_all(if fg { b"\x1b[32m" } else { b"\x1b[42m" }),
            Color::Yellow => w.write_all(if fg { b"\x1b[33m" } else { b"\x1b[43m" }),
            Color::Blue => w.write_all(if fg { b"\x1b[34m" } else { b"\x1b[44m" }),
            Color::Magenta => w.write_all(if fg { b"\x1b[35m" } else { b"\x1b[45m" }),
            Color::Cyan => w.write_all(if fg { b"\x1b[36m" } else { b"\x1b[46m" }),
            Color::Gray => w.write_all(if fg { b"\x1b[37m" } else { b"\x1b[47m" }),
            Color::DarkGray => w.write_all(if fg { b"\x1b[90m" } else { b"\x1b[100m" }),
            Color::LightRed => w.write_all(if fg { b"\x1b[91m" } else { b"\x1b[101m" }),
            Color::LightGreen => w.write_all(if fg { b"\x1b[92m" } else { b"\x1b[102m" }),
            Color::LightYellow => w.write_all(if fg { b"\x1b[93m" } else { b"\x1b[103m" }),
            Color::LightBlue => w.write_all(if fg { b"\x1b[94m" } else { b"\x1b[104m" }),
            Color::LightMagenta => w.write_all(if fg { b"\x1b[95m" } else { b"\x1b[105m" }),
            Color::LightCyan => w.write_all(if fg { b"\x1b[96m" } else { b"\x1b[106m" }),
            Color::White => w.write_all(if fg { b"\x1b[97m" } else { b"\x1b[107m" }),
            Color::Indexed(idx) => {
                if fg {
                    write!(w, "\x1b[38;5;{idx}m")
                } else {
                    write!(w, "\x1b[48;5;{idx}m")
                }
            }
            Color::Rgb(r, g, b) => {
                if fg {
                    write!(w, "\x1b[38;2;{r};{g};{b}m")
                } else {
                    write!(w, "\x1b[48;2;{r};{g};{b}m")
                }
            }
        }
    }

    fn query_terminal_size() -> Size {
        #[cfg(unix)]
        unsafe {
            let mut ws: libc::winsize = std::mem::zeroed();
            if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) == 0
                && ws.ws_col >= 20
                && ws.ws_row >= 6
            {
                return Size::new(ws.ws_col, ws.ws_row);
            }
        }
        let cols = std::env::var("COLUMNS")
            .ok()
            .and_then(|v| v.parse::<u16>().ok())
            .filter(|&c| c >= 20)
            .unwrap_or(120);
        let rows = std::env::var("LINES")
            .ok()
            .and_then(|v| v.parse::<u16>().ok())
            .filter(|&r| r >= 6)
            .unwrap_or(36);
        Size::new(cols, rows)
    }

    struct RawTerminalGuard {
        #[cfg(unix)]
        orig: Option<libc::termios>,
    }

    impl RawTerminalGuard {
        fn enter() -> io::Result<Self> {
            #[cfg(unix)]
            let orig = unsafe {
                let mut t: libc::termios = std::mem::zeroed();
                if libc::tcgetattr(libc::STDIN_FILENO, &mut t) == 0 {
                    let saved = t;
                    libc::cfmakeraw(&mut t);
                    let _ = libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &t);
                    Some(saved)
                } else {
                    None
                }
            };
            let mut out = io::stdout();
            out.write_all(b"\x1b[?1049h\x1b[?25l\x1b[?1000h\x1b[?1006h")?;
            out.flush()?;
            Ok(Self {
                #[cfg(unix)]
                orig,
            })
        }
    }

    impl Drop for RawTerminalGuard {
        fn drop(&mut self) {
            let mut out = io::stdout();
            let _ = out.write_all(b"\x1b[?1006l\x1b[?1000l\x1b[?25h\x1b[?1049l\x1b[0m");
            let _ = out.flush();
            #[cfg(unix)]
            if let Some(orig) = self.orig.take() {
                unsafe {
                    let _ = libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &orig);
                }
            }
        }
    }

    struct StdinTransport {
        last_size: Size,
        pending: std::collections::VecDeque<RunEvent>,
        #[cfg(not(unix))]
        rx: std::sync::mpsc::Receiver<u8>,
    }

    impl StdinTransport {
        fn new() -> io::Result<Self> {
            #[cfg(not(unix))]
            let rx = {
                use std::io::Read;
                let (tx, rx) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    let mut stdin = io::stdin();
                    let mut buf = [0u8; 64];
                    while let Ok(n) = stdin.read(&mut buf) {
                        if n == 0 {
                            break;
                        }
                        for &b in &buf[..n] {
                            if tx.send(b).is_err() {
                                return;
                            }
                        }
                    }
                });
                rx
            };
            Ok(Self {
                last_size: query_terminal_size(),
                pending: std::collections::VecDeque::new(),
                #[cfg(not(unix))]
                rx,
            })
        }

        fn poll(&mut self, wait: Duration) -> io::Result<RunEvent> {
            if let Some(ev) = self.pending.pop_front() {
                return Ok(ev);
            }

            let cur_size = query_terminal_size();
            if cur_size != self.last_size {
                self.last_size = cur_size;
                return Ok(RunEvent::Redraw);
            }

            let bytes = self.read_bytes(wait)?;
            if bytes.is_empty() {
                return Ok(RunEvent::Idle);
            }

            decode_ansi_input(&bytes, &mut self.pending);
            Ok(self.pending.pop_front().unwrap_or(RunEvent::Idle))
        }

        #[cfg(unix)]
        fn read_bytes(&mut self, wait: Duration) -> io::Result<Vec<u8>> {
            let timeout_ms = wait.as_millis().min(1000) as libc::c_int;
            let mut pfd = libc::pollfd {
                fd: libc::STDIN_FILENO,
                events: libc::POLLIN,
                revents: 0,
            };
            let rc = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
            if rc <= 0 || (pfd.revents & libc::POLLIN) == 0 {
                return Ok(Vec::new());
            }
            let mut buf = [0u8; 256];
            let n = unsafe {
                libc::read(
                    libc::STDIN_FILENO,
                    buf.as_mut_ptr().cast::<libc::c_void>(),
                    buf.len(),
                )
            };
            if n <= 0 {
                return Ok(Vec::new());
            }
            Ok(buf[..(n as usize)].to_vec())
        }

        #[cfg(not(unix))]
        fn read_bytes(&mut self, wait: Duration) -> io::Result<Vec<u8>> {
            let mut out = Vec::new();
            if let Ok(first) = self.rx.recv_timeout(wait) {
                out.push(first);
                while let Ok(b) = self.rx.try_recv() {
                    out.push(b);
                }
            }
            Ok(out)
        }
    }

    pub fn decode_ansi_input(bytes: &[u8], out: &mut std::collections::VecDeque<RunEvent>) {
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                0x11 => {
                    // Ctrl+Q 退出 TUI 宿主
                    out.push_back(RunEvent::Exit);
                    i += 1;
                }
                0x1c => {
                    // Ctrl+\
                    out.push_back(key_event("\\", ctrl_mod()));
                    i += 1;
                }
                b'\r' | b'\n' => {
                    out.push_back(key_event("Enter", Modifiers::default()));
                    i += 1;
                }
                b'\t' => {
                    out.push_back(key_event("Tab", Modifiers::default()));
                    i += 1;
                }
                0x7f | 0x08 => {
                    out.push_back(key_event("Backspace", Modifiers::default()));
                    i += 1;
                }
                0x1b => {
                    if i + 2 < bytes.len() && bytes[i + 1] == b'[' {
                        if bytes[i + 2] == b'<'
                            && let Some((consumed, ev)) = parse_sgr_mouse(&bytes[i..])
                        {
                            out.push_back(ev);
                            i += consumed;
                            continue;
                        }
                        let (consumed, key_opt) = match bytes[i + 2] {
                            b'A' => (3, Some(("ArrowUp", Modifiers::default()))),
                            b'B' => (3, Some(("ArrowDown", Modifiers::default()))),
                            b'C' => (3, Some(("ArrowRight", Modifiers::default()))),
                            b'D' => (3, Some(("ArrowLeft", Modifiers::default()))),
                            b'H' => (3, Some(("Home", Modifiers::default()))),
                            b'F' => (3, Some(("End", Modifiers::default()))),
                            b'Z' => (3, Some(("Tab", shift_mod()))),
                            _ => (1, None),
                        };
                        if let Some((k, m)) = key_opt {
                            out.push_back(key_event(k, m));
                            i += consumed;
                            continue;
                        }
                    }
                    out.push_back(key_event("Escape", Modifiers::default()));
                    i += 1;
                }
                b if (1..=26).contains(&b) => {
                    let ch = (b'a' + (b - 1)) as char;
                    out.push_back(key_event(&ch.to_string(), ctrl_mod()));
                    i += 1;
                }
                _ => {
                    if let Ok(s) = std::str::from_utf8(&bytes[i..]) {
                        for ch in s.chars() {
                            out.push_back(key_event(&ch.to_string(), Modifiers::default()));
                        }
                        break;
                    } else {
                        i += 1;
                    }
                }
            }
        }
    }

    fn key_event(key: &str, modifiers: Modifiers) -> RunEvent {
        RunEvent::Input(Input::Key {
            key: key.to_string(),
            modifiers,
            release: false,
        })
    }

    fn ctrl_mod() -> Modifiers {
        let mut m = Modifiers::default();
        m.ctrl = true;
        m
    }

    fn shift_mod() -> Modifiers {
        let mut m = Modifiers::default();
        m.shift = true;
        m
    }

    fn parse_sgr_mouse(slice: &[u8]) -> Option<(usize, RunEvent)> {
        let end_rel = slice
            .iter()
            .skip(3)
            .position(|&b| b == b'M' || b == b'm')?;
        let end = 3 + end_rel;
        let is_release = slice[end] == b'm';
        let body = std::str::from_utf8(&slice[3..end]).ok()?;
        let mut parts = body.split(';');
        let code: u16 = parts.next()?.parse().ok()?;
        let col: u16 = parts.next()?.parse::<u16>().ok()?.saturating_sub(1);
        let row: u16 = parts.next()?.parse::<u16>().ok()?.saturating_sub(1);

        let mut mods = Modifiers::default();
        mods.shift = (code & 4) != 0;
        mods.alt = (code & 8) != 0;
        mods.ctrl = (code & 16) != 0;

        let kind = if (code & 64) != 0 {
            let step = if (code & 1) == 0 { -1 } else { 1 };
            MouseKind::Wheel { x: 0, y: step }
        } else if (code & 32) != 0 {
            MouseKind::Move
        } else {
            let btn = match code & 3 {
                0 => MouseButton::Left,
                1 => MouseButton::Middle,
                _ => MouseButton::Right,
            };
            if is_release {
                MouseKind::Up(btn)
            } else {
                MouseKind::Down(btn)
            }
        };

        let mut mouse = MouseInput::default();
        mouse.kind = kind;
        mouse.column = col;
        mouse.row = row;
        mouse.modifiers = mods;

        Some((end + 1, RunEvent::Input(Input::Mouse(mouse))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_args_parses_tui_server_and_offline_flags() {
        let parsed = CliArgs::parse_from([
            "--tui",
            "--offline-demo",
            "--server",
            "http://127.0.0.1:4923",
        ])
        .unwrap();
        assert!(parsed.force_tui);
        assert!(parsed.offline_demo);
        assert_eq!(parsed.server_url.as_deref(), Some("http://127.0.0.1:4923"));
    }

    #[cfg(all(feature = "tui", not(target_arch = "wasm32")))]
    #[test]
    fn tui_appview_renders_falcon_root_into_ansi_backend() {
        let mut opts = nsh_platform_tui::AppOptions::default();
        opts.ui_sets = init_ui_sets();
        opts.limits = nsh_engine::Limits::standalone();
        let mut view = nsh_platform_tui::AppView::new(opts);
        view.load(build_root(FalconLaunchOptions {
            offline_demo: true,
            server_url: Some("http://127.0.0.1:4923".into()),
            initial_theme: None,
        }))
        .unwrap();

        let backend = tui_host::AnsiBackend::with_fixed_size(Vec::new(), 120, 36);
        let mut terminal = ratatui_core::terminal::Terminal::new(backend).unwrap();
        view.draw_terminal(&mut terminal).unwrap();
        view.stop();

        let rendered = String::from_utf8_lossy(terminal.backend().inner());
        assert!(!rendered.is_empty());
    }
}
