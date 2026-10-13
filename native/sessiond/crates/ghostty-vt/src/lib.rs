//! libghostty-vt 的安全封装，只覆盖 falcon-sessiond 的 VT 进程用到的部分：
//! 终端（写入、尺寸、选项）、回调（回 PTY 的字节、程序状态、提示符、标题、响铃、重置）、
//! 格式化导出、二进制快照与 continuation。
//!
//! C API 头文件原话 "definitely going to change"：绑定钉在 ghostty.pin 的 commit 上，升级时
//! 这个 crate 就是要跟着改的那一层（ADR 0020 决定二）。
//!
//! 回调的做法：每个 [`Terminal`] 带一个装在 `Box` 里的 [`Handler`]，地址作为 userdata 交给
//! libghostty-vt；回调只在 `vt_write` 期间同步发生，那时 `&mut Terminal` 正被借着、外面摸不到
//! handler，所以在蹦床里把 userdata 转回 `&mut H` 不会和别的引用重叠。
//!
//! libghostty-vt 出 bug 时是 trap（进程直接中止），跨 FFI 接不住——这正是它跑在单独的
//! VT 进程里、由 holder 看护的原因（ADR 0020 决定一）。这里不尝试兜底。

use std::ffi::c_void;
use std::marker::PhantomData;

use ghostty_vt_sys as sys;

pub use sys::GHOSTTY_COMMIT;

/// libghostty-vt 的错误码
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error(pub i32);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self.0 {
            sys::GHOSTTY_OUT_OF_MEMORY => "out of memory",
            sys::GHOSTTY_INVALID_VALUE => "invalid value",
            sys::GHOSTTY_OUT_OF_SPACE => "out of space",
            sys::GHOSTTY_NO_VALUE => "no value",
            _ => "error",
        };
        write!(f, "libghostty-vt: {name} ({})", self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

fn check(r: sys::GhosttyResult) -> Result<()> {
    if r == sys::GHOSTTY_SUCCESS { Ok(()) } else { Err(Error(r)) }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// `#rrggbb`
    pub fn parse_hex(s: &str) -> Option<Rgb> {
        let h = s.strip_prefix('#')?;
        if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let v = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
        Some(Rgb(v(0)?, v(2)?, v(4)?))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorScheme {
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Primary,
    Alternate,
}

/// OSC 7501 报告的状态（`Clear` 是动作）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusState {
    Idle,
    Working,
    Done,
    Blocked,
    Error,
    Clear,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    Permission,
    Question,
    Auth,
}

/// 一条已按规范校验、已解码的 OSC 7501 报告
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramStatus {
    pub state: StatusState,
    pub kind: Option<StatusKind>,
    /// None = 不确定进度
    pub progress: Option<u8>,
    pub id: String,
    pub app: Option<String>,
    pub title: Option<String>,
    pub message: Option<String>,
}

/// OSC 133 的提示符事件
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    /// `A`：提示符开始
    PromptStart,
    /// `B`：输入开始
    InputStart,
    /// `C`：命令开始输出
    OutputStart,
    /// `D`：命令结束
    CommandEnd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemanticPrompt {
    pub kind: PromptKind,
    pub exit_code: Option<i32>,
}

/// 终端往外的事件。都有缺省实现（= 不关心）
pub trait Handler {
    /// 终端要写回 PTY 的字节（查询的回答）
    fn write_pty(&mut self, _data: &[u8]) {}
    fn bell(&mut self) {}
    /// OSC 0 / 2 改了标题
    fn title_changed(&mut self, _title: &str) {}
    fn program_status(&mut self, _status: ProgramStatus) {}
    fn semantic_prompt(&mut self, _prompt: SemanticPrompt) {}
    /// RIS（`ESC c`）把终端整个重置了
    fn reset(&mut self) {}
    /// `CSI ? 996 n` 的回答；None = 不答
    fn color_scheme(&mut self) -> Option<ColorScheme> {
        None
    }
    /// XTVERSION（`CSI > q`）的回答；None = libghostty-vt 的缺省 "libghostty"
    fn xtversion(&self) -> Option<&str> {
        None
    }
}

/// 不关心任何事件
pub struct NoHandler;

impl Handler for NoHandler {}

fn string_of(s: &sys::GhosttyString) -> String {
    if s.ptr.is_null() || s.len == 0 {
        return String::new();
    }
    String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(s.ptr, s.len) }).into_owned()
}

