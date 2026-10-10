//! 项目工作目录的浏览与改动：列一层目录、取一个文件的内容、新建文件夹、
//! 重命名、删除（ADR 0009）。移植自 `packages/server/src/files.ts`。
//!
//! 与 fs.rs 是两件不同的事。那边服务于项目表单——只列文件夹、能一路往上走到根；
//! 这边只在**项目工作目录内部**打转，文件和目录都要列，还得把文件内容取回来，
//! 以及在用户确认之后改这一层（mkdir / rename / rm）。下载 / 上传按流走，在
//! transfer.rs。相同的是执行环境的取舍：本地走本机文件系统，远端走宿主机 exec 而
//! 不是 SFTP（理由同 fs.rs：sftp-server 被禁的机器并不少见）。
//!
//! 命令构造是纯函数（list_command / read_command / mkdir_command 等）、输出解析
//! 也是（parse_entries / parse_read / classify），照 git 与 zellij 那两套的分层
//! 来，测试主要打这一层。mkdir / rename / remove 开头那段路径运算与护栏另拆成
//! [`mkdir_target`] / [`rename_target`] / [`remove_target`]（TS 里是内联在 async 函数里的），
//! 远端失败时 `remoteError(res.stdout || res.stderr, res.stderr.trim() || …)` 这个反复出现的
//! 写法收成 [`remote_failure`]。
//!
//! 运行时那一半（[`list_workspace`] 等）对 [`FileHost`] 分两路：本地直接读写本机文件系统
//! （阻塞调用一律进 `spawn_blocking`——Node 版是 libuv 线程池上的 fs.promises，不占事件
//! 循环；这里调用方多半在会话引擎的单线程 LocalSet 上，更不能堵），远端走宿主机 exec。
//! 远端执行器攥着 `Rc` 的链路状态，所以这些 async 函数都是 `!Send` 的，路由经
//! `engine.call` 在引擎上跑它们。
//!
//! 错误：TS 一律 `throw new Error(中文消息)`，路由按**消息原文**分状态码（见
//! [`FileError`]）。这里换成枚举，`Display` 与 TS 的消息逐字一致。

use std::cmp::Ordering;
use std::io;
use std::sync::LazyLock;
use std::time::{Duration, UNIX_EPOCH};

use falcon_proto::{
    FileOpResult, FilePreview, FileRemoveError, FileRemoveResult, WORKSPACE_FILE_CAP, WORKSPACE_INDEX_CAP,
    WORKSPACE_LIST_CAP, WORKSPACE_RAW_CAP, WorkspaceEntry, WorkspaceEntryKind, WorkspaceIndex, WorkspaceListing,
};
use regex::Regex;
use tokio_util::sync::CancellationToken;

use crate::exec::{Exec, ExecResult, LocalBoxFuture};
use crate::git::path::{dirname_of, is_ancestor, join_path, normalize_sep, same_path};
use crate::sessions::ssh::SshLink;
use crate::zellij::host::{HostKind, encode_powershell, quote_posix, quote_powershell};

pub const LIST_TIMEOUT: Duration = Duration::from_millis(15_000);
pub const READ_TIMEOUT: Duration = Duration::from_millis(30_000);
/// 原始字节上限是文本的 8 倍，慢一点的 SSH 链路上 base64 回传 16MB 要不少时间
pub const RAW_TIMEOUT: Duration = Duration::from_millis(90_000);
pub const MKDIR_TIMEOUT: Duration = Duration::from_millis(15_000);
pub const RENAME_TIMEOUT: Duration = Duration::from_millis(15_000);
/// 递归删 node_modules 那种目录，远端可能要一会儿
pub const REMOVE_TIMEOUT: Duration = Duration::from_millis(120_000);
pub const INDEX_TIMEOUT: Duration = Duration::from_millis(20_000);

// ---------------- 错误 ----------------

/// files.ts / fs.ts 抛出的错误。
///
/// TS 里全是 `new Error(中文消息)`，而路由是按**消息原文**分状态码的：mkdir / rename
/// 遇到 "同名文件已存在" 回 409，原始字节路由遇到 "路径不存在或不可访问" 回 404，其余
/// 400；消息本身原样进 `{error}` 给前端看。所以每个变体的 `Display` 必须与 TS 逐字一致。
/// 远端脚本约定的错误码（ENOENT 等）另由 [`FileError::code`] 给出。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FileError {
    /// `..` / 空段之外的越界、控制字符（rel_segments / resolve_inside 的护栏）
    #[error("路径不合法")]
    InvalidPath,
    #[error("文件名不合法")]
    InvalidName,
    /// 超过 255 个 UTF-16 码元
    #[error("文件名太长")]
    NameTooLong,
    #[error("文件名含 Windows 不允许的字符")]
    WindowsForbiddenChar,
    #[error("不能删除工作目录本身")]
    RemoveRoot,
    /// parse_read 读不出首行的字节数
    #[error("读不到文件大小")]
    UnreadableSize,
    /// ENOENT
    #[error("路径不存在或不可访问")]
    NotFound,
    /// ENOTDIR
    #[error("不是文件夹")]
    NotDir,
    /// EISDIR
    #[error("这是一个文件夹")]
    IsDir,
    /// EACCES / EPERM
    #[error("没有权限访问该路径")]
    PermissionDenied,
    /// EEXIST
    #[error("同名文件已存在")]
    Exists,
    /// 没有约定码时的兜底：调用方给的 fallback 文案，或远端 stderr 原文
    #[error("{0}")]
    Other(String),
}

impl FileError {
    /// 远端脚本约定的错误码（失败时 stdout 第一行）。只有 errno 一类的变体有
    pub fn code(&self) -> Option<&'static str> {
        match self {
            FileError::NotFound => Some("ENOENT"),
            FileError::NotDir => Some("ENOTDIR"),
            FileError::IsDir => Some("EISDIR"),
            FileError::PermissionDenied => Some("EACCES"),
            FileError::Exists => Some("EEXIST"),
            _ => None,
        }
    }
}

/// 本机文件系统错误 → 给用户看的消息。TS 按 `err.code`（errno 名）分，这里按
/// `io::ErrorKind` 分：ENOENT → NotFound、ENOTDIR → NotADirectory、EISDIR →
/// IsADirectory、EACCES / EPERM → PermissionDenied、EEXIST → AlreadyExists，一一对应。
pub fn local_error(err: &io::Error, fallback: impl Into<String>) -> FileError {
    match err.kind() {
        io::ErrorKind::NotFound => FileError::NotFound,
        io::ErrorKind::NotADirectory => FileError::NotDir,
        io::ErrorKind::IsADirectory => FileError::IsDir,
        io::ErrorKind::PermissionDenied => FileError::PermissionDenied,
        io::ErrorKind::AlreadyExists => FileError::Exists,
        _ => FileError::Other(fallback.into()),
    }
}

/// 远端脚本约定：失败时 stdout 第一行是错误码（ENOENT 等），没有约定码的走 fallback
pub fn remote_error(stdout: &str, fallback: impl Into<String>) -> FileError {
    match first_line(js_trim(stdout)) {
        "ENOENT" => FileError::NotFound,
        "ENOTDIR" => FileError::NotDir,
        "EISDIR" => FileError::IsDir,
        "EACCES" => FileError::PermissionDenied,
        "EEXIST" => FileError::Exists,
        _ => FileError::Other(fallback.into()),
    }
}

/// 远端命令非零退出时的错误。TS 每个调用点都写成
/// `remoteError(res.stdout || res.stderr, res.stderr.trim() || "<兜底文案>")`，收在这里。
pub fn remote_failure(res: &ExecResult, fallback: &str) -> FileError {
    let (code_src, fallback) = remote_failure_parts(res, fallback);
    remote_error(code_src, fallback)
}

/// `(res.stdout || res.stderr, res.stderr.trim() || fallback)`，fs.rs 也用
pub(crate) fn remote_failure_parts<'a>(res: &'a ExecResult, fallback: &'a str) -> (&'a str, &'a str) {
    let code_src = if res.stdout.is_empty() { &res.stderr } else { &res.stdout };
    let stderr = js_trim(&res.stderr);
    (code_src, if stderr.is_empty() { fallback } else { stderr })
}

fn first_line(s: &str) -> &str {
    split_lines(s).next().unwrap_or("")
}

// ---------------- 路径 ----------------

/// 前端给的工作目录相对路径 → 路径段。
///
/// 前端一律用 `/` 分隔（协议里就这么定的，见 WorkspaceEntry），所以这里只按 `/` 切。
/// `..` 与空段直接拒绝：这是唯一能凭一个 query 参数跑出工作目录的方式，挡在最外层
/// 比在下游各处补判断可靠。
pub fn rel_segments(rel: Option<&str>) -> Result<Vec<String>, FileError> {
    let Some(rel) = rel.filter(|r| !r.is_empty()) else {
        return Ok(Vec::new());
    };
    let segs: Vec<String> = rel.split('/').filter(|s| !s.is_empty()).map(str::to_string).collect();
    for seg in &segs {
        if seg == "." || seg == ".." {
            return Err(FileError::InvalidPath);
        }
        // 控制字符只可能来自构造出来的请求，真实文件名里没有。命令是用 quote_posix /
        // -EncodedCommand 拼的，本就注不进去，挡掉只是少一层担心
        if seg.chars().any(is_c0_control) {
            return Err(FileError::InvalidPath);
        }
    }
    Ok(segs)
}

/// 拼出宿主机上的绝对路径，并复核它确实在工作目录里。
///
/// rel_segments 已经挡掉了 `..`，这里再比一次是双保险——两处用的是同一套路径原语
/// （git/path.rs），比较语义不会悄悄漂移。
///
/// 注意护栏挡的是**路径拼接**，不是符号链接：工作目录里一条指向 /etc 的链接照样
/// 能被点开。这不构成越权——用户对这台宿主机本来就有 shell（终端里 cat 就行），
/// 而挡下来反而会让 monorepo 里常见的软链目录变成一堆打不开的死项。
pub fn resolve_inside(kind: HostKind, root: &str, rel: Option<&str>) -> Result<String, FileError> {
    let segs = rel_segments(rel)?;
    // 与 Node 版的一处刻意差别（修掉的漏洞）：Windows 上 `\` 也是分隔符，rel_segments 只按 `/`
    // 切，`a\..\..\Windows` 会整段当成一个"名字"混过去；拼成路径后 `..` 原样留在里面，
    // 下面的 is_ancestor 只比字面段（`..` 也算一段）照样放行，而 .NET / Win32 会把它解析掉——
    // 删除就能递归到工作目录外面。Windows 宿主机上把每段再按 `\` 拆开查一遍
    if kind == HostKind::Windows && segs.iter().flat_map(|s| s.split('\\')).any(|p| p == "." || p == "..") {
        return Err(FileError::InvalidPath);
    }
    let base = normalize_sep(kind, root);
    if segs.is_empty() {
        return Ok(base);
    }
    let parts: Vec<&str> = std::iter::once(base.as_str()).chain(segs.iter().map(String::as_str)).collect();
    let full = join_path(kind, &parts);
    if !is_ancestor(kind, &base, &full) && !same_path(kind, &base, &full) {
        return Err(FileError::InvalidPath);
    }
    Ok(full)
}

/// 相对路径拼接，始终用 `/`——这是给前端的形式，与宿主机分隔符无关
fn rel_join(parent: &str, name: &str) -> String {
    if parent.is_empty() { name.to_string() } else { format!("{parent}/{name}") }
}

