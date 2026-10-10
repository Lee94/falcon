//! 宿主机可用 shell 侦测。移植自 `packages/server/src/shells.ts`。
//!
//! 项目表单里的 shell 覆盖从"盲填路径"升级为"从侦测结果里选"：一次往返扫一遍
//! 常见 shell，存在的以绝对路径返回。`default` 与会话创建路径的取值严格一致——
//! POSIX 是探测到的登录 shell（见 POSIX_PROBE），Windows 一律 PowerShell
//! （见 WINDOWS_PROBE_SCRIPT / defaultLocalShell）——选"默认"就等于不设覆盖。
//!
//! 留到 S3 的函数：`detectShells`（要走执行器）。它的纯逻辑——"探测失败就只剩默认项、
//! 成功就解析 stdout 且无视退出码"——拆成了 [`shells_from_probe`]，S3 的 detect_shells
//! 只需 `shells_from_probe(kind, default, exec(shells_probe(kind)).await.ok().as_ref())`。

use std::collections::HashSet;
use std::sync::LazyLock;

use falcon_proto::ShellsInfo;

use crate::exec::ExecResult;
use crate::zellij::host::{HostKind, encode_powershell};

/// 常见 POSIX shell。bash/zsh/fish 覆盖绝大多数；pwsh 是跨平台 PowerShell
const POSIX_CANDIDATES: [&str; 9] = ["bash", "zsh", "fish", "sh", "dash", "ksh", "tcsh", "nu", "pwsh"];

/// Windows 候选。powershell 排最前——它是 Windows 宿主机的默认；
/// bash.exe 通常来自 Git for Windows，nu/pwsh 是用户自装的。
const WINDOWS_CANDIDATES: [&str; 5] = ["powershell.exe", "pwsh.exe", "cmd.exe", "nu.exe", "bash.exe"];

/// 用分号串接的 `command -v` 而不是 for 循环：这条命令由远端登录 shell 执行，
/// `;` 与 `command -v` 在 sh/bash/zsh/fish 里语义一致，for 的语法则各不相同。
/// 缺某个 shell 时该条 command -v 静默失败，所以**不看退出码**，只解析 stdout。
pub static POSIX_SHELLS_PROBE: LazyLock<String> = LazyLock::new(|| {
    let probes: Vec<String> = POSIX_CANDIDATES.iter().map(|s| format!("command -v {s}")).collect();
    probes.join("; ") + "; true"
});

/// -CommandType Application：只要真实可执行文件，别把别名/函数当 shell 报出来
pub static WINDOWS_SHELLS_SCRIPT: LazyLock<String> = LazyLock::new(|| {
    let names: Vec<String> = WINDOWS_CANDIDATES.iter().map(|s| format!("'{s}'")).collect();
    [
        format!("foreach ($n in {}) {{", names.join(",")),
        "$c = Get-Command $n -CommandType Application -ErrorAction SilentlyContinue;".to_string(),
        "if ($c) { @($c)[0].Source }".to_string(),
        "}".to_string(),
    ]
    .join(" ")
});

pub static WINDOWS_SHELLS_PROBE: LazyLock<String> = LazyLock::new(|| encode_powershell(&WINDOWS_SHELLS_SCRIPT));

/// 按宿主机类型取探测命令（TS 里内联在 detectShells 开头）
pub fn shells_probe(kind: HostKind) -> &'static str {
    if kind == HostKind::Windows { &WINDOWS_SHELLS_PROBE } else { &POSIX_SHELLS_PROBE }
}

