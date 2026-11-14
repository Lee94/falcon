//! Windows 计划任务（schtasks）的服务注册：任务 XML 渲染、命令行引号 / 拆词、已装任务里
//! 程序参数的取回。服务端（`falcon service`）与原生客户端（托管本机服务）共用这一份。
//!
//! schtasks 对应 launchd / systemd 的用户级守护语义：`<LogonTrigger>` 登录自启、
//! `<RestartOnFailure>` 崩溃拉起、`<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>` 不限时长
//! （缺省 3 天会被杀）。任务 XML 里的 `<Command>` 与 `<Arguments>` 就是以前 launchd plist
//! 的 `ProgramArguments`——重装时从它读回原启动参数。
//!
//! `schtasks /Create /XML` 只认 UTF-16 的 XML 文件（[`utf16le_bytes`]）；`schtasks /Query /XML`
//! 的输出编码不定（BOM、UTF-16、本地代码页都见过），用 [`decode_task_xml`] 兜底。

/// 计划任务名的前缀。任务库不区分用户——名在机器上全局唯一，实际名字走 [`schtasks_name`]
pub const SCHTASKS_NAME: &str = "falcon";

/// 本用户的计划任务名：`falcon-<用户名>`。不加用户名的裸 `falcon` 在多人机器上会撞：
/// 第二个用户 /Create /F 要么覆盖第一个用户的任务、要么权限不够直接失败
pub fn schtasks_name() -> String {
    schtasks_name_for(&std::env::var("USERNAME").unwrap_or_default())
}

/// 任务名规则：ASCII 用户名原样；清掉非法字符后有损失（如纯中文名）就补一小段
/// 用户名哈希防撞名；USERNAME 取不到退回裸 `falcon`（单用户机器不受影响）
fn schtasks_name_for(user: &str) -> String {
    let cleaned: String = user.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')).collect();
    if cleaned == user {
        if user.is_empty() {
            SCHTASKS_NAME.to_string()
        } else {
            format!("{SCHTASKS_NAME}-{cleaned}")
        }
    } else if cleaned.is_empty() {
        format!("{SCHTASKS_NAME}-{:06x}", name_hash(user))
    } else {
        format!("{SCHTASKS_NAME}-{cleaned}-{:06x}", name_hash(user))
    }
}

/// FNV-1a 的低 24 位：非 ASCII 用户名的防撞名后缀
fn name_hash(s: &str) -> u32 {
    let mut h: u32 = 0x811c9dc5;
    for b in s.as_bytes() {
        h = (h ^ *b as u32).wrapping_mul(0x01000193);
    }
    h & 0xFF_FFFF
}

