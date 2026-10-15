//! 对话终端输出流上的 shell 标记（docs/design/ai-assistant.md 4.2），纯函数层。
//!
//! 标记由 shell_integration.rs 注入的钩子打出来：OSC 133 的 `A`（提示符开始）、`C`（命令开始
//! 输出）、`D;<退出码>`（命令结束），OSC 7 的 `file://主机/当前目录`。这里只**旁观**：喂进来的
//! 输出原样转给 Viewer，扫描器不改字节流（客户端的 alacritty 不认这些 OSC，丢掉就是了；持久
//! 会话的 VT 进程还要靠 133 清程序状态）。
//!
//! - 扫描器（[`MarkScanner`]）在 falcon-core 里：客户端也拿它找提示符；
//! - [`ShellTracker`] 维护「提示符上 / 命令在跑 / 不知道」与当前目录，并替 agent 收一条命令的
//!   输出：写入命令前 [`ShellTracker::arm`]，`C` 之后的文字算输出，`D` 带回退出码。shell 没打 `C`
//!   （老 bash 的 DEBUG trap 被用户占着）时，从上膛那一刻起收，会带上回显的那一行命令——宁可
//!   多一行，不能一直等不到。
//!
//! 每个提示符前都有一个 `D`（空回车也打）：只有「运行中」或正在收输出时才认它。

pub use falcon_core::shell_marks::{Mark, MarkScanner, Piece};

// ---------------------------------------------------------------- 状态机

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PromptState {
    /// 还没见过标记（刚接回、shell 没有集成）
    #[default]
    Unknown,
    AtPrompt,
    Running,
}

#[derive(Debug, Default)]
struct Capture {
    /// 见过 `C` 了：之后的文字才是输出
    started: bool,
    out: String,
}

/// 命令块的输出存多少：头 64KB + 尾 192KB，中间省掉。`cat` 一个大文件不该把对话撑爆，
/// 而开头（报错位置、表头）与结尾（结论）最有用
const BLOCK_HEAD: usize = 64 * 1024;
const BLOCK_TAIL: usize = 192 * 1024;

/// 用户自己敲的一条命令（替 agent 收的那条不算，它进工具卡片）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandBlock {
    /// 命令行（OSC 633;E）；shell 没报就是空串
    pub command: String,
    pub cwd: Option<String>,
    /// 原始输出（带 ANSI），过长时掐了中间
    pub output: String,
    pub exit_code: Option<i32>,
}

/// 正在跑的那条用户命令
#[derive(Debug, Default)]
struct OpenBlock {
    command: String,
    cwd: Option<String>,
    head: String,
    tail: String,
    /// 掐掉的字节数
    dropped: usize,
}

impl OpenBlock {
    fn push(&mut self, t: &str) {
        if self.head.len() < BLOCK_HEAD {
            let room = BLOCK_HEAD - self.head.len();
            let cut = floor_char_boundary(t, room);
            self.head.push_str(&t[..cut]);
            if cut == t.len() {
                return;
            }
            self.tail.push_str(&t[cut..]);
        } else {
            self.tail.push_str(t);
        }
        if self.tail.len() > BLOCK_TAIL * 2 {
            // 攒到两倍再一次性掐，别每块都挪内存
            let drop = floor_char_boundary(&self.tail, self.tail.len() - BLOCK_TAIL);
            self.dropped += drop;
            self.tail.drain(..drop);
        }
    }

    fn finish(mut self, exit_code: Option<i32>) -> CommandBlock {
        if self.tail.len() > BLOCK_TAIL {
            let drop = floor_char_boundary(&self.tail, self.tail.len() - BLOCK_TAIL);
            self.dropped += drop;
            self.tail.drain(..drop);
        }
        let output = if self.dropped > 0 {
            format!("{}\r\n\x1b[2m… 省略 {} 字节 …\x1b[0m\r\n{}", self.head, self.dropped, self.tail)
        } else {
            self.head + &self.tail
        };
        let output = strip_prompt_sp(&output).to_string();
        CommandBlock { command: self.command, cwd: self.cwd, output, exit_code }
    }
}