fn opt_string(s: &sys::GhosttyString) -> Option<String> {
    let v = string_of(s);
    (!v.is_empty()).then_some(v)
}

unsafe fn handler<'a, H: Handler>(ud: *mut c_void) -> &'a mut H {
    unsafe { &mut *(ud as *mut H) }
}

unsafe extern "C" fn tramp_write_pty<H: Handler>(_: sys::GhosttyTerminal, ud: *mut c_void, data: *const u8, len: usize) {
    if data.is_null() || len == 0 {
        return;
    }
    let bytes = unsafe { std::slice::from_raw_parts(data, len) };
    unsafe { handler::<H>(ud) }.write_pty(bytes);
}

unsafe extern "C" fn tramp_bell<H: Handler>(_: sys::GhosttyTerminal, ud: *mut c_void) {
    unsafe { handler::<H>(ud) }.bell();
}

unsafe extern "C" fn tramp_title<H: Handler>(t: sys::GhosttyTerminal, ud: *mut c_void) {
    let mut s = sys::GhosttyString::default();
    let title = if unsafe { sys::ghostty_terminal_get(t, sys::GHOSTTY_TERMINAL_DATA_TITLE, (&mut s as *mut sys::GhosttyString).cast()) }
        == sys::GHOSTTY_SUCCESS
    {
        string_of(&s)
    } else {
        String::new()
    };
    unsafe { handler::<H>(ud) }.title_changed(&title);
}

unsafe extern "C" fn tramp_status<H: Handler>(_: sys::GhosttyTerminal, ud: *mut c_void, r: *const sys::GhosttyTerminalProgramStatus) {
    let Some(r) = (unsafe { r.as_ref() }) else { return };
    let state = match r.state {
        sys::GHOSTTY_PROGRAM_STATUS_STATE_IDLE => StatusState::Idle,
        sys::GHOSTTY_PROGRAM_STATUS_STATE_WORKING => StatusState::Working,
        sys::GHOSTTY_PROGRAM_STATUS_STATE_DONE => StatusState::Done,
        sys::GHOSTTY_PROGRAM_STATUS_STATE_BLOCKED => StatusState::Blocked,
        sys::GHOSTTY_PROGRAM_STATUS_STATE_ERROR => StatusState::Error,
        sys::GHOSTTY_PROGRAM_STATUS_STATE_CLEAR => StatusState::Clear,
        _ => return,
    };
    let kind = match r.kind {
        sys::GHOSTTY_PROGRAM_STATUS_KIND_PERMISSION => Some(StatusKind::Permission),
        sys::GHOSTTY_PROGRAM_STATUS_KIND_QUESTION => Some(StatusKind::Question),
        sys::GHOSTTY_PROGRAM_STATUS_KIND_AUTH => Some(StatusKind::Auth),
        _ => None,
    };
    let status = ProgramStatus {
        state,
        kind,
        progress: (0..=100).contains(&r.progress).then_some(r.progress as u8),
        id: string_of(&r.id),
        app: opt_string(&r.app),
        title: opt_string(&r.title),
        message: opt_string(&r.message),
    };
    unsafe { handler::<H>(ud) }.program_status(status);
}

unsafe extern "C" fn tramp_prompt<H: Handler>(_: sys::GhosttyTerminal, ud: *mut c_void, e: *const sys::GhosttyTerminalSemanticPrompt) {
    let Some(e) = (unsafe { e.as_ref() }) else { return };
    let kind = match e.kind {
        sys::GHOSTTY_SEMANTIC_PROMPT_PROMPT_START => PromptKind::PromptStart,
        sys::GHOSTTY_SEMANTIC_PROMPT_INPUT_START => PromptKind::InputStart,
        sys::GHOSTTY_SEMANTIC_PROMPT_OUTPUT_START => PromptKind::OutputStart,
        sys::GHOSTTY_SEMANTIC_PROMPT_COMMAND_END => PromptKind::CommandEnd,
        _ => return,
    };
    let exit_code = e.has_exit_code.then_some(e.exit_code);
    unsafe { handler::<H>(ud) }.semantic_prompt(SemanticPrompt { kind, exit_code });
}