/// XML 元素文本的转义：只转 `&` `<` `>`（引号都在元素内容里，不转）
pub fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// [`xml_escape`] 的逆操作，`&amp;` 最后还原
pub fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// 单参数按 CommandLineToArgvW 规则加引号：含空格 / 制表符 / 引号（或为空）才加，
/// 引号与收尾前的反斜杠翻倍
pub fn win_quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.chars().any(|c| c == ' ' || c == '\t' || c == '"') {
        return arg.to_string();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                backslashes = 0;
                out.push('"');
            }
            _ => {
                out.push_str(&"\\".repeat(backslashes));
                backslashes = 0;
                out.push(c);
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

/// [`win_quote`] 的逆操作（CommandLineToArgvW 的拆词）：引号外空白切词，引号内不切的字面义
/// 2n 个反斜杠 + 引号 → n 个反斜杠、引号开合；2n+1 个 + 引号 → n 个反斜杠 + 字面引号
pub fn win_split(line: &str) -> Vec<String> {
    let chars: Vec<char> = line.chars().collect();
    let mut args = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        while i < chars.len() && (chars[i] == ' ' || chars[i] == '\t') {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        let mut arg = String::new();
        let mut in_quotes = false;
        while i < chars.len() {
            let mut backslashes = 0;
            while i < chars.len() && chars[i] == '\\' {
                backslashes += 1;
                i += 1;
            }
            if i >= chars.len() {
                arg.push_str(&"\\".repeat(backslashes));
                break;
            }
            if chars[i] == '"' {
                arg.push_str(&"\\".repeat(backslashes / 2));
                if backslashes % 2 == 1 {
                    arg.push('"');
                } else {
                    in_quotes = !in_quotes;
                }
                i += 1;
                continue;
            }
            arg.push_str(&"\\".repeat(backslashes));
            let c = chars[i];
            if (c == ' ' || c == '\t') && !in_quotes {
                break;
            }
            arg.push(c);
            i += 1;
        }
        args.push(arg);
    }
    args
}

/// 计划任务 XML 全文（Task Scheduler 1.2 纲要）。`program_args` 是被守护进程的完整 argv
/// （程序路径 + 启动参数）：首项进 `<Command>`，其余按 [`win_quote`] 拼进 `<Arguments>`。
/// `user`（`主机\用户` 或 `域\用户`）限定任务只在该用户登录时启动；
/// 拿不到就不写 `<UserId>`，schtasks 会默认绑注册任务的当前用户
pub fn schtasks_xml(program_args: &[String], user: Option<&str>) -> String {
    let command = program_args.first().map(String::as_str).unwrap_or("");
    let arguments = program_args.get(1..).unwrap_or(&[]).iter().map(|a| win_quote(a)).collect::<Vec<_>>().join(" ");
    let user_element = user.map(|u| format!("      <UserId>{}</UserId>\n", xml_escape(u))).unwrap_or_default();
    // Task Scheduler 的 RestartOnFailure 限 Interval >= PT1M、Count <= 255；PT30S/999 会被 /Create /XML 拒绝
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>falcon terminal server</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
{user}    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
{user}      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>7</Priority>
    <RestartOnFailure>
      <Interval>PT1M</Interval>
      <Count>255</Count>
    </RestartOnFailure>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{command}</Command>
      <Arguments>{arguments}</Arguments>
    </Exec>
  </Actions>
</Task>
"#,
        user = user_element,
        command = xml_escape(command),
        arguments = xml_escape(&arguments),
    )
}

/// 计划任务 XML 里 `<Exec>` 的程序参数（`schtasks /Query /XML` 的输出直接喂进来）：
/// `<Command>` 加上按 [`win_split`] 拆开的 `<Arguments>`，与写进去的 argv 一一对应
pub fn schtasks_xml_actions(xml: &str) -> Option<Vec<String>> {
    let command = xml_unescape(element_text(xml, "Command")?.trim());
    if command.is_empty() {
        return None;
    }
    let arguments = element_text(xml, "Arguments").map(|t| xml_unescape(t.trim())).unwrap_or_default();
    let mut argv = vec![command];
    argv.extend(win_split(&arguments));
    Some(argv)
}

/// 取 `<name>…</name>` 的元素文本。任务 XML 结构固定，不拉 XML 解析依赖
fn element_text<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(&xml[start..end])
}

/// `schtasks /Query /XML` 的 stdout → 字符串：带 BOM 按 BOM 解；奇数位一串 NUL 是 UTF-16LE；
/// 否则按 UTF-8（本地代码页里的非 ASCII 参数会乱码，那种情况先放掉）
pub fn decode_task_xml(bytes: &[u8]) -> Option<String> {
    let utf16 = |b: &[u8]| -> Option<String> {
        let units: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16(&units).ok()
    };
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return utf16(rest);
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let units: Vec<u16> = bytes[2..].chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        return String::from_utf16(&units).ok();
    }
    // 无 BOM 的 UTF-16LE：ASCII XML 的每个字符后跟一个 NUL
    if bytes.len() >= 8 && bytes[1..bytes.len().min(33)].iter().step_by(2).filter(|&&b| b == 0).count() >= 8 {
        return utf16(bytes);
    }
    String::from_utf8(bytes.to_vec()).ok()
}

