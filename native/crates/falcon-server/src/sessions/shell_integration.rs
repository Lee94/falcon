//! 对话终端的 shell integration（docs/design/ai-assistant.md 4.1）。
//!
//! 内置 agent 的命令要打进用户也在用的交互 shell，服务端得知道三件事：shell 此刻是不是停在
//! 提示符上、命令的输出从哪开始到哪结束、退出码多少。这些靠 shell 自己报——OSC 133（`A` 提示符
//! 开始、`C` 命令开始输出、`D;<退出码>` 命令结束）与 OSC 7（`file://主机/当前目录`）。服务端在
//! 输出流上认它们（shell_marks.rs）。
//!
//! - **只给对话终端注入**，项目里的普通会话不动——那是用户的地盘；
//! - 注入照 agent 启动脚本（agent.rs，ADR 0013）的路子：脚本写在宿主机 `<falcon 根>/shell-integration/`，
//!   每次附着都重写（自愈、随 falcon 升级），会话的 argv 是一个启动器，启动器带着集成脚本 `exec`
//!   用户的 shell。shell 的绝对路径**生成时写死**，不读 `$SHELL`（理由同 agent.rs）；
//! - 与普通会话同一个口径：守护进程起的是**交互式非登录** shell（argv 就是 `[shell]`，本机的登录
//!   环境由服务端解析好整份给，SSH 上 holder 由登录 shell 拉起），这里也只模拟交互式非登录：
//!   zsh 读 `.zshenv` + `.zshrc`，bash 读 `~/.bashrc`，fish 照常；
//! - 只认 zsh / bash / fish。别的 shell（sh、dash、tcsh、nu）与 Windows 宿主机都不注入，启动器
//!   直接 exec 原 shell：agent 那边退成独立进程执行命令（设计文档 5.3 的表）。PowerShell 的集成
//!   写得出来，但没有 Windows 真机验证，v1 不上。
//!
//! 钩子里的取舍（改之前先读）：
//! - **每个提示符前都打 `D;$?`**，不管上一条到底跑没跑命令（空回车也打）。服务端只在「运行中」时
//!   认 D，多余的无害；省掉了「是否真跑过命令」这个在 bash 里要靠子 shell 外的标志才记得住的状态；
//! - **不改 PS1**（不打 `B`）：prompt 主题（p10k、starship）每个 precmd 都会重设提示符，加进去的
//!   标记会被冲掉；服务端也用不上 B；
//! - zsh 的 precmd 钩子**排在最前**：排在用户的后面，`$?` 早被主题的钩子改掉了；preexec 排在最后，
//!   C 紧挨着命令真正开跑；
//! - bash ≥ 4.4 用 `PS0` 打 C（展开后、执行前）；更老的（macOS 自带的 3.2）用 DEBUG trap，而且
//!   只在用户自己没设 DEBUG trap 时才设——bash-preexec 一类工具占着它，抢过来会把它们弄坏。
//!   拿不到 C 时服务端从写入命令那一刻起收输出（会带上回显的那一行），见 shell_marks.rs。

use std::path::{Path, PathBuf};

use crate::git::path::join_path;
use crate::host::{HostKind, quote_posix};

/// 有集成脚本的 shell
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegratedShell {
    Zsh,
    Bash,
    Fish,
}

/// 按可执行文件名认 shell（`/usr/bin/zsh`、`/opt/homebrew/bin/fish`、`bash-5.2` 都算）
pub fn detect(shell: &str) -> Option<IntegratedShell> {
    let base = shell.rsplit(['/', '\\']).next().unwrap_or(shell).trim_start_matches('-');
    let name = base.split(['-', '.']).next().unwrap_or(base);
    match name {
        "zsh" => Some(IntegratedShell::Zsh),
        "bash" => Some(IntegratedShell::Bash),
        "fish" => Some(IntegratedShell::Fish),
        _ => None,
    }
}

