//! 查看窗口里编辑文本的两头：磁盘上的文本 ↔ 编辑器里的内容。
//!
//! 编辑器（gpui-component 的代码编辑器）显示时吞掉末尾那个换行——POSIX 文本以换行结尾，
//! 不吞就多出一个幽灵空行；回车又只插 `\n`。存回去时把这两件事按原文件还原：
//! - 原文件**全是** CRLF 换行，编辑里新敲的 `\n` 也写成 `\r\n`：Windows 仓库里的文件不该因为
//!   改了一行就变成混合换行（git diff 整片飘红）。混着的、全是 LF 的、一个换行都没有的，原样不动；
//! - 原文件以换行结尾，存回去也以同一种换行结尾；原来没有就不加。末尾有没有换行跟着原文件走，
//!   编辑器里改不了这一点（与 vim 的 fixeol 是同一个取舍），换来的是"只改了一个字，diff 就只有一个字"。

/// 原文件的形状：存回去时据此还原
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextShape {
    /// 每个换行都是 `\r\n`（且至少有一个）
    pub crlf: bool,
    /// 末尾那个被吞掉的换行（`"\n"` / `"\r\n"`），没有是 `None`
    pub trailing: Option<&'static str>,
}

/// 磁盘上的文本 → (编辑器里显示的内容, 原文件的形状)
pub fn for_editor(text: &str) -> (&str, TextShape) {
    let lf = text.matches('\n').count();
    let crlf = lf > 0 && text.matches("\r\n").count() == lf;
    let (body, trailing) = if let Some(b) = text.strip_suffix("\r\n") {
        (b, Some("\r\n"))
    } else if let Some(b) = text.strip_suffix('\n') {
        (b, Some("\n"))
    } else {
        (text, None)
    };
    (body, TextShape { crlf, trailing })
}

/// 编辑器里的内容 → 要写回磁盘的文本（[`for_editor`] 的逆运算：没改过的内容原样还原）
pub fn for_disk(body: &str, shape: TextShape) -> String {
    let mut out = String::with_capacity(body.len() + body.len() / 32 + 2);
    if shape.crlf {
        let mut prev = '\0';
        for c in body.chars() {
            if c == '\n' && prev != '\r' {
                out.push('\r');
            }
            out.push(c);
            prev = c;
        }
    } else {
        out.push_str(body);
    }
    if let Some(t) = shape.trailing {
        out.push_str(t);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untouched_text_roundtrips() {
        for text in ["", "a", "a\n", "a\nb\n", "a\r\nb\r\n", "a\r\nb", "mixed\r\nlf\n", "\n", "\r\n", "a\n\n", "中文\r\n"] {
            let (body, shape) = for_editor(text);
            assert_eq!(for_disk(body, shape), text, "{text:?}");
        }
    }

    #[test]
    fn swallows_one_trailing_newline() {
        assert_eq!(for_editor("a\n\n").0, "a\n");
        // CRLF 文件吞整个 \r\n，不留一个孤零零的 \r 在行尾
        assert_eq!(for_editor("a\r\n").0, "a");
        assert_eq!(for_editor("a").1.trailing, None);
    }

    #[test]
    fn new_lines_follow_a_crlf_file() {
        let (_, shape) = for_editor("a\r\nb\r\n");
        assert!(shape.crlf);
        assert_eq!(for_disk("a\r\nx\ny\r\nb", shape), "a\r\nx\r\ny\r\nb\r\n");
        // 混合换行的文件不去"纠正"它
        let (_, mixed) = for_editor("a\r\nb\n");
        assert!(!mixed.crlf);
        assert_eq!(for_disk("a\r\nb\nc", mixed), "a\r\nb\nc\n");
        // 没有换行的文件按 LF 处理
        let (_, single) = for_editor("a");
        assert!(!single.crlf);
        assert_eq!(for_disk("a\nb", single), "a\nb");
    }
}
