//! 粘贴图片：把浏览器剪贴板里的图片写到会话宿主机上，返回绝对路径。
//! 前端随后把路径粘进终端输入——Claude Code 等 TUI 认"输入框里的图片路径"。
//!
//! 落脚点统一是 falcon 根目录下的 paste/（本地 = --data-dir，远端 = ~/.falcon），
//! 与 Zellij 布局同根，卸载时一并带走。SSH 侧不走 SFTP（理由同 fs.ts）：
//! POSIX 用 `cat > file` 收 stdin 原始字节；Windows 的 exec 通道对二进制
//! 不可靠，改收 base64 再在 PowerShell 里解码。
//!
//! 移植自 `packages/server/src/paste.ts`。

use crate::git::path::join_path;
use crate::zellij::host::{HostKind, encode_powershell, quote_posix, quote_powershell};

/// 超过这个岁数的旧图在下一次粘贴时顺手删掉；正常粘贴当场就被读走了
const MAX_AGE_HOURS: u32 = 24;

/// Content-Type → 扩展名；认不出的类型返回 None（不落盘来路不明的字节）
pub fn image_ext(content_type: Option<&str>) -> Option<&'static str> {
    let content_type = content_type.filter(|s| !s.is_empty())?;
    // split 至少给一段，`[0]` 不会落空
    let mime = js_trim(content_type.split(';').next().unwrap_or_default()).to_lowercase();
    match mime.as_str() {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        _ => None,
    }
}

/// root 是宿主机上的 falcon 根目录（remoteRoot 或本地 dataDir）
pub fn paste_dir(kind: HostKind, root: &str) -> String {
    join_path(kind, &[root, "paste"])
}

/// TS 是 `img-${crypto.randomUUID().slice(0, 8)}`：UUID v4 的前 8 个 hex 全是随机位
/// （版本号那一位在第 13 个 hex 上），等价于 4 个随机字节的小写 hex。
pub fn paste_file_name(ext: &str) -> String {
    let mut bytes = [0u8; 4];
    getrandom::fill(&mut bytes).expect("系统随机源不可用");
    let tag: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("img-{tag}.{ext}")
}

/// POSIX：建目录、清旧图、stdin 原始字节写入。
/// find 缺 -mmin（极老的 busybox）时静默跳过清理，`;` 保证 cat 照常跑；
/// 整条命令的退出码就是 cat 的——mkdir 失败时重定向也会失败，同样非零。
pub fn posix_write_command(dir: &str, file_path: &str) -> String {
    let d = quote_posix(dir);
    format!(
        "d={d}; mkdir -p \"$d\" && \
         find \"$d\" -maxdepth 1 -type f -mmin +{} -delete 2>/dev/null; \
         cat > {}",
        MAX_AGE_HOURS * 60,
        quote_posix(file_path)
    )
}

/// Windows：stdin 收 base64 文本（OpenSSH for Windows 的 exec 通道会按代码页
/// 改写字节，原始二进制过不去），PowerShell 解码后 WriteAllBytes。
/// 写失败抛异常 → -EncodedCommand 退 1，stderr 里有信息。
pub fn windows_write_command(dir: &str, file_path: &str) -> String {
    encode_powershell(
        &[
            format!("$d = {}", quote_powershell(dir)),
            "New-Item -ItemType Directory -Force -Path $d | Out-Null".to_string(),
            format!(
                "Get-ChildItem -LiteralPath $d -File -EA SilentlyContinue | \
                 Where-Object {{ $_.LastWriteTime -lt (Get-Date).AddHours(-{MAX_AGE_HOURS}) }} | \
                 Remove-Item -Force -EA SilentlyContinue"
            ),
            "$b = [Convert]::FromBase64String([Console]::In.ReadToEnd())".to_string(),
            format!("[IO.File]::WriteAllBytes({}, $b)", quote_powershell(file_path)),
        ]
        .join("; "),
    )
}

