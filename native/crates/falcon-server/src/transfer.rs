//! 工作目录文件的下载与上传（ADR 0008）。
//!
//! 与 files.ts 的分工：那边是只读浏览，字节攒成 Buffer 回给 JSON 或原始字节路由，
//! 有 16MB 的上限；这边是**按流搬运整个文件**——下载把宿主机上的字节直接接到
//! HTTP 响应上，上传把请求体直接接到宿主机上的写入端，中途不落 Buffer，几百 MB
//! 的构建产物也过得去。
//!
//! 远端仍然不走 SFTP（理由同 fs.ts）：POSIX 的 exec 通道对原始字节是可靠的，
//! 下载就是 `cat`、上传就是 `cat >`；Windows 的 OpenSSH 会按代码页改写通道上的
//! 字节（见 paste.ts），两个方向都改走 base64——但不是整文件一坨，而是**每行独立
//! 可解的 base64 行**（Base64LineEncoder / Base64LineDecoder），两端都能流式处理。
//!
//! 上传的落盘规则：先写同目录下的临时文件，收满**声明的字节数**才改名到位。
//! 浏览器中途关掉标签页时，宿主机那头收到的只是 EOF，`cat` 照样退 0——不比字节数
//! 就会留下一个悄悄截断的文件，比上传失败糟得多。
//!
//! 命令构造与编解码是纯函数 / 纯 Transform，测试打这一层。
//!
//! 移植自 `packages/server/src/transfer.ts`。TS 的两个 Transform 在这里是可增量喂字节的
//! 结构体（[`Base64LineEncoder`] / [`Base64LineDecoder`]：`feed` 一块吐一块，`finish` 收尾），
//! 与执行器无关，S5 接流时套一层即可。
//!
//! 留到 S5 的函数（真正发起 exec 流 / 读写本机文件）：`openDownload`、`receiveUpload`、
//! `receiveLocal`、`receiveRemote`、`sweepStaleTmp`、`execStreamOrLinkError`、`withTimeout`。
//! 它们用到的常量（[`STALE_TMP_MINUTES`]、[`SHORT_MESSAGE`]）已经在这里。

use base64::Engine as _;

use crate::git::path::join_path;
use crate::zellij::host::{HostKind, encode_powershell, quote_posix, quote_powershell};

// ---------------- base64 行编解码 ----------------

/// 每行编码多少原始字节。3 的倍数，于是每一行都是一段完整的 base64，解码端
/// 逐行解即可，不必等到整个文件到齐。48KB 是 ssh2 通道包大小的整数倍，
/// 也让 PowerShell 那边一次 Read 就够填一行。
pub const B64_LINE_BYTES: usize = 48 * 1024;

/// 原始字节 → base64 行（每行 B64_LINE_BYTES 个输入字节，最后一行不足照发）
#[derive(Debug, Default)]
pub struct Base64LineEncoder {
    /// 还没凑满一行的输入字节，长度恒小于 B64_LINE_BYTES
    pending: Vec<u8>,
}

impl Base64LineEncoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂一块原始字节，返回这一块凑满的整行（ASCII，每行以 `\n` 结尾），可能为空。
    /// 行边界只跟着累计字节数走，不跟着包边界走。
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut input = chunk;
        if !self.pending.is_empty() {
            let need = B64_LINE_BYTES - self.pending.len();
            if input.len() < need {
                self.pending.extend_from_slice(input);
                return out;
            }
            self.pending.extend_from_slice(&input[..need]);
            push_b64_line(&self.pending, &mut out);
            self.pending.clear();
            input = &input[need..];
        }
        let mut lines = input.chunks_exact(B64_LINE_BYTES);
        for line in &mut lines {
            push_b64_line(line, &mut out);
        }
        self.pending.extend_from_slice(lines.remainder());
        out
    }

    /// 输入结束：不足一行的尾巴照发一行（没有尾巴就什么都不发）
    pub fn finish(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        if !self.pending.is_empty() {
            push_b64_line(&self.pending, &mut out);
            self.pending.clear();
        }
        out
    }
}

fn push_b64_line(bytes: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(base64::engine::general_purpose::STANDARD.encode(bytes).as_bytes());
    out.push(b'\n');
}

/// base64 行 → 原始字节。空行与行尾的 \r 忽略（PowerShell 的 WriteLine 是 CRLF）
#[derive(Debug, Default)]
pub struct Base64LineDecoder {
    /// 上一块末尾还没等到 `\n` 的半行
    tail: Vec<u8>,
}

