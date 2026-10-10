//! sudo / askpass 的脚本生成。移植自 `packages/server/src/askpass/scripts.ts`。
//!
//! Claude Code / grok 这类 agent 的 Bash 工具用管道起子进程，sudo 打不开
//! /dev/tty 就会报 "a terminal is required to authenticate"。PTY 本身没问题
//! （isatty / 控制终端都在），缺的是一条不经过 TTY 的问密通道。
//!
//! 做法：在会话 PATH 最前面放一个 `sudo` 包装。人在 Falcon 提示符里敲 sudo
//! （stdin 是 tty）原样交给 /usr/bin/sudo；管道里调 sudo 则补 `-A` 并把
//! SUDO_ASKPASS 指到同目录的 helper。helper 读旁边的 conf（不靠环境变量——
//! Claude Code 会把 SUDO_ASKPASS 从子进程环境里抠掉），长轮询 Falcon 后端，
//! 密码弹在网页上。
//!
//! 包装必须叫 `sudo` 且靠 PATH 抢在 /usr/bin 前面：agent 几乎都是 `bash -c
//! 'sudo …'`，极少写死绝对路径。绝对路径 `/usr/bin/sudo` 这条拦不住，v1 接受。
//!
//! 这个文件全是纯函数，没有留到 S3 的（hub 与安装在 askpass 的另外两个模块，S6）。

pub const SUDO_SHIM_NAME: &str = "sudo";
pub const ASKPASS_NAME: &str = "falcon-askpass";
pub const ASKPASS_CONF_NAME: &str = "askpass.conf";

pub const ASKPASS_TIMEOUT_MS: u64 = 120_000;

/// askpass.conf 的内容（TS 里是内联的 `{ url, token }`）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskpassConf {
    pub url: String,
    pub token: String,
}

pub fn render_askpass_conf(url: &str, token: &str) -> String {
    format!("URL={url}\nTOKEN={token}\n")
}

/// 解析 askpass.conf；URL 或 TOKEN 缺一个就是 None。
///
/// TS 按 `/\r?\n/` 切行再 trim；这里按 `\n` 切，多出来的行尾 `\r` 反正会被 trim 掉，结果相同。
pub fn parse_askpass_conf(text: &str) -> Option<AskpassConf> {
    let mut url = "";
    let mut token = "";
    for line in text.split('\n') {
        let t = js_trim(line);
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let Some((k, v)) = t.split_once('=') else {
            continue;
        };
        if k == "URL" {
            url = v;
        } else if k == "TOKEN" {
            token = v;
        }
    }
    if url.is_empty() || token.is_empty() {
        return None;
    }
    Some(AskpassConf { url: url.to_string(), token: token.to_string() })
}

/// stdin 是 tty → 人在终端里敲的，走原 sudo。
/// 否则视为 agent / 管道，需要 -A；已经带 -n / -S / -A 的不重复加。
///
/// 短参簇里的 n/S/A 也认（`sudo -nv`、`sudo -An`）。`-u` 的 u 不是这三位。
pub fn sudo_needs_askpass_flag<S: AsRef<str>>(argv: &[S]) -> bool {
    for a in argv.iter().map(AsRef::as_ref) {
        if a == "--" {
            break;
        }
        if a == "-n" || a == "--non-interactive" {
            return false;
        }
        if a == "-S" || a == "--stdin" {
            return false;
        }
        if a == "-A" || a == "--askpass" {
            return false;
        }
        if a.starts_with("--") {
            continue;
        }
        // TS 的 `a.length > 1` 按 UTF-16 码元算；`-` 之后再有任何字符，码元数与字节数都 > 1，
        // 两种算法在这里给出同样的真假
        if let Some(rest) = a.strip_prefix('-').filter(|r| !r.is_empty())
            && rest.chars().all(|c| c.is_ascii_alphabetic())
            && (rest.contains('n') || rest.contains('S') || rest.contains('A'))
        {
            return false;
        }
    }
    true
}

