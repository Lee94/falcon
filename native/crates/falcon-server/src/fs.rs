//! 目录浏览。本地走本机文件系统；SSH 走宿主机 exec，命令行用 git/path 的规则拼——
//! 后端 Windows 也可能在列一台 Linux 远端，`std::path` 会用错分隔符。
//! 移植自 `packages/server/src/fs.ts`。
//!
//! 不走 SFTP：跟 Zellij 安装同一个理由，sftp-server 被禁的机器并不少见。
//!
//! ## S1 只移纯函数
//!
//! 留到 S5 的函数（TS 名）：`windowsDrives`、`roots`、`parentOf`、`expandUser`、`listOne`、
//! `listRoots`、`listDirectories`（本机那一半：要本机文件系统、家目录，`parentOf` /
//! `expandUser` 依赖 `node:path` 的本平台语义与 `path.resolve`，要和 Rust 的本机路径
//! 解析一起定），以及 `execTimed`、`listRemoteOne`、`remoteRoots`、`listRemoteDirectories`
//! （要 ExecFn）。远端那一半里不碰 exec 的部分已拆成纯函数：[`remote_list_target`]（决定
//! 列哪里）、[`parse_remote_roots`]、[`remote_listing`]、[`remote_drive_listing`]，S5 只需
//! 补上 exec 与超时。
//!
//! 错误沿用 [`crate::files::FileError`]：消息文案与 files.ts 是同一套。

use std::cmp::Ordering;
use std::io;
use std::sync::LazyLock;
use std::time::Duration;

use falcon_proto::{FsDirEntry, FsListing};
use regex::Regex;

use crate::exec::ExecResult;
use crate::files::{FileError, js_trim, js_trim_end, locale_compare_base, remote_failure_parts, split_lines};
use crate::git::path::{dirname_of, is_absolute, join_path, normalize_sep};
use crate::zellij::host::{HostKind, encode_powershell, quote_posix, quote_powershell};

pub const LIST_TIMEOUT: Duration = Duration::from_millis(15_000);

/// 本机列目录失败 → 给用户看的消息。比 files.rs 的 `local_error` 少 EISDIR / EEXIST
/// （那两种在这里落 fallback）。
pub fn listing_error(err: &io::Error, fallback: impl Into<String>) -> FileError {
    match err.kind() {
        io::ErrorKind::NotFound => FileError::NotFound,
        io::ErrorKind::NotADirectory => FileError::NotDir,
        io::ErrorKind::PermissionDenied => FileError::PermissionDenied,
        _ => FileError::Other(fallback.into()),
    }
}

/// 点开头的沉底，其余按名字排（localeCompare，sensitivity base）
pub fn sort_entries(mut entries: Vec<FsDirEntry>) -> Vec<FsDirEntry> {
    entries.sort_by(|a, b| {
        let ah = a.name.starts_with('.');
        let bh = b.name.starts_with('.');
        if ah != bh {
            return if ah { Ordering::Greater } else { Ordering::Less };
        }
        locale_compare_base(&a.name, &b.name)
    });
    entries
}

pub fn list_dirs_command(kind: HostKind, dir: &str) -> String {
    if kind == HostKind::Windows {
        return encode_powershell(&format!(
            "$d = {}; \
             if (-not (Test-Path -LiteralPath $d)) {{ Write-Output 'ENOENT'; exit 1 }}; \
             if (-not (Test-Path -LiteralPath $d -PathType Container)) {{ Write-Output 'ENOTDIR'; exit 1 }}; \
             Get-ChildItem -LiteralPath $d -Force | Where-Object {{ $_.PSIsContainer }} | ForEach-Object {{ $_.Name }}",
            quote_powershell(dir)
        ));
    }
    [
        format!("d={}", quote_posix(dir)),
        r#"if [ ! -e "$d" ]; then printf '%s\n' ENOENT; exit 1; fi"#.to_string(),
        r#"if [ ! -d "$d" ]; then printf '%s\n' ENOTDIR; exit 1; fi"#.to_string(),
        r#"if [ ! -r "$d" ]; then printf '%s\n' EACCES; exit 1; fi"#.to_string(),
        r#"ls -1A "$d" | while IFS= read -r name; do if [ -d "$d/$name" ]; then printf '%s\n' "$name"; fi; done"#
            .to_string(),
    ]
    .join("; ")
}