/// 解析探测输出。POSIX 只认绝对路径行——`command -v` 对别名/函数可能吐出
/// 定义体而非路径；Windows 的 Get-Command .Source 恒为绝对路径，直接收。
pub fn parse_shell_list(kind: HostKind, stdout: &str) -> Vec<String> {
    let lines = stdout.split('\n').map(js_trim).filter(|l| !l.is_empty());
    if kind == HostKind::Windows {
        // /^([A-Za-z]:\\|\\\\)/
        return lines
            .filter(|l| {
                let b = l.as_bytes();
                (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\') || l.starts_with("\\\\")
            })
            .map(str::to_string)
            .collect();
    }
    lines.filter(|l| l.starts_with('/')).map(str::to_string).collect()
}

/// `s.split(/[\\/]/).pop() ?? s`：最后一个 `\` 或 `/` 之后的部分
fn base(s: &str) -> &str {
    s.rsplit(['\\', '/']).next().unwrap_or(s)
}

/// 把默认 shell 归入侦测结果并排到最前。
///
/// Windows 上默认值可能是裸名字（本地的 defaultLocalShell 返回 "powershell.exe"）
/// 或与侦测结果大小写不同：按 basename 不区分大小写匹配，命中就用侦测到的
/// 绝对路径当默认——下拉里不该同时出现 "powershell.exe" 和它的绝对路径。
pub fn merge_shells<S: AsRef<str>>(kind: HostKind, default_shell: &str, found: &[S]) -> ShellsInfo {
    let norm = |s: &str| if kind == HostKind::Windows { s.to_lowercase() } else { s.to_string() };

    let mut def = default_shell.to_string();
    if kind == HostKind::Windows {
        let want = norm(base(default_shell));
        if let Some(hit) = found.iter().map(AsRef::as_ref).find(|f| norm(base(f)) == want) {
            def = hit.to_string();
        }
    }

    let mut seen: HashSet<String> = HashSet::from([norm(&def)]);
    let mut shells = vec![def.clone()];
    for f in found.iter().map(AsRef::as_ref) {
        if seen.insert(norm(f)) {
            shells.push(f.to_string());
        }
    }
    ShellsInfo { kind, default: def, shells }
}

/// 侦测候选之外也常见的交互 shell。is_shell_command 的判定集合要比表单候选宽：
/// 表单漏列一个 shell 只是下拉里少一项，这里漏一个则每次关 tab 都误弹确认。
const EXTRA_SHELLS: [&str; 7] = ["csh", "mksh", "ash", "elvish", "xonsh", "nushell", "powershell"];

/// is_shell_command 的 `norm`：basename、去掉登录 shell 的一个 `-` 前缀、去掉 `.exe`
/// 后缀（只认 ASCII 大小写，JS 无 `u` 的 `/i`）、转小写
fn shell_norm(s: &str) -> String {
    let b = base(s);
    let b = b.strip_prefix('-').unwrap_or(b);
    let b = match b.len().checked_sub(4).and_then(|i| b.get(i..).map(|tail| (i, tail))) {
        Some((i, tail)) if tail.eq_ignore_ascii_case(".exe") => &b[..i],
        _ => b,
    };
    b.to_lowercase()
}

static KNOWN_SHELLS: LazyLock<HashSet<String>> = LazyLock::new(|| {
    POSIX_CANDIDATES.iter().chain(&WINDOWS_CANDIDATES).chain(&EXTRA_SHELLS).map(|s| shell_norm(s)).collect()
});

/// `/\.exe$/i`
fn ends_with_exe(s: &str) -> bool {
    s.len() >= 4 && s.get(s.len() - 4..).is_some_and(|t| t.eq_ignore_ascii_case(".exe"))
}

/// `/^--(?:login|interactive)$|^-[li]+$/`
fn is_login_or_interactive_flag(a: &str) -> bool {
    a == "--login"
        || a == "--interactive"
        || a.strip_prefix('-').is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b == b'l' || b == b'i'))
}