/// 宿主机上放集成脚本的目录：`<falcon 根>/shell-integration`
pub fn integration_dir(kind: HostKind, root: &str) -> String {
    join_path(kind, &[root, "shell-integration"])
}

pub const LAUNCHER_NAME: &str = "falcon-shell.sh";

/// 目录里要写的文件（相对路径、内容）。启动器另算（它里面写死了 shell 的路径）
pub fn script_files(shell: IntegratedShell) -> Vec<(&'static str, &'static str)> {
    match shell {
        IntegratedShell::Zsh => vec![("zsh/.zshenv", ZSHENV), ("zsh/.zshrc", ZSHRC)],
        IntegratedShell::Bash => vec![("bash/init.sh", BASH_INIT)],
        IntegratedShell::Fish => vec![("fish/init.fish", FISH_INIT)],
    }
}

/// 启动器：`exec` 用户的 shell，带上集成脚本。认不出的 shell 原样 exec——会话照样开得出来，
/// 只是没有集成
pub fn launcher_body(dir: &str, shell: &str) -> String {
    let q = quote_posix(shell);
    let exec = match detect(shell) {
        Some(IntegratedShell::Zsh) => {
            // ZDOTDIR 指到我们的目录；用户原来的 ZDOTDIR（没设 = 空）交给 .zshenv / .zshrc 还原
            format!(
                "FALCON_USER_ZDOTDIR=\"${{ZDOTDIR-}}\"; export FALCON_USER_ZDOTDIR\n\
                 ZDOTDIR={}; export ZDOTDIR\n\
                 exec {q}",
                quote_posix(&format!("{dir}/zsh")),
            )
        }
        Some(IntegratedShell::Bash) => format!("exec {q} --init-file {}", quote_posix(&format!("{dir}/bash/init.sh"))),
        Some(IntegratedShell::Fish) => {
            let source = format!("source {}", fish_quote(&format!("{dir}/fish/init.fish")));
            format!("exec {q} --init-command {}", quote_posix(&source))
        }
        None => format!("exec {q}"),
    };
    [
        "#!/bin/sh",
        "# falcon：AI 对话终端的启动器（docs/design/ai-assistant.md 4.1）。每次附着都会重写，别手改",
        "FALCON_SHELL_INTEGRATION=1; export FALCON_SHELL_INTEGRATION",
        &exec,
        "",
    ]
    .join("\n")
}

/// fish 的单引号字面量：只有 `\` 与 `'` 要转义
fn fish_quote(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// 本机：写脚本与启动器，返回启动器的绝对路径
pub fn write_local(data_dir: &Path, shell: &str) -> std::io::Result<PathBuf> {
    let dir = data_dir.join("shell-integration");
    let dir_str = dir.to_string_lossy().into_owned();
    if let Some(sh) = detect(shell) {
        for (rel, body) in script_files(sh) {
            let file = dir.join(rel);
            if let Some(parent) = file.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&file, body)?;
        }
    }
    std::fs::create_dir_all(&dir)?;
    let launcher = dir.join(LAUNCHER_NAME);
    std::fs::write(&launcher, launcher_body(&dir_str, shell))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(launcher)
}

/// 远端（POSIX）：建目录、写全部文件、给启动器加执行位的一条命令。退出码 = 链上第一个失败者
pub fn write_remote_command(dir: &str, shell: &str) -> String {
    let mut parts = vec![format!("d={}", quote_posix(dir)), "mkdir -p \"$d\"".to_string()];
    if let Some(sh) = detect(shell) {
        for (rel, body) in script_files(sh) {
            let sub = rel.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
            parts.push(format!("mkdir -p \"$d\"/{sub}"));
            parts.push(format!("printf %s {} > \"$d\"/{rel}", quote_posix(body)));
        }
    }
    parts.push(format!("printf %s {} > \"$d\"/{LAUNCHER_NAME}", quote_posix(&launcher_body(dir, shell))));
    parts.push(format!("chmod 755 \"$d\"/{LAUNCHER_NAME}"));
    // 第一句是赋值，后面的用 && 串：任何一步失败整条就失败，调用方退回普通 shell
    let (first, rest) = parts.split_first().expect("至少有赋值与 mkdir");
    format!("{first}; {}", rest.join(" && "))
}