pub fn list_drives_command() -> String {
    encode_powershell("Get-PSDrive -PSProvider FileSystem | ForEach-Object { $_.Root }")
}

/// 远端脚本约定：失败时 stdout 第一行是错误码。比 files.rs 的 `remote_error` 少
/// EISDIR / EEXIST（这里的脚本不会给）。
pub fn remote_error(stdout: &str, fallback: impl Into<String>) -> FileError {
    match split_lines(js_trim(stdout)).next().unwrap_or("") {
        "ENOENT" => FileError::NotFound,
        "ENOTDIR" => FileError::NotDir,
        "EACCES" => FileError::PermissionDenied,
        _ => FileError::Other(fallback.into()),
    }
}

/// `remoteError(res.stdout || res.stderr, res.stderr.trim() || fallback)`（listRemoteOne 的失败分支）
pub fn remote_failure(res: &ExecResult, fallback: &str) -> FileError {
    let (code_src, fallback) = remote_failure_parts(res, fallback);
    remote_error(code_src, fallback)
}

/// 远端目录的上一级。POSIX 根为 None；Windows 盘符根的上一级是 ""（盘符列表）
pub fn parent_of_remote(kind: HostKind, dir: &str) -> Option<String> {
    static DRIVE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z]:$").unwrap());
    if kind == HostKind::Posix {
        return if dir == "/" { None } else { Some(dirname_of(kind, dir)) };
    }
    let n = normalize_sep(HostKind::Windows, dir);
    if DRIVE.is_match(n.trim_end_matches('\\')) {
        return Some(String::new());
    }
    Some(dirname_of(kind, dir))
}

/// 展开 `~`；相对路径按远端 home 解析（远端没有"后端 cwd"这回事）
pub fn expand_remote(kind: HostKind, home: &str, input: &str) -> String {
    let trimmed = js_trim(input);
    if trimmed == "~" {
        return home.to_string();
    }
    if trimmed.starts_with("~/") || (kind == HostKind::Windows && trimmed.starts_with("~\\")) {
        return join_path(kind, &[home, &trimmed[2..]]);
    }
    if !is_absolute(kind, trimmed) {
        return join_path(kind, &[home, trimmed]);
    }
    normalize_sep(kind, trimmed)
}

/// 按行切、去行尾空白、丢空行
pub fn names_from(stdout: &str) -> Vec<String> {
    split_lines(stdout).map(js_trim_end).filter(|l| !l.is_empty()).map(str::to_string).collect()
}

/// listRemoteDirectories 决定列哪里（TS 里内联在函数体中）。
///
/// - `Some("")`：Windows 返回 None，表示列盘符（[`remote_drive_listing`]）；POSIX 等价于列 `/`；
/// - `None` 或全是空白：远端 home（打开浏览时的默认位置）；
/// - 其余：[`expand_remote`]。
///
/// 注意 `""` 与 `"  "` 不一样：前者是"回到根"，后者当没填。
pub fn remote_list_target(kind: HostKind, home: &str, input: Option<&str>) -> Option<String> {
    match input {
        Some("") => (kind != HostKind::Windows).then(|| "/".to_string()),
        None => Some(home.to_string()),
        Some(s) if js_trim(s).is_empty() => Some(home.to_string()),
        Some(s) => Some(expand_remote(kind, home, s)),
    }
}

/// remoteRoots 拿到 `Get-PSDrive` 输出之后的那段：只留形如 `X:\` 的盘符根。
/// （POSIX 远端不跑命令，根恒为 `["/"]`；命令非零退出时 TS 给空列表。）
pub fn parse_remote_roots(stdout: &str) -> Vec<String> {
    static DRIVE_ROOT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z]:\\").unwrap());
    names_from(stdout)
        .into_iter()
        .map(|r| normalize_sep(HostKind::Windows, &r))
        .filter(|r| DRIVE_ROOT.is_match(r))
        .collect()
}