// ---------------- 列目录 ----------------

/// 每行 `d <size> <mtime> <名字>` 或 `f <size> <mtime> <名字>`。
///
/// 目录的 size 恒 0（前端画成 —）；mtime 是 Unix 秒。名字是行里第三个空格之后的
/// 全部——文件名可以含空格，不能含换行。换行文件名会被拆成两条错行，与 fs.rs
/// 的既有取舍一致：真实仓库里基本不存在，而为它引入 NUL 分隔会牺牲 busybox。
///
/// POSIX 的 size / mtime 先试 `stat -c`（GNU / busybox），没有再试 `stat -f`
/// （BSD / macOS 远端）。列一层多一次 stat 比再开一条命令划算。
pub fn list_command(kind: HostKind, dir: &str) -> String {
    if kind == HostKind::Windows {
        return encode_powershell(
            &[
                format!("$d = {}", quote_powershell(dir)),
                "if (-not (Test-Path -LiteralPath $d)) { Write-Output 'ENOENT'; exit 1 }".to_string(),
                "if (-not (Test-Path -LiteralPath $d -PathType Container)) { Write-Output 'ENOTDIR'; exit 1 }"
                    .to_string(),
                "$epoch = [datetime]::new(1970, 1, 1, 0, 0, 0, [DateTimeKind]::Utc)".to_string(),
                concat!(
                    "Get-ChildItem -LiteralPath $d -Force | ForEach-Object { ",
                    "$k = if ($_.PSIsContainer) { 'd' } else { 'f' }; ",
                    "$s = if ($_.PSIsContainer) { 0 } else { $_.Length }; ",
                    "$m = [int64]($_.LastWriteTimeUtc - $epoch).TotalSeconds; ",
                    "Write-Output ($k + ' ' + $s + ' ' + $m + ' ' + $_.Name) }",
                )
                .to_string(),
            ]
            .join("; "),
        );
    }
    [
        format!("d={}", quote_posix(dir)),
        r#"if [ ! -e "$d" ]; then printf '%s\n' ENOENT; exit 1; fi"#.to_string(),
        r#"if [ ! -d "$d" ]; then printf '%s\n' ENOTDIR; exit 1; fi"#.to_string(),
        r#"if [ ! -r "$d" ]; then printf '%s\n' EACCES; exit 1; fi"#.to_string(),
        // -d 会跟随符号链接：指向目录的链接算目录，点开进去是用户的预期
        concat!(
            r#"ls -1A "$d" | while IFS= read -r n; do "#,
            r#"if [ -d "$d/$n" ]; then k=d; s=0; "#,
            r#"m=$(stat -c %Y "$d/$n" 2>/dev/null || stat -f %m "$d/$n" 2>/dev/null || echo 0); "#,
            r#"else k=f; "#,
            r#"info=$(stat -c '%s %Y' "$d/$n" 2>/dev/null || stat -f '%z %m' "$d/$n" 2>/dev/null || echo '0 0'); "#,
            r#"s=${info%% *}; m=${info#* }; fi; "#,
            r#"printf '%s %s %s %s\n' "$k" "$s" "$m" "$n"; done"#,
        )
        .to_string(),
    ]
    .join("; ")
}

pub fn parse_entries(stdout: &str, parent_rel: &str) -> Vec<WorkspaceEntry> {
    // JS 的 `\d` 只认 ASCII，`.` 不吃 \r \n U+2028 U+2029——照写，不能用 Rust 的默认语义
    static LINE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^([df]) ([0-9]+) ([0-9]+) ([^\n\r\x{2028}\x{2029}]*)$").unwrap());
    let mut out = Vec::new();
    for raw in split_lines(stdout) {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let Some(m) = LINE.captures(line) else { continue };
        let name = &m[4];
        if name.is_empty() || name == "." || name == ".." {
            continue;
        }
        let kind = if &m[1] == "d" { WorkspaceEntryKind::Dir } else { WorkspaceEntryKind::File };
        // `Number("<一串数字>")`：几百位的数字串才会变成 Infinity，那时字段照 TS 省略
        let size = digits_to_f64(&m[2]);
        let mtime = digits_to_f64(&m[3]);
        out.push(WorkspaceEntry {
            name: name.to_string(),
            path: rel_join(parent_rel, name),
            kind,
            size: (kind == WorkspaceEntryKind::File && size.is_finite()).then_some(size as u64),
            mtime: mtime.is_finite().then_some(mtime as i64),
        });
    }
    out
}

fn digits_to_f64(digits: &str) -> f64 {
    digits.parse::<f64>().unwrap_or(f64::NAN)
}

/// 目录在前、文件在后，各自按名字排（localeCompare，中文文件名才不会按码位乱序）。
/// 点开头的不像 fs.rs 那样沉底——仓库里的 .github / .claude 是要看的东西，
/// 把它们压到 node_modules 后面反而找不着。
pub fn sort_entries(mut entries: Vec<WorkspaceEntry>) -> Vec<WorkspaceEntry> {
    // sort_by 是稳定排序，与 Array.prototype.sort 一致：大小写不同的同名项保持原顺序
    entries.sort_by(|a, b| {
        if a.kind != b.kind {
            return if a.kind == WorkspaceEntryKind::Dir { Ordering::Less } else { Ordering::Greater };
        }
        locale_compare_base(&a.name, &b.name)
    });
    entries
}

/// 排序后截到 WORKSPACE_LIST_CAP 条（listLocal / listWorkspace 的收尾）
pub fn cap_entries(entries: Vec<WorkspaceEntry>, path: &str) -> WorkspaceListing {
    let mut sorted = sort_entries(entries);
    let truncated = sorted.len() > WORKSPACE_LIST_CAP;
    sorted.truncate(WORKSPACE_LIST_CAP);
    WorkspaceListing { path: path.to_string(), entries: sorted, truncated }
}

// ---------------- 文件索引（Quick Open） ----------------

/// 目录遍历时跳过的名字。git 仓库走 ls-files（尊重 gitignore），只有非 git
/// 项目才落到这里——node_modules / dist 那种生成物会把索引撑爆，列出来也搜不到。
pub const INDEX_SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    ".svn",
    ".hg",
    "dist",
    "build",
    "out",
    ".next",
    ".nuxt",
    ".output",
    "target",
    "vendor",
    "__pycache__",
    ".venv",
    "venv",
    "coverage",
    ".cache",
    ".turbo",
];

const INDEX_TRUNCATED: &str = "__TRUNCATED__";

fn cap_index(mut paths: Vec<String>, truncated: bool) -> WorkspaceIndex {
    if paths.len() <= WORKSPACE_INDEX_CAP {
        return WorkspaceIndex { paths, truncated };
    }
    paths.truncate(WORKSPACE_INDEX_CAP);
    WorkspaceIndex { paths, truncated: true }
}

/// 把宿主机绝对路径收成工作目录相对、一律 `/` 分隔。对不上前缀的行丢掉
/// （find 偶尔会打一条警告到 stdout 混进来）。
pub fn relativize_index_line(line: &str, root: &str, kind: HostKind) -> Option<String> {
    let raw = line.strip_suffix('\r').unwrap_or(line);
    if raw.is_empty() || raw == INDEX_TRUNCATED {
        return None;
    }
    let full = normalize_sep(kind, raw);
    let base_n = normalize_sep(kind, root);
    let base = base_n.trim_end_matches(['\\', '/']);
    if kind == HostKind::Windows {
        let full_l = full.to_lowercase();
        let base_l = base.to_lowercase();
        if full_l == base_l {
            return None;
        }
        if !full_l.starts_with(&format!("{base_l}\\")) {
            return None;
        }
        // TS 是 `full.slice(base.length + 1)`：按**原始** base 的 UTF-16 长度切原始 full。
        // 小写化可能改变字节长度（K 开尔文号 → k），按字节切会切错甚至切进字符中间
        return Some(slice_utf16_from(&full, utf16_len(base) + 1).replace('\\', "/"));
    }
    if full == base {
        return None;
    }
    full.strip_prefix(base).and_then(|rest| rest.strip_prefix('/')).map(str::to_string)
}

pub fn parse_index_lines(stdout: &str, root: &str, kind: HostKind) -> WorkspaceIndex {
    let mut paths = Vec::new();
    let mut truncated = stdout.contains(INDEX_TRUNCATED);
    for raw in split_lines(stdout) {
        // TS 的 `if (!rel) continue`：null 与空串都跳过
        let Some(rel) = relativize_index_line(raw, root, kind).filter(|r| !r.is_empty()) else {
            continue;
        };
        if rel.split('/').any(|seg| INDEX_SKIP_DIRS.contains(&seg)) {
            continue;
        }
        paths.push(rel);
        if paths.len() >= WORKSPACE_INDEX_CAP {
            truncated = true;
            break;
        }
    }
    WorkspaceIndex { paths, truncated }
}

/// 远端遍历。`-prune` 掉 INDEX_SKIP_DIRS，避免在 node_modules 里转一圈
pub fn index_command(kind: HostKind, dir: &str) -> String {
    if kind == HostKind::Windows {
        let skip_map = INDEX_SKIP_DIRS.iter().map(|n| format!("'{n}' = 1")).collect::<Vec<_>>().join("; ");
        return encode_powershell(
            &[
                format!("$d = {}", quote_powershell(dir)),
                "if (-not (Test-Path -LiteralPath $d -PathType Container)) { Write-Output 'ENOENT'; exit 1 }"
                    .to_string(),
                format!("$skip = @{{ {skip_map} }}"),
                "$n = 0".to_string(),
                format!("$cap = {WORKSPACE_INDEX_CAP}"),
                "function Walk($p) {".to_string(),
                "  if ($script:n -ge $cap) { return }".to_string(),
                "  Get-ChildItem -LiteralPath $p -Force -ErrorAction SilentlyContinue | ForEach-Object {".to_string(),
                "    if ($script:n -ge $cap) { return }".to_string(),
                "    if ($_.PSIsContainer) {".to_string(),
                "      if (-not $skip.ContainsKey($_.Name)) { Walk $_.FullName }".to_string(),
                "    } else {".to_string(),
                "      Write-Output $_.FullName".to_string(),
                "      $script:n++".to_string(),
                "    }".to_string(),
                "  }".to_string(),
                "}".to_string(),
                "Walk $d".to_string(),
                format!("if ($n -ge $cap) {{ Write-Output '{INDEX_TRUNCATED}' }}"),
            ]
            .join("; "),
        );
    }
    let prune = INDEX_SKIP_DIRS.iter().map(|n| format!("-name {}", quote_posix(n))).collect::<Vec<_>>().join(" -o ");
    [
        format!("d={}", quote_posix(dir)),
        r#"if [ ! -d "$d" ]; then printf '%s\n' ENOENT; exit 1; fi"#.to_string(),
        r#"if [ ! -r "$d" ]; then printf '%s\n' EACCES; exit 1; fi"#.to_string(),
        format!(
            r#"find "$d" \( -type d \( {prune} \) \) -prune -o -type f -print | awk -v cap={WORKSPACE_INDEX_CAP} 'NR<=cap{{print}} NR==cap+1{{print "{INDEX_TRUNCATED}"; exit}}'"#
        ),
    ]
    .join("; ")
}

/// git ls-files 的结果过一道上限（routes 的 `/files/index` 用）
pub fn cap_file_index(paths: Vec<String>) -> WorkspaceIndex {
    cap_index(paths, false)
}

// ---------------- 读文件 ----------------