unsafe extern "C" fn tramp_reset<H: Handler>(_: sys::GhosttyTerminal, ud: *mut c_void) {
    unsafe { handler::<H>(ud) }.reset();
}

unsafe extern "C" fn tramp_scheme<H: Handler>(_: sys::GhosttyTerminal, ud: *mut c_void, out: *mut sys::GhosttyColorScheme) -> bool {
    match unsafe { handler::<H>(ud) }.color_scheme() {
        Some(s) => {
            unsafe {
                *out = match s {
                    ColorScheme::Light => sys::GHOSTTY_COLOR_SCHEME_LIGHT,
                    ColorScheme::Dark => sys::GHOSTTY_COLOR_SCHEME_DARK,
                };
            }
            true
        }
        None => false,
    }
}

unsafe extern "C" fn tramp_xtversion<H: Handler>(_: sys::GhosttyTerminal, ud: *mut c_void) -> sys::GhosttyString {
    // 返回的指针只要在回调返回前有效：它指向 handler 里的字符串
    match unsafe { handler::<H>(ud) }.xtversion() {
        Some(v) => sys::GhosttyString { ptr: v.as_ptr(), len: v.len() },
        None => sys::GhosttyString { ptr: std::ptr::null(), len: 0 },
    }
}

/// 格式化导出的选项（对应 `GhosttyFormatterTerminalOptions`）
#[derive(Debug, Clone, Copy)]
pub struct FormatOptions {
    pub format: Format,
    /// 软折行处不写换行。回放给客户端时**必须开**：不开的话折行处写成 `\r\n`，
    /// 客户端的 WRAPLINE 全丢（原型实测）
    pub unwrap: bool,
    pub trim: bool,
    pub palette: bool,
    pub modes: bool,
    pub scrolling_region: bool,
    pub tabstops: bool,
    pub pwd: bool,
    pub keyboard: bool,
    pub cursor: bool,
    pub style: bool,
    pub hyperlink: bool,
    pub protection: bool,
    pub kitty_keyboard: bool,
    pub charsets: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Plain,
    Vt,
    Html,
}

impl FormatOptions {
    /// 只有文字（不带任何额外状态）
    pub fn plain(format: Format) -> Self {
        FormatOptions {
            format,
            unwrap: true,
            trim: false,
            palette: false,
            modes: false,
            scrolling_region: false,
            tabstops: false,
            pwd: false,
            keyboard: false,
            cursor: false,
            style: false,
            hyperlink: false,
            protection: false,
            kitty_keyboard: false,
            charsets: false,
        }
    }

    fn raw(&self) -> sys::GhosttyFormatterTerminalOptions {
        sys::GhosttyFormatterTerminalOptions {
            size: std::mem::size_of::<sys::GhosttyFormatterTerminalOptions>(),
            emit: match self.format {
                Format::Plain => sys::GHOSTTY_FORMATTER_FORMAT_PLAIN,
                Format::Vt => sys::GHOSTTY_FORMATTER_FORMAT_VT,
                Format::Html => sys::GHOSTTY_FORMATTER_FORMAT_HTML,
            },
            unwrap: self.unwrap,
            trim: self.trim,
            extra: sys::GhosttyFormatterTerminalExtra {
                size: std::mem::size_of::<sys::GhosttyFormatterTerminalExtra>(),
                palette: self.palette,
                modes: self.modes,
                scrolling_region: self.scrolling_region,
                tabstops: self.tabstops,
                pwd: self.pwd,
                keyboard: self.keyboard,
                screen: sys::GhosttyFormatterScreenExtra {
                    size: std::mem::size_of::<sys::GhosttyFormatterScreenExtra>(),
                    cursor: self.cursor,
                    style: self.style,
                    hyperlink: self.hyperlink,
                    protection: self.protection,
                    kitty_keyboard: self.kitty_keyboard,
                    charsets: self.charsets,
                },
            },
            selection: std::ptr::null(),
        }
    }
}

