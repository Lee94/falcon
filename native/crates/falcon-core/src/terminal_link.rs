//! 终端文件链接的宿主机路径解析。只做词法运算，不读取客户端文件系统。

use crate::file_path::parse_nav_path;
use crate::js::decode_uri_component;

/// OSC 7 当前目录跟踪。按字节跨输出块组装，UTF-8 文件名被网络帧切开也不会变成替换字符。
/// 正文与其它 OSC 不分配字符串；未结束的目录序列最多保存 8 KiB。
#[derive(Default)]
pub struct CwdScanner {
    state: u8,
    payload: Vec<u8>,
}

impl CwdScanner {
    pub fn feed(&mut self, bytes: &[u8]) -> Option<String> {
        let mut cwd = None;
        for &byte in bytes {
            self.state = match (self.state, byte) {
                (0, 0x1b) => 1,
                (1, b']') => 2,
                (1, 0x1b) => 1,
                (2, b'7') => 3,
                (3, b';') => {
                    self.payload.clear();
                    4
                }
                (4, 0x07) | (5, b'\\') => {
                    if let Ok(uri) = std::str::from_utf8(&self.payload)
                        && uri.starts_with("file://")
                        && let Some(path) = decode_file_uri(uri)
                    {
                        cwd = Some(path);
                    }
                    self.payload.clear();
                    0
                }
                (4, 0x1b) => 5,
                (4, _) if self.payload.len() < 8192 => {
                    self.payload.push(byte);
                    4
                }
                (4, _) => {
                    self.payload.clear();
                    6
                }
                (2 | 3 | 6, 0x07) => 0,
                (2 | 3 | 6, 0x1b) => 7,
                (2 | 3 | 6, _) => 6,
                (7, b'\\') => 0,
                (7, _) => 6,
                _ => 0,
            };
        }
        cwd
    }
}

/// 去掉诊断位置（:行[:列] / #L行），保留真正文件名里的 #、% 与空格。
pub fn file_path(raw: &str) -> Option<String> {
    let mut path = raw;
    if let Some((file, location)) = path.rsplit_once("#L")
        && !location.is_empty()
        && location.chars().all(|c| c.is_ascii_digit())
    {
        path = file;
    }
    path = path.trim_end_matches(':');
    for _ in 0..2 {
        if let Some((file, number)) = path.rsplit_once(':')
            && !number.is_empty()
            && number.bytes().all(|c| c.is_ascii_digit())
        {
            path = file;
        } else {
            break;
        }
    }
    let path = if path
        .get(..7)
        .is_some_and(|s| s.eq_ignore_ascii_case("file://"))
    {
        decode_file_uri(path)?
    } else {
        path.to_string()
    };
    (!path.is_empty() && !path.chars().any(char::is_control)).then_some(path)
}

fn decode_file_uri(uri: &str) -> Option<String> {
    let rest = &uri[7..];
    let slash = rest.find('/')?;
    // OSC 的 authority 通常是 shell 自报的宿主名，仍在该会话的宿主机上打开。
    let path = decode_uri_component(&rest[slash..])?;
    let path = if path.as_bytes().get(2) == Some(&b':')
        && path.as_bytes().get(1).is_some_and(u8::is_ascii_alphabetic)
    {
        path[1..].to_string()
    } else {
        path
    };
    (!path.is_empty() && !path.chars().any(char::is_control)).then_some(path)
}

pub fn resolve_file_path(path: &str, cwd: Option<&str>, home: Option<&str>) -> Option<String> {
    if path == "~" || path.starts_with("~/") || path.starts_with("~\\") {
        let home = home?;
        return parse_nav_path(
            &format!(
                "{}/{}",
                home.trim_end_matches(['/', '\\']),
                &path[1..].trim_start_matches(['/', '\\'])
            ),
            Some(home),
        );
    }
    parse_nav_path(path, cwd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_suffixes_and_file_uris() {
        for (input, expected) in [
            ("src/main.rs:12:3:", "src/main.rs"),
            ("/tmp/图.png#L8", "/tmp/图.png"),
            ("C:\\src\\main.rs:12", "C:\\src\\main.rs"),
            ("file://lab/tmp/a%20b%231.png", "/tmp/a b#1.png"),
            ("file:///C:/images/a%20b.png", "C:/images/a b.png"),
            ("/tmp/100%#1.png", "/tmp/100%#1.png"),
        ] {
            assert_eq!(file_path(input).as_deref(), Some(expected), "{input}");
        }
        assert_eq!(file_path("file:///tmp/a%00.png"), None);
    }

    #[test]
    fn resolve_uses_host_cwd_and_home() {
        assert_eq!(
            resolve_file_path("../a.png", Some("/srv/repo/src"), None).as_deref(),
            Some("/srv/repo/a.png")
        );
        assert_eq!(
            resolve_file_path("src\\a.rs", Some("C:\\repo"), None).as_deref(),
            Some("C:/repo/src/a.rs")
        );
        assert_eq!(
            resolve_file_path("~/图.png", Some("/srv/repo"), Some("/home/fay")).as_deref(),
            Some("/home/fay/图.png")
        );
        assert_eq!(resolve_file_path("~/a.png", None, None), None);
        assert_eq!(
            resolve_file_path("./src/main.rs", None, None).as_deref(),
            Some("src/main.rs")
        );
    }

    #[test]
    fn osc7_across_every_possible_byte_boundary() {
        let data = "noise\x1b]0;file:///wrong\x07\x1b]7;file://lab/tmp/图片\x1b\\tail".as_bytes();
        for boundary in 0..=data.len() {
            let mut scanner = CwdScanner::default();
            let first = scanner.feed(&data[..boundary]);
            let second = scanner.feed(&data[boundary..]);
            assert_eq!(
                second.or(first).as_deref(),
                Some("/tmp/图片"),
                "split at {boundary}"
            );
        }
        let mut scanner = CwdScanner::default();
        assert_eq!(
            scanner.feed(b"\x1b]7;file:///tmp/a%20b\x07").as_deref(),
            Some("/tmp/a b")
        );
        scanner.feed(format!("\x1b]7;{}\x07", "x".repeat(9000)).as_bytes());
        assert_eq!(
            scanner.feed(b"\x1b]7;file:///ok\x07").as_deref(),
            Some("/ok")
        );
        assert_eq!(
            scanner.feed(b"\x1b]7;file:///tmp/dir:12\x07").as_deref(),
            Some("/tmp/dir:12")
        );
    }
}
