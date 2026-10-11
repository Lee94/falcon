//! 终端 VT 模式跟踪：`packages/shared/src/termModes.ts` 的 Rust 移植，语义逐条对齐。
//!
//! 服务端用同一个跟踪器生成回放前缀；客户端用它读鼠标协议 / 编码来合成按键报文（这个用法来自
//! 旧 React 版的 rio 引擎，rio 已随它一起删除）。客户端不直接用 alacritty 的 `TermMode`：它只有
//! MOUSE_REPORT_CLICK / DRAG / MOTION 与 SGR_MOUSE 几个位，分不出 `?9`（X10）也不认
//! `?1016`（SGR 像素坐标），而 falcon-term 的 `mouse::MouseReporter` 要的正是这两个维度。
//! 客户端喂的是同一条输出流（回放前缀也在其中），跟踪结果与服务端一致。
//!
//! 按字节而不是按字符扫描：它关心的字符全是 ASCII，而 UTF-8 多字节序列的每个字节都 ≥ 0x80，
//! 不会被误认成 ESC / BEL / 参数字节，结果与 TS 按 UTF-16 码元扫描一致。
//!
//! 语义（原文注释的要点，细节见 TS）：
//! - 鼠标协议（?9/?1000/?1002/?1003）与编码（?1006/?1016）是单值状态，后设的赢；关掉其中
//!   任意一个号都把整个状态清空（xterm.js 就是这么写的）；
//! - alt-screen 记住是哪个变体（?47/?1047/?1049）开的；
//! - RIS（ESC c）全部回默认；DECSTR（CSI ! p）只重置键盘 / 光标类布尔模式，不碰鼠标与 alt-screen；
//! - ?2031（亮暗通知订阅）单独记，不进回放前缀。

/// 布尔型私有模式及其默认值（fresh xterm.js / term.reset() 之后的取值）。顺序即前缀里的输出顺序。
const BOOL_MODE_DEFAULTS: [(u16, bool); 8] = [
    (1, false),    // DECCKM 应用光标键——丢了它 vim / less 里按方向键会打出 ABCD
    (6, false),    // DECOM 起点模式
    (7, true),     // DECAWM 自动回绕
    (25, true),    // DECTCEM 光标可见
    (45, false),   // 反向回绕
    (66, false),   // DECNKM 应用小键盘（ESC = / ESC > 折算到同一状态）
    (1004, false), // focus 上报
    (2004, false), // bracketed paste——丢了它多行粘贴会被 shell 逐行执行
];

/// CSI 参数收集上限。真实模式序列不过十几字节，超长的只可能是畸形流。
const CSI_PARAM_MAX: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Ground,
    Esc,
    EscIntermediate,
    Csi,
    Str,
    StrEsc,
}

#[derive(Clone, Debug)]
pub struct TermModeTracker {
    bools: [bool; BOOL_MODE_DEFAULTS.len()],
    /// 0 = NONE；否则是最后一次 h 的协议号
    mouse_proto: u16,
    /// 0 = DEFAULT；否则 1006（SGR）/ 1016（SGR_PIXELS）
    mouse_enc: u16,
    /// 0 = normal buffer；否则是打开 alt-screen 用的那个号
    alt_screen: u16,
    /// ?2031 亮暗通知订阅。不进 prefix()。
    notify_2031: bool,
    state: State,
    csi_params: Vec<u8>,
}

impl Default for TermModeTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl TermModeTracker {
    pub fn new() -> Self {
        Self {
            bools: BOOL_MODE_DEFAULTS.map(|(_, def)| def),
            mouse_proto: 0,
            mouse_enc: 0,
            alt_screen: 0,
            notify_2031: false,
            state: State::Ground,
            csi_params: Vec::with_capacity(16),
        }
    }

    /// 是否有程序订阅了亮暗主题变化通知（DECSET 2031）。
    pub fn theme_notify(&self) -> bool {
        self.notify_2031
    }

    /// 当前鼠标协议：0 = 没开；否则 9 / 1000 / 1002 / 1003（最后一次 h 的号）。
    pub fn mouse_protocol(&self) -> u16 {
        self.mouse_proto
    }

    /// 当前鼠标编码：0 = 默认单字节；否则 1006（SGR）/ 1016（SGR 像素坐标）。
    pub fn mouse_encoding(&self) -> u16 {
        self.mouse_enc
    }

