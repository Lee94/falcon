//! AI 助手（docs/design/ai-assistant.md）里与 GPUI 无关的部分：面板里挑机器的清单与标签、底栏的 `用户@机器:~/目录`、命令输出的画法。
//!
//! 「机器」= falcon 后端本机，或一台已保存的 SSH Host（与「中转」同一个口径，ADR 0016）。
//! 机器的 key 与服务端路由参数同形：本机是 `local`，SSH Host 是它的 id。

use falcon_proto::SshHost;

/// 本机的 key（服务端 `/api/assistant/machines/local/...`）
pub const LOCAL: &str = "local";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineItem {
    pub key: String,
    /// 菜单与左上角显示的那一格
    pub label: String,
    /// 菜单里的第二行：SSH Host 的 `user@host:port`，本机没有
    pub detail: Option<String>,
}

/// 切机器的清单：本机排第一，SSH Host 按名字排（与设置里的主机列表同序）
pub fn machines(hosts: &[SshHost], local_word: &str) -> Vec<MachineItem> {
    let mut sorted: Vec<&SshHost> = hosts.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    std::iter::once(MachineItem { key: LOCAL.into(), label: local_word.into(), detail: None })
        .chain(sorted.into_iter().map(|h| MachineItem {
            key: h.id.clone(),
            label: host_label(h),
            detail: Some(host_detail(h)),
        }))
        .collect()
}

/// 这台机器还在不在（SSH Host 可能刚被删掉）
pub fn machine_exists(key: &str, hosts: &[SshHost]) -> bool {
    key == LOCAL || hosts.iter().any(|h| h.id == key)
}

/// 左上角那一格：本机就是「本机」，SSH Host 是 `用户@显示名`——显示名是用户自己起的，
/// 比 IP 好认；完整的 `user@host:port` 放在菜单的第二行
pub fn machine_label(key: &str, hosts: &[SshHost], local_word: &str) -> String {
    if key == LOCAL {
        return local_word.into();
    }
    hosts.iter().find(|h| h.id == key).map(host_label).unwrap_or_default()
}

/// 左上角那一格的完整写法：`用户@机器:~/目录`（照终端提示符的习惯）。目录与用户来自对话终端的
/// shell integration（服务端推的 `shell`）；还没有时退回 [`machine_label`]。SSH Host 的标签本身
/// 已经是 `用户@显示名`，不再重复用户
pub fn shell_title(
    key: &str,
    hosts: &[SshHost],
    local_word: &str,
    user: Option<&str>,
    cwd: Option<&str>,
    home: Option<&str>,
) -> String {
    let machine = match (key == LOCAL, user.filter(|u| !u.is_empty())) {
        (true, Some(user)) => format!("{user}@{local_word}"),
        _ => machine_label(key, hosts, local_word),
    };
    match cwd.filter(|c| !c.is_empty()) {
        Some(cwd) => format!("{machine}:{}", short_path(&tilde(cwd, home), MAX_CWD_CHARS)),
        None => machine,
    }
}

/// 左上角目录最多这么多字符，再长只留末尾几段：那一格旁边还有分段按钮和关闭键，
/// 长路径（临时目录、深层仓库）会把它们挤出底栏
const MAX_CWD_CHARS: usize = 36;

/// 太长的路径只留末尾能放下的几段，前面写成 `…/`；最后一段本身就太长时照样整段留着
pub fn short_path(path: &str, max: usize) -> String {
    if path.chars().count() <= max {
        return path.to_string();
    }
    let sep = if path.contains('\\') && !path.contains('/') { '\\' } else { '/' };
    let parts: Vec<&str> = path.split(sep).filter(|p| !p.is_empty()).collect();
    let mut tail: Vec<&str> = Vec::new();
    let mut len = 2; // `…/`
    for part in parts.iter().rev() {
        let add = part.chars().count() + usize::from(!tail.is_empty());
        if !tail.is_empty() && len + add > max {
            break;
        }
        len += add;
        tail.push(part);
    }
    tail.reverse();
    format!("…{sep}{}", tail.join(&sep.to_string()))
}