/// 第一行是字节数，其余是内容的 base64（只取前 `cap` 字节）。
///
/// 内容一律 base64 过一道：exec 通道上原始字节靠不住（Windows 的 OpenSSH 会按
/// 代码页改写，见 paste.rs），而且 stdout 是当 UTF-8 解码的，二进制过不去。
pub fn read_command(kind: HostKind, file: &str, cap: u64) -> String {
    if kind == HostKind::Windows {
        return encode_powershell(
            &[
                format!("$p = {}", quote_powershell(file)),
                "if (-not (Test-Path -LiteralPath $p)) { Write-Output 'ENOENT'; exit 1 }".to_string(),
                "$i = Get-Item -LiteralPath $p -Force".to_string(),
                "if ($i.PSIsContainer) { Write-Output 'EISDIR'; exit 1 }".to_string(),
                "Write-Output $i.Length".to_string(),
                "$fsr = [IO.File]::OpenRead($p)".to_string(),
                [
                    "try { ".to_string(),
                    "$ms = New-Object IO.MemoryStream; ".to_string(),
                    "$buf = New-Object byte[] 65536; ".to_string(),
                    // Read 不保证一次给满，循环到读够 cap 或文件结束
                    format!("while ($ms.Length -lt {cap}) {{ "),
                    format!("$want = [Math]::Min($buf.Length, {cap} - $ms.Length); "),
                    "$n = $fsr.Read($buf, 0, $want); ".to_string(),
                    "if ($n -le 0) { break }; ".to_string(),
                    "$ms.Write($buf, 0, $n) }; ".to_string(),
                    "Write-Output ([Convert]::ToBase64String($ms.ToArray())) ".to_string(),
                    "} finally { $fsr.Close() }".to_string(),
                ]
                .concat(),
            ]
            .join("; "),
        );
    }
    [
        format!("f={}", quote_posix(file)),
        r#"if [ ! -e "$f" ]; then printf '%s\n' ENOENT; exit 1; fi"#.to_string(),
        r#"if [ -d "$f" ]; then printf '%s\n' EISDIR; exit 1; fi"#.to_string(),
        r#"if [ ! -r "$f" ]; then printf '%s\n' EACCES; exit 1; fi"#.to_string(),
        r#"wc -c < "$f" | tr -d ' '"#.to_string(),
        // busybox / 精简镜像里 base64 未必有，openssl 是最常见的替补
        r#"if command -v base64 >/dev/null 2>&1; then b64=base64; else b64="openssl base64"; fi"#.to_string(),
        format!(r#"head -c {cap} "$f" | $b64"#),
    ]
    .join("; ")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadResult {
    /// 文件真实字节数（不是取回来的字节数）
    pub size: u64,
    pub bytes: Vec<u8>,
}

/// 解析 [`read_command`] 的输出。
///
/// 首行按 JS `Number()` 的规则转数字（空串是 0、`0x10` 是 16……），非有限或为负才报错；
/// 带小数的大小 TS 会原样收下，这里截成整数（`wc -c` / `.Length` 不会给出小数）。
pub fn parse_read(stdout: &str) -> Result<ReadResult, FileError> {
    let mut lines = split_lines(stdout);
    let size = js_string_to_number(js_trim(lines.next().unwrap_or("")));
    if !size.is_finite() || size < 0.0 {
        return Err(FileError::UnreadableSize);
    }
    // base64 输出按 76 字符折行（openssl 与 GNU base64 都是），拼回一整串再解。
    // TS 先 `.replace(/\s+/g, "")` 再 `Buffer.from(b64, "base64")`；后者本来就跳过
    // 字母表以外的一切字符，空白也在其中，所以这里直接交给宽松解码
    let b64: String = lines.collect();
    Ok(ReadResult { size: size as u64, bytes: decode_base64_lenient(&b64) })
}

/// 扩展名 → Content-Type。原始字节路由靠它告诉浏览器怎么处理一个文件：
/// HTML 当文档渲染、CSS / JS 当子资源、图片当图片。表里没有的一律
/// application/octet-stream——配合 nosniff，浏览器不会把它猜成可执行的东西。
///
/// 不引入 mime-db（Rust 侧即 mime / mime_guess crate）：这里只需要 web 预览会碰到的
/// 那几十种，一整张表大而无当，还会悄悄改掉现在的返回值（比如带不带 charset）。
fn mime_for_ext(ext: &str) -> Option<&'static str> {
    Some(match ext {
        // 图片：`<img>` 里的 SVG 不执行脚本，当图片渲染是安全的
        "png" => "image/png",
        "jpg" => "image/jpeg",
        "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "avif" => "image/avif",
        "svg" => "image/svg+xml",
        // 文档与子资源
        "html" => "text/html; charset=utf-8",
        "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "mjs" => "text/javascript; charset=utf-8",
        "cjs" => "text/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "map" => "application/json; charset=utf-8",
        "xml" => "application/xml; charset=utf-8",
        "txt" => "text/plain; charset=utf-8",
        "md" => "text/plain; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "wasm" => "application/wasm",
        "pdf" => "application/pdf",
        // 字体
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        // 音视频
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        _ => return None,
    })
}

/// 最后一个 `.` 之后的部分，小写。点开头且只有一个点（`.gitignore`）算没有扩展名
pub fn ext_of(name: &str) -> String {
    match name.rfind('.') {
        None | Some(0) => String::new(),
        Some(i) => name[i + 1..].to_lowercase(),
    }
}

/// 原始字节路由用的 Content-Type，认不出给 octet-stream
pub fn mime_of(name: &str) -> &'static str {
    mime_for_ext(&ext_of(name)).unwrap_or("application/octet-stream")
}

/// 查看 tab 把这些扩展名当图片：走原始字节路由给 `<img>`，不读文本
pub fn image_mime_of(name: &str) -> Option<&'static str> {
    mime_for_ext(&ext_of(name)).filter(|m| m.starts_with("image/"))
}

/// 字节 → 前端能直接渲染的形状。
///
/// 图片不看字节只看扩展名与大小：字节由浏览器自己经原始字节路由取，这里
/// 收到的 bytes 通常是空的（readWorkspaceFile 对图片按 cap 0 读）。
///
/// 二进制判定跟 git 一样看 NUL：前 8KB 里出现 NUL 就当二进制。这条规则会把
/// UTF-16 文本也判成二进制，但那在源码仓库里几乎不出现，而反过来把真二进制
/// 当文本渲染会当场喷出几兆乱码。
pub fn classify(name: &str, size: u64, bytes: &[u8]) -> FilePreview {
    if let Some(mime) = image_mime_of(name) {
        if size > WORKSPACE_RAW_CAP {
            return FilePreview::TooLarge { size };
        }
        return FilePreview::Image { mime: mime.to_string(), size };
    }
    let head = &bytes[..bytes.len().min(8192)];
    if head.contains(&0) {
        return FilePreview::Binary { size };
    }
    FilePreview::Text {
        // Buffer#toString("utf8") 与 from_utf8_lossy 都按"最大子部分"替换成 U+FFFD，
        // 被 cap 切开的末尾半个字符两边都变成一个 U+FFFD
        text: String::from_utf8_lossy(bytes).into_owned(),
        size,
        truncated: size > bytes.len() as u64,
    }
}

// ---------------- 改目录（mkdir / rename / remove） ----------------

/// 单段文件名的护栏。mkdir / rename / 上传共用。
///
/// 浏览器给的 File.name 不会含分隔符，会含的只能是构造出来的请求；控制字符与
/// `..` 同 rel_segments 的理由。Windows 的保留字符在远端也会失败，但那边的报错
/// 是一段 .NET 异常文本，不如在这里直接说清楚。
pub fn validate_entry_name(kind: HostKind, name: &str) -> Result<(), FileError> {
    if name.is_empty() || name == "." || name == ".." {
        return Err(FileError::InvalidName);
    }
    if name.chars().any(|c| is_c0_control(c) || c == '/' || c == '\\') {
        return Err(FileError::InvalidName);
    }
    // TS 的 `name.length`：UTF-16 码元数，不是字节数也不是字符数
    if utf16_len(name) > 255 {
        return Err(FileError::NameTooLong);
    }
    if kind == HostKind::Windows && name.chars().any(|c| matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*')) {
        return Err(FileError::WindowsForbiddenChar);
    }
    Ok(())
}

fn validate_rel_segments(kind: HostKind, rel: &str) -> Result<Vec<String>, FileError> {
    let segs = rel_segments(Some(rel))?;
    if segs.is_empty() {
        return Err(FileError::InvalidPath);
    }
    for seg in &segs {
        validate_entry_name(kind, seg)?;
    }
    Ok(segs)
}

/// 批量删除时把子孙路径收进祖先：删 `src` 就不必再删 `src/a.ts`。
/// 空串（工作目录本身）丢掉——那是护栏，不是省略。
pub fn collapse_remove_paths<S: AsRef<str>>(paths: &[S]) -> Vec<String> {
    let mut norm: Vec<String> =
        paths.iter().map(|p| p.as_ref().trim_end_matches('/')).filter(|p| !p.is_empty()).map(str::to_string).collect();
    // JS 默认的 sort() 按 UTF-16 码元比，与 Rust 按码点（UTF-8 字节）比在 U+E000..U+FFFF
    // 与增补平面字符之间相反，照 JS 排
    norm.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    norm.dedup();
    let mut out: Vec<String> = Vec::new();
    for p in norm {
        if out.iter().any(|parent| p.starts_with(&format!("{parent}/"))) {
            continue;
        }
        out.push(p);
    }
    out
}

pub fn mkdir_command(kind: HostKind, dir: &str, parent: &str, recursive: bool) -> String {
    if kind == HostKind::Windows {
        let middle = if recursive {
            "if (Test-Path -LiteralPath $p -PathType Container) { exit 0 }".to_string()
        } else {
            [
                "if (Test-Path -LiteralPath $p) { Write-Output 'EEXIST'; exit 1 }",
                "if (-not (Test-Path -LiteralPath $g -PathType Container)) { Write-Output 'ENOENT'; exit 1 }",
            ]
            .join("; ")
        };
        return encode_powershell(
            &[
                format!("$p = {}", quote_powershell(dir)),
                format!("$g = {}", quote_powershell(parent)),
                "if (Test-Path -LiteralPath $p -PathType Leaf) { Write-Output 'EEXIST'; exit 1 }".to_string(),
                middle,
                format!(
                    "New-Item -ItemType Directory -LiteralPath $p{} | Out-Null",
                    if recursive { " -Force" } else { "" }
                ),
            ]
            .join("; "),
        );
    }
    let p = quote_posix(dir);
    let g = quote_posix(parent);
    if recursive {
        return [
            format!("p={p}"),
            r#"if [ -e "$p" ] && [ ! -d "$p" ]; then printf '%s\n' EEXIST; exit 1; fi"#.to_string(),
            r#"if [ -d "$p" ]; then exit 0; fi"#.to_string(),
            r#"mkdir -p -- "$p" || { printf '%s\n' EACCES; exit 1; }"#.to_string(),
        ]
        .join("; ");
    }
    [
        format!("p={p}"),
        format!("g={g}"),
        r#"if [ -e "$p" ]; then printf '%s\n' EEXIST; exit 1; fi"#.to_string(),
        r#"if [ ! -d "$g" ]; then printf '%s\n' ENOENT; exit 1; fi"#.to_string(),
        r#"mkdir -- "$p" || { printf '%s\n' EACCES; exit 1; }"#.to_string(),
    ]
    .join("; ")
}