pub fn render_sudo_shim() -> String {
    [
        r##"#!/bin/sh
# falcon sudo 包装：管道/agent 走 askpass，交互终端原样透传。
REAL="${FALCON_REAL_SUDO:-/usr/bin/sudo}"
if [ ! -x "$REAL" ]; then
  if [ -x /bin/sudo ]; then REAL=/bin/sudo
  else
    echo "falcon: 找不到 sudo" >&2
    exit 127
  fi
fi

# stdin 是 tty = 人在 Falcon 提示符里敲的，不要改成网页弹窗。
if [ -t 0 ]; then
  exec "$REAL" "$@"
fi

needs_A=1
already_A=0
for a in "$@"; do
  if [ "$a" = "--" ]; then break; fi
  case "$a" in
    -n|--non-interactive|-S|--stdin) needs_A=0; break ;;
    -A|--askpass) already_A=1; needs_A=0; break ;;
    --*) ;;
    -*)
      rest=$(printf %s "$a" | cut -c2-)
      case "$rest" in
        *n*|*S*) needs_A=0; break ;;
        *A*) already_A=1; needs_A=0; break ;;
      esac
      ;;
  esac
done

HERE=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
ASKPASS="$HERE/"##,
        ASKPASS_NAME,
        r##""
export SUDO_ASKPASS="$ASKPASS"
if [ "$needs_A" = 1 ]; then
  exec "$REAL" -A "$@"
fi
exec "$REAL" "$@"
"##,
    ]
    .concat()
}