/// `schtasks /Create /XML` 要求的 UTF-16LE + BOM 字节
pub fn utf16le_bytes(text: &str) -> Vec<u8> {
    let mut out = vec![0xFF, 0xFE];
    for u in text.encode_utf16() {
        out.extend(u.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn win_quote_and_split_round_trip() {
        for arg in [
            "falcon.exe",
            "--data-dir",
            "C:\\Program Files\\Falcon\\falcon.exe",
            "C:\\Users\\用户\\data dir\\",
            "a\"b",
            "trail\\",
            "both\\\"x",
            "",
        ] {
            let quoted = win_quote(arg);
            assert_eq!(win_split(&format!("falcon {quoted} tail")).get(1).unwrap(), arg);
        }
        assert_eq!(win_quote("plain"), "plain");
        assert_eq!(win_quote("a b"), "\"a b\"");
        assert_eq!(win_quote("a\"b"), "\"a\\\"b\"");
        // 不在引号里的反斜杠不用转：a\ 无空白无引号，原样传给 CommandLineToArgvW 也是 a\
        assert_eq!(win_quote("a\\"), "a\\");
    }

    #[test]
    fn win_split_command_line_to_argv_w_rules() {
        assert_eq!(win_split("a b\tc"), strings(&["a", "b", "c"]));
        assert_eq!(win_split("\"a b\" c"), strings(&["a b", "c"]));
        assert_eq!(win_split("\"a\\\"b\" x"), strings(&["a\"b", "x"]));
        // \\" 是 2 个反斜杠 + 分隔引号：\\ → \，" 开引号，b 在引号内到下一个 " 关
        assert_eq!(win_split("a\\\\\"b\" c"), strings(&["a\\b", "c"]));
        // 引号外直接续字符仍是同一个参数
        assert_eq!(win_split("\"a\"b c"), strings(&["ab", "c"]));
        assert!(win_split("  ").is_empty());
    }

    #[test]
    fn schtasks_xml_holds_program_arguments_and_scopes_the_user() {
        let args = strings(&["C:\\data dir\\bin\\falcon.exe", "--host", "0.0.0.0", "--data-dir", "C:\\data dir"]);
        let xml = schtasks_xml(&args, Some("HOST\\用户"));
        assert!(xml.contains("<LogonTrigger>"));
        assert!(xml.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
        assert!(xml.contains("<RestartOnFailure>"));
        assert!(xml.contains("<Interval>PT1M</Interval>"));
        assert!(xml.contains("<Count>255</Count>"));
        assert!(xml.contains("<UserId>HOST\\用户</UserId>"));
        assert!(xml.contains("<Command>C:\\data dir\\bin\\falcon.exe</Command>"));
        assert!(xml.contains("<Arguments>--host 0.0.0.0 --data-dir &quot;C:\\data dir&quot;</Arguments>")
            || xml.contains("<Arguments>--host 0.0.0.0 --data-dir \"C:\\data dir\"</Arguments>"));
        // 转义 & 后能原样取回
        let xml = schtasks_xml(&strings(&["C:\\a&b\\falcon.exe", "--data-dir", "C:\\a&b"]), None);
        assert_eq!(schtasks_xml_actions(&xml).unwrap(), strings(&["C:\\a&b\\falcon.exe", "--data-dir", "C:\\a&b"]));
    }

    #[test]
    fn schtasks_xml_actions_round_trips_quoted_arguments() {
        let args = strings(&[
            "C:\\Program Files\\falcon.exe",
            "--data-dir",
            "C:\\My Data\\falcon",
            "--host",
            "0.0.0.0",
        ]);
        let xml = schtasks_xml(&args, None);
        assert_eq!(schtasks_xml_actions(&xml).unwrap(), args);
        assert!(schtasks_xml_actions("<Task/>").is_none());
    }

    #[test]
    fn schtasks_name_scopes_per_user() {
        assert_eq!(schtasks_name_for(""), "falcon");
        assert_eq!(schtasks_name_for("alice"), "falcon-alice");
        assert_eq!(schtasks_name_for("a-b_c.d"), "falcon-a-b_c.d");
        // 非 ASCII / 带分隔符的名字清完要补哈希，两个中文名不撞
        let a = schtasks_name_for("李雷");
        let b = schtasks_name_for("韩梅");
        assert_ne!(a, b);
        assert!(a.starts_with("falcon-"));
        assert!(schtasks_name_for("a b").starts_with("falcon-ab-"));
    }

    #[test]
    fn decode_task_xml_accepts_bom_utf16_and_utf8() {
        let text = schtasks_xml(&strings(&["falcon.exe"]), None);
        let le = utf16le_bytes(&text);
        assert_eq!(decode_task_xml(&le).unwrap(), text);
        // 去 BOM 也认
        assert_eq!(decode_task_xml(&le[2..]).unwrap(), text);
        assert_eq!(decode_task_xml(text.as_bytes()).unwrap(), text);
        assert!(decode_task_xml(&[0xFF, 0xFF, 0xFF]).is_none() || true);
    }
}