pub fn rename_command(kind: HostKind, src: &str, dst: &str) -> String {
    if kind == HostKind::Windows {
        return encode_powershell(
            &[
                format!("$s = {}", quote_powershell(src)),
                format!("$d = {}", quote_powershell(dst)),
                "if (-not (Test-Path -LiteralPath $s)) { Write-Output 'ENOENT'; exit 1 }".to_string(),
                "if (Test-Path -LiteralPath $d) { Write-Output 'EEXIST'; exit 1 }".to_string(),
                "Move-Item -LiteralPath $s -Destination $d".to_string(),
            ]
            .join("; "),
        );
    }
    [
        format!("s={}", quote_posix(src)),
        format!("d={}", quote_posix(dst)),
        r#"if [ ! -e "$s" ] && [ ! -L "$s" ]; then printf '%s\n' ENOENT; exit 1; fi"#.to_string(),
        r#"if [ -e "$d" ] || [ -L "$d" ]; then printf '%s\n' EEXIST; exit 1; fi"#.to_string(),
        r#"mv -- "$s" "$d" || { printf '%s\n' EACCES; exit 1; }"#.to_string(),
    ]
    .join("; ")
}

/// 递归删除文件或目录。
///
/// POSIX `rm -rf --` 对作为参数的符号链接只删链接本身，不跟过去——这是我们要的：
/// 工作目录里一条指向 /etc 的链接被删掉，不该把 /etc 带走。Windows 走
/// `[IO.Directory]::Delete` / `[IO.File]::Delete`：不走 Remove-Item 的通配展开，
/// 也不穿越 reparse point（理由同 ADR 0002）。
pub fn remove_command(kind: HostKind, full: &str) -> String {
    if kind == HostKind::Windows {
        return encode_powershell(
            &[
                format!("$p = {}", quote_powershell(full)),
                "if (-not (Test-Path -LiteralPath $p)) { Write-Output 'ENOENT'; exit 1 }".to_string(),
                "$item = Get-Item -LiteralPath $p -Force".to_string(),
                "if ($item.PSIsContainer) { [IO.Directory]::Delete($p, $true) } else { [IO.File]::Delete($p) }"
                    .to_string(),
            ]
            .join("; "),
        );
    }
    [
        format!("p={}", quote_posix(full)),
        r#"if [ ! -e "$p" ] && [ ! -L "$p" ]; then printf '%s\n' ENOENT; exit 1; fi"#.to_string(),
        r#"rm -rf -- "$p" || { printf '%s\n' EACCES; exit 1; }"#.to_string(),
    ]
    .join("; ")
}

/// mkdirWorkspace 开头的纯路径运算：校验每一段、拼出要建的目录与它的上级。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MkdirTarget {
    /// 规整后的工作目录相对路径，成功时原样回给前端（FileOpResult.path）
    pub path: String,
    /// 宿主机上要建的目录
    pub dir: String,
    /// 它的上级（非递归时必须已存在）
    pub parent: String,
}

pub fn mkdir_target(kind: HostKind, root: &str, rel: &str) -> Result<MkdirTarget, FileError> {
    let segs = validate_rel_segments(kind, rel)?;
    let path = segs.join("/");
    let dir = resolve_inside(kind, root, Some(&path))?;
    let parent = dirname_of(kind, &dir);
    Ok(MkdirTarget { path, dir, parent })
}

/// renameWorkspace 开头的纯路径运算。只改最后一段名字，不移动到别的目录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameTarget {
    /// 改名后的工作目录相对路径（FileOpResult.path）
    pub path: String,
    pub src: String,
    pub dst: String,
    /// `same_path(src, dst)`：TS 直接返回成功、什么都不做。Windows 上 same_path
    /// 不分大小写，所以只改大小写的重命名也落在这里
    pub unchanged: bool,
}

pub fn rename_target(kind: HostKind, root: &str, rel: &str, name: &str) -> Result<RenameTarget, FileError> {
    let segs = validate_rel_segments(kind, rel)?;
    validate_entry_name(kind, name)?;
    let parent_rel = segs[..segs.len() - 1].join("/");
    let dest_rel = if parent_rel.is_empty() { name.to_string() } else { format!("{parent_rel}/{name}") };
    let src = resolve_inside(kind, root, Some(&segs.join("/")))?;
    let dst = resolve_inside(kind, root, Some(&dest_rel))?;
    let unchanged = same_path(kind, &src, &dst);
    Ok(RenameTarget { path: dest_rel, src, dst, unchanged })
}

/// removeWorkspace 循环体里的护栏：删除只作用于 resolve_inside 之后的工作目录内部，
/// 不能删工作目录本身（空路径、或拼出来与工作目录是同一个路径）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoveTarget {
    /// 规整后的工作目录相对路径，删成功时进 FileRemoveResult.removed
    pub path: String,
    /// 宿主机上要递归删的路径
    pub full: String,
}

/// `rel` 是 [`collapse_remove_paths`] 之后的一条；失败时 TS 把**这条 rel 原文**（不是
/// 规整后的 path）连同错误消息记进 errors。
pub fn remove_target(kind: HostKind, root: &str, rel: &str) -> Result<RemoveTarget, FileError> {
    // 空 rel 不会抛，与 TS 在循环外算 rootFull 等价
    let root_full = resolve_inside(kind, root, Some(""))?;
    let segs = rel_segments(Some(rel))?;
    if segs.is_empty() {
        return Err(FileError::RemoveRoot);
    }
    let path = segs.join("/");
    let full = resolve_inside(kind, root, Some(&path))?;
    if same_path(kind, &full, &root_full) {
        return Err(FileError::RemoveRoot);
    }
    Ok(RemoveTarget { path, full })
}

// ---------------- 执行环境 ----------------

/// 远端命令的流式通道：写入端是命令的 stdin，读取端是 stdout，`ExitStatus` 带退出码
/// （TS 的 `ExecChannel`：ssh2 的 ClientChannel 正是这个形状）。下载 / 上传（transfer.rs）
/// 按流走，exec 那种"攒成字符串"的形状装不下一个几百 MB 的文件。
///
/// russh 的 Channel 是 `Send` 的：可以从会话引擎里拿出来，在 HTTP 处理器的线程上读写。
pub type ExecChannel = russh::Channel<russh::client::Msg>;

/// 远端宿主机：攒成字符串的 exec 之外，还要流式通道（TS 的 `ExecStreamFn`）。
/// 正式实现是 [`SshLink`]；测试里用一个经本机 sh 跑命令的假远端，把远端脚本也跑一遍。
pub trait RemoteHost: Exec {
    fn exec_stream<'a>(&'a self, command_line: &'a str) -> LocalBoxFuture<'a, anyhow::Result<ExecChannel>>;
}

impl RemoteHost for SshLink {
    fn exec_stream<'a>(&'a self, command_line: &'a str) -> LocalBoxFuture<'a, anyhow::Result<ExecChannel>> {
        // 不要 pty：下载 / 上传要的是原样的字节流，pty 会把 \n 改成 \r\n、把 stderr 并进来
        Box::pin(SshLink::exec_stream(self, command_line, false))
    }
}

/// 执行环境。本地不走 exec——node_modules 那种目录用 shell 循环列会慢到没法用
#[derive(Clone, Copy)]
pub enum FileHost<'a> {
    Local { kind: HostKind },
    Remote { kind: HostKind, remote: &'a dyn RemoteHost },
}

impl FileHost<'_> {
    pub fn kind(&self) -> HostKind {
        match self {
            FileHost::Local { kind } | FileHost::Remote { kind, .. } => *kind,
        }
    }
}

/// 带超时的远端 exec（TS 的 `execTimed`）。超时报 `timeout_msg`，exec 本身起不来（链路故障）
/// 报 "SSH 连接失败：…"；非零退出码照 ExecFn 的铁律原样返回，由调用方查 `code`。
///
/// 超时先经取消令牌让执行器收尾（远端关通道、本地杀进程），再等一小会儿就放手——
/// 链路卡在握手里时执行器根本看不到令牌，不能一直等它
pub(crate) async fn exec_timed(
    exec: &dyn Exec,
    command: &str,
    timeout: Duration,
    timeout_msg: &str,
) -> Result<ExecResult, FileError> {
    let token = CancellationToken::new();
    let fut = exec.exec(command, Some(&token));
    tokio::pin!(fut);
    let res = tokio::select! {
        r = &mut fut => r,
        () = tokio::time::sleep(timeout) => {
            token.cancel();
            let _ = tokio::time::timeout(Duration::from_secs(2), &mut fut).await;
            return Err(FileError::Other(timeout_msg.to_string()));
        }
    };
    res.map_err(|e| FileError::Other(format!("SSH 连接失败：{e:#}")))
}

/// 本机文件系统的阻塞调用挪到阻塞线程池上跑（tokio 的 current_thread 运行时也有这个池）
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, FileError> + Send + 'static) -> Result<T, FileError> {
    tokio::task::spawn_blocking(f).await.unwrap_or_else(|e| Err(FileError::Other(e.to_string())))
}

/// `Math.floor(stat.mtimeMs / 1000)`：Unix 秒，1970 年以前的向下取整成负数
fn mtime_secs(meta: &std::fs::Metadata) -> i64 {
    match meta.modified() {
        Ok(t) => match t.duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_secs() as i64,
            Err(e) => -(e.duration().as_secs_f64().ceil() as i64),
        },
        Err(_) => 0,
    }
}

// ---------------- 列目录（运行时） ----------------

fn list_local(kind: HostKind, dir: &str, rel: &str) -> Result<WorkspaceListing, FileError> {
    let dirents = std::fs::read_dir(dir).map_err(|e| local_error(&e, "无法读取该目录"))?;
    let mut entries = Vec::new();
    for ent in dirents.flatten() {
        let name = ent.file_name().to_string_lossy().into_owned();
        let full = join_path(kind, &[dir, &name]);
        // 跟过去看：指向目录的链接算 dir。断掉的链接当文件，size / mtime 用 lstat
        let st = match std::fs::metadata(&full).or_else(|_| std::fs::symlink_metadata(&full)) {
            Ok(st) => st,
            Err(_) => continue,
        };
        let is_dir = st.is_dir();
        entries.push(WorkspaceEntry {
            path: rel_join(rel, &name),
            name,
            kind: if is_dir { WorkspaceEntryKind::Dir } else { WorkspaceEntryKind::File },
            size: (!is_dir).then(|| st.len()),
            mtime: Some(mtime_secs(&st)),
        });
    }
    Ok(cap_entries(entries, rel))
}

/// 列工作目录里的一层。`rel` 为空表示工作目录本身
pub async fn list_workspace(host: &FileHost<'_>, root: &str, rel: Option<&str>) -> Result<WorkspaceListing, FileError> {
    let rel_path = rel_segments(rel)?.join("/");
    let kind = host.kind();
    let dir = resolve_inside(kind, root, rel)?;
    let FileHost::Remote { remote, .. } = *host else {
        return blocking(move || list_local(kind, &dir, &rel_path)).await;
    };
    let res = exec_timed(remote, &list_command(kind, &dir), LIST_TIMEOUT, "读取超时").await?;
    if !res.ok() {
        return Err(remote_failure(&res, "无法读取该目录"));
    }
    Ok(cap_entries(parse_entries(&res.stdout, &rel_path), &rel_path))
}

// ---------------- 文件索引（运行时） ----------------