/// 去掉输出末尾 zsh 的 PROMPT_SP（fish 的「⏎」同理）。
///
/// 上一条命令跑完，zsh 在**precmd 之前**打：PROMPT_EOL_MARK（缺省是反显的 `%`）+ 补满一行的
/// 空格 + `\r` + 与标记等宽的空格 + `\r`。终端上靠自动折行：输出没以换行结尾时标记留在那一行
/// 尾，提示符被挤到下一行；以换行结尾时标记正好被那几个空格盖掉。它打在 `D` 之前，就算进了
/// 命令输出。命令块不知道终端多宽、不会折行，原样画的话 `\r` 回到行首再写空格，输出的头几个字
/// 被盖掉、每块末尾多一行空白；agent 拿到的输出里也平白多一串空格。认出这个形状就整段去掉
/// （标记一起去：块里少一个「没有换行」的提示，不值得为它按终端宽度模拟折行）。
///
/// 形状：`…标记 空格{n} \r 空格{w} \r [ESC[K]`，标记是 w 个可见字符，可以夹着 SGR。对不上就
/// 原样返回
pub fn strip_prompt_sp(out: &str) -> &str {
    let t = out.strip_suffix("\x1b[K").unwrap_or(out);
    let Some(t) = t.strip_suffix('\r') else { return out };
    let w = t.len() - t.trim_end_matches(' ').len();
    let Some(t) = t[..t.len() - w].strip_suffix('\r') else { return out };
    let n = t.len() - t.trim_end_matches(' ').len();
    if w == 0 || n == 0 {
        return out;
    }
    let mut t = &t[..t.len() - n];
    // 从尾巴往前去掉 w 个可见字符，跳过其间的 SGR；碰到换行 / 回车说明不是它
    let mut left = w;
    while left > 0 {
        t = trim_trailing_csi(t);
        match t.chars().next_back() {
            Some('\n' | '\r') | None => return out,
            Some(c) => {
                t = &t[..t.len() - c.len_utf8()];
                left -= 1;
            }
        }
    }
    trim_trailing_csi(t)
}

/// 去掉末尾连着的 CSI 序列（`ESC [ 参数 中间字节 终结字节`）
fn trim_trailing_csi(mut t: &str) -> &str {
    loop {
        let Some(esc) = t.rfind('\x1b') else { return t };
        let tail = &t.as_bytes()[esc..];
        let csi = tail.len() >= 3
            && tail[1] == b'['
            && (0x40..=0x7e).contains(&tail[tail.len() - 1])
            && tail[2..tail.len() - 1].iter().all(|b| (0x20..=0x3f).contains(b));
        if !csi {
            return t;
        }
        t = &t[..esc];
    }
}

fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// 一次 [`ShellTracker::feed`] 的结果
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Tracked {
    /// 提示符状态或当前目录变了（要推给界面）
    pub changed: bool,
    /// 收输出期间（`C` 之后）新到的文字，给卡片做实时输出
    pub captured: String,
    /// 收完了：(全部输出, 退出码)
    pub finished: Option<(String, Option<i32>)>,
    /// 用户自己敲的命令跑完了：进时间线
    pub blocks: Vec<CommandBlock>,
}

#[derive(Debug, Default)]
pub struct ShellTracker {
    scanner: MarkScanner,
    state: PromptState,
    cwd: Option<String>,
    capture: Option<Capture>,
    /// 下一条命令的命令行（633;E 在 C 之前到）
    pending_command: Option<String>,
    /// 正在跑的用户命令
    open: Option<OpenBlock>,
    /// 宿主机上的登录用户与家目录（附着时由服务端填，不来自输出流）
    pub user: Option<String>,
    pub home: Option<String>,
    /// 这个 shell 注入了集成（认得出的 POSIX shell）。false 时等不到标记，命令改走独立进程
    pub integrated: bool,
}

impl ShellTracker {
    pub fn state(&self) -> PromptState {
        self.state
    }

    pub fn cwd(&self) -> Option<&str> {
        self.cwd.as_deref()
    }

    pub fn is_capturing(&self) -> bool {
        self.capture.is_some()
    }

    /// 要往 shell 里写一条命令了：从现在起替它收输出
    pub fn arm(&mut self) {
        self.capture = Some(Capture::default());
    }

    /// 不等了（停止、超时、会话没了）：交出已经收到的
    pub fn disarm(&mut self) -> Option<String> {
        self.capture.take().map(|c| c.out)
    }

