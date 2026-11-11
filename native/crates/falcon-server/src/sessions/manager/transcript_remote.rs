//! SSH 上官方 Claude 交互终端的只读 transcript 镜像。
//!
//! 每次调用只开一条无 PTY 的 Exec 通道；这里不持有 Backend，不发送终端输入、信号或
//! CLI 控制请求。调用方负责一秒一次的轮询，并且只在解析增量成功后提交返回的 cursor。
//! 登录环境只用于首次定位配置目录；即使尚无 transcript，成功返回的 cursor 也会记住
//! projects 绝对路径，下一次不再加载登录配置。发现文件以后始终读取同一个明确路径。

use std::rc::Rc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use tokio_util::sync::CancellationToken;

use crate::exec::{Exec, ExecResult};
use crate::host::quote_posix;
use crate::sessions::ssh::SshLink;

const MAX_CHUNK_BYTES: usize = 1024 * 1024;
const MAX_PATH_BYTES: usize = 16 * 1024;
const PREFIX_BYTES: usize = 128;
const NOISE_BYTES: usize = 64 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(10);
const CANCEL_TIMEOUT: Duration = Duration::from_millis(250);

#[derive(Clone)]
pub(super) struct RemoteTranscriptSource {
    link: Rc<SshLink>,
    shell: String,
    session_id: String,
}

#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub(super) struct RemoteTranscriptCursor {
    // POSIX 路径可以不是 UTF-8，也可以以换行结尾，不能经 ExecResult 的 String 直接传递。
    projects: Option<Vec<u8>>,
    path: Option<Vec<u8>>,
    identity: Option<(u64, u64)>,
    offset: u64,
    prefix: Vec<u8>,
    session_id: Option<String>,
}

pub(super) struct RemoteTranscriptChunk {
    pub bytes: Vec<u8>,
    pub reset: bool,
    pub cursor: RemoteTranscriptCursor,
    pub found: bool,
    pub has_more: bool,
}

impl RemoteTranscriptSource {
    pub(super) fn new(link: Rc<SshLink>, shell: String, session_id: &str) -> Result<Self, String> {
        validate_source(&shell, session_id)?;
        Ok(Self { link, shell, session_id: session_id.to_string() })
    }

    pub(super) async fn read_chunk(
        &self,
        cursor: &RemoteTranscriptCursor,
        max_bytes: usize,
    ) -> Result<RemoteTranscriptChunk, String> {
        read_with_exec(self.link.as_ref(), &self.shell, &self.session_id, cursor, max_bytes).await
    }
}

fn validate_source(shell: &str, session_id: &str) -> Result<(), String> {
    if session_id.len() != 36 || !session_id.bytes().enumerate().all(|(i, b)| {
        if matches!(i, 8 | 13 | 18 | 23) { b == b'-' } else { b.is_ascii_hexdigit() }
    }) {
        return Err("remote transcript requires a fixed canonical session UUID".into());
    }
    if !shell.starts_with('/') || shell.len() > MAX_PATH_BYTES || shell.contains('\0') {
        return Err("remote transcript requires an absolute POSIX login shell path".into());
    }
    Ok(())
}

fn validate_request(cursor: &RemoteTranscriptCursor, session_id: &str, max_bytes: usize) -> Result<(), String> {
    if max_bytes == 0 || max_bytes > MAX_CHUNK_BYTES {
        return Err(format!("remote transcript chunk limit must be between 1 and {MAX_CHUNK_BYTES} bytes"));
    }
    if cursor.session_id.as_deref().is_some_and(|id| id != session_id) {
        return Err("remote transcript cursor belongs to another session".into());
    }
    if cursor.offset > i64::MAX as u64 || cursor.prefix.len() > PREFIX_BYTES {
        return Err("invalid remote transcript cursor".into());
    }
    for path in [&cursor.projects, &cursor.path].into_iter().flatten() {
        validate_path(path)?;
    }
    if cursor.path.is_some() && cursor.projects.is_none() {
        return Err("remote transcript cursor has no projects directory".into());
    }
    Ok(())
}

fn validate_path(path: &[u8]) -> Result<(), String> {
    if path.first() != Some(&b'/') || path.len() > MAX_PATH_BYTES || path.contains(&0) {
        return Err("remote transcript returned an invalid absolute POSIX path".into());
    }
    Ok(())
}

fn output_limit(max_bytes: usize) -> usize {
    // 两条路径、128 字节指纹、协议元数据与登录配置的噪声分别留出空间；不按文件大小分配。
    max_bytes.div_ceil(3) * 4 + 2 * MAX_PATH_BYTES.div_ceil(3) * 4 + NOISE_BYTES + 4096
}