/// libghostty-vt 分配、要用 `ghostty_free` 还回去的一段字节，拷成 Vec 后立刻释放
unsafe fn take_alloc(ptr: *mut u8, len: usize) -> Vec<u8> {
    if ptr.is_null() {
        return Vec::new();
    }
    let out = unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec();
    unsafe { sys::ghostty_free(std::ptr::null(), ptr, len) };
    out
}

/// 一个终端。`H` 收它的事件
pub struct Terminal<H: Handler> {
    raw: sys::GhosttyTerminal,
    handler: Box<H>,
    _not_send: PhantomData<*mut ()>,
}

impl<H: Handler> Terminal<H> {
    pub fn new(cols: u16, rows: u16, handler: H) -> Result<Self> {
        let mut raw: sys::GhosttyTerminal = std::ptr::null_mut();
        check(unsafe { sys::ghostty_terminal_new(std::ptr::null(), &mut raw, cols.max(1), rows.max(1)) })?;
        let mut t = Terminal { raw, handler: Box::new(handler), _not_send: PhantomData };
        t.install()?;
        Ok(t)
    }

    /// 从二进制快照恢复一个终端（回调要重新装：快照里没有它们）
    pub fn decode_snapshot(bytes: &[u8], handler: H) -> Result<Self> {
        let mut dec: sys::GhosttySnapshotDecoder = std::ptr::null_mut();
        check(unsafe { sys::ghostty_snapshot_decoder_new_buf(std::ptr::null(), &mut dec, bytes.as_ptr(), bytes.len()) })?;
        let mut raw: sys::GhosttyTerminal = std::ptr::null_mut();
        let r = unsafe { sys::ghostty_snapshot_decoder_decode(dec, &mut raw) };
        unsafe { sys::ghostty_snapshot_decoder_free(dec) };
        check(r)?;
        let mut t = Terminal { raw, handler: Box::new(handler), _not_send: PhantomData };
        t.install()?;
        Ok(t)
    }

    fn set_ptr(&mut self, opt: sys::GhosttyTerminalOption, value: *const c_void) -> Result<()> {
        check(unsafe { sys::ghostty_terminal_set(self.raw, opt, value) })
    }

    fn install(&mut self) -> Result<()> {
        let ud = (&mut *self.handler as *mut H).cast::<c_void>();
        self.set_ptr(sys::GHOSTTY_TERMINAL_OPT_USERDATA, ud)?;
        let write_pty: unsafe extern "C" fn(sys::GhosttyTerminal, *mut c_void, *const u8, usize) = tramp_write_pty::<H>;
        let bell: unsafe extern "C" fn(sys::GhosttyTerminal, *mut c_void) = tramp_bell::<H>;
        let title: unsafe extern "C" fn(sys::GhosttyTerminal, *mut c_void) = tramp_title::<H>;
        let status: unsafe extern "C" fn(sys::GhosttyTerminal, *mut c_void, *const sys::GhosttyTerminalProgramStatus) =
            tramp_status::<H>;
        let prompt: unsafe extern "C" fn(sys::GhosttyTerminal, *mut c_void, *const sys::GhosttyTerminalSemanticPrompt) =
            tramp_prompt::<H>;
        let reset: unsafe extern "C" fn(sys::GhosttyTerminal, *mut c_void) = tramp_reset::<H>;
        let scheme: unsafe extern "C" fn(sys::GhosttyTerminal, *mut c_void, *mut sys::GhosttyColorScheme) -> bool =
            tramp_scheme::<H>;
        let xtversion: unsafe extern "C" fn(sys::GhosttyTerminal, *mut c_void) -> sys::GhosttyString = tramp_xtversion::<H>;
        self.set_ptr(sys::GHOSTTY_TERMINAL_OPT_WRITE_PTY, write_pty as *const c_void)?;
        self.set_ptr(sys::GHOSTTY_TERMINAL_OPT_BELL, bell as *const c_void)?;
        self.set_ptr(sys::GHOSTTY_TERMINAL_OPT_TITLE_CHANGED, title as *const c_void)?;
        self.set_ptr(sys::GHOSTTY_TERMINAL_OPT_PROGRAM_STATUS, status as *const c_void)?;
        self.set_ptr(sys::GHOSTTY_TERMINAL_OPT_SEMANTIC_PROMPT, prompt as *const c_void)?;
        self.set_ptr(sys::GHOSTTY_TERMINAL_OPT_RESET, reset as *const c_void)?;
        self.set_ptr(sys::GHOSTTY_TERMINAL_OPT_COLOR_SCHEME, scheme as *const c_void)?;
        self.set_ptr(sys::GHOSTTY_TERMINAL_OPT_XTVERSION, xtversion as *const c_void)?;
        Ok(())
    }