    /// 喂入一段 PTY 输出。序列可以在任意字节处被 chunk 边界切开，状态机跨调用续接。
    /// 热路径：文本段用 memchr 式整段跳过，不逐字节走状态机。
    pub fn track(&mut self, data: &[u8]) {
        let n = data.len();
        let mut i = 0;
        while i < n {
            match self.state {
                State::Ground => {
                    let Some(off) = data[i..].iter().position(|&b| b == 0x1b) else {
                        return;
                    };
                    i += off + 1;
                    self.state = State::Esc;
                }
                State::Esc => {
                    let c = data[i];
                    i += 1;
                    match c {
                        b'[' => {
                            self.state = State::Csi;
                            self.csi_params.clear();
                        }
                        // OSC / DCS / SOS / PM / APC：载荷不解析，吃到 BEL 或 ST 为止
                        b']' | b'P' | b'X' | b'^' | b'_' => self.state = State::Str,
                        b'c' => {
                            self.hard_reset();
                            self.state = State::Ground;
                        }
                        b'=' => {
                            self.set_bool(66, true);
                            self.state = State::Ground;
                        }
                        b'>' => {
                            self.set_bool(66, false);
                            self.state = State::Ground;
                        }
                        // 字符集指定等 ESC + 中间字节 + final 的形态
                        b' '..=b'/' => self.state = State::EscIntermediate,
                        // 连续 ESC 停在本态重新开始
                        0x1b => {}
                        // 其余单字符 ESC 序列直接结束
                        _ => self.state = State::Ground,
                    }
                }
                State::EscIntermediate => {
                    let c = data[i];
                    i += 1;
                    if !(b' '..=b'/').contains(&c) {
                        self.state = State::Ground;
                    }
                }
                State::Csi => {
                    while i < n {
                        let c = data[i];
                        i += 1;
                        if (0x40..=0x7e).contains(&c) {
                            let params = std::mem::take(&mut self.csi_params);
                            self.apply_csi(&params, c);
                            self.csi_params = params;
                            self.state = State::Ground;
                            break;
                        }
                        if c == 0x1b {
                            // 序列被新的 ESC 打断（对齐 VT 解析器的 abort 行为）
                            self.state = State::Esc;
                            break;
                        }
                        if c == 0x18 || c == 0x1a {
                            // CAN / SUB 中止序列
                            self.state = State::Ground;
                            break;
                        }
                        if self.csi_params.len() < CSI_PARAM_MAX {
                            self.csi_params.push(c);
                        }
                    }
                }
                State::Str => {
                    // BEL 与 ESC 一趟找，先碰到哪个算哪个。分两趟各找一次是 O(n²)：zellij 的
                    // OSC 8 超链接用 ST（ESC \）收尾、整段输出里没有 BEL，每个 OSC 都会把
                    // 剩下的几 MB 扫到底——4MB 回放在 debug 构建里解析了 4 分钟
                    let Some(off) = data[i..].iter().position(|&b| b == 0x07 || b == 0x1b) else {
                        // 整段都是载荷
                        return;
                    };
                    let hit = data[i + off];
                    i += off + 1;
                    self.state = if hit == 0x07 { State::Ground } else { State::StrEsc };
                }
                State::StrEsc => {
                    let c = data[i];
                    i += 1;
                    if c == b'\\' {
                        self.state = State::Ground; // ST
                    } else if c != 0x1b {
                        self.state = State::Str; // 载荷里的孤立 ESC
                    }
                }
            }
        }
    }

    /// 回放前缀：让 reset 后的终端回到跟踪到的当前模式。客户端用不上，留着与 TS 对照测试。
    pub fn prefix(&self) -> String {
        let mut out = String::new();
        // alt-screen 放最前：快照内容要画进正确的缓冲区
        if self.alt_screen != 0 {
            out.push_str(&format!("\x1b[?{}h", self.alt_screen));
        }
        for (i, (mode, def)) in BOOL_MODE_DEFAULTS.iter().enumerate() {
            let cur = self.bools[i];
            if cur != *def {
                out.push_str(&format!("\x1b[?{mode}{}", if cur { 'h' } else { 'l' }));
            }
        }
        if self.mouse_proto != 0 {
            out.push_str(&format!("\x1b[?{}h", self.mouse_proto));
        }
        if self.mouse_enc != 0 {
            out.push_str(&format!("\x1b[?{}h", self.mouse_enc));
        }
        out
    }