async fn read_with_exec(
    executor: &dyn Exec,
    shell: &str,
    session_id: &str,
    cursor: &RemoteTranscriptCursor,
    max_bytes: usize,
) -> Result<RemoteTranscriptChunk, String> {
    validate_source(shell, session_id)?;
    validate_request(cursor, session_id, max_bytes)?;
    let mut nonce = [0u8; 16];
    getrandom::fill(&mut nonce).map_err(|e| format!("cannot create remote transcript frame marker: {e}"))?;
    let marker = format!("FALCON_TRANSCRIPT_V1_{}", hex::encode(nonce));
    let command = read_command(shell, session_id, cursor, max_bytes, &marker);
    let cancel = CancellationToken::new();
    let _cancel_on_drop = cancel.clone().drop_guard();
    let mut pending = executor.exec(&command, Some(&cancel));
    let result = tokio::select! {
        result = &mut pending => result.map_err(|e| format!("remote transcript exec failed: {e}"))?,
        _ = tokio::time::sleep(READ_TIMEOUT) => {
            // 只关闭本次只读命令的 SSH 通道；不会调用会话的 PTY / holder / 进程接口。
            cancel.cancel();
            let _ = tokio::time::timeout(CANCEL_TIMEOUT, &mut pending).await;
            return Err("remote transcript read timed out".into());
        }
    };
    parse_result(result, &marker, cursor, session_id, max_bytes)
}

fn read_command(
    shell: &str,
    session_id: &str,
    cursor: &RemoteTranscriptCursor,
    max_bytes: usize,
    marker: &str,
) -> String {
    script_command(shell, session_id, cursor, max_bytes, marker, READ_SCRIPT)
}

fn script_command(
    shell: &str,
    session_id: &str,
    cursor: &RemoteTranscriptCursor,
    max_bytes: usize,
    marker: &str,
    script: &str,
) -> String {
    let (dev, inode) = cursor.identity.map(|(d, i)| (d.to_string(), i.to_string())).unwrap_or_default();
    let args = [
        marker.to_string(), session_id.to_string(), max_bytes.to_string(),
        STANDARD.encode(cursor.projects.as_deref().unwrap_or_default()),
        STANDARD.encode(cursor.path.as_deref().unwrap_or_default()),
        dev, inode, cursor.offset.to_string(), STANDARD.encode(&cursor.prefix), cursor.prefix.len().to_string(),
    ].iter().map(|arg| quote_posix(arg)).collect::<Vec<_>>().join(" ");
    let run = format!("/bin/sh -c {} falcon-transcript {args}", quote_posix(script));
    let inner = if cursor.projects.is_none() {
        // 这段参数展开由给定的交互登录 shell 执行，能看到 rc 中未 export 的配置变量；
        // 实际扫描与读取统一交给 /bin/sh，避免 zsh 的 nomatch、数组与 glob 选项差异。
        let login = format!("exec {run} \"${{CLAUDE_CONFIG_DIR-}}\" \"${{HOME-}}\"");
        format!("{} -i -l -c {}", quote_posix(shell), quote_posix(&login))
    } else {
        run
    };
    // Exec 本身收集完整 stdout/stderr，因此上限必须设在远端。POSIX 管道的退出码是 head
    // 的，内部命令退出码另用随机标记传回来，解析时两者都显式检查，不能把失败当成 EOF。
    // stderr 丢弃，避免登录脚本无限打印；超限截断会缺少 EXIT 标记，明确失败、不提交游标。
    let wrapper = format!(
        "exec 2>/dev/null; ( {inner}; result=$?; printf '\\n%s:EXIT:%s\\n' {} \"$result\" ) | /usr/bin/head -c {}",
        quote_posix(marker), output_limit(max_bytes),
    );
    format!("/bin/sh -c {}", quote_posix(&wrapper))
}

// 所有打开均为只读；不创建远端临时文件，也不触碰 Claude 的配置内容或凭据。
// 对同一路径打开多个只读 fd 后验证 dev/inode：payload 只从 fd 3 读取，前后指纹各用
// 独立的 fd，避免反复按路径打开而把轮换前后的字节拼在一起。允许读取期间继续 append；
// 检测到替换、缩小或前缀变化则整次失败，调用方用旧游标重试，下一次返回 reset。
const READ_SCRIPT: &str = r#"
PATH=/usr/bin:/bin
LC_ALL=C
export PATH LC_ALL
marker=$1
session=$2
limit=$3
root64=$4
path64=$5
old_dev=$6
old_ino=$7
old_offset=$8
old_prefix=$9
shift 9
old_prefix_len=$1
config=${2-}
home=${3-}
fail() {
    printf '\n%s:BEGIN\nERROR\n%s\n%s:END\n' "$marker" "$1" "$marker"
    exit 1
}
encode() { base64 | tr -d '\n'; }
if printf '' | base64 -d >/dev/null 2>&1; then
    decode() { base64 -d; }
else
    decode() { base64 -D; }
fi
if [ -z "$root64" ]; then
    if [ -n "$config" ]; then root=$config/projects
    elif [ -n "$home" ]; then root=$home/.claude/projects
    else fail INVALID_ROOT
    fi
else
    root=$(printf '%s' "$root64" | decode; printf '.')
    root=${root%.}
fi
case "$root" in /*) ;; *) fail INVALID_ROOT ;; esac
[ "${#root}" -le 16384 ] || fail PATH_LIMIT
root64=$(printf '%s' "$root" | encode)
path=
if [ -n "$path64" ]; then
    path=$(printf '%s' "$path64" | decode; printf '.')
    path=${path%.}
else
    if [ -d "$root" ]; then
        [ -r "$root" ] && [ -x "$root" ] || fail PERMISSION
        for candidate in "$root"/*/"$session.jsonl"; do
            [ -f "$candidate" ] || continue
            [ -z "$path" ] || fail AMBIGUOUS
            path=$candidate
        done
    elif [ -e "$root" ]; then
        fail INVALID_ROOT
    fi
