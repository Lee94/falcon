//! 终端里的链接：OSC 8 超链接（cell 上的 hyperlink）与正文里认出来的 URL。正则照
//! `@xterm/addon-web-links` 的默认正则（旧 React 版用的那个），认出的链接范围与那时一致。
//!
//! 两种都可以跨行：正文 URL 沿软折行（行尾格带 WRAPLINE）拼成逻辑行再认——长链接折到下一行是
//! 常态（`git push` 给的 PR 地址、OAuth 登录地址），只认一行时点上半截开的是截断的地址、点下半截
//! 什么都不开；OSC 8 按格子上的 URI 连续延伸，不看 WRAPLINE（自己硬折行的程序每段都带着整条 URI，
//! 中间隔着缩进就自然断开）。程序硬折行（自己插换行）的纯文本 URL 认不全，那是 OSC 8 的活。
//! 只看可见区：逻辑行从可见区上沿之外折下来的，只认得出可见的那截。

use std::sync::OnceLock;

use alacritty_terminal::term::cell::Flags;
use regex::Regex;

use crate::core::Snapshot;

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

/// 链接在可见区里的范围：`start` 到 `end`（(行, 列)，行优先，含首不含尾），可以跨行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkSpan {
    pub start: (usize, usize),
    pub end: (usize, usize),
    pub url: String,
}

impl LinkSpan {
    pub fn contains(&self, row: usize, col: usize) -> bool {
        (row, col) >= self.start && (row, col) < self.end
    }
}

impl Snapshot {
    /// 可见区 (行, 列) 上的链接：OSC 8 优先，其次正文里的 URL。
    pub fn link_at(&self, row: usize, col: usize) -> Option<LinkSpan> {
        let cols = self.size.cols;
        if row >= self.size.rows || col >= cols {
            return None;
        }
        if let Some(link) = self.cell(row, col).hyperlink() {
            let uri = link.uri().to_string();
            let same = |i: usize| self.cells[i].hyperlink().is_some_and(|h| h.uri() == uri);
            let here = row * cols + col;
            let mut start = here;
            while start > 0 && same(start - 1) {
                start -= 1;
            }
            let mut end = here + 1;
            while end < self.cells.len() && same(end) {
                end += 1;
            }
            return Some(LinkSpan { start: (start / cols, start % cols), end: (end / cols, end % cols), url: uri });
        }
        let (text, cells) = self.logical_line(row);
        find_urls(&text).into_iter().find_map(|(start, end, url)| {
            let (&first, &(last_row, last_col)) = (cells.get(start)?, cells.get(end.checked_sub(1)?)?);
            let span = LinkSpan { start: first, end: (last_row, last_col + 1), url };
            span.contains(row, col).then_some(span)
        })
    }

    /// 行尾那格带 WRAPLINE：这一行是软折行，下一行接着它
    fn wraps(&self, row: usize) -> bool {
        self.size.cols > 0 && self.cell(row, self.size.cols - 1).flags.contains(Flags::WRAPLINE)
    }

    /// `row` 所在的逻辑行（沿软折行上下拼接，止于可见区边缘）的文本，以及每个 char 所在的
    /// (行, 列)。宽字符的占位格跳过；宽字符在行尾放不下、挪到下一行时留在行尾的那个占位格也跳过，
    /// 不然它会变成一个空格把 URL 截断
    fn logical_line(&self, row: usize) -> (String, Vec<(usize, usize)>) {
        let mut first = row;
        while first > 0 && self.wraps(first - 1) {
            first -= 1;
        }
        let mut last = row;
        while last + 1 < self.size.rows && self.wraps(last) {
            last += 1;
        }
        let mut text = String::new();
        let mut cells = Vec::new();
        for r in first..=last {
            for c in 0..self.size.cols {
                let cell = self.cell(r, c);
                if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                    continue;
                }
                text.push(cell.c);
                cells.push((r, c));
            }
        }
        (text, cells)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{TermCore, TermOptions, TermSize};

    fn snap(cols: usize, rows: usize, bytes: &[u8]) -> Snapshot {
        let core = TermCore::new(TermSize::new(cols, rows), TermOptions::default());
        core.advance(bytes);
        core.snapshot()
    }

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

    #[test]
    fn url_on_one_row() {
        let s = snap(40, 3, b"see https://a.io/x now");
        let link = s.link_at(0, 6).unwrap();
        assert_eq!(link, LinkSpan { start: (0, 4), end: (0, 18), url: "https://a.io/x".into() });
        assert!(s.link_at(0, 2).is_none());
        assert!(s.link_at(0, 19).is_none());
    }

    #[test]
    fn url_across_soft_wraps_is_whole_from_every_row() {
        // 10 列：`> ` + 28 个字符的 URL 折成三行
        let url = "https://example.com/abcdefgh";
        let s = snap(10, 4, format!("> {url} end").as_bytes());
        for (row, col) in [(0, 3), (1, 0), (1, 9), (2, 9)] {
            let link = s.link_at(row, col).unwrap_or_else(|| panic!("({row}, {col}) 没认出链接"));
            assert_eq!(link.url, url);
            assert_eq!((link.start, link.end), ((0, 2), (2, 10)));
        }
        assert!(s.link_at(3, 1).is_none(), "` end` 不在链接里");
    }

    #[test]
    fn hard_line_breaks_split_urls() {
        // 程序自己插的换行没有 WRAPLINE：两行各认各的
        let s = snap(40, 3, b"https://a.io/one\r\nhttps://b.io/two");
        assert_eq!(s.link_at(0, 3).unwrap().url, "https://a.io/one");
        assert_eq!(s.link_at(1, 3).unwrap().url, "https://b.io/two");
    }

    #[test]
    fn wide_chars_before_url_keep_columns_right() {
        let s = snap(30, 2, "中文 http://x.io/a".as_bytes());
        let link = s.link_at(0, 6).unwrap();
        assert_eq!(link.url, "http://x.io/a");
        assert_eq!((link.start, link.end), ((0, 5), (0, 18)));
    }

    #[test]
    fn osc8_wins_and_spans_rows() {
        // 8 列，链接文字 12 个字符折成两行；URI 与文字无关
        let s = snap(8, 3, b"\x1b]8;;https://osc.io/p\x1b\\click-me-now\x1b]8;;\x1b\\ tail");
        let link = s.link_at(1, 2).unwrap();
        assert_eq!(link, LinkSpan { start: (0, 0), end: (1, 4), url: "https://osc.io/p".into() });
        assert_eq!(s.link_at(0, 0).unwrap(), link);
        assert!(s.link_at(1, 5).is_none());
    }
}