/// 判断一条前台命令是不是"就是 shell 自己在等输入"——关 tab 要不要拦的依据。
///
/// basename 是已知 shell（或该会话配置的 shell）才算空闲：带参数的如
/// `bash deploy.sh` 是在跑东西，得拦；只跟登录 / 交互标志的 `/bin/zsh -l` 不是
/// ——agent 会话的启动脚本 exec 出来就长这样（sessions/agent.rs），CLI 退出后
/// 落回的就是它。登录 shell 的 `-zsh` 前缀与 Windows 的 `.exe` 后缀都要归一化掉。
/// 解析不出名字时按空闲放行——这套侦测是道保险，宁可放过不可把关 tab 变成每次两步。
pub fn is_shell_command(command: &str, session_shell: Option<&str>) -> bool {
    let trimmed = js_trim(command);
    if trimmed.is_empty() {
        return true;
    }
    let is_shell = |s: &str| {
        let b = shell_norm(s);
        if b.is_empty() {
            return true;
        }
        if KNOWN_SHELLS.contains(&b) {
            return true;
        }
        session_shell.is_some_and(|sh| shell_norm(sh) == b)
    };
    // Windows 全路径可能含空格（C:\Program Files\...\pwsh.exe），按词拆会误判成
    // "带参数"。整串以 .exe 结尾时先按整条路径试——这只可能放行 shell，
    // 不会把 `less /bin/bash` 这种真在跑的命令看漏。
    if ends_with_exe(trimmed) && is_shell(trimmed) {
        return true;
    }
    // trimmed 非空且没有首尾空白，按空白游程切 = 按单个空白切再丢空段
    let mut tokens = trimmed.split(is_js_whitespace).filter(|t| !t.is_empty());
    let Some(first) = tokens.next() else {
        return false;
    };
    if !is_shell(first) {
        return false;
    }
    // `-l` / `--login` / `-i` / 组合的 `-il` 都只是"这个 shell 是登录 / 交互的"，
    // 不是在跑脚本
    tokens.all(is_login_or_interactive_flag)
}

/// detectShells 的纯逻辑：探测命令没跑起来（`None`，对应 TS 里 exec reject 被 catch 成
/// null）就只剩默认项；跑起来了就解析 stdout，**无视退出码**（缺 shell 时 command -v
/// 本来就会失败）。探测失败不抛：至少还有默认项可选。
pub fn shells_from_probe(kind: HostKind, default_shell: &str, res: Option<&ExecResult>) -> ShellsInfo {
    let found = res.map(|r| parse_shell_list(kind, &r.stdout)).unwrap_or_default();
    merge_shells(kind, default_shell, &found)
}

// ---------------- JS 语义的私有小工具 ----------------
//
// 与 zellij/command.rs 里的同名函数是同一份（falcon-core 的 js.rs 是 crate 私有的，
// falcon-server 里还没有共用位置），先各留一份。

/// ECMAScript 的 WhiteSpace + LineTerminator：`trim()` 与正则 `\s` 用的就是这一套
/// （含 U+FEFF、不含 U+0085，与 Rust 的 `char::is_whitespace` 恰好相反）。
fn is_js_whitespace(c: char) -> bool {
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
}