fi
[ "${#path}" -le 16384 ] || fail PATH_LIMIT
path64=$(printf '%s' "$path" | encode)
missing() {
    printf '\n%s:BEGIN\nMISSING\n%s\n%s\n%s:END\n' "$marker" "$root64" "$path64" "$marker"
    exit 0
}
[ -n "$path" ] || missing
case "$path" in /*) ;; *) fail INVALID_PATH ;; esac
if [ ! -e "$path" ]; then missing; fi
[ -f "$path" ] && [ -r "$path" ] || fail PERMISSION
exec 3< "$path" 4< "$path" 5< "$path" 6< "$path"
# GNU 从 /dev/fd/0 跟随到文件；BSD stat 不给路径时对 stdin 做 fstat。
# macOS 对 /dev/fd/N 做 stat 即使加 -L，device 仍可能是 fdescfs 的 device，不能与路径比较。
if stat -L -c '%d %i %s' /dev/fd/3 >/dev/null 2>&1; then
    metadata_fd() { stat -L -c '%d %i %s' /dev/fd/0 <&"$1"; }
    metadata_path() { stat -L -c '%d %i %s' "$1"; }
else
    metadata_fd() { stat -f '%d %i %z' <&"$1"; }
    metadata_path() { stat -L -f '%d %i %z' "$1"; }
fi
before=$(metadata_fd 3) || fail READ_FAILED
set -- $before
[ "$#" -eq 3 ] || fail READ_FAILED
dev=$1
ino=$2
size=$3
case "$dev:$ino:$size" in *[!0-9:]*|::*|:*|*:) fail READ_FAILED ;; esac
# shell 的位置运算采用有符号 64 位整数，超出的文件明确拒绝而不是溢出后回读。
[ "$size" -le 9223372036854775807 ] 2>/dev/null || fail SIZE_LIMIT
for fd in 4 5 6; do
    info=$(metadata_fd "$fd") || fail READ_FAILED
    set -- $info
    [ "$1:$2" = "$dev:$ino" ] || fail CHANGED
    [ "$3" -ge "$size" ] || fail CHANGED
done
previous=$(head -c "$old_prefix_len" <&4 | encode)
reset=0
start=$old_offset
if [ "$old_dev:$old_ino" != "$dev:$ino" ] || [ "$size" -lt "$old_offset" ] || [ "$previous" != "$old_prefix" ]; then
    reset=1
    start=0
fi
prefix_len=128
[ "$size" -ge "$prefix_len" ] || prefix_len=$size
prefix=$(head -c "$prefix_len" <&5 | encode)
want=$((size - start))
[ "$want" -le "$limit" ] || want=$limit
# tail 的 +N 是从文件开头数的第 N 字节；输入是 offset=0 的独立 fd，head 限制输出。
# 不用 dd bs=1 复制整块，否则 1 MiB 增量会产生上百万次逐字节读写。
if [ "$want" -gt 0 ]; then
    payload=$(tail -c "+$((start + 1))" <&3 | head -c "$want" | encode)
else
    payload=
fi
after=$(metadata_fd 3) || fail CHANGED
set -- $after
[ "$1:$2" = "$dev:$ino" ] && [ "$3" -ge "$size" ] || fail CHANGED
at_path=$(metadata_path "$path") || fail CHANGED
set -- $at_path
[ "$1:$2" = "$dev:$ino" ] && [ "$3" -ge "$size" ] || fail CHANGED
check_prefix=$(head -c "$prefix_len" <&6 | encode)
[ "$prefix" = "$check_prefix" ] || fail CHANGED
printf '\n%s:BEGIN\nFOUND\n%s\n%s\n%s\n%s\n%s\n%s\n%s\n%s\n%s\n%s:END\n' \
    "$marker" "$root64" "$path64" "$dev" "$ino" "$start" "$size" "$prefix" "$payload" "$reset" "$marker"
"#;

fn parse_result(
    result: ExecResult,
    marker: &str,
    previous: &RemoteTranscriptCursor,
    session_id: &str,
    max_bytes: usize,
) -> Result<RemoteTranscriptChunk, String> {
    // Exec 的非零退出码是正常返回值，在这里显式判错；不能只判断 stdout 是否为空。
    if result.code != Some(0) {
        return Err(format!("remote transcript command failed (exit {:?})", result.code));
    }
    if result.stdout.len() > output_limit(max_bytes) || result.stderr.len() > NOISE_BYTES {
        return Err("remote transcript output limit exceeded".into());
    }
    let begin = format!("\n{marker}:BEGIN\n");
    let end = format!("\n{marker}:END\n");
    let exit = format!("\n{marker}:EXIT:");
    let (noise, rest) = result.stdout.split_once(&begin).ok_or("remote transcript response is missing its frame (output may exceed the limit)")?;
    if noise.len() > NOISE_BYTES {
        return Err("remote transcript login output limit exceeded".into());
    }
    let (body, rest) = rest.split_once(&end).ok_or("incomplete remote transcript frame")?;
    let (after, exit_code) = rest.split_once(&exit).ok_or("remote transcript command did not finish (output may exceed the limit)")?;
    if after.len() > NOISE_BYTES || !exit_code.ends_with('\n') {
        return Err("invalid remote transcript exit frame".into());
    }
    let exit_code = exit_code.strip_suffix('\n').unwrap();
    let inner_code = exit_code.parse::<i32>().map_err(|_| "invalid remote transcript exit status")?;
    let fields: Vec<_> = body.split('\n').collect();
    if fields.first() == Some(&"ERROR") && fields.len() == 2 && inner_code != 0 {
        let reason = match fields[1] {
            "AMBIGUOUS" => "multiple transcript files match the fixed session UUID; use the original terminal",
            "CHANGED" => "transcript changed during the read; retry with the previous cursor",
            "INVALID_ROOT" => "transcript requires an absolute CLAUDE_CONFIG_DIR or HOME projects directory",
            "INVALID_PATH" => "transcript path must be absolute",
            "PATH_LIMIT" => "transcript path limit exceeded",
            "SIZE_LIMIT" => "transcript file size limit exceeded",
            "PERMISSION" => "transcript path is not a readable regular file or directory",
            _ => "cannot read remote transcript metadata or bytes",
        };
        return Err(reason.into());
    }
    if inner_code != 0 {
        return Err(format!("remote transcript reader failed (exit {inner_code})"));
    }
    let decode_path = |s: &str| -> Result<Vec<u8>, String> {
        let bytes = decode_bounded(s, MAX_PATH_BYTES)?;
        validate_path(&bytes)?;
        Ok(bytes)
    };
    let mut cursor = previous.clone();
    cursor.session_id = Some(session_id.to_string());
    match fields.as_slice() {
        ["MISSING", root, path] => {
            cursor.projects = Some(decode_path(root)?);
            cursor.path = if path.is_empty() { None } else { Some(decode_path(path)?) };
            check_paths(previous, &cursor, session_id)?;
            cursor.identity = None;
            cursor.offset = 0;
            cursor.prefix.clear();
            Ok(RemoteTranscriptChunk {
                bytes: Vec::new(), reset: previous.identity.is_some(), cursor, found: false, has_more: false,
            })
        }
        ["FOUND", root, path, dev, inode, start, size, prefix, payload, reset] => {
            cursor.projects = Some(decode_path(root)?);
            cursor.path = Some(decode_path(path)?);
            check_paths(previous, &cursor, session_id)?;
            let identity = (parse_number(dev)?, parse_number(inode)?);
            let start = parse_number(start)?;
            let size = parse_number(size)?;
            if size > i64::MAX as u64 || start > size {
                return Err("invalid remote transcript file range".into());
            }
            let prefix = decode_bounded(prefix, PREFIX_BYTES)?;
            if prefix.len() as u64 != size.min(PREFIX_BYTES as u64) {
                return Err("incomplete remote transcript prefix".into());
            }
            let reset = match *reset { "0" => false, "1" => true, _ => return Err("invalid remote transcript reset flag".into()) };
            // 除脚本判定外再验证一次游标，避免格式损坏被误当作可提交的连续增量。
            let expected_reset = previous.identity != Some(identity) || size < previous.offset
                || prefix.len() < previous.prefix.len() || !prefix.starts_with(&previous.prefix);
            if reset != expected_reset || start != if reset { 0 } else { previous.offset } {
                return Err("remote transcript generation or offset mismatch".into());
            }
            let bytes = decode_bounded(payload, max_bytes)?;
            if bytes.len() as u64 != (size - start).min(max_bytes as u64) {
                return Err("incomplete remote transcript payload; retry with the previous cursor".into());
            }
            cursor.identity = Some(identity);
            cursor.offset = start + bytes.len() as u64;
            cursor.prefix = prefix;
            let has_more = cursor.offset < size;
            Ok(RemoteTranscriptChunk { bytes, reset, cursor, found: true, has_more })
        }
        _ => Err("invalid remote transcript response fields".into()),
    }
}

fn check_paths(previous: &RemoteTranscriptCursor, cursor: &RemoteTranscriptCursor, session_id: &str) -> Result<(), String> {
    if previous.projects.is_some() && previous.projects != cursor.projects
        || previous.path.is_some() && previous.path != cursor.path
    {
        return Err("remote transcript unexpectedly changed its pinned path".into());
    }
    if let Some(path) = &cursor.path {
        let root = cursor.projects.as_ref().ok_or("remote transcript projects directory is missing")?;
        let relative = path.strip_prefix(root.as_slice()).and_then(|p| p.strip_prefix(b"/"))
            .ok_or("remote transcript path is outside its projects directory")?;
        let mut components = relative.split(|b| *b == b'/');
        let project = components.next().unwrap_or_default();
        if project.is_empty() || project == b"." || project == b".."
            || components.next() != Some(format!("{session_id}.jsonl").as_bytes()) || components.next().is_some()
        {
            return Err("remote transcript path does not match the fixed session UUID".into());
        }
    }
    Ok(())
}

fn parse_number(value: &str) -> Result<u64, String> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err("invalid remote transcript numeric field".into());
    }
    value.parse().map_err(|_| "remote transcript numeric field overflow".into())
}

fn decode_bounded(value: &str, limit: usize) -> Result<Vec<u8>, String> {
    if value.len() > limit.div_ceil(3) * 4 {
        return Err("remote transcript base64 field limit exceeded".into());
    }
    let bytes = STANDARD.decode(value).map_err(|_| "invalid remote transcript base64 field")?;
    if bytes.len() > limit { return Err("remote transcript decoded field limit exceeded".into()); }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::LocalBoxFuture;

    const SESSION: &str = "01234567-89ab-cdef-0123-456789abcdef";
    const MARKER: &str = "FALCON_TRANSCRIPT_TEST";

    #[test]
    fn uuid_shell_and_request_validation_blocks_path_injection() {
        for id in ["", "../x", "01234567-89ab-cdef-0123-456789abcde/", "01234567-89ab-cdef-0123-456789abcdeg", "$(touch bad)"] {
            assert!(validate_source("/bin/sh", id).is_err(), "{id}");
        }
        assert!(validate_source("/bin/sh", SESSION).is_ok());
        assert!(validate_source("/bin/sh", &SESSION.to_uppercase()).is_ok());
        for shell in ["sh", "", "/bin/sh\0bad"] { assert!(validate_source(shell, SESSION).is_err()); }
        assert!(validate_request(&RemoteTranscriptCursor::default(), SESSION, 0).is_err());
        assert!(validate_request(&RemoteTranscriptCursor::default(), SESSION, MAX_CHUNK_BYTES + 1).is_err());
        let cursor = RemoteTranscriptCursor { session_id: Some("another".into()), ..Default::default() };
        assert!(validate_request(&cursor, SESSION, 1).is_err());
    }

    fn framed(body: &str, status: i32) -> ExecResult {
        ExecResult { code: Some(0), stdout: format!("rc noise\n\n{MARKER}:BEGIN\n{body}\n{MARKER}:END\n\n{MARKER}:EXIT:{status}\n"), stderr: String::new() }
    }

    #[test]
    fn parser_rejects_exit_failures_truncation_and_oversized_fields() {
        let previous = RemoteTranscriptCursor::default();
        let parse = |r| parse_result(r, MARKER, &previous, SESSION, 4);
        let mut r = framed("MISSING\nL3RtcC9wcm9qZWN0cw==\n", 0);
        assert!(!parse(r.clone()).unwrap().found);
        r.code = Some(7);
        assert!(parse(r).err().unwrap().contains("exit Some(7)"));
        assert!(parse(framed("ERROR\nAMBIGUOUS", 1)).err().unwrap().contains("multiple transcript files"));
        assert!(parse(framed("ERROR\nCHANGED", 1)).err().unwrap().contains("previous cursor"));
        assert!(parse(framed("MISSING\nL3RtcA==\n", 5)).is_err());
        let mut r = framed("MISSING\nL3RtcA==\n", 0);
        r.stdout.truncate(r.stdout.len() - 15);
        assert!(parse(r).is_err());
        let mut r = framed("MISSING\nL3RtcA==\n", 0);
        r.stdout.insert_str(0, &"x".repeat(NOISE_BYTES + 1));
        assert!(parse(r).is_err());
        assert!(decode_bounded("!!!!", 10).is_err());
        assert!(decode_bounded(&STANDARD.encode([0; 5]), 4).is_err());
    }

    #[test]
    fn protocol_checks_generation_payload_length_and_pinned_paths() {
        let root = STANDARD.encode(b"/tmp/projects");
        let path = STANDARD.encode(format!("/tmp/projects/project/{SESSION}.jsonl"));
        let body = format!("FOUND\n{root}\n{path}\n1\n2\n0\n4\nYWJjZA==\nYWJjZA==\n1");
        let first = parse_result(framed(&body, 0), MARKER, &RemoteTranscriptCursor::default(), SESSION, 4).unwrap();
        assert_eq!(first.bytes, b"abcd");
        assert!(first.reset);
        assert!(!first.has_more);
        let bad = body.replace("YWJjZA==\n1", "YWI=\n1");
        assert!(parse_result(framed(&bad, 0), MARKER, &RemoteTranscriptCursor::default(), SESSION, 4).is_err());
        assert!(parse_result(framed(&body, 0), MARKER, &first.cursor, SESSION, 4).is_err());
        let escaped = body.replace(&path, &STANDARD.encode(format!("/tmp/projects/../{SESSION}.jsonl")));
        assert!(parse_result(framed(&escaped, 0), MARKER, &RemoteTranscriptCursor::default(), SESSION, 4).is_err());
        let moved = body.replace(&path, &STANDARD.encode(format!("/tmp/projects/other/{SESSION}.jsonl")));
        assert!(parse_result(framed(&moved, 0), MARKER, &first.cursor, SESSION, 4).is_err());
    }

    #[test]
    fn non_utf8_paths_are_preserved_in_cursor_and_next_command() {
        let root = b"/tmp/config-\xff\n/projects";
        let mut path = root.to_vec();
        path.extend_from_slice(b"/project-\xfe\n/");
        path.extend_from_slice(format!("{SESSION}.jsonl").as_bytes());
        let body = format!("FOUND\n{}\n{}\n1\n2\n0\n1\nYQ==\nYQ==\n1", STANDARD.encode(root), STANDARD.encode(&path));
        let chunk = parse_result(framed(&body, 0), MARKER, &RemoteTranscriptCursor::default(), SESSION, 1).unwrap();
        assert_eq!(chunk.cursor.projects.as_deref(), Some(root.as_slice()));
        assert_eq!(chunk.cursor.path.as_deref(), Some(path.as_slice()));
        let command = read_command("/bin/sh", SESSION, &chunk.cursor, 1, MARKER);
        assert!(command.contains(&STANDARD.encode(root)));
        assert!(command.contains(&STANDARD.encode(&path)));
        assert!(!command.contains(" -i -l -c "));
    }

    struct WaitingExec(std::cell::Cell<bool>);

    impl Exec for WaitingExec {
        fn exec<'a>(&'a self, _: &'a str, cancel: Option<&'a CancellationToken>) -> LocalBoxFuture<'a, anyhow::Result<ExecResult>> {
            Box::pin(async move {
                cancel.unwrap().cancelled().await;
                self.0.set(true);
                Ok(ExecResult::default())
            })
        }
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_cancels_only_the_read_exec_and_does_not_commit_a_cursor() {
        let exec = WaitingExec(std::cell::Cell::new(false));
        let cursor = RemoteTranscriptCursor::default();
        let before = cursor.clone();
        let error = read_with_exec(&exec, "/bin/sh", SESSION, &cursor, 4).await.err().unwrap();
        assert!(error.contains("timed out"));
        assert!(exec.0.get());
        assert_eq!(cursor, before);
    }

    #[cfg(unix)]
    mod posix {
        use super::*;
        use std::fs;
        use std::io::Write;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::PermissionsExt;
        use std::path::{Path, PathBuf};
        use std::process::Stdio;

        struct Fixture {
            temp: tempfile::TempDir,
            config: PathBuf,
            shell: String,
        }

        impl Fixture {
            fn new(config_name: &std::ffi::OsStr) -> Self {
                let temp = tempfile::tempdir().unwrap();
                let config = temp.path().join(config_name);
                fs::create_dir_all(config.join("projects")).unwrap();
                let shell = temp.path().join("login ' shell");
                let config64 = STANDARD.encode(config.as_os_str().as_bytes());
                let log = quote_posix(temp.path().join("logins").to_str().unwrap());
                // 仅模拟登录 shell 的参数和配置环境，绝不启动真实登录 shell 或读取真实 HOME。
                // 变量故意不 export，以验证登录命令在原 shell 中提取它们。
                let script = format!(
                    "#!/bin/sh\n[ \"$1:$2:$3\" = '-i:-l:-c' ] || exit 19\nprintf 'login\\n' >> {log}\nprintf 'rc chatter without final newline'\nCLAUDE_CONFIG_DIR=$(printf %s '{config64}' | base64 -d; printf '.')\nCLAUDE_CONFIG_DIR=${{CLAUDE_CONFIG_DIR%.}}\nshift 3\neval \"$1\"\n"
                );
                fs::write(&shell, script).unwrap();
                fs::set_permissions(&shell, fs::Permissions::from_mode(0o700)).unwrap();
                Self { temp, config, shell: shell.to_str().unwrap().to_string() }
            }

            fn transcript(&self, project: &str, bytes: &[u8]) -> PathBuf {
                let dir = self.config.join("projects").join(project);
                fs::create_dir_all(&dir).unwrap();
                let path = dir.join(format!("{SESSION}.jsonl"));
                fs::write(&path, bytes).unwrap();
                path
            }

            fn logins(&self) -> usize {
                fs::read_to_string(self.temp.path().join("logins")).map(|s| s.lines().count()).unwrap_or(0)
            }

            async fn read(&self, cursor: &RemoteTranscriptCursor, max: usize) -> Result<RemoteTranscriptChunk, String> {
                read_with_exec(self, &self.shell, SESSION, cursor, max).await
            }
        }

        impl Exec for Fixture {
            fn exec<'a>(&'a self, command: &'a str, cancel: Option<&'a CancellationToken>) -> LocalBoxFuture<'a, anyhow::Result<ExecResult>> {
                Box::pin(async move {
                    let mut cmd = tokio::process::Command::new("/bin/sh");
                    cmd.arg("-c").arg(command).env_clear().env("HOME", self.temp.path())
                        .env("PATH", "/usr/bin:/bin").current_dir(self.temp.path())
                        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
                    let child = cmd.spawn()?;
                    let output = tokio::select! {
                        result = child.wait_with_output() => result?,
                        _ = async {
                            match cancel {
                                Some(token) => token.cancelled().await,
                                None => std::future::pending::<()>().await,
                            }
                        } => return Ok(ExecResult::default()),
                    };
                    Ok(ExecResult {
                        code: output.status.code(), stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                    })
                })
            }
        }

        fn append(path: &Path, bytes: &[u8]) {
            fs::OpenOptions::new().append(true).open(path).unwrap().write_all(bytes).unwrap();
        }

        #[tokio::test]
        async fn incremental_bytes_retry_and_no_further_login_or_directory_guessing() {
            let f = Fixture::new(std::ffi::OsStr::new("config '\" $() ;\n"));
            let bytes = b"first\n\xe4\xb8\xad\xff\0tail\n";
            let path = f.transcript("arbitrary-project-name", bytes);
            let initial = RemoteTranscriptCursor::default();
            let first = f.read(&initial, 8).await.unwrap();
            assert!(first.reset && first.found && first.has_more);
            assert_eq!(first.bytes, &bytes[..8]); // 中文 UTF-8 正好被切开，reader 必须保持原字节。
            let retry = f.read(&initial, 8).await.unwrap();
            assert_eq!(retry.cursor, first.cursor);
            assert_eq!(retry.bytes, first.bytes);
            assert_eq!(initial, RemoteTranscriptCursor::default());
            let second = f.read(&first.cursor, 64).await.unwrap();
            assert!(!second.reset && second.found && !second.has_more);
            assert_eq!(second.bytes, &bytes[8..]);
            assert_eq!(f.logins(), 2); // 两次使用未提交的初始游标，其余读取不再执行登录配置。
            let empty = f.read(&second.cursor, 64).await.unwrap();
            assert!(empty.bytes.is_empty() && !empty.reset && empty.found);
            append(&path, b"appended\n");
            // 锁定明确路径后，其他目录出现同名文件也不触发重扫描。
            f.transcript("another-project", b"wrong");
            let added = f.read(&empty.cursor, 64).await.unwrap();
            assert_eq!(added.bytes, b"appended\n");
            assert!(!added.reset);
            assert_eq!(f.logins(), 2);
        }

        #[tokio::test]
        async fn absent_file_caches_login_environment_and_disappearance_clears_generation() {
            let f = Fixture::new(std::ffi::OsStr::new("custom-config"));
            let absent = f.read(&RemoteTranscriptCursor::default(), 32).await.unwrap();
            assert!(!absent.found && !absent.reset && !absent.has_more);
            assert!(absent.cursor.projects.is_some());
            assert!(!f.read(&absent.cursor, 32).await.unwrap().found);
            let path = f.transcript("not-cwd-encoded", b"first generation");
            let first = f.read(&absent.cursor, 32).await.unwrap();
            assert!(first.found && first.reset);
            fs::remove_file(&path).unwrap();
            let gone = f.read(&first.cursor, 32).await.unwrap();
            assert!(!gone.found && gone.reset && gone.bytes.is_empty());
            assert_eq!(gone.cursor.path, first.cursor.path);
            assert!(!f.read(&gone.cursor, 32).await.unwrap().reset);
            fs::write(&path, b"second generation").unwrap();
            let returned = f.read(&gone.cursor, 32).await.unwrap();
            assert!(returned.found && returned.reset);
            assert_eq!(returned.bytes, b"second generation");
            assert_eq!(f.logins(), 1);
        }

        #[tokio::test]
        async fn ambiguous_candidates_are_an_explicit_failure() {
            let f = Fixture::new(std::ffi::OsStr::new("config"));
            f.transcript("one", b"one");
            f.transcript("two", b"two");
            let error = f.read(&RemoteTranscriptCursor::default(), 32).await.err().unwrap();
            assert!(error.contains("multiple transcript files"), "{error}");
        }

        #[tokio::test]
        async fn inode_rotation_shrink_and_same_inode_fast_regrowth_reset_without_mixing() {
            let f = Fixture::new(std::ffi::OsStr::new("config"));
            let path = f.transcript("project", b"old generation contents");
            let first = f.read(&RemoteTranscriptCursor::default(), 256).await.unwrap();
            let replacement = path.with_extension("new");
            fs::write(&replacement, b"replacement generation contents").unwrap();
            fs::rename(replacement, &path).unwrap();
            let replaced = f.read(&first.cursor, 256).await.unwrap();
            assert!(replaced.reset);
            assert_ne!(first.cursor.identity, replaced.cursor.identity);
            assert_eq!(replaced.bytes, b"replacement generation contents");
            fs::write(&path, b"x").unwrap();
            let shrunk = f.read(&replaced.cursor, 256).await.unwrap();
            assert!(shrunk.reset);
            assert_eq!(shrunk.bytes, b"x");
            fs::write(&path, b"fast truncate and regrow beyond the committed offset").unwrap();
            let grown = f.read(&shrunk.cursor, 256).await.unwrap();
            assert!(grown.reset);
            assert_eq!(grown.cursor.identity, shrunk.cursor.identity);
            assert_eq!(grown.bytes, b"fast truncate and regrow beyond the committed offset");
        }

        #[tokio::test]
        async fn unicode_paths_and_trailing_newlines_round_trip_through_base64() {
            // macOS 的文件系统拒绝非法 UTF-8 文件名；原始非 UTF-8 路径另在纯协议测试覆盖。
            let f = Fixture::new(std::ffi::OsStr::new("config-中\n"));
            let path = f.transcript("project '\" ; $(ignored)\n", b"a");
            let first = f.read(&RemoteTranscriptCursor::default(), 4).await.unwrap();
            assert_eq!(first.cursor.path.as_deref(), Some(path.as_os_str().as_bytes()));
            append(&path, b"b");
            let second = f.read(&first.cursor, 4).await.unwrap();
            assert_eq!(second.bytes, b"b");
            assert!(!second.reset);
            assert_eq!(f.logins(), 1);
        }

        #[tokio::test]
        async fn login_home_fallback_and_invalid_roots_use_only_the_isolated_environment() {
            let f = Fixture::new(std::ffi::OsStr::new(".claude"));
            f.transcript("project", b"HOME fallback");
            let original = fs::read_to_string(&f.shell).unwrap();
            fs::write(&f.shell, original.replace("shift 3\n", "CLAUDE_CONFIG_DIR=\nshift 3\n")).unwrap();
            let chunk = f.read(&RemoteTranscriptCursor::default(), 32).await.unwrap();
            assert_eq!(chunk.bytes, b"HOME fallback");
            for assignment in ["CLAUDE_CONFIG_DIR=relative", "CLAUDE_CONFIG_DIR=; HOME="] {
                fs::write(&f.shell, original.replace("shift 3\n", &format!("{assignment}\nshift 3\n"))).unwrap();
                let error = f.read(&RemoteTranscriptCursor::default(), 32).await.err().unwrap();
                assert!(error.contains("absolute CLAUDE_CONFIG_DIR or HOME"), "{error}");
            }
        }

        #[tokio::test]
        async fn real_login_noise_is_bounded_before_exec_collects_it() {
            let f = Fixture::new(std::ffi::OsStr::new("config"));
            let original = fs::read_to_string(&f.shell).unwrap();
            let noise = format!("yes noise | head -c {}\nshift 3\n", output_limit(4) + 1024);
            fs::write(&f.shell, original.replace("shift 3\n", &noise)).unwrap();
            let command = read_command(&f.shell, SESSION, &RemoteTranscriptCursor::default(), 4, MARKER);
            let result = f.exec(&command, None).await.unwrap();
            assert_eq!(result.code, Some(0));
            assert_eq!(result.stdout.len(), output_limit(4));
            assert!(result.stderr.is_empty());
            let error = parse_result(result, MARKER, &RemoteTranscriptCursor::default(), SESSION, 4).err().unwrap();
            assert!(error.contains("limit"), "{error}");
        }

        #[tokio::test]
        async fn rotation_or_rewrite_during_read_discards_the_whole_chunk() {
            let f = Fixture::new(std::ffi::OsStr::new("config"));
            let path = f.transcript("project", b"old generation bytes");
            let first = f.read(&RemoteTranscriptCursor::default(), 4).await.unwrap();
            let before = first.cursor.clone();
            let replacement = path.with_extension("new");
            fs::write(&replacement, b"new generation has more bytes").unwrap();
            let mutate = format!("mv {} {}\nafter=$(metadata_fd 3)", quote_posix(replacement.to_str().unwrap()), quote_posix(path.to_str().unwrap()));
            // 仅测试版本在 payload 已读取后执行确定性轮换；生产脚本始终只读。
            let script = READ_SCRIPT.replace("after=$(metadata_fd 3)", &mutate);
            let command = script_command(&f.shell, SESSION, &first.cursor, 4, MARKER, &script);
            let result = f.exec(&command, None).await.unwrap();
            let error = parse_result(result, MARKER, &first.cursor, SESSION, 4).err().unwrap();
            assert!(error.contains("changed during the read"), "{error}");
            assert_eq!(first.cursor, before);
            let next = f.read(&first.cursor, 64).await.unwrap();
            assert!(next.reset);
            assert_eq!(next.bytes, b"new generation has more bytes");
            let mutate = format!("printf '%s' 'rewritten generation is longer again' > {}\nafter=$(metadata_fd 3)", quote_posix(path.to_str().unwrap()));
            let script = READ_SCRIPT.replace("after=$(metadata_fd 3)", &mutate);
            let command = script_command(&f.shell, SESSION, &next.cursor, 4, MARKER, &script);
            let result = f.exec(&command, None).await.unwrap();
            let error = parse_result(result, MARKER, &next.cursor, SESSION, 4).err().unwrap();
            assert!(error.contains("changed during the read"), "{error}");
            let rewritten = f.read(&next.cursor, 64).await.unwrap();
            assert!(rewritten.reset);
            assert_eq!(rewritten.bytes, b"rewritten generation is longer again");
        }

        #[tokio::test]
        async fn empty_file_growth_and_large_chunk_limits_are_exact() {
            let f = Fixture::new(std::ffi::OsStr::new("config"));
            let path = f.transcript("project", b"");
            let empty = f.read(&RemoteTranscriptCursor::default(), 1).await.unwrap();
            assert!(empty.found && empty.reset && empty.bytes.is_empty() && !empty.has_more);
            let bytes = (0..MAX_CHUNK_BYTES + 17).map(|i| (i % 251) as u8).collect::<Vec<_>>();
            append(&path, &bytes);
            let full = f.read(&empty.cursor, MAX_CHUNK_BYTES).await.unwrap();
            assert!(!full.reset && full.has_more);
            assert_eq!(full.bytes, bytes[..MAX_CHUNK_BYTES]);
            let tail = f.read(&full.cursor, MAX_CHUNK_BYTES).await.unwrap();
            assert_eq!(tail.bytes, bytes[MAX_CHUNK_BYTES..]);
            assert!(!tail.reset && !tail.has_more);
        }
    }
}
