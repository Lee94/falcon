//! 多仓库容器表单里的路径小工具。对应 web 的 `lib/multiPath.ts`，纯函数、零 I/O。
//!
//! 只做**建议值**：算出来的结果显式填进表单字段、随请求原样上送，服务端不重算——
//! 所以这里的分隔符猜测（看第一条路径长什么样）漂移无害。

use crate::js::js_trim;

/// 一组成员路径的最近公共父目录，给「会话初始 cwd」当建议值。
/// 没有有用的公共父目录（不同盘符、只剩文件系统根）返回 `""`——
/// 填一个 "/" 进去当 cwd 还不如留空走家目录。
pub fn common_parent_dir<S: AsRef<str>>(paths: &[S]) -> String {
    let clean: Vec<&str> = paths
        .iter()
        .map(|p| js_trim(p.as_ref()).trim_end_matches(['\\', '/']))
        .filter(|p| !p.is_empty())
        .collect();
    let Some(first_path) = clean.first() else { return String::new() };
    let windows = first_path.contains('\\') || has_drive_letter(first_path);
    let norm = |s: &str| if windows { s.to_lowercase() } else { s.to_string() };
    let seg_lists: Vec<Vec<&str>> = clean.iter().map(|p| split_seps(p)).collect();
    let first = &seg_lists[0];
    let mut common = first.len();
    for segs in &seg_lists[1..] {
        let mut i = 0;
        while i < common && i < segs.len() && norm(segs[i]) == norm(first[i]) {
            i += 1;
        }
        common = i;
    }
    // 保留第一条路径的原始大小写；posix 的首段是 ""（根），windows 是盘符
    let segs = &first[..common];
    if segs.len() < 2 {
        return String::new();
    }
    segs.join(if windows { "\\" } else { "/" })
}

/// `/^[A-Za-z]:/`
fn has_drive_letter(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

/// `split(/[\\/]+/)`：连续分隔符算一个，开头的分隔符留下一个空段
fn split_seps(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut in_sep = false;
    for (i, c) in s.char_indices() {
        let sep = c == '/' || c == '\\';
        if sep && !in_sep {
            out.push(&s[start..i]);
        }
        if !sep && in_sep {
            start = i;
        }
        in_sep = sep;
    }
    if in_sep {
        out.push("");
    } else {
        out.push(&s[start..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posix_nearest_common_ancestor() {
        assert_eq!(common_parent_dir(&["/home/u/code/web", "/home/u/code/srv"]), "/home/u/code");
        assert_eq!(common_parent_dir(&["/home/u/code/web"]), "/home/u/code/web");
    }

    #[test]
    fn a_member_that_is_the_ancestor_of_another_is_itself_the_answer() {
        assert_eq!(common_parent_dir(&["/a/web", "/a/web/vendor/lib"]), "/a/web");
    }

    #[test]
    fn windows_backslash_join_case_insensitive_compare_keeps_first_spelling() {
        assert_eq!(common_parent_dir(&["D:\\Code\\Web", "d:\\code\\srv"]), "D:\\Code");
    }

    #[test]
    fn returns_empty_when_only_the_fs_root_or_nothing_is_shared() {
        assert_eq!(common_parent_dir(&["/a/x", "/b/y"]), "");
        assert_eq!(common_parent_dir(&["C:\\a", "D:\\b"]), "");
        assert_eq!(common_parent_dir::<&str>(&[]), "");
        assert_eq!(common_parent_dir(&["  "]), "");
    }

    #[test]
    fn tolerates_trailing_separators() {
        assert_eq!(common_parent_dir(&["/a/b/", "/a/c/"]), "/a");
    }

    #[test]
    fn split_matches_js_regex_split() {
        assert_eq!(split_seps("/a//b"), ["", "a", "b"]);
        assert_eq!(split_seps("a\\/b"), ["a", "b"]);
        assert_eq!(split_seps("a"), ["a"]);
        assert_eq!(split_seps("a/"), ["a", ""]);
    }
}