    fn apply_csi(&mut self, params: &[u8], fin: u8) {
        if fin == b'p' {
            if params == b"!" {
                self.soft_reset();
            }
            return;
        }
        if (fin != b'h' && fin != b'l') || params.first() != Some(&b'?') {
            return;
        }
        let set = fin == b'h';
        for part in params[1..].split(|&b| b == b';') {
            // Number("") 是 0、Number("1a") 是 NaN：两者都被 TS 的 `mode <= 0` / isInteger 挡掉
            let Some(mode) = std::str::from_utf8(part)
                .ok()
                .and_then(|s| s.parse::<u32>().ok())
                .filter(|m| *m > 0)
            else {
                continue;
            };
            let Ok(mode) = u16::try_from(mode) else {
                continue;
            };
            match mode {
                9 | 1000 | 1002 | 1003 => self.mouse_proto = if set { mode } else { 0 },
                1006 | 1016 => self.mouse_enc = if set { mode } else { 0 },
                47 | 1047 | 1049 => self.alt_screen = if set { mode } else { 0 },
                2031 => self.notify_2031 = set,
                _ => self.set_bool(mode, set),
            }
        }
    }

    fn set_bool(&mut self, mode: u16, value: bool) {
        if let Some(i) = BOOL_MODE_DEFAULTS.iter().position(|(m, _)| *m == mode) {
            self.bools[i] = value;
        }
    }

    fn hard_reset(&mut self) {
        self.bools = BOOL_MODE_DEFAULTS.map(|(_, def)| def);
        self.mouse_proto = 0;
        self.mouse_enc = 0;
        self.alt_screen = 0;
        self.notify_2031 = false;
    }

    /// DECSTR：xterm.js softReset 恰好把跟踪的布尔模式全部回默认，鼠标与 alt-screen 不动。
    fn soft_reset(&mut self) {
        self.bools = BOOL_MODE_DEFAULTS.map(|(_, def)| def);
    }
}

#[cfg(test)]
mod tests {
    //! 用例逐条移植自 packages/shared/src/termModes.test.ts。
    use super::*;

    fn tracked(chunks: &[&str]) -> String {
        let mut t = TermModeTracker::new();
        for c in chunks {
            t.track(c.as_bytes());
        }
        t.prefix()
    }

    #[test]
    fn default_state_has_no_prefix() {
        assert_eq!(tracked(&[""]), "");
        assert_eq!(tracked(&["plain text\r\nmore"]), "");
    }

    #[test]
    fn rebuilds_zellij_attach_modes() {
        let prefix = tracked(&["\x1b[?1049h\x1b[?1002h\x1b[?1006h\x1b[?25l"]);
        assert!(prefix.starts_with("\x1b[?1049h"));
        assert!(prefix.contains("\x1b[?1002h"));
        assert!(prefix.contains("\x1b[?1006h"));
        assert!(prefix.contains("\x1b[?25l"));
    }

    #[test]
    fn modes_turned_off_leave_prefix() {
        assert_eq!(tracked(&["\x1b[?2004h\x1b[?2004l"]), "");
        assert_eq!(tracked(&["\x1b[?1049h\x1b[?1049l"]), "");
    }

    #[test]
    fn default_on_modes_only_emit_when_off() {
        assert_eq!(tracked(&["\x1b[?7h"]), "");
        assert_eq!(tracked(&["\x1b[?7l"]), "\x1b[?7l");
        assert_eq!(tracked(&["\x1b[?25l\x1b[?25h"]), "");
    }

    #[test]
    fn mouse_protocol_is_single_valued() {
        assert_eq!(tracked(&["\x1b[?1000h\x1b[?1002h"]), "\x1b[?1002h");
        assert_eq!(tracked(&["\x1b[?1002h\x1b[?1000l"]), "");
        assert_eq!(tracked(&["\x1b[?1006h\x1b[?1016h"]), "\x1b[?1016h");
        assert_eq!(tracked(&["\x1b[?1016h\x1b[?1006l"]), "");
    }

    #[test]
    fn multi_param_sequence() {
        let prefix = tracked(&["\x1b[?1049;1002;1006h"]);
        assert!(prefix.starts_with("\x1b[?1049h"));
        assert!(prefix.contains("\x1b[?1002h"));
        assert!(prefix.contains("\x1b[?1006h"));
    }

    #[test]
    fn survives_chunk_boundaries() {
        assert_eq!(tracked(&["\x1b", "[?20", "04h"]), "\x1b[?2004h");
        assert_eq!(tracked(&["text\x1b[", "?1002", "h tail"]), "\x1b[?1002h");
    }