/// 远端启动器的绝对路径
pub fn remote_launcher_path(dir: &str) -> String {
    format!("{dir}/{LAUNCHER_NAME}")
}

// ---------------------------------------------------------------- 脚本

/// zsh 先读 `$ZDOTDIR/.zshenv`。这里替用户读他自己的那份；他的 .zshenv 可能改 ZDOTDIR，
/// 改了就记下来，后面找 .zshrc 用新的
const ZSHENV: &str = r#"# falcon：AI 对话终端的 shell integration（docs/design/ai-assistant.md 4.1）。每次附着都会重写，别手改
__falcon_zdotdir="$ZDOTDIR"
ZDOTDIR="${FALCON_USER_ZDOTDIR:-$HOME}"
if [[ -f "$ZDOTDIR/.zshenv" ]]; then
  builtin source "$ZDOTDIR/.zshenv"
fi
if [[ "$ZDOTDIR" != "${FALCON_USER_ZDOTDIR:-$HOME}" ]]; then
  FALCON_USER_ZDOTDIR="$ZDOTDIR"
fi
ZDOTDIR="$__falcon_zdotdir"
unset __falcon_zdotdir
"#;

/// 交互式 zsh 接着读 `$ZDOTDIR/.zshrc`：替用户读他的，再把 ZDOTDIR 还给他（之后开的子 zsh、
/// compinit 的缓存都该用他的目录），最后挂钩子
const ZSHRC: &str = r#"# falcon：AI 对话终端的 shell integration（docs/design/ai-assistant.md 4.1）。每次附着都会重写，别手改
ZDOTDIR="${FALCON_USER_ZDOTDIR:-$HOME}"
if [[ -f "$ZDOTDIR/.zshrc" ]]; then
  builtin source "$ZDOTDIR/.zshrc"
fi
if [[ -z "$FALCON_USER_ZDOTDIR" ]]; then
  builtin unset ZDOTDIR
fi
builtin unset FALCON_USER_ZDOTDIR

