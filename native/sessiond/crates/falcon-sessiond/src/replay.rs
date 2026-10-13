//! 跑在字节上的两个纯函数：导出快照后的调色板改写、回 PTY 前的 kitty 键盘回复过滤。

/// 把 SGR 里的 `38;5;N` / `48;5;N`（N < 16）改写成 `3N` / `9(N-8)` / `4N` / `10(N-8)`。
///
/// libghostty-vt 的格式化器把 0–15 号色一律写成 256 色的形式；客户端的 alacritty 收到
/// `38;5;1` 存成 `Indexed(1)`，收到 `31` 存成 `Named(Red)`——同一个调色板槽位，但
/// 「粗体用亮色」只认 Named，回放后粗体字的颜色就和实时流里不一样了（原型逐格比对实测）。
/// 冒号形式（`38:5:N`）同样处理。别的序列原样不动。
pub fn rewrite_palette16(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if input[i] == 0x1b && input.get(i + 1) == Some(&b'[') {
            // 找 SGR：参数只含数字、`;`、`:`，以 `m` 结尾
            let start = i + 2;
            let mut j = start;
            while j < input.len() && (input[j].is_ascii_digit() || input[j] == b';' || input[j] == b':') {
                j += 1;
            }
            if j < input.len() && input[j] == b'm' {
                out.extend_from_slice(b"\x1b[");
                out.extend_from_slice(&rewrite_params(&input[start..j]));
                out.push(b'm');
                i = j + 1;
                continue;
            }
        }
        out.push(input[i]);
        i += 1;
    }
    out
}

fn rewrite_params(params: &[u8]) -> Vec<u8> {
    let s = String::from_utf8_lossy(params);
    let toks: Vec<&str> = s.split(';').collect();
    let mut out: Vec<String> = Vec::with_capacity(toks.len());
    let mut k = 0;
    while k < toks.len() {
        let t = toks[k];
        // 冒号形式自成一个参数
        if let Some(rest) = t.strip_prefix("38:5:").or_else(|| t.strip_prefix("48:5:")) {
            if let Some(code) = short_code(t.starts_with("38"), rest) {
                out.push(code);
                k += 1;
                continue;
            }
        }
        if (t == "38" || t == "48") && toks.get(k + 1) == Some(&"5") {
            if let Some(code) = toks.get(k + 2).and_then(|n| short_code(t == "38", n)) {
                out.push(code);
                k += 3;
                continue;
            }
        }
        out.push(t.to_string());
        k += 1;
    }
    out.join(";").into_bytes()
}

fn short_code(fg: bool, n: &str) -> Option<String> {
    let n: u8 = n.parse().ok()?;
    Some(match (fg, n) {
        (true, 0..=7) => format!("{}", 30 + n),
        (true, 8..=15) => format!("{}", 90 + n - 8),
        (false, 0..=7) => format!("{}", 40 + n),
        (false, 8..=15) => format!("{}", 100 + n - 8),
        _ => return None,
    })
}

/// 去掉 kitty 键盘协议查询（`CSI ? u`）的回答 `CSI ? <flags> u`。
///
/// libghostty-vt 缺省会回 `CSI ? 0 u`，等于声称支持 kitty 键盘——程序就会改发 kitty 编码的
/// 按键请求，而客户端的 `falcon-term/src/keys.rs` 只有传统 xterm 编码。不回答时程序先收到
/// DA 的回答，就判定不支持（ADR 0020 决定五）。等客户端实现 CSI u 编码再放开。
pub fn strip_kitty_query_reply(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if data[i..].starts_with(b"\x1b[?") {
            let mut j = i + 3;
            while j < data.len() && data[j].is_ascii_digit() {
                j += 1;
            }
            if j < data.len() && data[j] == b'u' {
                i = j + 1;
                continue;
            }
        }
        out.push(data[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_the_sixteen_colors_and_leaves_the_rest() {
        assert_eq!(rewrite_palette16(b"\x1b[0;38;5;1mred"), b"\x1b[0;31mred");
        assert_eq!(rewrite_palette16(b"\x1b[38;5;9;48;5;4m"), b"\x1b[91;44m");
        assert_eq!(rewrite_palette16(b"\x1b[48;5;15m"), b"\x1b[107m");
        assert_eq!(rewrite_palette16(b"\x1b[38;5;16m"), b"\x1b[38;5;16m", "16 以上是真 256 色");
        assert_eq!(rewrite_palette16(b"\x1b[38;2;1;2;3m"), b"\x1b[38;2;1;2;3m", "真彩不动");
        assert_eq!(rewrite_palette16(b"\x1b[38:5:3m"), b"\x1b[33m");
        assert_eq!(rewrite_palette16(b"\x1b[58;5;1m"), b"\x1b[58;5;1m", "下划线颜色不动");
        assert_eq!(rewrite_palette16(b"\x1b[1;5H\x1b[?25h"), b"\x1b[1;5H\x1b[?25h", "非 SGR 不动");
        assert_eq!(rewrite_palette16("中\x1b[38;5;2m文".as_bytes()), "中\x1b[32m文".as_bytes());
        assert_eq!(rewrite_palette16(b"\x1b[38;5"), b"\x1b[38;5", "半截序列原样");
    }

    #[test]
    fn strips_only_the_kitty_query_reply() {
        assert_eq!(strip_kitty_query_reply(b"\x1b[?0u"), b"");
        assert_eq!(strip_kitty_query_reply(b"\x1b[?0u\x1b[?62;22c"), b"\x1b[?62;22c");
        assert_eq!(strip_kitty_query_reply(b"\x1b[?2004;2$y"), b"\x1b[?2004;2$y");
        assert_eq!(strip_kitty_query_reply(b"\x1b[3;5R"), b"\x1b[3;5R");
    }
}
