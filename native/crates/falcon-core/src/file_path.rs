//! 文件面板的路径运算：把工作目录相对路径和宿主机绝对路径互相翻译，
//! 以及从文件夹上传的相对路径里抽出要 mkdir 的中间层。对应旧 React 版的 `lib/filePath.ts`。
//!
//! **不能用 `std::path`**：宿主机可能是 Linux，客户端将来在 Windows 上（同 server 的
//! `git/path.ts` 不用 `node:path` 的理由）。客户端不知道宿主机是 POSIX 还是 Windows，
//! 只能从 workingDir 的样子猜分隔符（含反斜杠或盘符 → Windows）。相对路径在协议里一律 `/`。

use std::collections::BTreeSet;

use crate::js::{js_to_fixed, js_trim};

/// 工作目录相对路径 → 宿主机上显示用的绝对路径
pub fn join_host_path(root: Option<&str>, rel: &str) -> String {
    let Some(root) = root.filter(|r| !r.is_empty()) else {
        return if rel.is_empty() { "/".into() } else { rel.into() };
    };
    let sep = host_sep(root);
    let base = trim_trailing_seps(root);
    if rel.is_empty() {
        return base.to_string();
    }
    format!("{base}{sep}{}", rel.replace('/', sep))
}

/// 路径栏里用户敲进去的字符串 → 工作目录相对路径。
///
/// - 空 / 只有分隔符 → 工作目录本身（`""`）
/// - 以 workingDir 为前缀的绝对路径 → 去掉前缀
/// - 看起来像相对路径 → 原样（分隔符收成 `/`）
/// - 绝对路径但不在工作目录里、或含 `..` → `None`（拒绝越界）
pub fn parse_nav_path(input: &str, root: Option<&str>) -> Option<String> {
    let trimmed = js_trim(input);
    if trimmed.is_empty() || trimmed == "/" || trimmed == "\\" {
        return Some(String::new());
    }
    let root = root.filter(|r| !r.is_empty());
    let windows = is_windows_root(root);
    let swapped = if windows { trimmed.replace('/', "\\") } else { trimmed.replace('\\', "/") };
    let normalized = trim_trailing_seps(&swapped);

    if let Some(root) = root {
        let base = trim_trailing_seps(root);
        let base_cmp = if windows { base.to_lowercase() } else { base.to_string() };
        let path_cmp = if windows { normalized.to_lowercase() } else { normalized.to_string() };
        if path_cmp == base_cmp {
            return Some(String::new());
        }
        let prefix = format!("{base_cmp}{}", if windows { "\\" } else { "/" });
        if path_cmp.starts_with(&prefix) {
            // TS 按 base 的长度切原串；小写化不改变 ASCII 之外的长度时两边一致，
            // 改变时（极少见的大小写映射）用码元数对齐原串
            let rest = slice_after_units(normalized, utf16_units(base) + 1);
            return to_rel(rest, windows);
        }
        // 绝对路径但不在工作目录里
        if is_absolute_input(normalized, windows) {
            return None;
        }
    } else if is_absolute_input(normalized, false) || is_absolute_input(normalized, true) {
        // 没有 workingDir 可对照时，把开头的 / 当成工作目录根（`/src` → src）
        if let Some(rest) = normalized.strip_prefix('/') {
            return to_rel(rest, false);
        }
        if has_drive_prefix(normalized) {
            return None;
        }
    }

    to_rel(normalized.trim_start_matches(['\\', '/']), windows)
}

pub fn parent_rel(rel: &str) -> Option<String> {
    if rel.is_empty() {
        return None;
    }
    Some(rel_dir(rel))
}

pub fn rel_dir(rel: &str) -> String {
    rel.rfind('/').map(|i| rel[..i].to_string()).unwrap_or_default()
}

pub fn is_hidden_name(name: &str) -> bool {
    name.starts_with('.')
}

/// 文件夹上传：每个文件的相对路径形如 `src/lib/a.ts`，中间层 `src`、`src/lib` 得先
/// mkdir。返回去重后最深的那一批——mkdir -p 一次就能带上祖先。
pub fn deepest_upload_dirs<S: AsRef<str>>(cwd: &str, relative_paths: &[S]) -> Vec<String> {
    let mut dirs: BTreeSet<String> = BTreeSet::new();
    for rel in relative_paths {
        let mut segs: Vec<&str> = rel.as_ref().split('/').filter(|s| !s.is_empty()).collect();
        segs.pop();
        let mut acc = cwd.to_string();
        for s in segs {
            acc = if acc.is_empty() { s.to_string() } else { format!("{acc}/{s}") };
            dirs.insert(acc.clone());
        }
    }
    // TS 的 `[...dirs].sort()` 按 UTF-16 码元排；BTreeSet 按字节（UTF-8）排，只有
    // BMP 之外的字符与 U+E000–U+FFFF 相比时两者才会不同，而这里的结果只拿来过滤、
    // 顺序本身不影响取哪些
    let all: Vec<String> = dirs.into_iter().collect();
    all.iter().filter(|d| !all.iter().any(|other| other.starts_with(&format!("{d}/")))).cloned().collect()
}