/// 家目录写成 `~`。只认整段前缀（`/home/u` 不吃 `/home/user2`）；Windows 的反斜杠也认
pub fn tilde(cwd: &str, home: Option<&str>) -> String {
    let Some(home) = home.map(|h| h.trim_end_matches(['/', '\\'])).filter(|h| !h.is_empty()) else {
        return cwd.to_string();
    };
    if cwd == home {
        return "~".into();
    }
    match cwd.strip_prefix(home) {
        Some(rest) if rest.starts_with('/') || rest.starts_with('\\') => format!("~{rest}"),
        _ => cwd.to_string(),
    }
}

fn host_label(h: &SshHost) -> String {
    format!("{}@{}", h.username, h.name)
}

fn host_detail(h: &SshHost) -> String {
    if h.port == 22 { format!("{}@{}", h.username, h.host) } else { format!("{}@{}:{}", h.username, h.host, h.port) }
}

/// 命令的原始输出（带 ANSI、`\r` 覆盖、退格）→ 纯文本：给模型看，也给卡片显示。
///
/// - 去掉 CSI（`ESC [ … 终止字节`）、OSC（`ESC ] … BEL / ST`）与其它两字节转义；
/// - 一行里的 `\r` 是回到行首重写（进度条）：只留最后一次写的内容；行尾的 `\r\n` 照常换行；
/// - 退格吃掉前一个字符；其余控制字符（响铃等）丢掉，制表符留着。
pub fn plain_output(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut line: Vec<char> = Vec::new();
    let mut col = 0usize;
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.next() {
                Some('[') => {
                    // 参数与中间字节，直到 0x40–0x7E 的终止字节
                    for c in chars.by_ref() {
                        if ('\x40'..='\x7e').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') | Some('P') | Some('_') | Some('^') => {
                    // 字符串类序列：到 BEL 或 ST 为止
                    while let Some(c) = chars.next() {
                        if c == '\x07' {
                            break;
                        }
                        if c == '\x1b' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\n' => {
                out.extend(line.drain(..));
                out.push('\n');
                col = 0;
            }
            '\r' => col = 0,
            '\x08' => col = col.saturating_sub(1),
            '\t' => put(&mut line, &mut col, '\t'),
            c if c.is_control() => {}
            c => put(&mut line, &mut col, c),
        }
    }
    out.extend(line);
    // 每行行尾的空白（进度条重写剩下的、补齐用的空格）没有信息量
    out.lines().map(str::trim_end).collect::<Vec<_>>().join("\n") + if out.ends_with('\n') { "\n" } else { "" }
}

fn put(line: &mut Vec<char>, col: &mut usize, c: char) {
    if *col < line.len() {
        line[*col] = c;
    } else {
        line.push(c);
    }
    *col += 1;
}

/// 终端颜色：调色板里的 16 色、256 色的序号，或者 24 位真彩。界面按主题的调色板换成颜色
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TermColor {
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// 一段同样样式的文字
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StyledSpan {
    pub text: String,
    pub fg: Option<TermColor>,
    pub bg: Option<TermColor>,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Sgr {
    fg: Option<TermColor>,
    bg: Option<TermColor>,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    inverse: bool,
}

impl Sgr {
    fn apply(&mut self, params: &[u16]) {
        if params.is_empty() {
            *self = Sgr::default();
            return;
        }
        let mut i = 0;
        while i < params.len() {
            match params[i] {
                0 => *self = Sgr::default(),
                1 => self.bold = true,
                2 => self.dim = true,
                3 => self.italic = true,
                4 => self.underline = true,
                7 => self.inverse = true,
                22 => (self.bold, self.dim) = (false, false),
                23 => self.italic = false,
                24 => self.underline = false,
                27 => self.inverse = false,
                n @ 30..=37 => self.fg = Some(TermColor::Indexed((n - 30) as u8)),
                n @ 90..=97 => self.fg = Some(TermColor::Indexed((n - 90 + 8) as u8)),
                39 => self.fg = None,
                n @ 40..=47 => self.bg = Some(TermColor::Indexed((n - 40) as u8)),
                n @ 100..=107 => self.bg = Some(TermColor::Indexed((n - 100 + 8) as u8)),
                49 => self.bg = None,
                n @ (38 | 48) => {
                    let color = match params.get(i + 1) {
                        Some(5) => {
                            let c = params.get(i + 2).map(|c| TermColor::Indexed(*c as u8));
                            i += 2;
                            c
                        }
                        Some(2) => {
                            let p = |k: usize| params.get(i + k).copied().unwrap_or(0) as u8;
                            let c = Some(TermColor::Rgb(p(2), p(3), p(4)));
                            i += 4;
                            c
                        }
                        _ => None,
                    };
                    if n == 38 { self.fg = color } else { self.bg = color }
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn span(&self, text: String) -> StyledSpan {
        let (fg, bg) = if self.inverse { (self.bg.or(Some(TermColor::Indexed(0))), self.fg.or(Some(TermColor::Indexed(7)))) } else { (self.fg, self.bg) };
        StyledSpan { text, fg, bg, bold: self.bold, dim: self.dim, italic: self.italic, underline: self.underline }
    }
}

/// 命令的原始输出 → 一行行带样式的文字（命令块的静态画法：不为每块开一个终端模拟器）。
///
/// 口径同 [`plain_output`]：`\r` 回到行首重写、退格吃一个字符、只认 SGR（颜色、粗、暗、斜、下划线、
/// 反显），其它转义丢掉。光标上移重画（进度条、TUI）画不对，那类输出本来也不该当静态文字看。
/// 最多留最后 `max_lines` 行
pub fn styled_output(raw: &str, max_lines: usize) -> Vec<Vec<StyledSpan>> {
    let mut lines: Vec<Vec<(char, Sgr)>> = Vec::new();
    let mut line: Vec<(char, Sgr)> = Vec::new();
    let mut col = 0usize;
    let mut sgr = Sgr::default();
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.next() {
                Some('[') => {
                    let mut params = String::new();
                    let mut fin = None;
                    for c in chars.by_ref() {
                        if ('\x40'..='\x7e').contains(&c) {
                            fin = Some(c);
                            break;
                        }
                        params.push(c);
                    }
                    if fin == Some('m') && !params.starts_with(['?', '>', '<', '=']) {
                        let nums: Vec<u16> = params.split([';', ':']).map(|p| p.parse().unwrap_or(0)).collect();
                        sgr.apply(if params.is_empty() { &[] } else { &nums });
                    }
                }
                Some(']') | Some('P') | Some('_') | Some('^') => {
                    while let Some(c) = chars.next() {
                        if c == '\x07' {
                            break;
                        }
                        if c == '\x1b' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\n' => {
                lines.push(std::mem::take(&mut line));
                col = 0;
                if lines.len() > max_lines * 2 {
                    lines.drain(..lines.len() - max_lines);
                }
            }
            '\r' => col = 0,
            '\x08' => col = col.saturating_sub(1),
            c if c.is_control() && c != '\t' => {}
            c => {
                if col < line.len() {
                    line[col] = (c, sgr);
                } else {
                    line.push((c, sgr));
                }
                col += 1;
            }
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    let skip = lines.len().saturating_sub(max_lines);
    let mut out = lines
        .into_iter()
        .skip(skip)
        .map(|cells| {
            // 行尾空白（重写剩下的、补齐用的）没有信息量；有底色的保留
            let end = cells.iter().rposition(|(c, s)| !c.is_whitespace() || s.bg.is_some()).map_or(0, |i| i + 1);
            let mut spans: Vec<StyledSpan> = Vec::new();
            let mut cur: Option<(Sgr, String)> = None;
            for (c, s) in &cells[..end] {
                match &mut cur {
                    Some((cs, text)) if cs == s => text.push(*c),
                    _ => {
                        if let Some((cs, text)) = cur.take() {
                            spans.push(cs.span(text));
                        }
                        cur = Some((*s, c.to_string()));
                    }
                }
            }
            if let Some((cs, text)) = cur {
                spans.push(cs.span(text));
            }
            spans
        })
        .collect::<Vec<_>>();
    // 末尾的空行（只剩补齐的空格、被回车盖成空白的）不画：块的底边不该多出一截
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out
}

/// 超过 `max` 个字符就掐掉中间，留头留尾——命令输出的开头（报错位置）与结尾（结论）最有用
pub fn truncate_middle(text: &str, max: usize) -> String {
    let n = text.chars().count();
    if n <= max {
        return text.to_string();
    }
    let keep = max / 2;
    let head: String = text.chars().take(keep).collect();
    let tail: String = text.chars().skip(n - keep).collect();
    format!("{head}\n…（中间省略 {} 个字符）…\n{tail}", n - 2 * keep)
}

/// 卡片上 diff 的一行
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffLine {
    Same(String),
    Removed(String),
    Added(String),
    /// 中间省掉的这么多行没变的
    Gap(usize),
}

/// 两段文字的行级 diff，没变的行只在改动前后各留 `context` 行。LCS（O(n·m)），一边超过
/// `MAX_DIFF_LINES` 行就不算了，退成「全删 + 全加」——卡片上本来也只画得下开头一截
pub fn line_diff(old: &str, new: &str, context: usize) -> Vec<DiffLine> {
    const MAX_DIFF_LINES: usize = 600;
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let raw: Vec<DiffLine> = if a.len() > MAX_DIFF_LINES || b.len() > MAX_DIFF_LINES {
        a.iter().map(|l| DiffLine::Removed((*l).into())).chain(b.iter().map(|l| DiffLine::Added((*l).into()))).collect()
    } else {
        // lcs[i][j] = a[i..] 与 b[j..] 的最长公共子序列长度
        let (n, m) = (a.len(), b.len());
        let mut lcs = vec![vec![0u32; m + 1]; n + 1];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                lcs[i][j] = if a[i] == b[j] { lcs[i + 1][j + 1] + 1 } else { lcs[i + 1][j].max(lcs[i][j + 1]) };
            }
        }
        let (mut i, mut j) = (0, 0);
        let mut out = Vec::new();
        while i < n || j < m {
            if i < n && j < m && a[i] == b[j] {
                out.push(DiffLine::Same(a[i].into()));
                i += 1;
                j += 1;
            } else if i < n && (j == m || lcs[i + 1][j] >= lcs[i][j + 1]) {
                // 平局先删后加：diff 的惯例，`-旧` 在 `+新` 上面
                out.push(DiffLine::Removed(a[i].into()));
                i += 1;
            } else {
                out.push(DiffLine::Added(b[j].into()));
                j += 1;
            }
        }
        out
    };
    // 没变的行：离改动不到 context 行的留着，其余折成 Gap
    let changed: Vec<usize> = raw.iter().enumerate().filter(|(_, l)| !matches!(l, DiffLine::Same(_))).map(|(i, _)| i).collect();
    let near = |i: usize| changed.iter().any(|&c| c.abs_diff(i) <= context);
    let mut out = Vec::new();
    let mut gap = 0;
    for (i, line) in raw.into_iter().enumerate() {
        if matches!(line, DiffLine::Same(_)) && !near(i) {
            gap += 1;
            continue;
        }
        if gap > 0 {
            out.push(DiffLine::Gap(gap));
            gap = 0;
        }
        out.push(line);
    }
    if gap > 0 {
        out.push(DiffLine::Gap(gap));
    }
    out
}

/// 设置里一行命令（`npx -y @agentclientprotocol/claude-agent-acp`）→ 程序 + 参数。认单引号、
/// 双引号与反斜杠转义，够写路径里带空格的命令；不做变量展开（不经 shell）
pub fn split_command(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut has = false;
    let mut chars = line.chars();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') | (None, '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            (Some(_), c) => cur.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                has = true;
            }
            (None, c) if c.is_whitespace() => {
                if has || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// [`split_command`] 的反向：程序 + 参数拼回一行，带空格 / 引号的词加双引号
pub fn join_command(command: &str, args: &[String]) -> String {
    std::iter::once(command)
        .chain(args.iter().map(String::as_str))
        .map(|w| {
            if !w.is_empty() && !w.chars().any(|c| c.is_whitespace() || "'\"\\".contains(c)) {
                w.to_string()
            } else {
                // 双引号：split_command 在双引号里认反斜杠转义，单引号里不认
                format!("\"{}\"", w.replace('\\', "\\\\").replace('"', "\\\""))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(id: &str, name: &str, port: u16) -> SshHost {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": name, "host": format!("{name}.lan"), "port": port, "username": "fay",
            "authMethod": "agent", "hasSecret": false, "projectCount": 0, "createdAt": 1,
        }))
        .unwrap()
    }

    #[test]
    fn local_first_then_hosts_by_name() {
        let hosts = [host("b", "dev", 22), host("a", "box", 2222)];
        let list = machines(&hosts, "本机");
        let keys: Vec<&str> = list.iter().map(|m| m.key.as_str()).collect();
        assert_eq!(keys, ["local", "a", "b"]);
        assert_eq!(list[0].detail, None);
        assert_eq!(list[1].label, "fay@box");
        assert_eq!(list[1].detail.as_deref(), Some("fay@box.lan:2222"));
        assert_eq!(list[2].detail.as_deref(), Some("fay@dev.lan"));
    }

    #[test]
    fn shell_title_reads_like_a_prompt() {
        let hosts = [host("a", "box", 22)];
        assert_eq!(shell_title(LOCAL, &hosts, "本机", Some("fay"), Some("/Users/fay/Code"), Some("/Users/fay")), "fay@本机:~/Code");
        assert_eq!(shell_title(LOCAL, &hosts, "本机", None, None, None), "本机");
        assert_eq!(shell_title(LOCAL, &hosts, "本机", Some("fay"), None, None), "fay@本机");
        assert_eq!(shell_title("a", &hosts, "本机", Some("root"), Some("/srv"), Some("/root")), "fay@box:/srv");
    }

    #[test]
    fn long_paths_keep_their_tail() {
        assert_eq!(short_path("~/Code/mojito", 36), "~/Code/mojito");
        assert_eq!(
            short_path("/private/tmp/claude-501/-Volumes-faye-Code-mojito/c6180991/scratchpad/work", 36),
            "…/c6180991/scratchpad/work"
        );
        assert_eq!(short_path("/a/averyveryveryverylongdirectoryname", 10), "…/averyveryveryverylongdirectoryname");
    }

    #[test]
    fn tilde_only_on_whole_segments() {
        assert_eq!(tilde("/home/u", Some("/home/u")), "~");
        assert_eq!(tilde("/home/u/x", Some("/home/u/")), "~/x");
        assert_eq!(tilde("/home/user2", Some("/home/u")), "/home/user2");
        assert_eq!(tilde("/tmp", None), "/tmp");
        assert_eq!(tilde("C:\\Users\\a\\x", Some("C:\\Users\\a")), "~\\x");
    }

    #[test]
    fn plain_output_strips_escapes_and_applies_carriage_returns() {
        assert_eq!(plain_output("\x1b[1;31mred\x1b[0m ok\r\n"), "red ok\n");
        // 进度条：同一行里 \r 重写，只留最后一次
        assert_eq!(plain_output("10%\r50%\r100%\r\ndone\n"), "100%\ndone\n");
        assert_eq!(plain_output("abc\x08\x08X\n"), "aXc\n");
        assert_eq!(plain_output("\x1b]0;title\x07\x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\\x07\n"), "link\n");
        assert_eq!(plain_output("no newline"), "no newline");
        assert_eq!(plain_output("pad   \r\nx"), "pad\nx");
    }

    #[test]
    fn styled_output_drops_trailing_blank_lines() {
        assert_eq!(styled_output("hi\r\n   \r\n\r\n", 10).len(), 1);
        assert!(styled_output("  \r\n", 10).is_empty());
        // 中间的空行留着
        assert_eq!(styled_output("a\r\n\r\nb\r\n", 10).len(), 3);
    }

    #[test]
    fn styled_output_keeps_colors_and_applies_rewrites() {
        let lines = styled_output("\x1b[1;31merror\x1b[0m: bad\r\n10%\r99%\r\n\x1b[38;5;208mo\x1b[38;2;1;2;3mr\x1b[m\n", 100);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0][0], StyledSpan { text: "error".into(), fg: Some(TermColor::Indexed(1)), bold: true, ..Default::default() });
        assert_eq!(lines[0][1].text, ": bad");
        assert_eq!(lines[0][1].fg, None);
        assert_eq!(lines[1][0].text, "99%");
        assert_eq!(lines[2][0].fg, Some(TermColor::Indexed(208)));
        assert_eq!(lines[2][1].fg, Some(TermColor::Rgb(1, 2, 3)));
        // 只留最后几行
        let many: String = (0..50).map(|i| format!("l{i}\n")).collect();
        let tail = styled_output(&many, 3);
        assert_eq!(tail.iter().map(|l| l[0].text.clone()).collect::<Vec<_>>(), ["l47", "l48", "l49"]);
        // 反显：前景背景对调
        let inv = styled_output("\x1b[7mx\x1b[27m", 10);
        assert_eq!(inv[0][0].bg, Some(TermColor::Indexed(7)));
    }

    #[test]
    fn truncate_keeps_head_and_tail() {
        assert_eq!(truncate_middle("short", 10), "short");
        let t = truncate_middle(&"a".repeat(10).chars().chain("b".repeat(10).chars()).collect::<String>(), 10);
        assert!(t.starts_with("aaaaa\n…"));
        assert!(t.ends_with("…\nbbbbb"));
        assert!(t.contains("10"));
    }

    #[test]
    fn line_diff_keeps_context_around_changes() {
        use DiffLine::*;
        let old = "a\nb\nc\nd\ne\nf\ng\nh";
        let new = "a\nb\nc\nd\nE\nf\ng\nh";
        assert_eq!(
            line_diff(old, new, 1),
            [Gap(3), Same("d".into()), Removed("e".into()), Added("E".into()), Same("f".into()), Gap(2)]
        );
        // 新文件：全是加
        assert_eq!(line_diff("", "x\ny", 3), [Added("x".into()), Added("y".into())]);
        assert_eq!(line_diff("same", "same", 3), [Gap(1)]);
    }

    #[test]
    fn command_lines_split_and_join_back() {
        assert_eq!(split_command("npx -y @agentclientprotocol/claude-agent-acp"), ["npx", "-y", "@agentclientprotocol/claude-agent-acp"]);
        assert_eq!(split_command("  '/Applications/My Agent/bin/agent'  --acp \"a b\" c\\ d ''"), ["/Applications/My Agent/bin/agent", "--acp", "a b", "c d", ""]);
        assert!(split_command("   ").is_empty());
        let args = vec!["--acp".to_string(), "a b".to_string()];
        let line = join_command("/opt/my agent", &args);
        assert_eq!(line, "\"/opt/my agent\" --acp \"a b\"");
        assert_eq!(split_command(&line), ["/opt/my agent", "--acp", "a b"]);
        let tricky = vec!["it's".to_string(), "say \"hi\"".to_string(), "c:\\x".to_string(), String::new()];
        assert_eq!(split_command(&join_command("p", &tricky))[1..], tricky[..]);
    }

    #[test]
    fn labels_and_existence() {
        let hosts = [host("a", "box", 22)];
        assert_eq!(machine_label(LOCAL, &hosts, "本机"), "本机");
        assert_eq!(machine_label("a", &hosts, "本机"), "fay@box");
        assert_eq!(machine_label("gone", &hosts, "本机"), "");
        assert!(machine_exists(LOCAL, &[]));
        assert!(machine_exists("a", &hosts));
        assert!(!machine_exists("gone", &hosts));
    }
}