/// listRemoteOne 成功之后的那段：子目录名 → 排好序的列表
pub fn remote_listing(kind: HostKind, home: &str, dir: &str, roots: Vec<String>, stdout: &str) -> FsListing {
    let entries = sort_entries(
        names_from(stdout).into_iter().map(|name| FsDirEntry { path: join_path(kind, &[dir, &name]), name }).collect(),
    );
    FsListing { path: dir.to_string(), parent: parent_of_remote(kind, dir), home: home.to_string(), roots, entries }
}

/// Windows 远端的盘符列表（虚拟层，`path == ""`）
pub fn remote_drive_listing(home: &str, roots: Vec<String>) -> FsListing {
    let entries = roots
        .iter()
        .map(|r| FsDirEntry { name: r.strip_suffix('\\').unwrap_or(r).to_string(), path: r.clone() })
        .collect();
    FsListing { path: String::new(), parent: None, home: home.to_string(), roots, entries }
}

#[cfg(test)]
mod tests {
    //! fs.ts 没有测试文件；这里是 Rust 侧补的。
    use super::*;
    use crate::zellij::host::tests::decode;

    const POSIX: HostKind = HostKind::Posix;
    const WINDOWS: HostKind = HostKind::Windows;

    fn dir(name: &str, path: &str) -> FsDirEntry {
        FsDirEntry { name: name.into(), path: path.into() }
    }

    #[test]
    fn list_dirs_command_shapes() {
        let posix = list_dirs_command(POSIX, "/home/o'b");
        assert_eq!(
            posix,
            concat!(
                r#"d='/home/o'\''b'; "#,
                r#"if [ ! -e "$d" ]; then printf '%s\n' ENOENT; exit 1; fi; "#,
                r#"if [ ! -d "$d" ]; then printf '%s\n' ENOTDIR; exit 1; fi; "#,
                r#"if [ ! -r "$d" ]; then printf '%s\n' EACCES; exit 1; fi; "#,
                r#"ls -1A "$d" | while IFS= read -r name; do if [ -d "$d/$name" ]; then printf '%s\n' "$name"; fi; done"#,
            )
        );
        let script = decode(&list_dirs_command(WINDOWS, "C:\\it's"));
        assert!(script.ends_with(
            "$d = 'C:\\it''s'; \
             if (-not (Test-Path -LiteralPath $d)) { Write-Output 'ENOENT'; exit 1 }; \
             if (-not (Test-Path -LiteralPath $d -PathType Container)) { Write-Output 'ENOTDIR'; exit 1 }; \
             Get-ChildItem -LiteralPath $d -Force | Where-Object { $_.PSIsContainer } | ForEach-Object { $_.Name }"
        ));
        assert!(
            decode(&list_drives_command()).ends_with("Get-PSDrive -PSProvider FileSystem | ForEach-Object { $_.Root }")
        );
    }

    #[test]
    fn sort_hidden_last_then_name() {
        let sorted =
            sort_entries(vec![dir(".config", "/h/.config"), dir("b", "/h/b"), dir("A", "/h/A"), dir("a", "/h/a")]);
        let names: Vec<&str> = sorted.iter().map(|e| e.name.as_str()).collect();
        // A 与 a 一级相等，稳定排序保持原顺序
        assert_eq!(names, ["A", "a", "b", ".config"]);
    }

    #[test]
    fn parent_of_remote_roots() {
        assert_eq!(parent_of_remote(POSIX, "/"), None);
        assert_eq!(parent_of_remote(POSIX, "/home"), Some("/".into()));
        assert_eq!(parent_of_remote(POSIX, "/home/u/"), Some("/home".into()));
        assert_eq!(parent_of_remote(WINDOWS, "C:\\"), Some(String::new()));
        assert_eq!(parent_of_remote(WINDOWS, "c:"), Some(String::new()));
        assert_eq!(parent_of_remote(WINDOWS, "C:\\Users"), Some("C:\\".into()));
        assert_eq!(parent_of_remote(WINDOWS, "C:/Users/a/"), Some("C:\\Users".into()));
    }