    pub fn handler(&self) -> &H {
        &self.handler
    }

    pub fn handler_mut(&mut self) -> &mut H {
        &mut self.handler
    }

    /// 喂一段 PTY 输出。任何畸形输入都不会失败（库把它当不可信输入处理）
    pub fn vt_write(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        unsafe { sys::ghostty_terminal_vt_write(self.raw, data.as_ptr(), data.len()) };
    }

    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        check(unsafe { sys::ghostty_terminal_resize(self.raw, cols.max(1), rows.max(1), 0, 0) })
    }

    pub fn set_scrollback_max_lines(&mut self, lines: usize) -> Result<()> {
        self.set_ptr(sys::GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_LINES, (&lines as *const usize).cast())
    }

    pub fn set_scrollback_max_bytes(&mut self, bytes: usize) -> Result<()> {
        self.set_ptr(sys::GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_BYTES, (&bytes as *const usize).cast())
    }

    /// 开启 continuation 跟踪：快照切点落在半截序列上时，[`Self::continuation`] 能交出补齐用的字节。
    /// 要在写入第一个字节之前开，否则之前那段半截序列重建不了
    pub fn set_continuation_max_bytes(&mut self, bytes: usize) -> Result<()> {
        self.set_ptr(sys::GHOSTTY_TERMINAL_OPT_CONTINUATION_MAX_BYTES, (&bytes as *const usize).cast())
    }

    fn set_color(&mut self, opt: sys::GhosttyTerminalOption, c: Option<Rgb>) -> Result<()> {
        match c {
            Some(Rgb(r, g, b)) => {
                let v = sys::GhosttyColorRgb { r, g, b };
                self.set_ptr(opt, (&v as *const sys::GhosttyColorRgb).cast())
            }
            None => self.set_ptr(opt, std::ptr::null()),
        }
    }

    /// 默认前景 / 背景 / 光标色：OSC 10 / 11 / 12 查询按它们回答，没设就不答
    pub fn set_default_colors(&mut self, fg: Option<Rgb>, bg: Option<Rgb>, cursor: Option<Rgb>) -> Result<()> {
        self.set_color(sys::GHOSTTY_TERMINAL_OPT_COLOR_FOREGROUND, fg)?;
        self.set_color(sys::GHOSTTY_TERMINAL_OPT_COLOR_BACKGROUND, bg)?;
        self.set_color(sys::GHOSTTY_TERMINAL_OPT_COLOR_CURSOR, cursor)
    }

    fn get<T: Default>(&self, data: sys::GhosttyTerminalData) -> Result<T> {
        let mut v = T::default();
        check(unsafe { sys::ghostty_terminal_get(self.raw, data, (&mut v as *mut T).cast()) })?;
        Ok(v)
    }

    pub fn cols(&self) -> u16 {
        self.get::<u16>(sys::GHOSTTY_TERMINAL_DATA_COLS).unwrap_or(0)
    }

    pub fn rows(&self) -> u16 {
        self.get::<u16>(sys::GHOSTTY_TERMINAL_DATA_ROWS).unwrap_or(0)
    }

    pub fn active_screen(&self) -> Screen {
        match self.get::<sys::GhosttyTerminalScreen>(sys::GHOSTTY_TERMINAL_DATA_ACTIVE_SCREEN) {
            Ok(sys::GHOSTTY_TERMINAL_SCREEN_ALTERNATE) => Screen::Alternate,
            _ => Screen::Primary,
        }
    }

    /// 光标位置（列, 行），都从 0 起，行相对当前屏的顶部
    pub fn cursor(&self) -> (u16, u16) {
        let x = self.get::<u16>(sys::GHOSTTY_TERMINAL_DATA_CURSOR_X).unwrap_or(0);
        let y = self.get::<u16>(sys::GHOSTTY_TERMINAL_DATA_CURSOR_Y).unwrap_or(0);
        (x, y)
    }

    pub fn scrollback_rows(&self) -> usize {
        self.get::<usize>(sys::GHOSTTY_TERMINAL_DATA_SCROLLBACK_ROWS).unwrap_or(0)
    }

    pub fn title(&self) -> String {
        self.get::<sys::GhosttyString>(sys::GHOSTTY_TERMINAL_DATA_TITLE).map(|s| string_of(&s)).unwrap_or_default()
    }

    /// 流此刻是否停在 ground（不在任何序列 / 多字节字符的中间）
    pub fn at_ground(&self) -> bool {
        self.get::<bool>(sys::GHOSTTY_TERMINAL_DATA_VT_GROUND).unwrap_or(true)
    }

    /// 格式化导出整个当前屏（含历史）
    pub fn format(&self, opts: &FormatOptions) -> Result<Vec<u8>> {
        let mut f: sys::GhosttyFormatter = std::ptr::null_mut();
        check(unsafe { sys::ghostty_formatter_terminal_new(std::ptr::null(), &mut f, self.raw, opts.raw()) })?;
        let mut ptr: *mut u8 = std::ptr::null_mut();
        let mut len = 0usize;
        let r = unsafe { sys::ghostty_formatter_format_alloc(f, std::ptr::null(), &mut ptr, &mut len) };
        unsafe { sys::ghostty_formatter_free(f) };
        check(r)?;
        Ok(unsafe { take_alloc(ptr, len) })
    }

    /// 解析器停在半截序列 / 半个 UTF-8 字符时，补齐它要的原始字节；在 ground 时为空。
    /// 跟踪没开或超了上限时为 None（此时只能接受切点处的小瑕疵）
    pub fn continuation(&self) -> Option<Vec<u8>> {
        let mut ptr: *mut u8 = std::ptr::null_mut();
        let mut len = 0usize;
        let r = unsafe { sys::ghostty_terminal_continuation_alloc(self.raw, std::ptr::null(), &mut ptr, &mut len) };
        if r != sys::GHOSTTY_SUCCESS {
            return None;
        }
        Some(unsafe { take_alloc(ptr, len) })
    }

    /// 二进制快照（`GHOSTSNP`，自带版本号）。VT 进程热升级时用它交接
    pub fn encode_snapshot(&self) -> Result<Vec<u8>> {
        let mut ptr: *mut u8 = std::ptr::null_mut();
        let mut len = 0usize;
        check(unsafe { sys::ghostty_snapshot_encode_alloc(self.raw, std::ptr::null(), &mut ptr, &mut len) })?;
        Ok(unsafe { take_alloc(ptr, len) })
    }

    /// 同一状态的一个独立副本（备用屏导出时要在副本上退出备用屏）。事件不关心
    pub fn duplicate(&self) -> Result<Terminal<NoHandler>> {
        Terminal::decode_snapshot(&self.encode_snapshot()?, NoHandler)
    }
}

