//! 终端文本里的 URL 识别。正则照 `@xterm/addon-web-links` 的默认正则（旧 React 版用的那个），认出的
//! 链接范围与那时一致（OSC 8 超链接另走 cell 上的 hyperlink，不经过这里）。

use std::sync::OnceLock;

use regex::Regex;

/// addon-web-links 的 strictUrlRegex：`https?://` 开头，结尾不收常见的收尾标点与括号。
fn url_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(https?|HTTPS?)://[^\s"'!*(){}|\\^<>`]*[^\s"':,.!?{}|\\^~\[\]`()<>]"#,
        )
        .expect("url regex")
    })
}

/// 一行文本里的全部 URL，返回 (起始列, 结束列（不含）, URL)。列按 char 计——调用方给的行文本
/// 要一格一个 char（宽字符的占位格已跳过时，列号要调用方自己换算）。
pub fn find_urls(line: &str) -> Vec<(usize, usize, String)> {
    url_regex()
        .find_iter(line)
        .map(|m| {
            let start = line[..m.start()].chars().count();
            let len = m.as_str().chars().count();
            (start, start + len, m.as_str().to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_urls_and_trims_trailing_punctuation() {
        let found = find_urls("see https://example.com/a?b=1. and (http://x.io/y)");
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].2, "https://example.com/a?b=1");
        assert_eq!(found[1].2, "http://x.io/y");
        assert_eq!(found[0].0, 4);
    }

    #[test]
    fn counts_columns_in_chars() {
        let found = find_urls("中文 https://a.b");
        assert_eq!(found[0].0, 3);
    }
}
