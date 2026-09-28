//! Quick Open（⌘P）的客户端过滤。对应 web 的 `lib/fileSearch.ts`。
//!
//! 后端给整份路径清单，这里按 VS Code 的直觉打分：文件名命中优于路径命中，前缀优于
//! 包含，子序列垫底。两个客户端对同一个查询必须排出同一个顺序
//! （`tests/vectors/fileSearch.json`）。

use crate::js::{js_trim, locale_compare, utf16_len};

pub const FILE_SEARCH_LIMIT: usize = 50;

pub fn basename(path: &str) -> &str {
    path.rfind('/').map(|i| &path[i + 1..]).unwrap_or(path)
}

pub fn dirname(path: &str) -> &str {
    path.rfind('/').map(|i| &path[..i]).unwrap_or("")
}

/// needle 的字符按顺序出现在 hay 里（不要求相邻）。
///
/// 已知出入：TS 拿 hay 的码位与 needle 的 **UTF-16 码元**比（`needle[i]`），needle 里
/// 有 BMP 之外的字符（emoji）时永远对不上；这里按字符比，能对上。
fn subseq(hay: &str, needle: &[char]) -> bool {
    let mut i = 0;
    for c in hay.chars() {
        if needle.get(i) == Some(&c) {
            i += 1;
        }
        if i == needle.len() {
            return true;
        }
    }
    false
}

/// 越小越靠前。对不上返回 `None`
pub fn score_path(path: &str, needle: &str) -> Option<u32> {
    let q = js_trim(needle).to_lowercase();
    if q.is_empty() {
        return Some(0);
    }
    let base = basename(path).to_lowercase();
    let lower = path.to_lowercase();
    let qc: Vec<char> = q.chars().collect();
    if base == q {
        Some(0)
    } else if base.starts_with(&q) {
        Some(1)
    } else if base.contains(&q) {
        Some(2)
    } else if lower.contains(&q) {
        Some(3)
    } else if subseq(&base, &qc) {
        Some(4)
    } else if subseq(&lower, &qc) {
        Some(5)
    } else {
        None
    }
}

/// 按分数、再按路径长度（UTF-16 码元，照 JS 的 `length`）、再按 localeCompare 排，
/// 取前 `limit` 条。`limit` 在 TS 里缺省是 [`FILE_SEARCH_LIMIT`]。
pub fn filter_files<S: AsRef<str>>(paths: &[S], query: &str, limit: usize) -> Vec<String> {
    let needle = js_trim(query);
    if needle.is_empty() {
        return paths.iter().take(limit).map(|p| p.as_ref().to_string()).collect();
    }
    let mut scored: Vec<(&str, u32, usize)> = paths
        .iter()
        .filter_map(|p| {
            let p = p.as_ref();
            score_path(p, needle).map(|s| (p, s, utf16_len(p)))
        })
        .collect();
    scored.sort_by(|a, b| a.1.cmp(&b.1).then(a.2.cmp(&b.2)).then_with(|| locale_compare(a.0, b.0)));
    scored.into_iter().take(limit).map(|(p, _, _)| p.to_string()).collect()
}