/// 纯 Python 3：远端 POSIX 没有 node，urllib 是标准库。
/// 出错只写 stderr——stdout 会被 sudo 当成密码。
pub fn render_askpass_helper() -> String {
    [
        r##"#!/usr/bin/env python3
import json, os, sys, urllib.error, urllib.request

def load_conf(path):
    url = token = ""
    with open(path, "r", encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            k, v = line.split("=", 1)
            if k == "URL":
                url = v
            elif k == "TOKEN":
                token = v
    if not url or not token:
        raise SystemExit("falcon-askpass: askpass.conf 不完整")
    return url, token

def main():
    prompt = sys.argv[1] if len(sys.argv) > 1 else "Password:"
    here = os.path.dirname(os.path.abspath(__file__))
    url, token = load_conf(os.path.join(here, ""##,
        ASKPASS_CONF_NAME,
        r##""))
    session = os.environ.get("FALCON_SESSION_ID") or None
    body = json.dumps({"prompt": prompt, "sessionId": session}).encode("utf-8")
    req = urllib.request.Request(
        url,
        data=body,
        method="POST",
        headers={
            "Authorization": "Bearer " + token,
            "Content-Type": "application/json",
        },
    )
    try:
        with urllib.request.urlopen(req, timeout=125) as resp:
            data = json.loads(resp.read().decode("utf-8"))
    except Exception as e:
        print(f"falcon-askpass: {e}", file=sys.stderr)
        raise SystemExit(1)
    password = data.get("password")
    if not isinstance(password, str) or password == "":
        raise SystemExit(1)
    sys.stdout.write(password if password.endswith("\n") else password + "\n")

if __name__ == "__main__":
    main()
"##,
    ]
    .concat()
}

/// ECMAScript 的 WhiteSpace + LineTerminator（`String.prototype.trim` 去的那一套：
/// 含 U+FEFF、不含 U+0085，与 Rust 的 `str::trim` 恰好相反）。falcon-server 里还没有
/// 共用位置，zellij/command.rs、shells.rs 各有一份同样的。
fn js_trim(s: &str) -> &str {
    s.trim_matches(|c| {
        matches!(
            c,
            '\u{9}'
                | '\u{a}'
                | '\u{b}'
                | '\u{c}'
                | '\u{d}'
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- sudoNeedsAskpassFlag ----

    /// plain sudo command needs -A
    #[test]
    fn sudo_needs_askpass_flag_plain_sudo_command_needs_a() {
        assert!(sudo_needs_askpass_flag(&["true"]));
        assert!(sudo_needs_askpass_flag(&["-u", "root", "id"]));
        assert!(sudo_needs_askpass_flag(&["-E", "env"]));
    }

    /// does not stack -A on -n / -S / -A
    #[test]
    fn sudo_needs_askpass_flag_does_not_stack_a_on_n_s_a() {
        assert!(!sudo_needs_askpass_flag(&["-n", "true"]));
        assert!(!sudo_needs_askpass_flag(&["--non-interactive", "true"]));
        assert!(!sudo_needs_askpass_flag(&["-S", "true"]));
        assert!(!sudo_needs_askpass_flag(&["-A", "true"]));
        assert!(!sudo_needs_askpass_flag(&["-nv", "true"]));
        assert!(!sudo_needs_askpass_flag(&["-An", "true"]));
    }

    /// stops at --
    #[test]
    fn sudo_needs_askpass_flag_stops_at_double_dash() {
        assert!(sudo_needs_askpass_flag(&["--", "-n"]));
    }

    // ---- askpass.conf ----

    /// round-trips url and token
    #[test]
    fn askpass_conf_round_trips_url_and_token() {
        let text = render_askpass_conf("http://127.0.0.1:4923/api/askpass", "abc");
        assert_eq!(
            parse_askpass_conf(&text),
            Some(AskpassConf { url: "http://127.0.0.1:4923/api/askpass".into(), token: "abc".into() })
        );
    }

    /// rejects incomplete conf
    #[test]
    fn askpass_conf_rejects_incomplete_conf() {
        assert_eq!(parse_askpass_conf("URL=http://x\n"), None);
        assert_eq!(parse_askpass_conf(""), None);
    }

    // ---- sudo shim (no tty) ----

    /// injects -A for a plain command and leaves -n alone
    #[cfg(unix)]
    #[test]
    fn sudo_shim_injects_a_for_plain_command_and_leaves_n_alone() {
        use std::os::unix::fs::PermissionsExt;
        use std::process::{Command, Stdio};

        let dir = tempfile::Builder::new().prefix("falcon-sudo-").tempdir().unwrap();
        let shim = dir.path().join("sudo");
        let fake = dir.path().join("real-sudo");
        std::fs::write(&shim, render_sudo_shim()).unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(&fake, "#!/bin/sh\nprintf '%s\\n' \"$@\"\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        // TS 借 python3 的 execvp 跑包装；这里直接交给 /bin/sh（包装的 shebang 就是它），
        // $0 仍是包装的路径。stdin 是管道，不是 tty
        let run = |args: &[&str]| {
            let out = Command::new("/bin/sh")
                .arg(&shim)
                .args(args)
                .env("FALCON_REAL_SUDO", &fake)
                .stdin(Stdio::piped())
                .output()
                .unwrap();
            (out.status.code(), String::from_utf8(out.stdout).unwrap(), String::from_utf8_lossy(&out.stderr).into_owned())
        };

        let (code, stdout, stderr) = run(&["true"]);
        assert_eq!(code, Some(0), "{stderr}");
        assert_eq!(stdout, "-A\ntrue\n");

        let (code, stdout, stderr) = run(&["-n", "true"]);
        assert_eq!(code, Some(0), "{stderr}");
        assert_eq!(stdout, "-n\ntrue\n");
    }

    // ---- 以下不在 scripts.test.ts 里 ----

    #[test]
    fn extra_parse_conf_edges() {
        // CRLF、注释、值里的 `=`、首尾空白
        let conf = parse_askpass_conf("# c\r\n  URL=http://h/a?x=1  \r\nTOKEN=t=2\r\nJUNK\r\n").unwrap();
        assert_eq!(conf, AskpassConf { url: "http://h/a?x=1".into(), token: "t=2".into() });
        // 后出现的覆盖先出现的
        assert_eq!(parse_askpass_conf("URL=a\nTOKEN=b\nURL=c\n").unwrap().url, "c");
        assert!(sudo_needs_askpass_flag(&["-1n"]), "短参簇里有非字母就不算");
        assert!(sudo_needs_askpass_flag(&["-"]));
    }

    #[test]
    fn extra_scripts_embed_names_and_escapes() {
        let shim = render_sudo_shim();
        assert!(shim.contains("\nASKPASS=\"$HERE/falcon-askpass\"\nexport SUDO_ASKPASS=\"$ASKPASS\"\n"));
        assert!(shim.contains("REAL=\"${FALCON_REAL_SUDO:-/usr/bin/sudo}\"\n"));
        assert!(shim.ends_with("exec \"$REAL\" \"$@\"\n"));
        let helper = render_askpass_helper();
        assert!(helper.contains("load_conf(os.path.join(here, \"askpass.conf\"))\n"));
        // Python 源码里的 "\n" 是反斜杠 + n 两个字符
        assert!(helper.contains(r#"password if password.endswith("\n") else password + "\n")"#));
        assert!(helper.ends_with("    main()\n"));
    }
}