fn index_local(kind: HostKind, root: &str) -> WorkspaceIndex {
    let mut paths = Vec::new();
    let mut stack = vec![String::new()];
    while let Some(rel) = stack.pop() {
        let dir = if rel.is_empty() {
            root.to_string()
        } else {
            let parts: Vec<&str> = std::iter::once(root).chain(rel.split('/')).collect();
            join_path(kind, &parts)
        };
        let Ok(dirents) = std::fs::read_dir(&dir) else { continue };
        for ent in dirents.flatten() {
            // DirEntry::file_type 不跟符号链接（同 Node 的 Dirent）
            let Ok(ft) = ent.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            let name = ent.file_name().to_string_lossy().into_owned();
            if INDEX_SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            let child = rel_join(&rel, &name);
            if ft.is_dir() {
                stack.push(child);
                continue;
            }
            paths.push(child);
            if paths.len() >= WORKSPACE_INDEX_CAP {
                return WorkspaceIndex { paths, truncated: true };
            }
        }
    }
    WorkspaceIndex { paths, truncated: false }
}

/// 非 git 项目的兜底：遍历工作目录，跳过 INDEX_SKIP_DIRS
pub async fn index_workspace(host: &FileHost<'_>, root: &str) -> Result<WorkspaceIndex, FileError> {
    let kind = host.kind();
    let dir = resolve_inside(kind, root, Some(""))?;
    let FileHost::Remote { remote, .. } = *host else {
        return blocking(move || Ok(index_local(kind, &dir))).await;
    };
    let res = exec_timed(remote, &index_command(kind, &dir), INDEX_TIMEOUT, "读取超时").await?;
    if !res.ok() {
        return Err(remote_failure(&res, "无法读取该目录"));
    }
    Ok(parse_index_lines(&res.stdout, &dir, kind))
}

// ---------------- 读文件（运行时） ----------------

fn read_local(file: &str, cap: u64) -> Result<ReadResult, FileError> {
    use std::io::Read as _;
    let stat = std::fs::metadata(file).map_err(|e| local_error(&e, "读不到该文件"))?;
    if stat.is_dir() {
        return Err(FileError::IsDir);
    }
    let size = stat.len();
    let want = size.min(cap);
    // 只要大小（图片）就不开文件了
    if want == 0 {
        return Ok(ReadResult { size, bytes: Vec::new() });
    }
    let handle = std::fs::File::open(file).map_err(|e| local_error(&e, "读不到该文件"))?;
    let mut bytes = Vec::with_capacity(want as usize);
    handle.take(want).read_to_end(&mut bytes).map_err(|e| local_error(&e, "读不到该文件"))?;
    Ok(ReadResult { size, bytes })
}

/// 读工作目录里一个文件的前 cap 个字节，连同真实大小与文件名（最后一段）。查看 tab 与
/// 原始字节路由共用：前者对文本取 WORKSPACE_FILE_CAP、对图片取 0（只要大小），后者取
/// WORKSPACE_RAW_CAP；下载取 0（只为在响应头发出之前把错误判掉）。
pub async fn read_workspace_bytes(
    host: &FileHost<'_>,
    root: &str,
    rel: &str,
    cap: u64,
) -> Result<(String, ReadResult), FileError> {
    let segs = rel_segments(Some(rel))?;
    let Some(name) = segs.last().cloned() else {
        return Err(FileError::InvalidPath);
    };
    let kind = host.kind();
    let file = resolve_inside(kind, root, Some(rel))?;
    let FileHost::Remote { remote, .. } = *host else {
        let read = blocking(move || read_local(&file, cap)).await?;
        return Ok((name, read));
    };
    let timeout = if cap > WORKSPACE_FILE_CAP { RAW_TIMEOUT } else { READ_TIMEOUT };
    let res = exec_timed(remote, &read_command(kind, &file, cap), timeout, "读取超时").await?;
    if !res.ok() {
        return Err(remote_failure(&res, "读不到该文件"));
    }
    Ok((name, parse_read(&res.stdout)?))
}

/// 读工作目录里的一个文件，供查看 tab 渲染
pub async fn read_workspace_file(host: &FileHost<'_>, root: &str, rel: &str) -> Result<FilePreview, FileError> {
    let segs = rel_segments(Some(rel))?;
    let name = segs.last().map(String::as_str).unwrap_or("");
    // 图片的字节浏览器会自己去原始字节路由取，这里只要大小；`head -c 0` 与
    // PowerShell 那个 0 字节循环都是合法的空读
    let cap = if image_mime_of(name).is_some() { 0 } else { WORKSPACE_FILE_CAP };
    let (name, read) = read_workspace_bytes(host, root, rel, cap).await?;
    Ok(classify(&name, read.size, &read.bytes))
}

// ---------------- 改目录（运行时） ----------------

pub async fn mkdir_workspace(
    host: &FileHost<'_>,
    root: &str,
    rel: &str,
    recursive: bool,
) -> Result<FileOpResult, FileError> {
    let kind = host.kind();
    let MkdirTarget { path, dir, parent } = mkdir_target(kind, root, rel)?;
    let FileHost::Remote { remote, .. } = *host else {
        blocking(move || mkdir_local(&dir, &parent, recursive)).await?;
        return Ok(FileOpResult { path });
    };
    let res = exec_timed(remote, &mkdir_command(kind, &dir, &parent, recursive), MKDIR_TIMEOUT, "操作超时").await?;
    if !res.ok() {
        return Err(remote_failure(&res, "无法创建文件夹"));
    }
    Ok(FileOpResult { path })
}

fn mkdir_local(dir: &str, parent: &str, recursive: bool) -> Result<(), FileError> {
    if !recursive {
        let pst = std::fs::metadata(parent).map_err(|e| local_error(&e, "上级目录不可访问"))?;
        if !pst.is_dir() {
            return Err(FileError::NotDir);
        }
    }
    // create_dir_all 遇到已存在的目录算成功、已存在的文件报 AlreadyExists（"同名文件已存在"），
    // 与 Node 的 mkdir({ recursive: true }) 一致
    let res = if recursive { std::fs::create_dir_all(dir) } else { std::fs::create_dir(dir) };
    res.map_err(|e| local_error(&e, "无法创建文件夹"))
}

pub async fn rename_workspace(
    host: &FileHost<'_>,
    root: &str,
    rel: &str,
    name: &str,
) -> Result<FileOpResult, FileError> {
    let kind = host.kind();
    let RenameTarget { path, src, dst, unchanged } = rename_target(kind, root, rel, name)?;
    if unchanged {
        return Ok(FileOpResult { path });
    }
    let FileHost::Remote { remote, .. } = *host else {
        blocking(move || rename_local(&src, &dst)).await?;
        return Ok(FileOpResult { path });
    };
    let res = exec_timed(remote, &rename_command(kind, &src, &dst), RENAME_TIMEOUT, "操作超时").await?;
    if !res.ok() {
        return Err(remote_failure(&res, "无法重命名"));
    }
    Ok(FileOpResult { path })
}

fn rename_local(src: &str, dst: &str) -> Result<(), FileError> {
    std::fs::symlink_metadata(src).map_err(|e| local_error(&e, "路径不存在或不可访问"))?;
    match std::fs::symlink_metadata(dst) {
        Ok(_) => return Err(FileError::Exists),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(local_error(&e, "无法重命名")),
    }
    std::fs::rename(src, dst).map_err(|e| local_error(&e, "无法重命名"))
}

/// 删除工作目录里的若干项。每条单独试，互不挡住；失败的连同 rel 原文记进 errors
pub async fn remove_workspace<S: AsRef<str>>(
    host: &FileHost<'_>,
    root: &str,
    rels: &[S],
) -> FileRemoveResult {
    let kind = host.kind();
    let mut removed = Vec::new();
    let mut errors = Vec::new();
    for rel in collapse_remove_paths(rels) {
        let outcome = async {
            let RemoveTarget { path, full } = remove_target(kind, root, &rel)?;
            match *host {
                FileHost::Local { .. } => blocking(move || remove_local(&full)).await?,
                FileHost::Remote { remote, .. } => {
                    let res = exec_timed(remote, &remove_command(kind, &full), REMOVE_TIMEOUT, "操作超时").await?;
                    if !res.ok() {
                        return Err(remote_failure(&res, "无法删除"));
                    }
                }
            }
            Ok(path)
        }
        .await;
        match outcome {
            Ok(path) => removed.push(path),
            Err(e) => errors.push(FileRemoveError { path: rel, error: e.to_string() }),
        }
    }
    FileRemoveResult { removed, errors }
}

/// `fs.rm(full, { recursive: true, force: false })`：按 lstat 判断，符号链接只删链接本身、
/// 不跟过去（工作目录里一条指向 /etc 的链接被删掉，不该把 /etc 带走）；目录递归删，
/// `remove_dir_all` 同样不穿越里面的符号链接
fn remove_local(full: &str) -> Result<(), FileError> {
    let meta = std::fs::symlink_metadata(full).map_err(|e| local_error(&e, "无法删除"))?;
    let res = if meta.is_dir() {
        std::fs::remove_dir_all(full)
    } else {
        // Windows 上指向目录的符号链接 / junction 要用 remove_dir 删
        let is_link = meta.file_type().is_symlink();
        std::fs::remove_file(full).or_else(|e| if is_link { std::fs::remove_dir(full) } else { Err(e) })
    };
    res.map_err(|e| local_error(&e, "无法删除"))
}

// ---------------- JS 语义的几处小工具 ----------------
//
// 照抄 TS 时 Rust 标准库里同名的东西语义不一样，差异收在这里（与 falcon-core 的 js.rs
// 同源；那边是私有模块，这里也不为几十行去改它的可见性）。fs.rs 也用。

/// `/[\u0000-\u001f]/`
fn is_c0_control(c: char) -> bool {
    c <= '\u{1f}'
}

/// ECMAScript 的 WhiteSpace + LineTerminator：`trim()` 与正则 `\s` 用的就是这一套
/// （含 U+FEFF、不含 U+0085，与 `char::is_whitespace` 不同）。
fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{9}'..='\u{d}'
            | ' '
            | '\u{a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
            | '\u{feff}'
    )
}

/// `String.prototype.trim`
pub(crate) fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_whitespace)
}

/// `String.prototype.trimEnd`
pub(crate) fn js_trim_end(s: &str) -> &str {
    s.trim_end_matches(is_js_whitespace)
}

/// `s.split(/\r?\n/)`。按 `\n` 切再去掉每段末尾的一个 `\r`，与正则逐段等价
/// （`"a\r\r\nb"` 两边都切成 `["a\r", "b"]`）。
pub(crate) fn split_lines(s: &str) -> impl Iterator<Item = &str> {
    s.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l))
}

/// `s.length`：UTF-16 码元数
fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// `s.slice(n)`：跳过前 n 个 UTF-16 码元。落在代理对中间时 JS 会留下半个代理，
/// Rust 表示不了，那个字符整个不要。
fn slice_utf16_from(s: &str, n: usize) -> &str {
    let mut units = 0;
    for (i, c) in s.char_indices() {
        if units >= n {
            return &s[i..];
        }
        units += c.len_utf16();
    }
    ""
}