/// `String.prototype.trim`：JS 的空白集合 = Unicode White_Space 去掉 U+0085、加上 U+FEFF，
/// 与 Rust 的 `str::trim` 差这两个码点
fn js_trim(s: &str) -> &str {
    s.trim_matches(|c: char| (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}')
}

/// 本地会话：直接写进 `<dataDir>/paste`，清理逻辑与远端一致
pub fn write_local_paste_file(data_dir: &std::path::Path, ext: &str, data: &[u8]) -> std::io::Result<std::path::PathBuf> {
    let dir = data_dir.join("paste");
    std::fs::create_dir_all(&dir)?;

    let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(u64::from(MAX_AGE_HOURS) * 3600);
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            // 清理是顺手的事，失败不挡写入
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_file() && meta.modified().is_ok_and(|m| m < cutoff) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    let file = dir.join(paste_file_name(ext));
    std::fs::write(&file, data)?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zellij::host::tests::decode;
    use HostKind::{Posix, Windows};
    use regex::Regex;

    fn assert_match(hay: &str, re: &str) {
        assert!(Regex::new(re).unwrap().is_match(hay), "{hay:?} 不匹配 /{re}/");
    }

    /// TS：describe("imageExt")
    mod image_ext {
        use super::*;

        /// it("maps the supported image types")
        #[test]
        fn maps_the_supported_image_types() {
            assert_eq!(image_ext(Some("image/png")), Some("png"));
            assert_eq!(image_ext(Some("image/jpeg")), Some("jpg"));
            assert_eq!(image_ext(Some("image/gif")), Some("gif"));
            assert_eq!(image_ext(Some("image/webp")), Some("webp"));
        }

        /// it("ignores parameters and case")
        #[test]
        fn ignores_parameters_and_case() {
            assert_eq!(image_ext(Some("image/PNG; charset=binary")), Some("png"));
        }

        /// it("rejects everything else — unknown bytes must not land on the host")
        #[test]
        fn rejects_everything_else() {
            assert_eq!(image_ext(Some("image/svg+xml")), None);
            assert_eq!(image_ext(Some("application/octet-stream")), None);
            assert_eq!(image_ext(None), None);
        }

        /// Rust 侧补充：空串与 None 同样是"没给"；trim 的字符集照 JS（U+FEFF 算空白、U+0085 不算）
        #[test]
        fn empty_and_js_trim_charset() {
            assert_eq!(image_ext(Some("")), None);
            assert_eq!(image_ext(Some("\u{feff} image/gif \t; x=1")), Some("gif"));
            assert_eq!(image_ext(Some("image/gif\u{85}")), None);
        }
    }

    /// TS：describe("pasteDir / pasteFileName")
    mod paste_dir_and_file_name {
        use super::*;

        /// it("lives under the falcon root on either platform")
        #[test]
        fn lives_under_the_falcon_root_on_either_platform() {
            assert_eq!(paste_dir(Posix, "/home/u/.falcon"), "/home/u/.falcon/paste");
            assert_eq!(paste_dir(Windows, "C:\\Users\\u\\.falcon"), "C:\\Users\\u\\.falcon\\paste");
        }

        /// it("generates short whitespace-free names so the pasted path never needs quoting")
        #[test]
        fn generates_short_whitespace_free_names() {
            let name = paste_file_name("png");
            assert_match(&name, r"^img-[0-9a-f]{8}\.png$");
        }
    }

    /// TS：describe("posixWriteCommand")
    mod posix_write_command {
        use super::*;

        /// it("creates the dir, sweeps stale files, then cats stdin into the target")
        #[test]
        fn creates_dir_sweeps_stale_files_then_cats_stdin() {
            let cmd = posix_write_command("/home/u/.falcon/paste", "/home/u/.falcon/paste/img-1.png");
            assert_match(&cmd, r#"mkdir -p "\$d""#);
            assert_match(&cmd, r#"find "\$d" -maxdepth 1 -type f -mmin \+1440 -delete 2>/dev/null; "#);
            assert!(cmd.ends_with("cat > '/home/u/.falcon/paste/img-1.png'"));
        }

        /// it("quotes paths so a home dir with spaces or quotes cannot split the command")
        #[test]
        fn quotes_paths_with_spaces_or_quotes() {
            let cmd = posix_write_command("/home/o'brien/.falcon/paste", "/home/o'brien/.falcon/paste/i.png");
            assert!(cmd.starts_with(r"d='/home/o'\''brien/.falcon/paste'"));
            assert!(cmd.ends_with(r"cat > '/home/o'\''brien/.falcon/paste/i.png'"));
        }

        /// Rust 侧补充：整串与 TS 输出逐字节一致（tsx 跑 paste.ts 取得）
        #[test]
        fn full_command_matches_ts() {
            assert_eq!(
                posix_write_command("/home/o'brien/.falcon/paste", "/home/o'brien/.falcon/paste/i.png"),
                r#"d='/home/o'\''brien/.falcon/paste'; mkdir -p "$d" && find "$d" -maxdepth 1 -type f -mmin +1440 -delete 2>/dev/null; cat > '/home/o'\''brien/.falcon/paste/i.png'"#
            );
        }

        /// Rust 侧补充：用 sh 真跑一遍——建目录、收 stdin 原始字节、删掉陈旧文件
        #[cfg(unix)]
        #[test]
        fn runs_in_sh() {
            use std::io::Write as _;
            use std::process::{Command, Stdio};

            let tmp = tempfile::tempdir().unwrap();
            let dir = tmp.path().join("a b'c").join("paste");
            let file = dir.join("img-1.png");
            let (dir, file) = (dir.to_str().unwrap(), file.to_str().unwrap());
            let data: Vec<u8> = (0..=255u8).chain([0, b'\n', b'\r']).collect();
            let mut child = Command::new("sh")
                .arg("-c")
                .arg(posix_write_command(dir, file))
                .stdin(Stdio::piped())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(&data).unwrap();
            assert!(child.wait().unwrap().success());
            assert_eq!(std::fs::read(file).unwrap(), data);
        }
    }

    /// TS：describe("windowsWriteCommand")
    mod windows_write_command {
        use super::*;

        /// it("is an -EncodedCommand invocation immune to the remote DefaultShell")
        #[test]
        fn is_an_encoded_command_invocation() {
            let cmd = windows_write_command("C:\\u\\.falcon\\paste", "C:\\u\\.falcon\\paste\\i.png");
            assert!(cmd.starts_with("powershell -NoProfile -NonInteractive -EncodedCommand "));
        }

        /// it("reads base64 from stdin and writes decoded bytes to the target path")
        #[test]
        fn reads_base64_from_stdin_and_writes_decoded_bytes() {
            let script = decode(&windows_write_command(
                "C:\\Users\\a b\\.falcon\\paste",
                "C:\\Users\\a b\\.falcon\\paste\\i.png",
            ));
            assert_match(&script, r"New-Item -ItemType Directory -Force -Path \$d");
            assert_match(&script, r"\[Convert\]::FromBase64String\(\[Console\]::In\.ReadToEnd\(\)\)");
            assert!(script.contains(r"[IO.File]::WriteAllBytes('C:\Users\a b\.falcon\paste\i.png', $b)"));
            assert!(script.contains(r"$d = 'C:\Users\a b\.falcon\paste'"));
        }

        /// it("sweeps files older than 24h before writing")
        #[test]
        fn sweeps_files_older_than_24h() {
            let script = decode(&windows_write_command("C:\\u\\p", "C:\\u\\p\\i.png"));
            assert_match(&script, r"AddHours\(-24\)");
            assert_match(&script, r"Remove-Item -Force -EA SilentlyContinue");
        }

        /// Rust 侧补充：脚本全文与 TS 逐字节一致（tsx 跑 paste.ts 取得）
        #[test]
        fn full_script_matches_ts() {
            assert_eq!(
                decode(&windows_write_command("C:\\Users\\it's\\.falcon\\paste", "C:\\Users\\it's\\.falcon\\paste\\i.png")),
                "$ProgressPreference = 'SilentlyContinue'; [Console]::OutputEncoding = [Text.Encoding]::UTF8; \
                 $d = 'C:\\Users\\it''s\\.falcon\\paste'; \
                 New-Item -ItemType Directory -Force -Path $d | Out-Null; \
                 Get-ChildItem -LiteralPath $d -File -EA SilentlyContinue | \
                 Where-Object { $_.LastWriteTime -lt (Get-Date).AddHours(-24) } | \
                 Remove-Item -Force -EA SilentlyContinue; \
                 $b = [Convert]::FromBase64String([Console]::In.ReadToEnd()); \
                 [IO.File]::WriteAllBytes('C:\\Users\\it''s\\.falcon\\paste\\i.png', $b)"
            );
        }
    }
}
