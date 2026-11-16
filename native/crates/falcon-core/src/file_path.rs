//! 文件面板的路径运算：把工作目录相对路径和宿主机绝对路径互相翻译，
//! 以及从文件夹上传的相对路径里抽出要 mkdir 的中间层。对应旧 React 版的 `lib/filePath.ts`。
//!
//! **不能用 `std::path`**：宿主机可能是 Linux，客户端将来在 Windows 上（同 server 的
//! `git/path.ts` 不用 `node:path` 的理由）。客户端不知道宿主机是 POSIX 还是 Windows，
//! 只能从当前宿主机路径的样子猜分隔符（盘符或 UNC → Windows）。协议里统一 `/`，POSIX
//! 文件名中的反斜杠保留。导航只是改当前浏览位置，不改项目的终端初始 cwd。

use std::collections::BTreeSet;

use crate::js::{js_to_fixed, js_trim};

/// 工作目录相对路径 → 宿主机上显示用的绝对路径；导航到项目外后绝对路径原样保留。
pub fn join_host_path(root: Option<&str>, rel: &str) -> String {
    if is_absolute_input(rel, false) || is_absolute_input(rel, true) {
        return if is_windows_root(Some(rel)) { rel.replace('/', "\\") } else { rel.into() };
    }
    let Some(root) = root.filter(|r| !r.is_empty()) else {
        return if rel.is_empty() { "/".into() } else { rel.into() };
    };
    let sep = host_sep(root);
    let base = trim_trailing_seps(root);
    if rel.is_empty() {
        return if base.is_empty() {
            sep.into()
        } else if has_drive_prefix(root) && base.ends_with(':') {
            format!("{base}{sep}")
        } else {
            base.into()
        };
    }
    format!("{base}{sep}{}", rel.replace('/', sep))
}

/// 路径栏输入 → 文件 API 的路径：有当前宿主机目录时返回绝对路径，不再以项目为边界。
///
/// 相对输入以当前目录为基准，允许 `.` / `..`；文件系统根（含盘符根、UNC 共享根）不能再
/// 往上。没有工作目录可对照时，普通相对路径照常交给后端以登录目录解析。
pub fn parse_nav_path(input: &str, current: Option<&str>) -> Option<String> {
    let input = js_trim(input);
    if input.chars().any(char::is_control) {
        return None;
    }
    if input.is_empty() {
        return Some(String::new());
    }
    let windows = is_windows_root(current) || has_drive_prefix(input) || input.starts_with("\\\\") || input.starts_with("//")
        || (current.is_none() && is_windows_root(Some(input)));
    let input = if windows { input.replace('\\', "/") } else { input.into() };
    let current = current.filter(|r| !r.is_empty()).map(|r| if windows { r.replace('\\', "/") } else { r.into() });
    // C:foo 依赖宿主进程的每盘当前目录，导航栏没这份状态，不把它拼成当前目录里的文件名。
    if windows && input.as_bytes().get(1) == Some(&b':') && !has_drive_prefix(&input) {
        return None;
    }
    let path = if input.starts_with('/') || has_drive_prefix(&input) {
        // Windows 上的 /foo 是当前盘符下的根路径，不把它误当 POSIX 路径。
        if windows && input.starts_with('/') && !input.starts_with("//") {
            let base = current.as_deref()?;
            let root = host_root(base)?;
            format!("{}{input}", root.trim_end_matches('/'))
        } else {
            input
        }
    } else if let Some(base) = &current {
        format!("{}/{input}", base.trim_end_matches('/'))
    } else {
        input
    };
    normalize_nav_path(&path)
}

/// 当前导航位置的上层；工作目录不再是边界，只有文件系统根没有上层。
pub fn parent_nav_path(path: &str) -> Option<String> {
    let normalized = if is_windows_root(Some(path)) { path.replace('\\', "/") } else { path.into() };
    let path = normalize_nav_path(&normalized)?;
    let root = host_root(&path).unwrap_or("");
    if path == root || path.is_empty() {
        return None;
    }
    let parent = path.rfind('/').map(|i| &path[..i]).unwrap_or("");
    Some(if parent.len() < root.len() { root.into() } else { parent.into() })
}

/// 多文件夹项目没有必需的 working_dir：先打开首个成员，不误列后端的启动目录。
pub fn project_nav_root(project: &falcon_proto::Project) -> Option<String> {
    project
        .multi
        .as_ref()
        .and_then(|m| m.repos.first())
        .map(|m| m.dir.clone())
        .or_else(|| project.working_dir.clone())
}

fn host_root(path: &str) -> Option<&str> {
    if has_drive_prefix(path) {
        return Some(&path[..3]);
    }
    if let Some(unc) = path.strip_prefix("//") {
        let server = unc.find('/')?;
        let share_end = unc[server + 1..].find('/').map_or(path.len(), |i| 2 + server + 1 + i);
        return Some(&path[..share_end]);
    }
    path.starts_with('/').then_some("/")
}