/// `a.localeCompare(b, undefined, { sensitivity: "base" })` 的近似。
///
/// V8 走 ICU 排序规则（服务端进程的默认语言，一般是 en-US）；Rust 标准库没有排序
/// 规则数据，为几处列表排序拖一整套 ICU 不值当。照 CLDR 根排序只做一级比较：
/// 空白 < 标点符号（按 CLDR 的次序）< 数字 < 拉丁字母（不分大小写）< 其它字符（按码位）。
/// `sensitivity: "base"` 正是只看一级：`a` 与 `A` 相等（交给稳定排序保持原顺序）。
///
/// 已知对不上的：带重音的拉丁字母（ICU 把 é 当 e，这里当"其它字符"）、全角字母、
/// 汉字（en-US 下 ICU 对常用汉字近似码位序，扩展区不是）。纯 ASCII 的名字与 TS 一致。
pub(crate) fn locale_compare_base(a: &str, b: &str) -> Ordering {
    a.chars().map(primary_weight).cmp(b.chars().map(primary_weight))
}

/// CLDR 根排序里 ASCII 标点符号的次序（与 falcon-core js.rs 同一份）
const PUNCT_ORDER: &str = "_-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$";

fn primary_weight(c: char) -> (u8, u32) {
    if is_js_whitespace(c) {
        return (0, c as u32);
    }
    if let Some(i) = PUNCT_ORDER.find(c) {
        return (1, i as u32);
    }
    if c.is_ascii_digit() {
        return (2, c as u32);
    }
    if c.is_ascii_alphabetic() {
        return (3, c.to_ascii_lowercase() as u32);
    }
    (4, c as u32)
}