    #[test]
    fn expand_remote_home_relative_absolute() {
        assert_eq!(expand_remote(POSIX, "/home/u", " ~ "), "/home/u");
        assert_eq!(expand_remote(POSIX, "/home/u", "~/code"), "/home/u/code");
        // POSIX 上 ~\ 不是家目录写法，按相对路径接在 home 下
        assert_eq!(expand_remote(POSIX, "/home/u", "~\\x"), "/home/u/~\\x");
        assert_eq!(expand_remote(POSIX, "/home/u", "code"), "/home/u/code");
        assert_eq!(expand_remote(POSIX, "/home/u", "/etc"), "/etc");
        assert_eq!(expand_remote(WINDOWS, "C:\\Users\\u", "~\\code"), "C:\\Users\\u\\code");
        assert_eq!(expand_remote(WINDOWS, "C:\\Users\\u", "D:/data"), "D:\\data");
        assert_eq!(expand_remote(WINDOWS, "C:\\Users\\u", "code"), "C:\\Users\\u\\code");
    }

    #[test]
    fn remote_list_target_empty_vs_blank() {
        assert_eq!(remote_list_target(WINDOWS, "C:\\Users\\u", Some("")), None);
        assert_eq!(remote_list_target(POSIX, "/home/u", Some("")), Some("/".into()));
        assert_eq!(remote_list_target(POSIX, "/home/u", None), Some("/home/u".into()));
        assert_eq!(remote_list_target(POSIX, "/home/u", Some("  ")), Some("/home/u".into()));
        assert_eq!(remote_list_target(POSIX, "/home/u", Some("~/a")), Some("/home/u/a".into()));
    }

    #[test]
    fn names_roots_and_listings() {
        assert_eq!(names_from("a \r\n\n b\t\nc"), ["a", " b", "c"]);
        assert_eq!(parse_remote_roots("C:\\\r\nD:/\r\n\\\\srv\\share\r\nTemp\r\n"), ["C:\\", "D:\\"]);

        let l = remote_listing(POSIX, "/home/u", "/home/u", vec!["/".into()], ".cache\nsrc\nDocs\n");
        assert_eq!(l.path, "/home/u");
        assert_eq!(l.parent.as_deref(), Some("/home"));
        assert_eq!(
            l.entries,
            [dir("Docs", "/home/u/Docs"), dir("src", "/home/u/src"), dir(".cache", "/home/u/.cache")]
        );

        let w = remote_listing(WINDOWS, "C:\\Users\\u", "C:\\", vec!["C:\\".into()], "Users\r\n");
        assert_eq!(w.parent.as_deref(), Some(""));
        assert_eq!(w.entries, [dir("Users", "C:\\Users")]);

        let d = remote_drive_listing("C:\\Users\\u", vec!["C:\\".into(), "D:\\".into()]);
        assert_eq!(d.path, "");
        assert_eq!(d.parent, None);
        assert_eq!(d.entries, [dir("C:", "C:\\"), dir("D:", "D:\\")]);
    }

    #[test]
    fn error_mapping() {
        assert_eq!(remote_error("ENOTDIR\n", "x"), FileError::NotDir);
        // fs.ts 的脚本不报 EEXIST / EISDIR，落 fallback
        assert_eq!(remote_error("EEXIST", "无法读取该目录").to_string(), "无法读取该目录");
        let res = ExecResult { code: Some(1), stdout: String::new(), stderr: "ssh: boom\n".into() };
        assert_eq!(remote_failure(&res, "无法读取该目录").to_string(), "ssh: boom");
        assert_eq!(listing_error(&io::Error::from(io::ErrorKind::NotFound), "x").to_string(), "路径不存在或不可访问");
        assert_eq!(
            listing_error(&io::Error::from(io::ErrorKind::AlreadyExists), "无法读取该目录").to_string(),
            "无法读取该目录"
        );
    }
}