/// 修改时间（Unix 秒）→ `YYYY-MM-DD HH:MM:SS`。
///
/// React 版用 `new Date()` 的**本地时区**；Rust 标准库没有时区数据库，这里由调用方传
/// 这一时刻的本地 UTC 偏移（秒，东正西负）。偏移对得上时与 React 版的输出逐字相同（单测
/// 的期望值就取自它）。
pub fn format_mtime(sec: Option<f64>, utc_offset_secs: i64) -> String {
    let Some(sec) = sec.filter(|s| s.is_finite()) else { return "—".into() };
    // Date 的时间值要落在 ±8.64e15 ms 内，否则是 Invalid Date；小数毫秒截掉
    let ms = (sec * 1000.0).trunc();
    if ms.abs() > 8.64e15 {
        return "—".into();
    }
    let local_ms = ms as i64 + utc_offset_secs * 1000;
    let days = local_ms.div_euclid(86_400_000);
    let in_day = local_ms.rem_euclid(86_400_000) / 1000;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        in_day / 3600,
        (in_day % 3600) / 60,
        in_day % 60
    )
}

/// 1970-01-01 起的天数 → 公历年月日（Howard Hinnant 的 civil_from_days）
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// 字节数 → `370 B` / `10.4 KB` / `1.5 MB` / `2.00 GB`（`toFixed` 的舍入照 JS）
pub fn format_size(n: Option<f64>) -> String {
    let Some(n) = n.filter(|n| n.is_finite()) else { return "—".into() };
    if n < 1024.0 {
        return format!("{} B", js_number_display(n));
    }
    if n < 1024.0 * 1024.0 {
        return format!("{} KB", js_to_fixed(n / 1024.0, 1));
    }
    if n < 1024.0 * 1024.0 * 1024.0 {
        return format!("{} MB", js_to_fixed(n / 1024.0 / 1024.0, 1));
    }
    format!("{} GB", js_to_fixed(n / 1024.0 / 1024.0 / 1024.0, 2))
}

/// 模板字符串里 `${n}` 的样子：整数不带小数点
fn js_number_display(n: f64) -> String {
    if n.fract() == 0.0 { format!("{}", n as i64) } else { format!("{n}") }
}

fn host_sep(root: &str) -> &'static str {
    if is_windows_root(Some(root)) { "\\" } else { "/" }
}

/// `/^[A-Za-z]:[\\/]/`
fn has_drive_prefix(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')
}

fn is_windows_root(root: Option<&str>) -> bool {
    let Some(root) = root.filter(|r| !r.is_empty()) else { return false };
    has_drive_prefix(root) || (root.contains('\\') && !root.contains('/'))
}

fn is_absolute_input(path: &str, windows: bool) -> bool {
    if windows {
        return has_drive_prefix(path) || path.starts_with("\\\\");
    }
    path.starts_with('/')
}

/// `replace(/[\\/]+$/, "")`
fn trim_trailing_seps(s: &str) -> &str {
    s.trim_end_matches(['\\', '/'])
}

fn to_rel(path: &str, windows: bool) -> Option<String> {
    let segs: Vec<&str> = if windows {
        path.split(['\\', '/']).filter(|s| !s.is_empty()).collect()
    } else {
        path.split('/').filter(|s| !s.is_empty()).collect()
    };
    if segs.iter().any(|s| *s == "." || *s == "..") {
        return None;
    }
    Some(segs.join("/"))
}