impl<H: Handler> Drop for Terminal<H> {
    fn drop(&mut self) {
        unsafe { sys::ghostty_terminal_free(self.raw) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Rec {
        pty: Vec<u8>,
        status: Vec<ProgramStatus>,
        prompts: Vec<PromptKind>,
        titles: Vec<String>,
        resets: u32,
        scheme: Option<ColorScheme>,
        version: String,
    }

    impl Handler for Rec {
        fn write_pty(&mut self, d: &[u8]) {
            self.pty.extend_from_slice(d);
        }
        fn program_status(&mut self, s: ProgramStatus) {
            self.status.push(s);
        }
        fn semantic_prompt(&mut self, p: SemanticPrompt) {
            self.prompts.push(p.kind);
        }
        fn title_changed(&mut self, t: &str) {
            self.titles.push(t.to_string());
        }
        fn reset(&mut self) {
            self.resets += 1;
        }
        fn color_scheme(&mut self) -> Option<ColorScheme> {
            self.scheme
        }
        fn xtversion(&self) -> Option<&str> {
            (!self.version.is_empty()).then_some(self.version.as_str())
        }
    }

    fn term() -> Terminal<Rec> {
        Terminal::new(80, 24, Rec { version: "falcon 9.9.9".into(), ..Default::default() }).unwrap()
    }

    #[test]
    fn queries_are_answered_through_write_pty() {
        let mut t = term();
        t.vt_write(b"\x1b]7501;?\x1b\\\x1b[c\x1b[>q");
        let reply = String::from_utf8_lossy(&t.handler().pty).into_owned();
        let p7501 = reply.find("\x1b]7501;?").expect(&reply);
        let da = reply.find("\x1b[?62;").expect(&reply);
        let xt = reply.find("\x1bP>|falcon 9.9.9").expect(&reply);
        assert!(p7501 < da && da < xt, "顺序要对：{reply:?}");
    }

    #[test]
    fn color_scheme_and_default_colors() {
        let mut t = term();
        t.vt_write(b"\x1b[?996n\x1b]11;?\x1b\\");
        assert!(t.handler().pty.is_empty(), "没设就不答：{:?}", String::from_utf8_lossy(&t.handler().pty));
        t.handler_mut().scheme = Some(ColorScheme::Light);
        t.set_default_colors(Some(Rgb(0, 0, 0)), Some(Rgb(0xff, 0xfe, 0xfd)), None).unwrap();
        t.vt_write(b"\x1b[?996n\x1b]11;?\x1b\\");
        let reply = String::from_utf8_lossy(&t.handler().pty).into_owned();
        assert!(reply.contains("\x1b[?997;2n"), "{reply:?}");
        assert!(reply.contains("rgb:ffff/fefe/fdfd"), "{reply:?}");
    }

    #[test]
    fn program_status_prompt_title_and_reset_events() {
        let mut t = term();
        t.vt_write(b"\x1b]7501;state=blocked:kind=permission:app=claude-code:title=RWRpdA==:msg=QXBwbHk/\x1b\\");
        t.vt_write(b"\x1b]7501;state=bogus\x07\x1b]2;hello\x07\x1b]133;A\x07");
        let h = t.handler();
        assert_eq!(h.status.len(), 1, "{:?}", h.status);
        let s = &h.status[0];
        assert_eq!((s.state, s.kind), (StatusState::Blocked, Some(StatusKind::Permission)));
        assert_eq!((s.title.as_deref(), s.message.as_deref()), (Some("Edit"), Some("Apply?")));
        assert_eq!(h.titles, ["hello"]);
        assert_eq!(h.prompts, [PromptKind::PromptStart]);
        t.vt_write(b"\x1bc");
        assert_eq!(t.handler().resets, 1);
    }

    #[test]
    fn continuation_reconstructs_a_split_sequence() {
        let mut t = term();
        t.set_continuation_max_bytes(1 << 20).unwrap();
        t.vt_write(b"abc\x1b]0;ti");
        assert!(!t.at_ground());
        assert_eq!(t.continuation().unwrap(), b"\x1b]0;ti");
        t.vt_write(b"tle\x07");
        assert!(t.at_ground());
        assert_eq!(t.continuation().unwrap(), b"");
        t.vt_write("中".as_bytes().split_at(2).0);
        assert_eq!(t.continuation().unwrap(), &"中".as_bytes()[..2]);
    }

    #[test]
    fn snapshot_round_trip_and_alt_screen() {
        let mut t = term();
        t.set_scrollback_max_lines(10_000).unwrap();
        for i in 0..100 {
            t.vt_write(format!("line {i}\r\n").as_bytes());
        }
        t.vt_write(b"\x1b[?1049h\x1b[Halt");
        assert_eq!(t.active_screen(), Screen::Alternate);
        let mut copy = t.duplicate().unwrap();
        assert_eq!(copy.active_screen(), Screen::Alternate);
        copy.vt_write(b"\x1b[?1049l");
        let primary = String::from_utf8(copy.format(&FormatOptions::plain(Format::Plain)).unwrap()).unwrap();
        assert!(primary.contains("line 0") && primary.contains("line 99"), "{primary}");
        let alt = String::from_utf8(t.format(&FormatOptions::plain(Format::Plain)).unwrap()).unwrap();
        assert!(alt.starts_with("alt"), "{alt:?}");
    }
}