/// `String.prototype.trim`
fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_whitespace)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    const PREFIX: &str = "powershell -NoProfile -NonInteractive -EncodedCommand ";

    // ---- shells probe ----

    /// POSIX 探测不用 for：分号串接在 sh/bash/zsh/fish 里语义一致
    #[test]
    fn posix_probe_chains_with_semicolons_not_for() {
        assert!(!POSIX_SHELLS_PROBE.contains("for "));
        assert!(POSIX_SHELLS_PROBE.contains("command -v bash; command -v zsh"));
        // 缺 shell 时最后一条 command -v 失败，true 兜住整体退出码
        assert!(POSIX_SHELLS_PROBE.ends_with("; true"));
    }

    /// Windows 探测走 EncodedCommand，不受远端 DefaultShell 影响
    #[test]
    fn windows_probe_uses_encoded_command() {
        assert!(WINDOWS_SHELLS_PROBE.starts_with(PREFIX));
        let bytes = base64::engine::general_purpose::STANDARD.decode(&WINDOWS_SHELLS_PROBE[PREFIX.len()..]).unwrap();
        let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let script = String::from_utf16(&units).unwrap();
        assert!(script.contains("Get-Command $n -CommandType Application"));
        // powershell 排最前 = Windows 默认
        assert!(script.contains("'powershell.exe','pwsh.exe'"));
    }

    // ---- parseShellList ----

    /// POSIX 只认绝对路径行（command -v 对别名可能吐定义体）
    #[test]
    fn parse_shell_list_posix_only_absolute_paths() {
        let out = "/bin/bash\n/usr/bin/zsh\nalias fish='fish -l'\n\n/bin/sh\n";
        assert_eq!(parse_shell_list(HostKind::Posix, out), ["/bin/bash", "/usr/bin/zsh", "/bin/sh"]);
    }

    /// Windows 只认盘符 / UNC 路径行
    #[test]
    fn parse_shell_list_windows_only_drive_or_unc_paths() {
        let out = "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe\r\n\
                   C:\\Program Files\\PowerShell\\7\\pwsh.exe\r\n\
                   warning: something\r\n";
        assert_eq!(
            parse_shell_list(HostKind::Windows, out),
            ["C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe", "C:\\Program Files\\PowerShell\\7\\pwsh.exe"]
        );
    }

    // ---- mergeShells ----

    /// POSIX：默认 shell 排最前并去重
    #[test]
    fn merge_shells_posix_default_first_and_deduped() {
        let info = merge_shells(HostKind::Posix, "/usr/bin/zsh", &["/bin/bash", "/usr/bin/zsh", "/bin/sh"]);
        assert_eq!(info.default, "/usr/bin/zsh");
        assert_eq!(info.shells, ["/usr/bin/zsh", "/bin/bash", "/bin/sh"]);
    }

    /// Windows：裸名字默认（powershell.exe）按 basename 归到侦测出的绝对路径
    #[test]
    fn merge_shells_windows_bare_default_maps_to_detected_path() {
        let ps = "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe";
        let info = merge_shells(HostKind::Windows, "powershell.exe", &[ps, "C:\\Windows\\System32\\cmd.exe"]);
        assert_eq!(info.default, ps);
        assert_eq!(info.shells, [ps, "C:\\Windows\\System32\\cmd.exe"]);
    }

    /// Windows：去重不区分大小写
    #[test]
    fn merge_shells_windows_dedup_is_case_insensitive() {
        let info = merge_shells(HostKind::Windows, "C:\\WINDOWS\\system32\\cmd.exe", &["C:\\Windows\\System32\\cmd.exe"]);
        assert_eq!(info.shells.len(), 1);
    }

    /// 侦测结果为空时仍有默认项
    #[test]
    fn merge_shells_keeps_default_when_nothing_found() {
        let info = merge_shells::<&str>(HostKind::Posix, "/bin/sh", &[]);
        assert_eq!(info.shells, ["/bin/sh"]);
    }

    // ---- isShellCommand ----

    /// 裸 shell 名与绝对路径都算空闲
    #[test]
    fn is_shell_command_bare_names_and_absolute_paths_are_idle() {
        assert!(is_shell_command("zsh", None));
        assert!(is_shell_command("/bin/bash", None));
        assert!(is_shell_command("fish", None));
    }

    /// 登录 shell 的 - 前缀与 Windows 的 .exe 后缀要归一化
    #[test]
    fn is_shell_command_normalizes_login_dash_and_exe_suffix() {
        assert!(is_shell_command("-zsh", None));
        assert!(is_shell_command("powershell.exe", None));
        assert!(is_shell_command("C:\\Program Files\\PowerShell\\7\\pwsh.exe", None));
    }

    /// 前台真在跑东西时不算空闲
    #[test]
    fn is_shell_command_running_programs_are_not_idle() {
        assert!(!is_shell_command("vim main.rs", None));
        assert!(!is_shell_command("sleep 300", None));
        assert!(!is_shell_command("claude", None));
    }

    /// shell 带参数是在跑脚本，不是在等输入
    #[test]
    fn is_shell_command_shell_with_args_is_running_a_script() {
        assert!(!is_shell_command("bash deploy.sh", None));
        assert!(!is_shell_command("zsh -c 'make'", None));
    }

    /// 只跟登录 / 交互标志仍是 shell 在等输入（agent 会话 exec 出来的样子）
    #[test]
    fn is_shell_command_login_or_interactive_flags_only_is_idle() {
        assert!(is_shell_command("/bin/zsh -l", None));
        assert!(is_shell_command("bash --login", None));
        assert!(is_shell_command("/bin/zsh -il", None));
    }

    /// 会话配置的非常见 shell 也算空闲
    #[test]
    fn is_shell_command_session_configured_shell_is_idle() {
        assert!(!is_shell_command("myshell", None));
        assert!(is_shell_command("myshell", Some("/opt/bin/myshell")));
    }

    /// 解析不出名字按空闲放行——侦测是保险，不该拦住关 tab
    #[test]
    fn is_shell_command_unparseable_is_idle() {
        assert!(is_shell_command("", None));
        assert!(is_shell_command("   ", None));
    }

    // ---- detectShells（纯逻辑部分 shells_from_probe） ----

    /// 探测命令失败不抛，退回只有默认项
    #[test]
    fn detect_shells_probe_failure_falls_back_to_default_only() {
        let info = shells_from_probe(HostKind::Posix, "/usr/bin/zsh", None);
        assert_eq!(
            info,
            ShellsInfo { kind: HostKind::Posix, default: "/usr/bin/zsh".into(), shells: vec!["/usr/bin/zsh".into()] }
        );
    }

    /// 正常路径：解析 stdout、无视退出码
    #[test]
    fn detect_shells_parses_stdout_ignoring_exit_code() {
        let res = ExecResult { code: Some(1), stdout: "/bin/bash\n/usr/bin/zsh\n".into(), stderr: String::new() };
        let info = shells_from_probe(HostKind::Posix, "/usr/bin/zsh", Some(&res));
        assert_eq!(info.shells, ["/usr/bin/zsh", "/bin/bash"]);
    }

    // ---- 以下不在 shells.test.ts 里：逐字节对照 TS 产出与几处 JS 语义 ----

    #[test]
    fn extra_probes_match_ts_byte_for_byte() {
        assert_eq!(
            *POSIX_SHELLS_PROBE,
            "command -v bash; command -v zsh; command -v fish; command -v sh; command -v dash; \
             command -v ksh; command -v tcsh; command -v nu; command -v pwsh; true"
        );
        assert_eq!(
            *WINDOWS_SHELLS_SCRIPT,
            "foreach ($n in 'powershell.exe','pwsh.exe','cmd.exe','nu.exe','bash.exe') { \
             $c = Get-Command $n -CommandType Application -ErrorAction SilentlyContinue; \
             if ($c) { @($c)[0].Source } }"
        );
        assert_eq!(shells_probe(HostKind::Posix), POSIX_SHELLS_PROBE.as_str());
        assert_eq!(shells_probe(HostKind::Windows), WINDOWS_SHELLS_PROBE.as_str());
    }

    #[test]
    fn extra_is_shell_command_edges() {
        assert!(is_shell_command("CMD.EXE", None));
        assert!(!is_shell_command("bash -", None), "/^-[li]+$/ 不认光杆 -");
        assert!(!is_shell_command("bash -x", None));
        assert!(is_shell_command("bash --interactive -li", None));
        assert!(is_shell_command("\u{feff}zsh\u{feff}", None), "JS 的 trim 去 U+FEFF");
        assert!(!is_shell_command("zsh\u{85}-c", None), "JS 的 \\s 不含 U+0085，整串是一个词");
    }
}