fn utf16_units(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// `s.slice(n)`，n 按 UTF-16 码元计
fn slice_after_units(s: &str, n: usize) -> &str {
    let mut units = 0;
    for (i, c) in s.char_indices() {
        if units >= n {
            return &s[i..];
        }
        units += c.len_utf16();
    }
    ""
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_host_path_uses_the_working_dir_separator() {
        assert_eq!(join_host_path(Some("/home/u/repo"), ""), "/home/u/repo");
        assert_eq!(join_host_path(Some("/home/u/repo"), "src/a.ts"), "/home/u/repo/src/a.ts");
        assert_eq!(join_host_path(Some("C:\\code\\repo"), "src/a.ts"), "C:\\code\\repo\\src\\a.ts");
        assert_eq!(join_host_path(None, "src"), "src");
        assert_eq!(join_host_path(None, ""), "/");
    }

    #[test]
    fn parse_nav_path_strips_the_working_dir_and_refuses_escapes() {
        let root = Some("/home/u/repo");
        assert_eq!(parse_nav_path("/home/u/repo", root).as_deref(), Some(""));
        assert_eq!(parse_nav_path("/home/u/repo/", root).as_deref(), Some(""));
        assert_eq!(parse_nav_path("/home/u/repo/src", root).as_deref(), Some("src"));
        assert_eq!(parse_nav_path("/home/u/repo/src/a.ts", root).as_deref(), Some("src/a.ts"));
        assert_eq!(parse_nav_path("src/foo", root).as_deref(), Some("src/foo"));
        assert_eq!(parse_nav_path("/etc", root), None);
        assert_eq!(parse_nav_path("../x", root), None);
        assert_eq!(parse_nav_path("src/../etc", root), None);
        // 没有 workingDir 时，开头的 / 当成工作目录根
        assert_eq!(parse_nav_path("/src/a", None).as_deref(), Some("src/a"));
        assert_eq!(parse_nav_path("src", None).as_deref(), Some("src"));
    }

    #[test]
    fn parse_nav_path_is_case_insensitive_on_windows() {
        let root = Some("C:\\code\\repo");
        assert_eq!(parse_nav_path("c:\\code\\repo\\src", root).as_deref(), Some("src"));
        assert_eq!(parse_nav_path("C:/code/repo/src", root).as_deref(), Some("src"));
        assert_eq!(parse_nav_path("D:\\other", root), None);
    }

    #[test]
    fn parent_rel_dir_and_hidden() {
        assert_eq!(parent_rel(""), None);
        assert_eq!(parent_rel("src").as_deref(), Some(""));
        assert_eq!(parent_rel("src/a.ts").as_deref(), Some("src"));
        assert_eq!(rel_dir("src/a.ts"), "src");
        assert_eq!(rel_dir("a.ts"), "");
        assert!(is_hidden_name(".env"));
        assert!(!is_hidden_name("env"));
    }

    #[test]
    fn deepest_upload_dirs_keeps_only_the_deepest_layer() {
        assert_eq!(deepest_upload_dirs("", &["src/a.ts", "src/lib/b.ts", "README.md"]), ["src/lib"]);
        assert_eq!(deepest_upload_dirs("pkg", &["foo/a.ts", "foo/bar/b.ts"]), ["pkg/foo/bar"]);
        assert!(deepest_upload_dirs("pkg", &["a.ts"]).is_empty());
    }

    #[test]
    fn format_size_matches_web() {
        assert_eq!(format_size(None), "—");
        assert_eq!(format_size(Some(370.0)), "370 B");
        assert_eq!(format_size(Some(10.42 * 1024.0)), "10.4 KB");
        // 以下是 Rust 侧补的：精确中点按 JS 的 toFixed 取大
        assert_eq!(format_size(Some(10.25 * 1024.0)), "10.3 KB");
        assert_eq!(format_size(Some(1.5 * 1024.0 * 1024.0)), "1.5 MB");
        assert_eq!(format_size(Some(3.0 * 1024.0 * 1024.0 * 1024.0)), "3.00 GB");
        assert_eq!(format_size(Some(f64::NAN)), "—");
    }

    #[test]
    fn format_mtime_with_explicit_offset() {
        assert_eq!(format_mtime(None, 0), "—");
        assert_eq!(format_mtime(Some(f64::INFINITY), 0), "—");
        // 期望值取自 React 版的 formatMtime 在 TZ=UTC / Asia/Shanghai / America/New_York 下的输出，
        // 偏移是那一时刻的真实 UTC 偏移（1900 年前是 LMT，带秒：上海 +8:05:43、纽约 −4:56:02；
        // getTimezoneOffset() 只给到分钟，调用方要按秒算）
        for (sec, offset, want) in [
            (0.0, 0, "1970-01-01 00:00:00"),
            (1_790_244_610.0, 0, "2026-09-24 10:10:10"),
            (1_790_244_610.999, 0, "2026-09-24 10:10:10"),
            (-1.0, 0, "1969-12-31 23:59:59"),
            (951_782_400.0, 0, "2000-02-29 00:00:00"),
            (-62_198_755_200.0, 0, "-1-01-01 00:00:00"),
            (1_790_244_610.0, 28_800, "2026-09-24 18:10:10"),
            (-1.0, 28_800, "1970-01-01 07:59:59"),
            (-62_198_755_200.0, 29_143, "-1-01-01 08:05:43"),
            (1_790_244_610.0, -14_400, "2026-09-24 06:10:10"),
            (951_782_400.0, -18_000, "2000-02-28 19:00:00"),
            (-62_198_755_200.0, -17_762, "-2-12-31 19:03:58"),
        ] {
            assert_eq!(format_mtime(Some(sec), offset), want, "{sec} @ {offset}");
        }
        assert_eq!(format_mtime(Some(1e13), 0), "—");
    }
}