    /// 服务端刚接回、前台问出来是 shell：当作停在提示符上（设计文档 5.3 第 1 步）
    pub fn assume_prompt(&mut self) -> bool {
        if self.state == PromptState::Unknown {
            self.state = PromptState::AtPrompt;
            return true;
        }
        false
    }

    pub fn feed(&mut self, data: &str) -> Tracked {
        let mut res = Tracked::default();
        for piece in self.scanner.feed(data) {
            match piece {
                Piece::Text(t) => {
                    if let Some(c) = &mut self.capture {
                        c.out.push_str(&t);
                        if c.started {
                            res.captured.push_str(&t);
                        }
                    }
                    if let Some(b) = &mut self.open {
                        b.push(&t);
                    }
                }
                Piece::Mark(Mark::PromptStart) => {
                    // 没等到 D 就又到了提示符（shell 被 kill 之类）：那一块照样收尾
                    if let Some(b) = self.open.take() {
                        res.blocks.push(b.finish(None));
                    }
                    self.set_state(PromptState::AtPrompt, &mut res)
                }
                Piece::Mark(Mark::CommandLine(cmd)) => self.pending_command = Some(cmd),
                Piece::Mark(Mark::CommandStart) => {
                    self.set_state(PromptState::Running, &mut res);
                    let command = self.pending_command.take().unwrap_or_default();
                    match &mut self.capture {
                        Some(c) if !c.started => {
                            // 回显的那一行命令不算输出
                            c.started = true;
                            c.out.clear();
                        }
                        Some(_) => {}
                        // 不是 agent 的：用户自己敲的，开一个命令块
                        None => self.open = Some(OpenBlock { command, cwd: self.cwd.clone(), ..Default::default() }),
                    }
                }
                Piece::Mark(Mark::CommandEnd(code)) => {
                    let ours = self.capture.as_ref().is_some_and(|c| c.started || self.state != PromptState::Running);
                    if self.state == PromptState::Running || ours {
                        if let Some(c) = self.capture.take() {
                            res.finished = Some((strip_prompt_sp(&c.out).to_string(), code));
                        }
                        if let Some(b) = self.open.take() {
                            res.blocks.push(b.finish(code));
                        }
                        self.set_state(PromptState::AtPrompt, &mut res);
                    }
                }
                Piece::Mark(Mark::Cwd(path)) => {
                    if self.cwd.as_deref() != Some(path.as_str()) {
                        self.cwd = Some(path);
                        res.changed = true;
                    }
                }
            }
        }
        res
    }