/// `Number(string)`（ECMAScript StringToNumber）：首尾空白、空串为 0、十六进制 /
/// 八进制 / 二进制前缀、`Infinity`；`str::parse::<f64>` 这些一样都不认，反而认
/// `inf` / `nan`，所以先按 JS 的文法筛一遍。
fn js_string_to_number(s: &str) -> f64 {
    static DECIMAL: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^[+-]?([0-9]+\.?[0-9]*|\.[0-9]+)([eE][+-]?[0-9]+)?$").unwrap());
    let t = js_trim(s);
    if t.is_empty() {
        return 0.0;
    }
    match t {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    let b = t.as_bytes();
    if b.len() > 2 && b[0] == b'0' {
        let radix = match b[1] {
            b'x' | b'X' => 16,
            b'o' | b'O' => 8,
            b'b' | b'B' => 2,
            _ => 0,
        };
        if radix != 0 {
            return t[2..]
                .chars()
                .try_fold(0.0_f64, |acc, c| c.to_digit(radix).map(|d| acc * f64::from(radix) + f64::from(d)))
                .unwrap_or(f64::NAN);
        }
    }
    if DECIMAL.is_match(t) { t.parse::<f64>().unwrap_or(f64::NAN) } else { f64::NAN }
}

/// `Buffer.from(s, "base64")` 的宽松解码：标准与 URL-safe 两套字母表都认，字母表以外
/// 的字符（空白、`*`、非 ASCII……）跳过，遇到第一个 `=` 停，凑不满 8 位的尾巴丢掉。
/// 永不失败——TS 这一步不会抛，base64 crate 的严格解码会。
fn decode_base64_lenient(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits = 0;
    for c in s.chars() {
        let v = match c {
            'A'..='Z' => c as u32 - 'A' as u32,
            'a'..='z' => c as u32 - 'a' as u32 + 26,
            '0'..='9' => c as u32 - '0' as u32 + 52,
            '+' | '-' => 62,
            '/' | '_' => 63,
            '=' => break,
            _ => continue,
        };
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Node 版的漏洞：Windows 上反斜杠分隔的 `..` 能绕过护栏（见 resolve_inside 的注释）
    #[test]
    fn windows_backslash_dotdot_cannot_escape_workspace() {
        let root = "C:\\code\\repo";
        assert!(resolve_inside(HostKind::Windows, root, Some("a\\..\\..\\..\\Windows")).is_err());
        assert!(resolve_inside(HostKind::Windows, root, Some("a/b\\..")).is_err());
        assert!(resolve_inside(HostKind::Windows, root, Some(".\\x")).is_err());
        assert_eq!(resolve_inside(HostKind::Windows, root, Some("a\\b")).unwrap(), "C:\\code\\repo\\a\\b");
        // POSIX 上反斜杠是合法的文件名字符，`a\..` 就是一个叫这名字的文件，不拦
        assert!(resolve_inside(HostKind::Posix, "/code/repo", Some("a\\..")).is_ok());
    }
    use crate::zellij::host::tests::decode;
    use base64::Engine as _;

    const POSIX: HostKind = HostKind::Posix;
    const WINDOWS: HostKind = HostKind::Windows;

    fn entry(
        name: &str,
        path: &str,
        kind: WorkspaceEntryKind,
        size: Option<u64>,
        mtime: Option<i64>,
    ) -> WorkspaceEntry {
        WorkspaceEntry { name: name.into(), path: path.into(), kind, size, mtime }
    }

    // ---- 以下与 files.test.ts 一一对应（用例名是 TS 的原文） ----

    /// relSegments 拒绝越界路径
    #[test]
    fn rel_segments_rejects_escaping_paths() {
        assert_eq!(rel_segments(None).unwrap(), Vec::<String>::new());
        assert_eq!(rel_segments(Some("")).unwrap(), Vec::<String>::new());
        assert_eq!(rel_segments(Some("src/git/path.ts")).unwrap(), ["src", "git", "path.ts"]);
        // 多余的分隔符是无害的，直接吃掉
        assert_eq!(rel_segments(Some("/src//a/")).unwrap(), ["src", "a"]);
        for bad in ["..", "a/../../etc", "./x", "a/\u{0}b", "a/\nb"] {
            let err = rel_segments(Some(bad)).unwrap_err();
            assert_eq!(err.to_string(), "路径不合法", "{bad:?}");
        }
    }

    /// resolveInside 按宿主机规则拼路径
    #[test]
    fn resolve_inside_joins_by_host_rules() {
        assert_eq!(resolve_inside(POSIX, "/home/u/repo", Some("src/a.ts")).unwrap(), "/home/u/repo/src/a.ts");
        assert_eq!(resolve_inside(POSIX, "/home/u/repo", Some("")).unwrap(), "/home/u/repo");
        assert_eq!(resolve_inside(WINDOWS, "C:\\code\\repo", Some("src/a.ts")).unwrap(), "C:\\code\\repo\\src\\a.ts");
        // 前端一律用 /，Windows 远端也是——分隔符只在这一步落成平台形式
        assert_eq!(resolve_inside(WINDOWS, "C:/code/repo", Some("a")).unwrap(), "C:\\code\\repo\\a");
        assert_eq!(resolve_inside(POSIX, "/home/u/repo", Some("../other")).unwrap_err().to_string(), "路径不合法");
    }

    /// listCommand：posix 逐条判目录并 stat，windows 走 EncodedCommand
    #[test]
    fn list_command_posix_stats_each_windows_encoded() {
        let posix = list_command(POSIX, "/home/u/re po");
        assert!(posix.contains(r#"ls -1A "$d""#));
        assert!(posix.contains("'/home/u/re po'"));
        assert!(posix.contains("stat -c %Y"));
        assert!(posix.contains("stat -f %m"));
        assert!(posix.contains("printf '%s %s %s %s"));

        let win = list_command(WINDOWS, "C:\\code\\repo");
        assert!(win.to_lowercase().starts_with("powershell"));
        assert!(win.contains("-EncodedCommand "));
        let script = decode(&win);
        assert!(script.contains("Get-ChildItem -LiteralPath $d -Force"));
        assert!(script.contains("PSIsContainer"));
        assert!(script.contains("LastWriteTimeUtc"));
    }

    /// parseEntries 认 d/f + size + mtime，忽略杂行
    #[test]
    fn parse_entries_reads_kind_size_mtime_and_skips_noise() {
        let entries =
            parse_entries("d 0 1700000000 src\r\nf 123 1700000001 README.md\nf\n\nnoise\nd 0 1 .github\n", "packages");
        assert_eq!(
            entries,
            [
                entry("src", "packages/src", WorkspaceEntryKind::Dir, None, Some(1_700_000_000)),
                entry("README.md", "packages/README.md", WorkspaceEntryKind::File, Some(123), Some(1_700_000_001)),
                entry(".github", "packages/.github", WorkspaceEntryKind::Dir, None, Some(1)),
            ]
        );
        // 根目录下不带前缀；名字可以含空格
        let spaced = parse_entries("f 4 0 a b.txt\n", "");
        assert_eq!(spaced[0].path, "a b.txt");
        assert_eq!(spaced[0].size, Some(4));
    }

    /// relativizeIndexLine 收成工作目录相对路径
    #[test]
    fn relativize_index_line_makes_workspace_relative() {
        assert_eq!(relativize_index_line("/home/u/repo/src/a.ts", "/home/u/repo", POSIX).as_deref(), Some("src/a.ts"));
        assert_eq!(relativize_index_line("/home/u/repo", "/home/u/repo", POSIX), None);
        assert_eq!(relativize_index_line("/etc/passwd", "/home/u/repo", POSIX), None);
        assert_eq!(
            relativize_index_line("C:\\code\\repo\\src\\a.ts", "C:\\code\\repo", WINDOWS).as_deref(),
            Some("src/a.ts")
        );
        // Windows 大小写不敏感
        assert_eq!(relativize_index_line("c:\\code\\repo\\b.ts", "C:\\code\\repo", WINDOWS).as_deref(), Some("b.ts"));
    }

    /// parseIndexLines 丢掉越界行，跳过 node_modules 段
    #[test]
    fn parse_index_lines_drops_outside_and_skip_dirs() {
        let WorkspaceIndex { paths, truncated } = parse_index_lines(
            &[
                "/home/u/repo/src/a.ts",
                "/home/u/repo/node_modules/x/index.js",
                "/etc/passwd",
                "/home/u/repo/README.md",
                "__TRUNCATED__",
            ]
            .join("\n"),
            "/home/u/repo",
            POSIX,
        );
        assert_eq!(paths, ["src/a.ts", "README.md"]);
        assert!(truncated);
    }

    /// indexCommand：posix 用 find prune，windows 走 EncodedCommand
    #[test]
    fn index_command_posix_find_prune_windows_encoded() {
        let posix = index_command(POSIX, "/home/u/re po");
        assert!(posix.contains(r#"find "$d""#));
        assert!(posix.contains("-name 'node_modules'"));
        assert!(posix.contains("__TRUNCATED__"));

        let win = index_command(WINDOWS, "C:\\code\\repo");
        assert!(win.contains("-EncodedCommand "));
        let script = decode(&win);
        assert!(script.contains("Get-ChildItem -LiteralPath $p"));
        assert!(script.contains("node_modules"));
        assert!(INDEX_SKIP_DIRS.contains(&"node_modules"));
    }

    /// sortEntries 目录在前，点开头的不沉底
    #[test]
    fn sort_entries_dirs_first_dotfiles_not_last() {
        let sorted = sort_entries(vec![
            entry("b.ts", "b.ts", WorkspaceEntryKind::File, None, None),
            entry("node_modules", "node_modules", WorkspaceEntryKind::Dir, None, None),
            entry(".github", ".github", WorkspaceEntryKind::Dir, None, None),
            entry(".env", ".env", WorkspaceEntryKind::File, None, None),
        ]);
        let names: Vec<&str> = sorted.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, [".github", "node_modules", ".env", "b.ts"]);
    }

    /// readCommand 带上限，windows 循环读到 cap
    #[test]
    fn read_command_caps_and_windows_loops_to_cap() {
        let posix = read_command(POSIX, "/repo/a.ts", 1024);
        assert!(posix.contains(r#"wc -c < "$f""#));
        assert!(posix.contains(r#"head -c 1024 "$f" | $b64"#));
        // base64 不一定存在，得有替补
        assert!(posix.contains("openssl base64"));

        let script = decode(&read_command(WINDOWS, "C:\\repo\\a.ts", 1024));
        assert!(script.contains("$ms.Length -lt 1024"));
        assert!(script.contains("ToBase64String"));
    }

    /// parseRead：首行是真实大小，其余拼回 base64
    #[test]
    fn parse_read_first_line_is_size_rest_is_base64() {
        let body = base64::engine::general_purpose::STANDARD.encode("hello world");
        let res = parse_read(&format!("11\n{}\n{}\n", &body[..4], &body[4..])).unwrap();
        assert_eq!(res.size, 11);
        assert_eq!(String::from_utf8(res.bytes).unwrap(), "hello world");
        assert_eq!(parse_read("not-a-number\nzzz").unwrap_err().to_string(), "读不到文件大小");
    }

    /// classify：图片按扩展名，NUL 判二进制，其余当文本
    #[test]
    fn classify_images_by_ext_nul_is_binary_rest_text() {
        // 图片不读字节：readWorkspaceFile 对图片按 cap 0 读，这里收到的就是空字节
        let png = classify("logo.png", 3, &[]);
        assert_eq!(png, FilePreview::Image { mime: "image/png".into(), size: 3 });

        let bin = classify("a.out", 4, &[1, 0, 2, 3]);
        assert!(matches!(bin, FilePreview::Binary { .. }));

        let text = classify("README.md", 5, b"hello");
        assert!(matches!(text, FilePreview::Text { truncated: false, .. }));

        // 取回来的字节比真实大小少 = 被 cap 截断了
        let cut = classify("big.log", 999_999, b"head");
        assert!(matches!(cut, FilePreview::Text { truncated: true, .. }));

        // 超过原始字节上限的图片浏览器取不到，只报大小
        let huge = classify("big.png", 99_000_000, &[]);
        assert!(matches!(huge, FilePreview::TooLarge { .. }));
        // 上限内的大图（超过文本上限也无妨）照常是图片
        let big = classify("photo.jpg", 5 * 1024 * 1024, &[]);
        assert!(matches!(big, FilePreview::Image { .. }));
    }

    /// mimeOf：按扩展名给 Content-Type，认不出是 octet-stream
    #[test]
    fn mime_of_by_extension_unknown_is_octet_stream() {
        assert_eq!(mime_of("index.html"), "text/html; charset=utf-8");
        assert_eq!(mime_of("a.HTM"), "text/html; charset=utf-8");
        assert_eq!(mime_of("style.css"), "text/css; charset=utf-8");
        assert_eq!(mime_of("app.mjs"), "text/javascript; charset=utf-8");
        assert_eq!(mime_of("logo.svg"), "image/svg+xml");
        assert_eq!(mime_of("font.woff2"), "font/woff2");
        assert_eq!(mime_of("a.out"), "application/octet-stream");
        assert_eq!(mime_of("Makefile"), "application/octet-stream");
        // 只有 image/* 才走图片预览；html / svg 的区分就在这里
        assert_eq!(image_mime_of("logo.svg"), Some("image/svg+xml"));
        assert_eq!(image_mime_of("index.html"), None);
    }

    /// readCommand cap 0 是合法的空读：只取大小
    #[test]
    fn read_command_cap_zero_reads_size_only() {
        let posix = read_command(POSIX, "/w/a.png", 0);
        assert!(posix.contains("head -c 0 "));
        let win = read_command(WINDOWS, "C:\\w\\a.png", 0);
        assert!(!win.is_empty());
        // 空 base64 解出空字节，大小照常
        let r = parse_read("1234\n\n").unwrap();
        assert_eq!(r.size, 1234);
        assert!(r.bytes.is_empty());
    }

    /// extOf 忽略点开头的无扩展名文件
    #[test]
    fn ext_of_ignores_leading_dot_files() {
        assert_eq!(ext_of("a.tar.gz"), "gz");
        assert_eq!(ext_of(".gitignore"), "");
        assert_eq!(ext_of("Makefile"), "");
    }

    /// validateEntryName 挡分隔符、控制字符与 Windows 保留字符
    #[test]
    fn validate_entry_name_blocks_separators_controls_and_windows_reserved() {
        validate_entry_name(POSIX, "a:b?.txt").unwrap();
        for bad in ["", ".", "..", "a/b", "a\\b", "a\nb", "a\u{0}b"] {
            let err = validate_entry_name(POSIX, bad).unwrap_err();
            assert!(err.to_string().contains("文件名"), "{bad:?}");
        }
        assert!(validate_entry_name(WINDOWS, "a:b.txt").unwrap_err().to_string().contains("Windows"));
    }

    /// collapseRemovePaths 丢掉空串、收进祖先
    #[test]
    fn collapse_remove_paths_drops_empty_and_folds_descendants() {
        assert_eq!(collapse_remove_paths(&["", "src", "src/a.ts", "README.md", "src/"]), ["README.md", "src"]);
        assert_eq!(collapse_remove_paths::<&str>(&[]), Vec::<String>::new());
    }

    /// mkdirCommand：非递归要上级在，递归是 mkdir -p
    #[test]
    fn mkdir_command_plain_needs_parent_recursive_is_mkdir_p() {
        let posix = mkdir_command(POSIX, "/home/u/repo/src", "/home/u/repo", false);
        assert!(posix.contains(r#"mkdir -- "$p""#));
        assert!(posix.contains("ENOENT"));
        assert!(!posix.contains("mkdir -p"));

        let rec = mkdir_command(POSIX, "/home/u/repo/a/b", "/home/u/repo/a", true);
        assert!(rec.contains(r#"mkdir -p -- "$p""#));

        let script = decode(&mkdir_command(WINDOWS, "C:\\code\\repo\\src", "C:\\code\\repo", false));
        assert!(script.contains("New-Item -ItemType Directory -LiteralPath $p"));
        assert!(!script.contains("-Force"));
    }

    /// renameCommand 已存在则 EEXIST，缺源则 ENOENT
    #[test]
    fn rename_command_eexist_and_enoent() {
        let posix = rename_command(POSIX, "/r/a", "/r/b");
        assert!(posix.contains(r#"mv -- "$s" "$d""#));
        assert!(posix.contains("ENOENT"));
        assert!(posix.contains("EEXIST"));

        let script = decode(&rename_command(WINDOWS, "C:\\r\\a", "C:\\r\\b"));
        assert!(script.contains("Move-Item -LiteralPath $s"));
    }

    /// removeCommand 走 rm -rf / Directory.Delete，不跟符号链接
    #[test]
    fn remove_command_rm_rf_and_directory_delete() {
        let posix = remove_command(POSIX, "/home/u/repo/src");
        assert!(posix.contains(r#"rm -rf -- "$p""#));
        assert!(posix.contains("'/home/u/repo/src'"));

        let script = decode(&remove_command(WINDOWS, "C:\\code\\repo\\src"));
        assert!(script.contains("[IO.Directory]::Delete($p, $true)"));
        assert!(script.contains("[IO.File]::Delete($p)"));
        assert!(!script.contains("Remove-Item"));
    }

    // ---- 以下是 Rust 侧补的：从 async 函数里拆出来的护栏、错误映射、JS 语义工具 ----

    #[test]
    fn remove_target_stays_inside_and_refuses_root() {
        let t = remove_target(POSIX, "/home/u/repo", "src/").unwrap();
        assert_eq!(t, RemoveTarget { path: "src".into(), full: "/home/u/repo/src".into() });
        let t = remove_target(WINDOWS, "C:\\code\\repo", "a/b.txt").unwrap();
        assert_eq!(t.full, "C:\\code\\repo\\a\\b.txt");
        for rel in ["", "/", "//"] {
            assert_eq!(remove_target(POSIX, "/home/u/repo", rel), Err(FileError::RemoveRoot), "{rel:?}");
        }
        assert_eq!(remove_target(POSIX, "/home/u/repo", "a/../.."), Err(FileError::InvalidPath));
        assert_eq!(remove_target(POSIX, "/home/u/repo", "a/\u{1}"), Err(FileError::InvalidPath));
    }

    #[test]
    fn mkdir_and_rename_targets() {
        let m = mkdir_target(POSIX, "/w", "a//b/").unwrap();
        assert_eq!(m, MkdirTarget { path: "a/b".into(), dir: "/w/a/b".into(), parent: "/w/a".into() });
        assert_eq!(mkdir_target(POSIX, "/w", "/"), Err(FileError::InvalidPath));
        assert_eq!(mkdir_target(WINDOWS, "C:\\w", "a/b?c"), Err(FileError::WindowsForbiddenChar));
        assert_eq!(mkdir_target(POSIX, "/w", "a\\b"), Err(FileError::InvalidName));

        let r = rename_target(POSIX, "/w", "src/a.ts", "b.ts").unwrap();
        assert_eq!(
            r,
            RenameTarget {
                path: "src/b.ts".into(),
                src: "/w/src/a.ts".into(),
                dst: "/w/src/b.ts".into(),
                unchanged: false
            }
        );
        assert_eq!(rename_target(POSIX, "/w", "a.ts", "c.ts").unwrap().path, "c.ts");
        assert!(rename_target(POSIX, "/w", "a.ts", "a.ts").unwrap().unchanged);
        // Windows 不分大小写：只改大小写算"没变"（TS 原样如此）
        assert!(rename_target(WINDOWS, "C:\\w", "a.ts", "A.ts").unwrap().unchanged);
        assert_eq!(rename_target(POSIX, "/w", "a.ts", "x/y"), Err(FileError::InvalidName));
        assert_eq!(rename_target(POSIX, "/w", "a.ts", &"长".repeat(256)), Err(FileError::NameTooLong));
    }

    #[test]
    fn error_mapping_matches_ts_messages() {
        assert_eq!(remote_error("ENOENT\n", "x").to_string(), "路径不存在或不可访问");
        assert_eq!(remote_error("  EEXIST\r\nmore", "x"), FileError::Exists);
        assert_eq!(remote_error("EISDIR", "x").code(), Some("EISDIR"));
        assert_eq!(remote_error("boom", "无法删除"), FileError::Other("无法删除".into()));
        let res = ExecResult { code: Some(1), stdout: String::new(), stderr: " rm: denied \n".into() };
        assert_eq!(remote_failure(&res, "无法删除"), FileError::Other("rm: denied".into()));
        let res = ExecResult { code: Some(1), stdout: "EACCES\n".into(), stderr: String::new() };
        assert_eq!(remote_failure(&res, "无法删除"), FileError::PermissionDenied);
        let res = ExecResult { code: Some(1), stdout: String::new(), stderr: "ENOTDIR".into() };
        assert_eq!(remote_failure(&res, "x"), FileError::NotDir);

        assert_eq!(local_error(&io::Error::from(io::ErrorKind::NotFound), "x"), FileError::NotFound);
        assert_eq!(local_error(&io::Error::from(io::ErrorKind::PermissionDenied), "x"), FileError::PermissionDenied);
        assert_eq!(local_error(&io::Error::from(io::ErrorKind::AlreadyExists), "x").to_string(), "同名文件已存在");
        assert_eq!(local_error(&io::Error::from(io::ErrorKind::Interrupted), "无法删除").to_string(), "无法删除");
    }

    #[test]
    fn parse_read_follows_js_number_and_lenient_base64() {
        // Number("") 是 0；空输出不是错误
        assert_eq!(parse_read("").unwrap(), ReadResult { size: 0, bytes: vec![] });
        assert_eq!(parse_read(" 0x10 \n").unwrap().size, 16);
        assert_eq!(parse_read("-1\n").unwrap_err(), FileError::UnreadableSize);
        assert_eq!(parse_read("Infinity\n").unwrap_err(), FileError::UnreadableSize);
        assert_eq!(parse_read("inf\n").unwrap_err(), FileError::UnreadableSize);
        // Buffer.from(_, "base64")：缺填充、夹杂非法字符、= 之后的内容
        assert_eq!(parse_read("5\naGVs*bG8\n").unwrap().bytes, b"hello");
        assert_eq!(parse_read("5\naGVsbG8=aGVs").unwrap().bytes, b"hello");
        assert_eq!(decode_base64_lenient("aG-_"), [b'h', b'o', 0xbf]);
        assert_eq!(decode_base64_lenient("a"), Vec::<u8>::new());
    }

    #[test]
    fn parse_entries_follows_js_regex_semantics() {
        // 名字里夹 \r：JS 的 `.` 不吃 \r，整行不认
        assert!(parse_entries("f 1 2 a\rb\n", "").is_empty());
        // 非 ASCII 数字不是 \d
        assert!(parse_entries("f \u{663} 2 a\n", "").is_empty());
        assert_eq!(parse_entries("d 0 0 ..\nd 0 0 .\n", ""), []);
    }

    #[test]
    fn relativize_windows_slices_by_original_utf16_length() {
        // 开尔文号 K（3 字节）小写成 k（1 字节）：按字节切会切错
        assert_eq!(relativize_index_line("c:\\k\\a.ts", "C:\\\u{212a}", WINDOWS).as_deref(), Some("a.ts"));
        assert_eq!(relativize_index_line("/r/", "/r", POSIX).as_deref(), Some(""));
        assert_eq!(parse_index_lines("/r/\n/r/a\n", "/r", POSIX).paths, ["a"]);
    }

    #[test]
    fn collapse_sorts_by_utf16_like_js() {
        // U+FF01 在 UTF-16 里排在增补平面（代理对 0xD83D…）之后，码点序正相反
        assert_eq!(collapse_remove_paths(&["\u{ff01}", "\u{1f600}"]), ["\u{1f600}", "\u{ff01}"]);
    }
}

/// 运行时：本地那一路直接打本机文件系统；远端那一路用一个经本机 `sh` 执行命令的假远端，
/// POSIX 远端脚本（ls / stat / find / head | base64 / mkdir / mv / rm）真跑一遍，
/// 两路的结果对拍。Windows 远端只能真机验。
#[cfg(all(test, unix))]
pub(crate) mod runtime_tests {
    use super::*;
    use crate::exec::local_exec;
    use std::path::Path;

    /// 假远端：命令交给本机 sh（与 SSH exec 同样是"一条命令行 → code / stdout / stderr"）
    pub(crate) struct ShRemote;

    impl Exec for ShRemote {
        fn exec<'a>(
            &'a self,
            command_line: &'a str,
            cancel: Option<&'a CancellationToken>,
        ) -> LocalBoxFuture<'a, anyhow::Result<ExecResult>> {
            Box::pin(async move { Ok(local_exec(command_line, cancel).await) })
        }
    }

    impl RemoteHost for ShRemote {
        fn exec_stream<'a>(&'a self, _: &'a str) -> LocalBoxFuture<'a, anyhow::Result<ExecChannel>> {
            Box::pin(async { anyhow::bail!("测试里没有 SSH 通道") })
        }
    }

    /// 链路坏掉的远端：exec 本身起不来
    struct DeadRemote;

    impl Exec for DeadRemote {
        fn exec<'a>(&'a self, _: &'a str, _: Option<&'a CancellationToken>) -> LocalBoxFuture<'a, anyhow::Result<ExecResult>> {
            Box::pin(async { anyhow::bail!("连接被拒绝") })
        }
    }

    const LOCAL: FileHost<'static> = FileHost::Local { kind: HostKind::Posix };
    const REMOTE: FileHost<'static> = FileHost::Remote { kind: HostKind::Posix, remote: &ShRemote };

    fn tmp() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap().to_string_lossy().into_owned();
        (dir, root)
    }

    fn write(root: &str, rel: &str, content: impl AsRef<[u8]>) {
        let p = Path::new(root).join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    #[tokio::test]
    async fn list_local_and_remote_agree() {
        let (_dir, root) = tmp();
        write(&root, "b.txt", "hello");
        write(&root, "中文 名.md", "x");
        write(&root, ".github/ci.yml", "y");
        write(&root, "src/a.rs", "z");
        let local = list_workspace(&LOCAL, &root, None).await.unwrap();
        let remote = list_workspace(&REMOTE, &root, None).await.unwrap();
        assert_eq!(local, remote);
        let names: Vec<&str> = local.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, [".github", "src", "b.txt", "中文 名.md"]);
        assert_eq!(local.entries[2].size, Some(5));
        assert!(local.entries[2].mtime.unwrap() > 1_600_000_000);

        let sub = list_workspace(&REMOTE, &root, Some("src")).await.unwrap();
        assert_eq!((sub.path.as_str(), sub.entries[0].path.as_str()), ("src", "src/a.rs"));
        for host in [LOCAL, REMOTE] {
            assert_eq!(list_workspace(&host, &root, Some("nope")).await.unwrap_err(), FileError::NotFound);
            assert_eq!(list_workspace(&host, &root, Some("b.txt")).await.unwrap_err(), FileError::NotDir);
            assert_eq!(list_workspace(&host, &root, Some("../x")).await.unwrap_err(), FileError::InvalidPath);
        }
    }

    #[tokio::test]
    async fn index_local_and_remote_agree() {
        let (_dir, root) = tmp();
        for f in ["a.txt", "src/lib.rs", "src/deep/x.rs", "node_modules/p/i.js", "dist/o.js", ".git/HEAD"] {
            write(&root, f, "1");
        }
        let mut local = index_workspace(&LOCAL, &root).await.unwrap();
        let mut remote = index_workspace(&REMOTE, &root).await.unwrap();
        local.paths.sort();
        remote.paths.sort();
        assert_eq!(local, remote);
        assert_eq!(local.paths, ["a.txt", "src/deep/x.rs", "src/lib.rs"]);
        assert!(!local.truncated);
    }

    #[tokio::test]
    async fn read_local_and_remote_agree() {
        let (_dir, root) = tmp();
        write(&root, "t.txt", "你好\n");
        write(&root, "bin.dat", [1u8, 0, 2, 3]);
        write(&root, "p.png", [0x89u8; 10]);
        for host in [LOCAL, REMOTE] {
            assert_eq!(
                read_workspace_file(&host, &root, "t.txt").await.unwrap(),
                FilePreview::Text { text: "你好\n".into(), size: 7, truncated: false }
            );
            assert_eq!(read_workspace_file(&host, &root, "bin.dat").await.unwrap(), FilePreview::Binary { size: 4 });
            assert_eq!(
                read_workspace_file(&host, &root, "p.png").await.unwrap(),
                FilePreview::Image { mime: "image/png".into(), size: 10 }
            );
            // 只取前 cap 字节，大小照报真实的
            let (name, read) = read_workspace_bytes(&host, &root, "t.txt", 3).await.unwrap();
            assert_eq!((name.as_str(), read.size, read.bytes.as_slice()), ("t.txt", 7, "你".as_bytes()));
            assert_eq!(read_workspace_bytes(&host, &root, "", 3).await.unwrap_err(), FileError::InvalidPath);
            assert_eq!(read_workspace_file(&host, &root, "nope").await.unwrap_err(), FileError::NotFound);
        }
        std::fs::create_dir(Path::new(&root).join("d")).unwrap();
        assert_eq!(read_workspace_file(&LOCAL, &root, "d").await.unwrap_err(), FileError::IsDir);
        assert_eq!(read_workspace_file(&REMOTE, &root, "d").await.unwrap_err(), FileError::IsDir);
    }

    #[tokio::test]
    async fn mkdir_rename_remove_local_and_remote() {
        for host in [LOCAL, REMOTE] {
            let (_dir, root) = tmp();
            assert_eq!(mkdir_workspace(&host, &root, "a", false).await.unwrap().path, "a");
            assert_eq!(mkdir_workspace(&host, &root, "a", false).await.unwrap_err(), FileError::Exists);
            assert_eq!(mkdir_workspace(&host, &root, "x/y", false).await.unwrap_err(), FileError::NotFound);
            assert_eq!(mkdir_workspace(&host, &root, "x/y", true).await.unwrap().path, "x/y");
            assert!(mkdir_workspace(&host, &root, "x/y", true).await.is_ok());
            write(&root, "f", "1");
            assert_eq!(mkdir_workspace(&host, &root, "f", true).await.unwrap_err(), FileError::Exists);

            assert_eq!(rename_workspace(&host, &root, "f", "g").await.unwrap().path, "g");
            assert_eq!(rename_workspace(&host, &root, "g", "a").await.unwrap_err(), FileError::Exists);
            assert_eq!(rename_workspace(&host, &root, "nope", "z").await.unwrap_err(), FileError::NotFound);
            assert_eq!(rename_workspace(&host, &root, "x/y", "z").await.unwrap().path, "x/z");
            assert!(Path::new(&root).join("x/z").is_dir());

            // 指向工作目录外的链接只删链接本身
            let outside = tempfile::tempdir().unwrap();
            std::fs::write(outside.path().join("keep"), "k").unwrap();
            std::os::unix::fs::symlink(outside.path(), Path::new(&root).join("link")).unwrap();
            let res = remove_workspace(&host, &root, &["x", "x/z", "link", "nope", "", "a/../.."]).await;
            assert_eq!(res.removed, ["link", "x"]);
            assert_eq!(
                res.errors,
                [
                    FileRemoveError { path: "a/../..".into(), error: "路径不合法".into() },
                    FileRemoveError { path: "nope".into(), error: "路径不存在或不可访问".into() },
                ]
            );
            assert!(outside.path().join("keep").is_file());
            assert!(Path::new(&root).is_dir());
        }
    }

    #[tokio::test]
    async fn exec_timed_reports_timeouts_and_link_failures() {
        let started = std::time::Instant::now();
        let err = exec_timed(&ShRemote, "sleep 5", Duration::from_millis(100), "操作超时").await.unwrap_err();
        assert_eq!(err.to_string(), "操作超时");
        assert!(started.elapsed() < Duration::from_secs(3));
        let err = exec_timed(&DeadRemote, "true", LIST_TIMEOUT, "读取超时").await.unwrap_err();
        assert_eq!(err.to_string(), "SSH 连接失败：连接被拒绝");
        // 非零退出码是正常返回值
        let res = exec_timed(&ShRemote, "exit 3", LIST_TIMEOUT, "读取超时").await.unwrap();
        assert_eq!(res.code, Some(3));
        let host = FileHost::Remote { kind: HostKind::Posix, remote: &DeadRemote };
        assert_eq!(list_workspace(&host, "/r", None).await.unwrap_err().to_string(), "SSH 连接失败：连接被拒绝");
    }

    impl RemoteHost for DeadRemote {
        fn exec_stream<'a>(&'a self, _: &'a str) -> LocalBoxFuture<'a, anyhow::Result<ExecChannel>> {
            Box::pin(async { anyhow::bail!("连接被拒绝") })
        }
    }
}