impl Base64LineDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂一块通道上的字节，返回其中凑齐的整行解出来的原始字节，可能为空。
    ///
    /// TS 先按 latin1 把字节解成字符串再按 `\n` 切（"base64 只有 ASCII，latin1 解码不会把
    /// 跨包的字节切坏"）；这里直接在字节上切，latin1 是逐字节一一映射，结果相同。
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut rest = chunk;
        while let Some(i) = rest.iter().position(|&b| b == b'\n') {
            if self.tail.is_empty() {
                push_decoded_line(&rest[..i], &mut out);
            } else {
                self.tail.extend_from_slice(&rest[..i]);
                push_decoded_line(&self.tail, &mut out);
                self.tail.clear();
            }
            rest = &rest[i + 1..];
        }
        self.tail.extend_from_slice(rest);
        out
    }

    /// 输入结束：最后一行可能没有换行
    pub fn finish(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        push_decoded_line(&std::mem::take(&mut self.tail), &mut out);
        out
    }
}

fn push_decoded_line(line: &[u8], out: &mut Vec<u8>) {
    let text = js_trim_latin1(line);
    if !text.is_empty() {
        node_base64_decode(text, out);
    }
}

/// latin1 字符串上的 `String.prototype.trim`：U+0000–U+00FF 里 JS 认作空白的只有
/// \t \n \v \f \r、空格与 U+00A0（U+0085 不算）
fn js_trim_latin1(mut s: &[u8]) -> &[u8] {
    let is_space = |b: &u8| matches!(b, 0x09..=0x0d | 0x20 | 0xa0);
    while let Some((first, rest)) = s.split_first()
        && is_space(first)
    {
        s = rest;
    }
    while let Some((last, rest)) = s.split_last()
        && is_space(last)
    {
        s = rest;
    }
    s
}