    fn set_state(&mut self, state: PromptState, res: &mut Tracked) {
        if self.state != state {
            self.state = state;
            res.changed = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一条命令的全过程：上膛 → 回显 → C → 输出 → D;退出码 → A
    #[test]
    fn captures_output_between_c_and_d() {
        let mut t = ShellTracker::default();
        let r = t.feed("\x1b]133;D;0\x07\x1b]133;A\x07\x1b]7;file://h/home/u\x07% ");
        assert!(r.changed);
        assert_eq!(t.state(), PromptState::AtPrompt);
        assert_eq!(t.cwd(), Some("/home/u"));

        t.arm();
        let r = t.feed("ls -la\r\n\x1b]133;C\x07file1\r\n");
        assert_eq!(t.state(), PromptState::Running);
        assert_eq!(r.captured, "file1\r\n");
        let r = t.feed("file2\r\n\x1b]133;D;3\x07\x1b]133;A\x07% ");
        assert_eq!(r.captured, "file2\r\n");
        assert_eq!(r.finished, Some(("file1\r\nfile2\r\n".into(), Some(3))));
        assert_eq!(t.state(), PromptState::AtPrompt);
        assert!(!t.is_capturing());
    }

    /// 提示符前的 D（空回车、第一个提示符）不算命令结束
    #[test]
    fn stray_d_at_prompt_is_ignored() {
        let mut t = ShellTracker::default();
        t.feed("\x1b]133;A\x07% ");
        let r = t.feed("\x1b]133;D;0\x07\x1b]133;A\x07% ");
        assert_eq!(r.finished, None);
        assert!(!r.changed);
    }

    /// 用户自己敲的命令：状态跟着变，但不收输出
    #[test]
    fn user_commands_change_state_without_capture() {
        let mut t = ShellTracker::default();
        t.feed("\x1b]133;A\x07");
        let r = t.feed("\x1b]133;C\x07hi\r\n");
        assert!(r.changed);
        assert_eq!(r.captured, "");
        let r = t.feed("\x1b]133;D;0\x07\x1b]133;A\x07");
        assert_eq!(r.finished, None);
        assert_eq!(t.state(), PromptState::AtPrompt);
    }

    /// shell 不打 C：从上膛起收（带着回显），D 照样结束
    #[test]
    fn missing_c_falls_back_to_everything_since_arm() {
        let mut t = ShellTracker::default();
        t.feed("\x1b]133;A\x07$ ");
        t.arm();
        let r = t.feed("echo hi\r\nhi\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ ");
        assert_eq!(r.finished, Some(("echo hi\r\nhi\r\n".into(), Some(0))));
    }

    #[test]
    fn disarm_returns_partial_output() {
        let mut t = ShellTracker::default();
        t.arm();
        t.feed("\x1b]133;C\x07partial");
        assert_eq!(t.disarm().as_deref(), Some("partial"));
        assert!(!t.is_capturing());
        // 见过 C：还在运行中，不能当成停在提示符上
        assert!(!t.assume_prompt());
    }

    /// 用户自己敲的命令：C 开块（带 633;E 的命令行与当时的目录），D 收尾
    #[test]
    fn user_commands_become_blocks() {
        let mut t = ShellTracker::default();
        t.feed("\x1b]133;D;0\x07\x1b]133;A\x07\x1b]7;file://h/repo\x07$ ");
        let r = t.feed("ls\r\n\x1b]633;E;ls\x07\x1b]133;C\x07a\r\nb\r\n");
        assert!(r.blocks.is_empty());
        let r = t.feed("\x1b]133;D;0\x07\x1b]133;A\x07$ ");
        assert_eq!(
            r.blocks,
            [CommandBlock { command: "ls".into(), cwd: Some("/repo".into()), output: "a\r\nb\r\n".into(), exit_code: Some(0) }]
        );
        // agent 的那条不进块
        t.arm();
        let r = t.feed("\x1b]633;E;pwd\x07\x1b]133;C\x07/repo\r\n\x1b]133;D;0\x07\x1b]133;A\x07");
        assert!(r.blocks.is_empty());
        assert!(r.finished.is_some());
    }

    #[test]
    fn prompt_sp_is_stripped() {
        // zsh 缺省的 PROMPT_EOL_MARK（%B%S%#%s%b）展开后的样子
        let mark = "\x1b[1m\x1b[7m%\x1b[27m\x1b[1m\x1b[0m";
        let pad = " ".repeat(79);
        // 输出以换行结尾：标记被盖掉，整段去掉
        assert_eq!(strip_prompt_sp(&format!("hello\r\n{mark}{pad}\r \r")), "hello\r\n");
        // 没以换行结尾：标记留在行尾的那一段也去掉，输出本身不动
        assert_eq!(strip_prompt_sp(&format!("a\\nb{mark}{pad}\r \r")), "a\\nb");
        // fish 多一个清行
        assert_eq!(strip_prompt_sp(&format!("x\r\n\x1b[2m⏎\x1b[0m{pad}\r \r\x1b[K")), "x\r\n");
        // 对不上的原样返回
        for s in ["", "plain\r\n", "progress 50%\r", "a\r\n   \r \r", "\r \r"] {
            assert_eq!(strip_prompt_sp(s), s, "{s:?}");
        }
    }

    #[test]
    fn huge_block_output_keeps_head_and_tail() {
        let mut t = ShellTracker::default();
        t.feed("\x1b]133;A\x07\x1b]633;E;cat big\x07\x1b]133;C\x07");
        let line = "0123456789".repeat(100) + "\r\n";
        for _ in 0..600 {
            t.feed(&line);
        }
        let r = t.feed("END\x1b]133;D;0\x07");
        let out = &r.blocks[0].output;
        assert!(out.starts_with("0123456789"));
        assert!(out.ends_with("END"));
        assert!(out.contains("省略"));
        assert!(out.len() < BLOCK_HEAD + BLOCK_TAIL + 200);
    }

    #[test]
    fn assume_prompt_only_from_unknown() {
        let mut t = ShellTracker::default();
        assert!(t.assume_prompt());
        assert_eq!(t.state(), PromptState::AtPrompt);
        t.feed("\x1b]133;C\x07");
        assert!(!t.assume_prompt());
        assert_eq!(t.state(), PromptState::Running);
    }
}