fn normalize_nav_path(path: &str) -> Option<String> {
    let root = host_root(path).unwrap_or("");
    if path.starts_with("//")
        && (root.is_empty() || root.ends_with('/') || root.split('/').skip(2).any(|p| p == "." || p == ".."))
    {
        return None;
    }
    let mut parts = Vec::new();
    for part in path[root.len()..].split('/') {
        match part {
            "" | "." => {}
            ".." if parts.last() == Some(&"..") => parts.push(part),
            ".." if !parts.is_empty() => { parts.pop(); }
            ".." if root.is_empty() => parts.push(part),
            ".." => {}
            _ => parts.push(part),
        }
    }
    let tail = parts.join("/");
    Some(if tail.is_empty() {
        root.into()
    } else if root.is_empty() || root.ends_with('/') {
        format!("{root}{tail}")
    } else {
        format!("{root}/{tail}")
    })
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
            acc = if acc.is_empty() { s.to_string() } else { format!("{}/{s}", acc.trim_end_matches('/')) };
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
    has_drive_prefix(root) || root.starts_with("//") || (root.contains('\\') && !root.contains('/'))
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
        assert_eq!(join_host_path(Some("/repo"), "/elsewhere/a"), "/elsewhere/a");
        assert_eq!(join_host_path(Some("C:\\repo"), "D:/elsewhere/a"), "D:\\elsewhere\\a");
        assert_eq!(join_host_path(Some("/"), ""), "/");
        assert_eq!(join_host_path(Some("C:\\"), ""), "C:\\");
    }

    #[test]
    fn multi_project_starts_at_first_member_without_a_working_directory() {
        let mut project: falcon_proto::Project = serde_json::from_value(serde_json::json!({
            "id": "multi", "name": "multi", "type": "local", "createdAt": 0,
            "multi": { "repos": [{"dir": "/code/web"}, {"dir": "/elsewhere/api"}] }
        })).unwrap();
        assert_eq!(project_nav_root(&project).as_deref(), Some("/code/web"));
        project.working_dir = Some("/code".into());
        assert_eq!(project_nav_root(&project).as_deref(), Some("/code/web"));
        project.multi = None;
        assert_eq!(project_nav_root(&project).as_deref(), Some("/code"));
    }

    #[test]
    fn parse_nav_path_can_leave_the_project_and_resolves_from_current_directory() {
        let current = Some("/home/u/repo");
        assert_eq!(parse_nav_path("/home/u/repo/", current).as_deref(), Some("/home/u/repo"));
        assert_eq!(parse_nav_path("src/foo", current).as_deref(), Some("/home/u/repo/src/foo"));
        assert_eq!(parse_nav_path("/etc", current).as_deref(), Some("/etc"));
        assert_eq!(parse_nav_path("../x", current).as_deref(), Some("/home/u/x"));
        assert_eq!(parse_nav_path("src/../etc", current).as_deref(), Some("/home/u/repo/etc"));
        assert_eq!(parse_nav_path("/", current).as_deref(), Some("/"));
        assert_eq!(parse_nav_path("../../../../x", current).as_deref(), Some("/x"));
        assert_eq!(parse_nav_path("/src/a", None).as_deref(), Some("/src/a"));
        assert_eq!(parse_nav_path("src", None).as_deref(), Some("src"));
        assert_eq!(parse_nav_path("a\0b", current), None);
        assert_eq!(parse_nav_path("dir\\name", current).as_deref(), Some("/home/u/repo/dir\\name"));
    }

    #[test]
    fn windows_navigation_supports_other_drives_and_unc_roots() {
        let current = Some("C:\\code\\repo");
        assert_eq!(parse_nav_path("c:\\code\\repo\\src", current).as_deref(), Some("c:/code/repo/src"));
        assert_eq!(parse_nav_path("..", current).as_deref(), Some("C:/code"));
        assert_eq!(parse_nav_path("D:\\other", current).as_deref(), Some("D:/other"));
        assert_eq!(parse_nav_path("C:other", current), None);
        assert_eq!(parse_nav_path("/", current).as_deref(), Some("C:/"));
        assert_eq!(parse_nav_path("..", Some("C:\\")).as_deref(), Some("C:/"));
        assert_eq!(parse_nav_path("..", Some("\\\\host\\share\\folder")).as_deref(), Some("//host/share"));
        assert_eq!(parse_nav_path("..", Some("\\\\host\\share")).as_deref(), Some("//host/share"));
        assert_eq!(parse_nav_path("next\\folder", Some("//host/share")).as_deref(), Some("//host/share/next/folder"));
        assert_eq!(parse_nav_path("//host/", None), None);
    }

    #[test]
    fn parent_nav_dir_and_hidden() {
        assert_eq!(parent_nav_path(""), None);
        assert_eq!(parent_nav_path("src").as_deref(), Some(""));
        assert_eq!(parent_nav_path("src/a.ts").as_deref(), Some("src"));
        assert_eq!(parent_nav_path("/home/u/repo").as_deref(), Some("/home/u"));
        assert_eq!(parent_nav_path("/home").as_deref(), Some("/"));
        assert_eq!(parent_nav_path("/tmp/a\\b/dir").as_deref(), Some("/tmp/a\\b"));
        assert_eq!(parent_nav_path("/"), None);
        assert_eq!(parent_nav_path("C:/code").as_deref(), Some("C:/"));
        assert_eq!(parent_nav_path("C:/"), None);
        assert_eq!(parent_nav_path("//host/share/dir").as_deref(), Some("//host/share"));
        assert_eq!(parent_nav_path("//host/share"), None);
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
        assert_eq!(deepest_upload_dirs("/", &["pkg/a.rs"]), ["/pkg"]);
        assert_eq!(deepest_upload_dirs("C:/", &["pkg/a.rs"]), ["C:/pkg"]);
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