# 每个提示符前：D;上一条的退出码（空回车也打，服务端只在运行中认它）、A、OSC 7 当前目录。
# 排在 precmd 钩子最前面：排在主题后面 $? 早就被改掉了
__falcon_precmd() {
  local ret=$?
  builtin printf '\e]133;D;%s\a\e]133;A\a\e]7;file://%s%s\a' "$ret" "$HOST" "$PWD"
}
# 命令开跑前：命令行（OSC 633;E，命令块的标题；`\` 与 `;`、控制字符转义成 \xNN）、C。
# 排在 preexec 钩子最后，紧挨着命令
__falcon_preexec() {
  local c="$1"
  c=${c//\\/\\\\}
  c=${c//;/\\x3b}
  c=${c//$'\n'/\\x0a}
  c=${c//$'\a'/\\x07}
  c=${c//$'\e'/\\x1b}
  builtin printf '\e]633;E;%s\a\e]133;C\a' "$c"
}
typeset -ga precmd_functions preexec_functions
precmd_functions=(__falcon_precmd ${precmd_functions:#__falcon_precmd})
preexec_functions=(${preexec_functions:#__falcon_preexec} __falcon_preexec)
"#;

/// `bash --init-file` 换掉了 ~/.bashrc：先替用户读它，再挂钩子
const BASH_INIT: &str = r#"# falcon：AI 对话终端的 shell integration（docs/design/ai-assistant.md 4.1）。每次附着都会重写，别手改
if [ -f ~/.bashrc ]; then
  . ~/.bashrc
fi

# 提示符前：D;上一条的退出码（空回车也打，服务端只在运行中认它）、A、OSC 7。排在 PROMPT_COMMAND
# 最前面才拿得到真的 $?；return 原值，后面的主题照样看得到
__falcon_prompt() {
  local ret=$?
  __falcon_armed=
  printf '\033]133;D;%s\007\033]133;A\007\033]7;file://%s%s\007' "$ret" "$HOSTNAME" "$PWD"
  return $ret
}

if [ "${BASH_VERSINFO[0]}" -gt 5 ] || { [ "${BASH_VERSINFO[0]}" -eq 5 ] && [ "${BASH_VERSINFO[1]}" -ge 1 ]; }; then
  # 5.1 起 PROMPT_COMMAND 可以是数组，挨个执行
  PROMPT_COMMAND=(__falcon_prompt "${PROMPT_COMMAND[@]}")
else
  PROMPT_COMMAND="__falcon_prompt${PROMPT_COMMAND:+; $PROMPT_COMMAND}"
fi

# 刚读进来的那条命令行（命令块的标题，OSC 633;E）。从历史里取：读命令时已经进了历史。
# 用 `history 1` 不用 `fc -ln -1`：bash 3.2 里后者在 DEBUG trap 中报越界。剥序号要一段一段来
# （空格、数字、空格），不然以数字开头的命令会被一起吃掉。
# HISTCONTROL=ignorespace 下以空格开头的命令不进历史，取到的会是上一条——标题错，不影响别的
__falcon_cmdline() {
  local c
  c=$(HISTTIMEFORMAT= builtin history 1 2>/dev/null)
  c="${c#"${c%%[![:space:]]*}"}"
  c="${c#"${c%%[![:digit:]]*}"}"
  c="${c#"${c%%[![:space:]]*}"}"
  c=${c//\\/\\\\}
  c=${c//;/\\x3b}
  c=${c//$'\n'/\\x0a}
  c=${c//$'\a'/\\x07}
  c=${c//$'\e'/\\x1b}
  printf '\033]633;E;%s\007' "$c"
}

if [ "${BASH_VERSINFO[0]}" -gt 4 ] || { [ "${BASH_VERSINFO[0]}" -eq 4 ] && [ "${BASH_VERSINFO[1]}" -ge 4 ]; }; then
  # PS0：读完命令、执行之前展开（命令替换在子 shell 里跑，历史照样看得见）
  PS0="${PS0-}"'$(__falcon_cmdline)'$'\033]133;C\007'
elif [ -z "$(trap -p DEBUG)" ]; then
  # 老 bash 没有 PS0：DEBUG trap 在每条简单命令前触发。提示符的事做完才上膛
  # （PROMPT_COMMAND 末尾那句），下一条就是用户的命令
  __falcon_armed=
  __falcon_debug() {
    [ -n "$__falcon_armed" ] || return 0
    case "$BASH_COMMAND" in __falcon_prompt*) return 0 ;; esac
    __falcon_armed=
    __falcon_cmdline
    printf '\033]133;C\007'
  }
  trap '__falcon_debug' DEBUG
  PROMPT_COMMAND="$PROMPT_COMMAND; __falcon_armed=1"
fi
"#;

/// fish 的事件钩子。fish 4 自己也会打 OSC 133，重复的标记无害
const FISH_INIT: &str = r#"# falcon：AI 对话终端的 shell integration（docs/design/ai-assistant.md 4.1）。每次附着都会重写，别手改
function __falcon_preexec --on-event fish_preexec
    set -l c (string replace -a -- '\\' '\\\\' $argv[1] | string replace -a -- ';' '\\x3b')
    printf '\e]633;E;%s\a\e]133;C\a' (string join '\\x0a' -- $c)
end
function __falcon_postexec --on-event fish_postexec
    printf '\e]133;D;%s\a' $status
end
function __falcon_prompt --on-event fish_prompt
    printf '\e]133;A\a\e]7;file://%s%s\a' $hostname "$PWD"
end
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_shells_by_basename() {
        assert_eq!(detect("/bin/zsh"), Some(IntegratedShell::Zsh));
        assert_eq!(detect("/opt/homebrew/bin/bash"), Some(IntegratedShell::Bash));
        assert_eq!(detect("bash-5.2"), Some(IntegratedShell::Bash));
        assert_eq!(detect("-zsh"), Some(IntegratedShell::Zsh));
        assert_eq!(detect("/usr/local/bin/fish"), Some(IntegratedShell::Fish));
        assert_eq!(detect("/bin/sh"), None);
        assert_eq!(detect("/usr/bin/dash"), None);
        assert_eq!(detect("C:\\Program Files\\PowerShell\\7\\pwsh.exe"), None);
    }

    /// 绝不读 $SHELL（理由同 agent.rs），shell 的绝对路径写死在启动器里
    #[test]
    fn launcher_execs_the_baked_in_shell() {
        let zsh = launcher_body("/h/.falcon/shell-integration", "/bin/zsh");
        assert!(zsh.starts_with("#!/bin/sh\n"));
        assert!(!zsh.contains("$SHELL"));
        assert!(zsh.contains("ZDOTDIR='/h/.falcon/shell-integration/zsh'; export ZDOTDIR"));
        assert!(zsh.contains("FALCON_USER_ZDOTDIR=\"${ZDOTDIR-}\""));
        assert!(zsh.trim_end().ends_with("exec '/bin/zsh'"));

        let bash = launcher_body("/h/x", "/usr/bin/bash");
        assert!(bash.contains("exec '/usr/bin/bash' --init-file '/h/x/bash/init.sh'"));

        let fish = launcher_body("/h/it's", "/usr/bin/fish");
        let arg = fish.trim_end().strip_prefix(fish.trim_end().split("--init-command ").next().unwrap()).unwrap();
        let arg = arg.strip_prefix("--init-command ").unwrap();
        // sh 一层、fish 一层：sh 剥掉外层之后，fish 拿到的是它自己的单引号字面量
        let out = std::process::Command::new("/bin/sh").arg("-c").arg(format!("printf %s {arg}")).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), r"source '/h/it\'s/fish/init.fish'");

        let sh = launcher_body("/h/x", "/bin/dash");
        assert!(sh.trim_end().ends_with("exec '/bin/dash'"));
        assert!(!sh.contains("ZDOTDIR"));
    }

    #[test]
    fn scripts_emit_the_marks_the_scanner_reads() {
        for sh in [IntegratedShell::Zsh, IntegratedShell::Bash, IntegratedShell::Fish] {
            let all: String = script_files(sh).into_iter().map(|(_, b)| b).collect();
            assert!(all.contains("133;A"), "{sh:?}");
            assert!(all.contains("133;C"), "{sh:?}");
            assert!(all.contains("133;D;"), "{sh:?}");
            assert!(all.contains("633;E;"), "{sh:?}");
            assert!(all.contains("]7;file://"), "{sh:?}");
        }
    }

    #[test]
    fn remote_command_writes_everything_and_chmods_the_launcher() {
        let cmd = write_remote_command("/home/u/.falcon/shell-integration", "/usr/bin/zsh");
        assert!(cmd.starts_with("d='/home/u/.falcon/shell-integration'; mkdir -p \"$d\" && "));
        assert!(cmd.contains("> \"$d\"/zsh/.zshenv"));
        assert!(cmd.contains("> \"$d\"/zsh/.zshrc"));
        assert!(cmd.contains("> \"$d\"/falcon-shell.sh && chmod 755 \"$d\"/falcon-shell.sh"));
        // 不认识的 shell 只写启动器
        let plain = write_remote_command("/d", "/bin/tcsh");
        assert!(!plain.contains(".zshrc") && !plain.contains("init.sh"));
        assert!(plain.contains("falcon-shell.sh"));
    }

    /// 各 shell 自己做一遍语法检查（`-n` 只解析不执行）
    #[cfg(unix)]
    #[test]
    fn scripts_parse() {
        let dir = tempfile::tempdir().unwrap();
        for (shell, sh) in [("zsh", IntegratedShell::Zsh), ("bash", IntegratedShell::Bash), ("fish", IntegratedShell::Fish)] {
            let Some(bin) = which(shell) else { continue };
            for (rel, body) in script_files(sh) {
                let file = dir.path().join(rel.replace('/', "_"));
                std::fs::write(&file, body).unwrap();
                let out = std::process::Command::new(&bin).arg("-n").arg(&file).output().unwrap();
                assert!(out.status.success(), "{rel}: {}", String::from_utf8_lossy(&out.stderr));
            }
        }
    }

    #[cfg(unix)]
    fn which(name: &str) -> Option<PathBuf> {
        ["/bin", "/usr/bin", "/usr/local/bin", "/opt/homebrew/bin"].iter().map(|d| Path::new(d).join(name)).find(|p| p.exists())
    }

    /// 真跑一遍：本机写出来的启动器在 PTY 上起交互 shell，命令前后打出 C / D;0，提示符前打 A 与
    /// OSC 7。HOME 指到空目录，不读用户真实的 rc
    #[cfg(unix)]
    #[test]
    fn launcher_on_a_pty_emits_marks() {
        use portable_pty::{CommandBuilder, PtySize, native_pty_system};
        use std::io::{Read as _, Write as _};
        use std::time::{Duration, Instant};
        let dir = tempfile::tempdir().unwrap();
        for shell in ["zsh", "bash"] {
            let Some(bin) = which(shell) else { continue };
            let home = dir.path().join(format!("home-{shell}"));
            std::fs::create_dir_all(&home).unwrap();
            let launcher = write_local(&dir.path().join(format!("data-{shell}")), &bin.to_string_lossy()).unwrap();
            let pair = native_pty_system().openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 }).unwrap();
            let mut cmd = CommandBuilder::new(&launcher);
            cmd.env_clear();
            cmd.env("HOME", &home);
            cmd.env("PATH", "/usr/bin:/bin");
            cmd.env("TERM", "xterm-256color");
            cmd.cwd(&home);
            let mut child = pair.slave.spawn_command(cmd).unwrap();
            drop(pair.slave);
            let mut reader = pair.master.try_clone_reader().unwrap();
            let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                while let Ok(n) = reader.read(&mut buf) {
                    if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            });
            let mut writer = pair.master.take_writer().unwrap();
            let mut seen = String::new();
            let wait_for = |needle: &str, seen: &mut String| {
                let deadline = Instant::now() + Duration::from_secs(10);
                while !seen.contains(needle) && Instant::now() < deadline {
                    if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(100)) {
                        seen.push_str(&String::from_utf8_lossy(&chunk));
                    }
                }
                assert!(seen.contains(needle), "{shell}: 等不到 {needle:?}，收到 {seen:?}");
            };
            wait_for("\x1b]133;A", &mut seen);
            // 第一个提示符前也有一个 D（每个提示符前都打，见模块注释）：只看命令之后的
            seen.clear();
            writer.write_all(b"echo falcon-$((40+2))\r").unwrap();
            wait_for("\x1b]133;C", &mut seen);
            // 命令行（633;E）在 C 之前
            assert!(seen.contains("\x1b]633;E;echo falcon-$((40+2))\x07"), "{shell}: 没有命令行：{seen:?}");
            let after_c = |seen: &str| seen.split_once("\x1b]133;C").map(|(_, t)| t.to_string()).unwrap_or_default();
            wait_for("\x1b]133;D;0", &mut seen);
            let out = after_c(&seen);
            let d = out.find("\x1b]133;D;0").unwrap_or_else(|| panic!("{shell}: C 之后没有 D：{seen:?}"));
            assert!(out[..d].contains("falcon-42"), "{shell}: C…D 之间没有输出：{seen:?}");
            assert!(seen.contains("\x1b]7;file://"), "{shell}: 没有 OSC 7");
            seen.clear();
            writer.write_all(b"false\r").unwrap();
            wait_for("\x1b]133;D;1", &mut seen);
            writer.write_all(b"exit\r").unwrap();
            let _ = child.wait();
        }
    }
}