    #[test]
    fn alt_screen_remembers_variant() {
        assert_eq!(tracked(&["\x1b[?47h"]), "\x1b[?47h");
        assert_eq!(tracked(&["\x1b[?1047h"]), "\x1b[?1047h");
    }

    #[test]
    fn keypad_esc_equals() {
        assert_eq!(tracked(&["\x1b="]), "\x1b[?66h");
        assert_eq!(tracked(&["\x1b=\x1b>"]), "");
    }

    #[test]
    fn ris_resets_everything() {
        assert_eq!(tracked(&["\x1b[?1049h\x1b[?1002h\x1b[?7l\x1bc"]), "");
    }

    #[test]
    fn decstr_keeps_mouse_and_alt_screen() {
        let prefix = tracked(&["\x1b[?1049h\x1b[?1002h\x1b[?1h\x1b[?2004h\x1b[!p"]);
        assert!(prefix.contains("\x1b[?1049h"));
        assert!(prefix.contains("\x1b[?1002h"));
        assert!(!prefix.contains("\x1b[?1h"));
        assert!(!prefix.contains("\x1b[?2004h"));
    }

    #[test]
    fn string_payloads_are_ignored() {
        assert_eq!(tracked(&["\x1b]0;title with \x1b[?1002h inside\x07"]), "");
        assert_eq!(tracked(&["\x1bPq payload \x1b[?1002h more\x1b\\"]), "");
        assert_eq!(tracked(&["\x1b]52;c;", "\x1b[?1002h", "\x07"]), "");
    }

    #[test]
    fn non_private_csi_ignored() {
        assert_eq!(
            tracked(&["\x1b[1h\x1b[2J\x1b[38;5;196m\x1b(0\x1b[?2004h"]),
            "\x1b[?2004h"
        );
    }

    #[test]
    fn interrupted_csi_does_not_eat_next() {
        assert_eq!(tracked(&["\x1b[?10\x1b[?2004h"]), "\x1b[?2004h");
    }

    #[test]
    fn untracked_private_modes_skip_prefix() {
        assert_eq!(tracked(&["\x1b[?12h\x1b[?2026h\x1b[?1048h"]), "");
    }

    #[test]
    fn mouse_getters() {
        let mut t = TermModeTracker::new();
        assert_eq!(t.mouse_protocol(), 0);
        assert_eq!(t.mouse_encoding(), 0);
        t.track(b"\x1b[?1002h\x1b[?1006h");
        assert_eq!(t.mouse_protocol(), 1002);
        assert_eq!(t.mouse_encoding(), 1006);
        t.track(b"\x1b[?1006l");
        assert_eq!(t.mouse_protocol(), 1002);
        assert_eq!(t.mouse_encoding(), 0);
        t.track(b"\x1bc");
        assert_eq!(t.mouse_protocol(), 0);
    }

    #[test]
    fn theme_notify_2031() {
        let mut t = TermModeTracker::new();
        assert!(!t.theme_notify());
        t.track(b"\x1b[?2031h");
        assert!(t.theme_notify());
        assert_eq!(t.prefix(), "");
        t.track(b"\x1b[?2031l");
        assert!(!t.theme_notify());
    }

    #[test]
    fn theme_notify_cleared_by_ris_not_decstr() {
        let mut t = TermModeTracker::new();
        t.track(b"\x1b[?2031h\x1b[!p");
        assert!(t.theme_notify());
        t.track(b"\x1bc");
        assert!(!t.theme_notify());
    }

    #[test]
    fn non_ascii_text_is_skipped() {
        // UTF-8 多字节序列不会被误认成控制字节
        assert_eq!(tracked(&["中文\x1b[?2004h文字"]), "\x1b[?2004h");
        assert_eq!(tracked(&["\x1b中\x1b[?1002h"]), "\x1b[?1002h");
    }

    /// 原生独有：大段全是 ST 收尾的 OSC（zellij 的 OSC 8 超链接，一个 BEL 都没有）必须线性。
    /// 退回"BEL、ESC 各扫一趟"的写法，这 2MB 在 debug 构建里要跑几分钟，测试会明显卡住
    #[test]
    fn st_terminated_osc_flood_is_linear() {
        let mut data = "\x1b]8;;\x1b\\x".repeat(2 * 1024 * 1024 / 9);
        data.push_str("\x1b[?2004h");
        assert_eq!(tracked(&[&data]), "\x1b[?2004h");
    }
}
