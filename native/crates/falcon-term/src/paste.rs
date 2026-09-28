//! 粘贴文本 → 终端输入。口径照 xterm.js（web 的默认引擎，`browser/Clipboard.ts`），两个客户端
//! 往会话里送的字节一致：
//!
//! - 换行一律归一成 `\r`（`\r?\n` → `\r`）：终端里回车才是"提交这一行"；
//! - bracketed paste（`?2004h`）开着时包 `ESC[200~ … ESC[201~`，并把载荷里的 ESC 换成可见的
//!   U+241B（␛）。不换的话，粘贴内容里夹一个 `ESC[201~` 就能提前关掉括号，后面的内容被 shell
//!   当成键入逐行执行——这是粘贴注入的经典手法。
//!
//! 没开 bracketed paste 时多行文本会被逐行执行，这是终端协议本身的语义，这里不替程序兜底。

pub fn encode_paste(text: &str, bracketed: bool) -> String {
    let normalized = text.replace("\r\n", "\r").replace('\n', "\r");
    if !bracketed {
        return normalized;
    }
    let sanitized = normalized.replace('\x1b', "\u{241b}");
    format!("\x1b[200~{sanitized}\x1b[201~")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_newlines() {
        assert_eq!(encode_paste("a\r\nb\nc", false), "a\rb\rc");
    }

    #[test]
    fn brackets_and_sanitizes() {
        assert_eq!(encode_paste("ls\n", true), "\x1b[200~ls\r\x1b[201~");
        assert_eq!(
            encode_paste("x\x1b[201~rm -rf ~\n", true),
            "\x1b[200~x\u{241b}[201~rm -rf ~\r\x1b[201~"
        );
    }
}