/// Node 的 `Buffer.from(text, "base64")`（text 是 latin1 字节）。它从不报错，宽松得出奇；
/// 这里照抄，免得同一段通道字节两边解出不同的结果：
/// - 标准与 URL 安全两套字母都认（`+ /` 与 `- _`）；
/// - 字母表以外的字节（空白、`\r`、乱码）直接跳过；
/// - 遇到第一个 `=` 就停，后面的全部不要；
/// - 末尾不成组的位照常解出（剩 2 个字符 → 1 字节，3 个 → 2 字节），剩 1 个字符的丢掉。
///
/// 合法的标准 base64 行（PowerShell `ToBase64String` 产出的就是）结果与严格解码完全一样。
fn node_base64_decode(text: &[u8], out: &mut Vec<u8>) {
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    for &c in text {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            _ => continue,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
}

// ---------------- 下载 ----------------

/// 把文件按原始字节写到 stdout。POSIX 直接 cat；Windows 分块读、每块一行 base64，
/// `[Console]::Out.WriteLine` 绕开 PowerShell 的对象管道（Write-Output 一行行过
/// 格式化器慢得多）。Read 一次没给满也无所谓——每行独立解码，行长不必固定。
pub fn download_command(kind: HostKind, file: &str) -> String {
    if kind == HostKind::Windows {
        return encode_powershell(
            &[
                format!("$p = {}", quote_powershell(file)),
                "$fs = [IO.File]::OpenRead($p)".to_string(),
                format!(
                    "try {{ \
                     $buf = New-Object byte[] {B64_LINE_BYTES}; \
                     while (($n = $fs.Read($buf, 0, $buf.Length)) -gt 0) {{ \
                     [Console]::Out.WriteLine([Convert]::ToBase64String($buf, 0, $n)) }} \
                     }} finally {{ $fs.Close() }}"
                ),
            ]
            .join("; "),
        );
    }
    format!("cat {}", quote_posix(file))
}

/// Content-Disposition。`filename*` 是 RFC 5987 的 UTF-8 形式，中文文件名靠它；
/// `filename=` 是给不认 5987 的老客户端的 ASCII 兜底——引号与反斜杠会破坏
/// quoted-string，非 ASCII 字符各家解法不一，统一换成下划线。
pub fn content_disposition(name: &str) -> String {
    // TS 的 /[^\x20-\x7e]/g 没带 u 旗标，按 UTF-16 码元替换：BMP 外的字符（emoji 等）
    // 是一对代理，换成两个下划线
    let mut ascii = String::with_capacity(name.len());
    for c in name.chars() {
        match c {
            '"' | '\\' => ascii.push('_'),
            '\x20'..='\x7e' => ascii.push(c),
            _ => ascii.extend(std::iter::repeat_n('_', c.len_utf16())),
        }
    }
    format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{}", encode_rfc5987(name))
}

/// encodeURIComponent 放过的 `'()*` 在 5987 的 attr-char 里是不允许的。
///
/// 两步合一：encodeURIComponent 不转义的是 `A-Z a-z 0-9 - _ . ! ~ * ' ( )`，
/// 再把其中的 `'()*` 换成 %XX，剩下放过的就是 `A-Z a-z 0-9 - _ . ! ~`；
/// 其余按 UTF-8 字节逐个 %XX（大写 hex，与 encodeURIComponent 一致）。
fn encode_rfc5987(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'!' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

// ---------------- 上传 ----------------

/// 上传中途被掐断（浏览器关标签页、SSH 断线、Windows 的 sshd 关通道时会把进程树
/// 整个杀掉——脚本自己的清理跑不到）会留下 `.<名字>.<随机>.falcon-upload`。
/// 下一次往同一目录上传时顺手把超过这个岁数的扫掉；正在传的那些都比它年轻。
pub const STALE_TMP_MINUTES: u32 = 6 * 60;
const TMP_SUFFIX: &str = ".falcon-upload";

/// 上传中断时的报错文案。远端脚本以 ESHORT 报告，本地按实写字节数判；路由按原文透传
pub const SHORT_MESSAGE: &str = "上传中断，收到的字节数与声明的不一致";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadTarget {
    /// 工作目录相对路径（`/` 分隔），回给前端
    pub rel: String,
    /// 宿主机上的目标绝对路径
    pub file: String,
    /// 目标所在目录
    pub parent: String,
    /// 同目录下的临时文件，收满字节后改名成 file
    pub tmp: String,
}

/// 上传文件名的护栏。浏览器给的 File.name 不会含分隔符，会含的只能是构造出来的
/// 请求；控制字符与 `..` 同 relSegments 的理由。Windows 的保留字符在远端也会
/// 失败，但那边的报错是一段 .NET 异常文本，不如在这里直接说清楚。
pub fn validate_upload_name(kind: HostKind, name: &str) -> Result<(), String> {
    validate_entry_name(kind, name)
}

/// 上传的目标、上级目录与同目录临时文件。
///
/// `tag` 是临时文件名里的随机段，TS 的默认参数 `crypto.randomBytes(4).toString("hex")`；
/// 给 `None` 时同样取 4 个随机字节的小写 hex，测试里传固定值。
pub fn upload_target(
    kind: HostKind,
    root: &str,
    dir_rel: Option<&str>,
    name: &str,
    tag: Option<&str>,
) -> Result<UploadTarget, String> {
    let tag = match tag {
        Some(t) => t.to_string(),
        None => random_tag(),
    };
    validate_upload_name(kind, name)?;
    let dir_segs = rel_segments(dir_rel)?;
    let rel = dir_segs.iter().map(String::as_str).chain([name]).collect::<Vec<_>>().join("/");
    let parent = resolve_inside(kind, root, Some(&dir_segs.join("/")))?;
    let file = resolve_inside(kind, root, Some(&rel))?;
    let tmp = join_path(kind, &[parent.as_str(), &format!(".{name}.{tag}{TMP_SUFFIX}")]);
    Ok(UploadTarget { rel, file, parent, tmp })
}

fn random_tag() -> String {
    let mut bytes = [0u8; 4];
    getrandom::fill(&mut bytes).expect("系统随机源不可用");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// stdin → 临时文件 → 核对字节数 → 改名到位。
///
/// 检查顺序里 EEXIST 排在最后：目标是文件夹、上级目录不存在这些"覆盖也救不了"
/// 的情况先报，前端问过用户"要覆盖吗"之后带 overwrite 重发才不会撞第二个错。
/// 每条失败分支都自己 rm 临时文件——脚本没有 trap 可依赖（Windows 那边没有）。
///
/// `size` 收 `i64`：TS 守的是 `Number.isInteger(size) && size >= 0`，整数类型已经挡掉
/// 非整数，负数仍在这里拒。
pub fn upload_command(kind: HostKind, target: &UploadTarget, size: i64, overwrite: bool) -> Result<String, String> {
    if size < 0 {
        return Err("字节数不合法".to_string());
    }
    if kind == HostKind::Windows {
        let mut lines = vec![
            "$ErrorActionPreference = 'Stop'".to_string(),
            format!("$f = {}", quote_powershell(&target.file)),
            format!("$t = {}", quote_powershell(&target.tmp)),
            format!("$p = {}", quote_powershell(&target.parent)),
            "if (Test-Path -LiteralPath $f -PathType Container) { Write-Output 'EISDIR'; exit 1 }".to_string(),
            "if (-not (Test-Path -LiteralPath $p -PathType Container)) { Write-Output 'ENOENT'; exit 1 }".to_string(),
        ];
        if !overwrite {
            lines.push("if (Test-Path -LiteralPath $f) { Write-Output 'EEXIST'; exit 1 }".to_string());
        }
        lines.extend([
            format!(
                "Get-ChildItem -LiteralPath $p -Filter '*{TMP_SUFFIX}' -File -Force -EA SilentlyContinue | \
                 Where-Object {{ $_.LastWriteTime -lt (Get-Date).AddMinutes(-{STALE_TMP_MINUTES}) }} | \
                 Remove-Item -Force -EA SilentlyContinue"
            ),
            "$fs = [IO.File]::Create($t)".to_string(),
            // ReadLine 到 EOF 给 $null；空行是编码端不会发的，跳过只是稳妥
            "try { while ($null -ne ($line = [Console]::In.ReadLine())) { \
             if ($line.Length -gt 0) { $b = [Convert]::FromBase64String($line); $fs.Write($b, 0, $b.Length) } } \
             } finally { $fs.Close() }"
                .to_string(),
            format!(
                "if ((Get-Item -LiteralPath $t -Force).Length -ne {size}) {{ \
                 Remove-Item -LiteralPath $t -Force; Write-Output 'ESHORT'; exit 1 }}"
            ),
            "Move-Item -LiteralPath $t -Destination $f -Force".to_string(),
        ]);
        return Ok(encode_powershell(&lines.join("; ")));
    }
    let mut lines = vec![
        format!("f={}", quote_posix(&target.file)),
        format!("t={}", quote_posix(&target.tmp)),
        format!("p={}", quote_posix(&target.parent)),
        r#"if [ -d "$f" ]; then printf '%s\n' EISDIR; exit 1; fi"#.to_string(),
        r#"if [ ! -d "$p" ]; then printf '%s\n' ENOENT; exit 1; fi"#.to_string(),
        r#"if [ ! -w "$p" ]; then printf '%s\n' EACCES; exit 1; fi"#.to_string(),
    ];
    if !overwrite {
        lines.push(r#"if [ -e "$f" ]; then printf '%s\n' EEXIST; exit 1; fi"#.to_string());
    }
    lines.extend([
        // 极老的 busybox 没有 -mmin，2>/dev/null 吞掉报错，`;` 让 cat 照常跑
        format!(
            r#"find "$p" -maxdepth 1 -type f -name {} -mmin +{STALE_TMP_MINUTES} -delete 2>/dev/null"#,
            quote_posix(&format!(".*{TMP_SUFFIX}"))
        ),
        r#"cat > "$t" || { rm -f "$t"; exit 1; }"#.to_string(),
        r#"n=$(wc -c < "$t" | tr -d ' ')"#.to_string(),
        // 先 rm 再 printf：客户端已断开时 stdout 可能是关着的，printf 会吃 SIGPIPE 把
        // shell 带走，清理必须排在它前面（Windows 那边同样的顺序）
        format!(r#"if [ "$n" -ne {size} ]; then rm -f "$t"; printf '%s\n' ESHORT; exit 1; fi"#),
        r#"mv -f "$t" "$f" || { rm -f "$t"; exit 1; }"#.to_string(),
    ]);
    Ok(lines.join("; "))
}

// ---------------- files.ts 的护栏原语 ----------------
// 上传落点与文件名校验直接用 files.rs 的同一套护栏（relSegments / resolveInside /
// validateEntryName）。这一层的错误是 String：路由按文案判状态码（如"同名文件已存在"回 409）。

fn rel_segments(rel: Option<&str>) -> Result<Vec<String>, String> {
    crate::files::rel_segments(rel).map_err(|e| e.to_string())
}

fn resolve_inside(kind: HostKind, root: &str, rel: Option<&str>) -> Result<String, String> {
    crate::files::resolve_inside(kind, root, rel).map_err(|e| e.to_string())
}

fn validate_entry_name(kind: HostKind, name: &str) -> Result<(), String> {
    crate::files::validate_entry_name(kind, name).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zellij::host::tests::decode;
    use HostKind::{Posix, Windows};
    use base64::engine::general_purpose::STANDARD;
    use regex::Regex;

    fn assert_match(hay: &str, re: &str) {
        assert!(Regex::new(re).unwrap().is_match(hay), "{hay:?} 不匹配 /{re}/");
    }

    fn assert_no_match(hay: &str, re: &str) {
        assert!(!Regex::new(re).unwrap().is_match(hay), "{hay:?} 不该匹配 /{re}/");
    }

    /// 把整段输入按给定的块喂进解码器，收齐输出
    fn decode_chunks<'a>(chunks: impl IntoIterator<Item = &'a [u8]>) -> Vec<u8> {
        let mut dec = Base64LineDecoder::new();
        let mut out = Vec::new();
        for c in chunks {
            out.extend(dec.feed(c));
        }
        out.extend(dec.finish());
        out
    }

    fn target(kind: HostKind, root: &str, dir: &str, name: &str, tag: &str) -> UploadTarget {
        upload_target(kind, root, Some(dir), name, Some(tag)).unwrap()
    }

    /// TS：「Base64LineEncoder：每行编码 B64_LINE_BYTES 个字节，跨包也能对齐」
    #[test]
    fn encoder_lines_hold_b64_line_bytes_across_chunks() {
        let data = vec![0xabu8; B64_LINE_BYTES * 2 + 7];
        // 故意按奇怪的边界切包，行边界不能跟着包边界走
        let mut enc = Base64LineEncoder::new();
        let mut out = Vec::new();
        for c in [&data[..100], &data[100..B64_LINE_BYTES + 5], &data[B64_LINE_BYTES + 5..]] {
            out.extend(enc.feed(c));
        }
        out.extend(enc.finish());
        let out = String::from_utf8(out).unwrap();
        let mut lines: Vec<&str> = out.split('\n').collect();
        assert_eq!(lines.pop(), Some(""), "以换行结尾");
        assert_eq!(lines.len(), 3);
        assert_eq!(STANDARD.decode(lines[0]).unwrap().len(), B64_LINE_BYTES);
        assert_eq!(STANDARD.decode(lines[1]).unwrap().len(), B64_LINE_BYTES);
        assert_eq!(STANDARD.decode(lines[2]).unwrap().len(), 7);
        let re = Regex::new(r"^[A-Za-z0-9+/=]+$").unwrap();
        assert!(lines.iter().all(|l| re.is_match(l)));
    }

    /// TS：「Base64LineDecoder：逐行解码，忽略 CRLF 与空行，行长不必固定」
    #[test]
    fn decoder_ignores_crlf_and_blank_lines() {
        let parts: [Vec<u8>; 3] = ["hello ".into(), "世界".into(), vec![7u8; 1000]];
        let text = format!(
            "{}\r\n\r\n{}\n{}",
            STANDARD.encode(&parts[0]),
            STANDARD.encode(&parts[1]),
            STANDARD.encode(&parts[2])
        );
        // 按单字节切包，模拟最坏的网络分片
        let out = decode_chunks(text.as_bytes().chunks(1));
        assert_eq!(out, parts.concat());
    }

    /// TS：「编码 → 解码 往返无损」
    #[test]
    fn encode_then_decode_round_trips() {
        let data: Vec<u8> = (0..200_001u32).map(|i| (i.wrapping_mul(7919) & 0xff) as u8).collect();
        let mut enc = Base64LineEncoder::new();
        let mut dec = Base64LineDecoder::new();
        let mut out = dec.feed(&enc.feed(&data));
        out.extend(dec.feed(&enc.finish()));
        out.extend(dec.finish());
        assert_eq!(out, data);
    }

    /// TS：「downloadCommand：posix 直接 cat，windows 分块 base64 行」
    #[test]
    fn download_command_posix_cat_windows_b64_lines() {
        assert_eq!(download_command(Posix, "/home/u/repo/a b'c.bin"), r"cat '/home/u/repo/a b'\''c.bin'");
        let win = decode(&download_command(Windows, "C:\\repo\\it's.bin"));
        assert_match(&win, r"\$p = 'C:\\repo\\it''s\.bin'");
        assert_match(&win, r"OpenRead\(\$p\)");
        assert_match(&win, r"ToBase64String\(\$buf, 0, \$n\)");
        assert_match(&win, r"WriteLine");
    }

    /// TS：「contentDisposition：ASCII 兜底 + RFC 5987 的 UTF-8 形式」
    #[test]
    fn content_disposition_ascii_fallback_and_rfc5987() {
        assert_eq!(
            content_disposition("报告 (final)*.pdf"),
            r#"attachment; filename="__ (final)*.pdf"; filename*=UTF-8''%E6%8A%A5%E5%91%8A%20%28final%29%2A.pdf"#
        );
        // 引号与反斜杠会破坏 quoted-string
        assert_eq!(
            content_disposition("a\"b\\c.txt"),
            r#"attachment; filename="a_b_c.txt"; filename*=UTF-8''a%22b%5Cc.txt"#
        );
    }

    /// TS：「validateUploadName 挡分隔符、控制字符与 Windows 保留字符」
    #[test]
    fn validate_upload_name_rejects_separators_controls_and_windows_reserved() {
        validate_upload_name(Posix, "a:b?.txt").unwrap();
        let long = "x".repeat(256);
        for bad in ["", ".", "..", "a/b", "a\\b", "a\nb", long.as_str()] {
            let err = validate_upload_name(Posix, bad).expect_err(&format!("{bad:?}"));
            assert!(err.contains("文件名"), "{bad:?}: {err}");
        }
        assert!(validate_upload_name(Windows, "a:b.txt").unwrap_err().contains("Windows"));
    }

    /// TS：「uploadTarget：目标、上级目录与同目录临时文件」
    #[test]
    fn upload_target_file_parent_and_sibling_tmp() {
        let t = target(Posix, "/home/u/repo", "docs/img", "logo.png", "abcd");
        assert_eq!(
            t,
            UploadTarget {
                rel: "docs/img/logo.png".into(),
                file: "/home/u/repo/docs/img/logo.png".into(),
                parent: "/home/u/repo/docs/img".into(),
                tmp: "/home/u/repo/docs/img/.logo.png.abcd.falcon-upload".into(),
            }
        );
        let root = target(Windows, "C:/code/repo", "", "a.txt", "ff");
        assert_eq!(root.rel, "a.txt");
        assert_eq!(root.file, "C:\\code\\repo\\a.txt");
        assert_eq!(root.parent, "C:\\code\\repo");
        assert_eq!(root.tmp, "C:\\code\\repo\\.a.txt.ff.falcon-upload");
        assert!(upload_target(Posix, "/home/u/repo", Some("../x"), "a.txt", None).unwrap_err().contains("路径不合法"));
    }

    /// TS：「uploadCommand：posix 先检查再 cat 到临时文件，核对字节数后 mv」
    #[test]
    fn upload_command_posix_checks_then_cat_then_mv() {
        let t = target(Posix, "/r", "d", "a.txt", "t1");
        let cmd = upload_command(Posix, &t, 1234, false).unwrap();
        assert_match(&cmd, r"f='/r/d/a\.txt'; t='/r/d/\.a\.txt\.t1\.falcon-upload'; p='/r/d'");
        assert_match(&cmd, r#"if \[ -e "\$f" \]; then printf '%s\\n' EEXIST; exit 1; fi"#);
        assert_match(&cmd, r#"find "\$p" -maxdepth 1 -type f -name '\.\*\.falcon-upload' -mmin \+360 -delete 2>/dev/null"#);
        assert_match(&cmd, r#"cat > "\$t" \|\| \{ rm -f "\$t"; exit 1; \}"#);
        assert_match(&cmd, r#"-ne 1234 \]; then rm -f "\$t"; printf '%s\\n' ESHORT; exit 1"#);
        assert_match(&cmd, r#"mv -f "\$t" "\$f""#);
        // 检查顺序：目标是目录 / 上级不存在先于 EEXIST，覆盖也救不了的先报
        assert!(cmd.find("EISDIR") < cmd.find("ENOENT"));
        assert!(cmd.find("ENOENT") < cmd.find("EEXIST"));
        // overwrite 时不再拦 EEXIST
        assert_no_match(&upload_command(Posix, &t, 1234, true).unwrap(), "EEXIST");
        assert!(upload_command(Posix, &t, -1, false).unwrap_err().contains("字节数"));
    }

    /// TS：「uploadCommand：windows 逐行解 base64 写临时文件，核对长度后 Move-Item」
    #[test]
    fn upload_command_windows_decodes_lines_then_move_item() {
        let t = target(Windows, "C:\\r", "d", "a.txt", "t1");
        let win = decode(&upload_command(Windows, &t, 99, false).unwrap());
        assert_match(&win, r"\$ErrorActionPreference = 'Stop'");
        assert_match(&win, r"\$t = 'C:\\r\\d\\\.a\.txt\.t1\.falcon-upload'");
        assert_match(&win, r"Write-Output 'EEXIST'");
        assert_match(&win, r"-Filter '\*\.falcon-upload' .*AddMinutes\(-360\)");
        assert_match(&win, r"\[Console\]::In\.ReadLine\(\)");
        assert_match(&win, r"FromBase64String\(\$line\)");
        assert_match(&win, r"\.Length -ne 99\)");
        assert_match(&win, r"Move-Item -LiteralPath \$t -Destination \$f -Force");
        assert_no_match(&decode(&upload_command(Windows, &t, 99, true).unwrap()), "EEXIST");
    }

    // ---------------- Rust 侧补充 ----------------

    /// 解码器照 Node `Buffer.from(s, "base64")` 的宽松语义；期望值由 node 实跑取得
    #[test]
    fn decoder_matches_node_lenient_base64() {
        let cases: &[(&[u8], &[u8])] = &[
            (b"QQ==", b"A"),
            (b"QQ", b"A"),
            (b"Q", b""),
            (b"QU JD", b"ABC"),
            (b"QQ==QUJD", b"A"),
            (b"-_8", b"\xfb\xff"),
            (b"QUJ*DRA", b"ABCD"),
            (b"\xffQUJD\xa0", b"ABC"),
            (b"QUJDR", b"ABC"),
            (b"Q=UJD", b""),
            (b"QUJ=D", b"AB"),
            (b"AB", b"\x00"),
            (b"=QUJD", b""),
            (b"QUJ-", b"AB~"),
            (b"Q\x00U\x00JD\x85", b"ABC"),
        ];
        for (input, want) in cases {
            assert_eq!(decode_chunks([*input]), *want, "{:?}", String::from_utf8_lossy(input));
        }
        // 每行独立解：上一行的残位不会拼到下一行
        assert_eq!(decode_chunks([&b"QUJDR\nQUJD\r\n"[..]]), b"ABCABC");
    }

    /// 编码端：包边界恰好落在行边界、空块、空输入
    #[test]
    fn encoder_edge_chunks() {
        let mut enc = Base64LineEncoder::new();
        assert!(enc.feed(&[]).is_empty());
        assert!(enc.finish().is_empty(), "空输入不发空行");

        let data = vec![1u8; B64_LINE_BYTES];
        let mut enc = Base64LineEncoder::new();
        let out = enc.feed(&data);
        assert_eq!(out, format!("{}\n", STANDARD.encode(&data)).into_bytes());
        assert!(enc.finish().is_empty());

        let mut enc = Base64LineEncoder::new();
        assert!(enc.feed(&data[..B64_LINE_BYTES - 1]).is_empty());
        assert_eq!(enc.feed(&[1, 2]), format!("{}\n", STANDARD.encode(&data)).into_bytes());
        assert_eq!(enc.finish(), b"Ag==\n");
    }

    /// TS 的正则不带 u 旗标，按 UTF-16 码元替换：BMP 外的字符算两个
    #[test]
    fn content_disposition_astral_chars_are_two_units() {
        assert_eq!(
            content_disposition("🦅 it's~!.txt"),
            "attachment; filename=\"__ it's~!.txt\"; filename*=UTF-8''%F0%9F%A6%85%20it%27s~!.txt"
        );
    }

    /// 文件名长度按 UTF-16 码元算（TS 的 .length），255 个刚好放行
    #[test]
    fn validate_upload_name_length_counts_utf16_units() {
        validate_upload_name(Posix, &"中".repeat(255)).unwrap();
        assert_eq!(validate_upload_name(Posix, &"🦅".repeat(128)).unwrap_err(), "文件名太长");
        assert_eq!(validate_upload_name(Posix, "a\u{1f}b").unwrap_err(), "文件名不合法");
        assert!(upload_target(Posix, "/r", Some("a/\u{7}/b"), "x", None).unwrap_err().contains("路径不合法"));
    }

    /// 不给 tag 时取 4 个随机字节的小写 hex
    #[test]
    fn upload_target_random_tag() {
        let t = upload_target(Posix, "/r", None, "a.txt", None).unwrap();
        assert_match(&t.tmp, r"^/r/\.a\.txt\.[0-9a-f]{8}\.falcon-upload$");
    }

    /// 整串与 TS 逐字节一致（tsx 跑 transfer.ts 取得）
    #[test]
    fn full_commands_match_ts() {
        let t = target(Posix, "/r", "d", "a.txt", "t1");
        assert_eq!(
            upload_command(Posix, &t, 1234, false).unwrap(),
            r#"f='/r/d/a.txt'; t='/r/d/.a.txt.t1.falcon-upload'; p='/r/d'; if [ -d "$f" ]; then printf '%s\n' EISDIR; exit 1; fi; if [ ! -d "$p" ]; then printf '%s\n' ENOENT; exit 1; fi; if [ ! -w "$p" ]; then printf '%s\n' EACCES; exit 1; fi; if [ -e "$f" ]; then printf '%s\n' EEXIST; exit 1; fi; find "$p" -maxdepth 1 -type f -name '.*.falcon-upload' -mmin +360 -delete 2>/dev/null; cat > "$t" || { rm -f "$t"; exit 1; }; n=$(wc -c < "$t" | tr -d ' '); if [ "$n" -ne 1234 ]; then rm -f "$t"; printf '%s\n' ESHORT; exit 1; fi; mv -f "$t" "$f" || { rm -f "$t"; exit 1; }"#
        );
        let t = target(Windows, "C:\\r", "d", "a.txt", "t1");
        assert_eq!(
            decode(&upload_command(Windows, &t, 99, false).unwrap()),
            "$ProgressPreference = 'SilentlyContinue'; [Console]::OutputEncoding = [Text.Encoding]::UTF8; \
             $ErrorActionPreference = 'Stop'; $f = 'C:\\r\\d\\a.txt'; $t = 'C:\\r\\d\\.a.txt.t1.falcon-upload'; \
             $p = 'C:\\r\\d'; if (Test-Path -LiteralPath $f -PathType Container) { Write-Output 'EISDIR'; exit 1 }; \
             if (-not (Test-Path -LiteralPath $p -PathType Container)) { Write-Output 'ENOENT'; exit 1 }; \
             if (Test-Path -LiteralPath $f) { Write-Output 'EEXIST'; exit 1 }; \
             Get-ChildItem -LiteralPath $p -Filter '*.falcon-upload' -File -Force -EA SilentlyContinue | \
             Where-Object { $_.LastWriteTime -lt (Get-Date).AddMinutes(-360) } | Remove-Item -Force -EA SilentlyContinue; \
             $fs = [IO.File]::Create($t); try { while ($null -ne ($line = [Console]::In.ReadLine())) { \
             if ($line.Length -gt 0) { $b = [Convert]::FromBase64String($line); $fs.Write($b, 0, $b.Length) } } \
             } finally { $fs.Close() }; if ((Get-Item -LiteralPath $t -Force).Length -ne 99) { \
             Remove-Item -LiteralPath $t -Force; Write-Output 'ESHORT'; exit 1 }; \
             Move-Item -LiteralPath $t -Destination $f -Force"
        );
        assert_eq!(
            decode(&download_command(Windows, "C:\\repo\\it's.bin")),
            "$ProgressPreference = 'SilentlyContinue'; [Console]::OutputEncoding = [Text.Encoding]::UTF8; \
             $p = 'C:\\repo\\it''s.bin'; $fs = [IO.File]::OpenRead($p); try { $buf = New-Object byte[] 49152; \
             while (($n = $fs.Read($buf, 0, $buf.Length)) -gt 0) { \
             [Console]::Out.WriteLine([Convert]::ToBase64String($buf, 0, $n)) } } finally { $fs.Close() }"
        );
    }

    /// 用 sh 真跑上传脚本：收满字节才落盘；少了报 ESHORT 且不留临时文件；EEXIST / EISDIR / ENOENT
    #[cfg(unix)]
    #[test]
    fn upload_command_runs_in_sh() {
        use std::io::Write as _;
        use std::process::{Command, Stdio};

        let run = |t: &UploadTarget, size: i64, overwrite: bool, body: &[u8]| {
            let mut child = Command::new("sh")
                .arg("-c")
                .arg(upload_command(Posix, t, size, overwrite).unwrap())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            // 前置检查失败时脚本不读 stdin 就退出，写端会撞 EPIPE——那是预期内的
            let _ = child.stdin.take().unwrap().write_all(body);
            let out = child.wait_with_output().unwrap();
            (out.status.code(), String::from_utf8(out.stdout).unwrap())
        };
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("it's root");
        std::fs::create_dir_all(root.join("d")).unwrap();
        let root = root.to_str().unwrap();
        let body: Vec<u8> = (0..=255u8).cycle().take(5000).collect();
        let t = target(Posix, root, "d", "a b'c.bin", "t1");

        assert_eq!(run(&t, 5000, false, &body), (Some(0), String::new()));
        assert_eq!(std::fs::read(&t.file).unwrap(), body);
        assert!(!std::path::Path::new(&t.tmp).exists());

        assert_eq!(run(&t, 5000, false, &body), (Some(1), "EEXIST\n".into()));
        assert_eq!(run(&t, 6000, true, &body), (Some(1), "ESHORT\n".into()));
        assert!(!std::path::Path::new(&t.tmp).exists(), "ESHORT 不留临时文件");
        assert_eq!(std::fs::read(&t.file).unwrap(), body, "ESHORT 不动原文件");
        assert_eq!(run(&t, 3, true, b"new"), (Some(0), String::new()));
        assert_eq!(std::fs::read(&t.file).unwrap(), b"new");

        let dir_target = target(Posix, root, "", "d", "t2");
        assert_eq!(run(&dir_target, 0, true, b""), (Some(1), "EISDIR\n".into()));
        let missing = target(Posix, root, "nope", "x", "t3");
        assert_eq!(run(&missing, 0, false, b""), (Some(1), "ENOENT\n".into()));
    }
}
