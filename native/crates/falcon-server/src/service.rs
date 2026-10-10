//! `falcon service <install|uninstall|start|stop|status>` —— 把 falcon 注册成系统服务，
//! 由 init 守护：崩溃自动拉起、登出不退、开机自启。移植自 `packages/server/src/service.ts`
//! 的纯函数部分（参数判定、服务配置渲染、路径、`launchctl print` 的摘取）与
//! `installServiceBinary`（有测试，文件系统用例走 tempfile）。
//!
//! 刻意不自己写守护父进程：launchd / systemd 在这件事上比任何应用内实现都可靠
//! （自身不会挂、接管日志、处理开机时序），我们只负责生成配置并调用它们。
//!
//! install 之后的启动参数（--host / --port / --data-dir）原样写进服务配置，
//! 服务启动时仍由 parseArgs 解析——这里不做校验，两边永远一致。
//!
//! 渲染出的 plist / unit 与 TS 逐字节一致（测试里的期望值是从 service.ts 的模板原文代入
//! 同样的参数实跑出来的）。
//!
//! 留到 S7 的（执行 / 进程 / 读本机环境）：`runServiceCli`（分发与 process.exit，判定规则见
//! [`parse_service_cli`]）、`launchd` / `systemd`（写配置、调 launchctl / systemctl、打印结果）、
//! `relaunchJob`（bootout 后轮询 print、bootstrap 重试）、`run`、`sleepMs`、`fail`、
//! `warnIfNonLoopback`（依赖 config.ts 的 parseArgs / isLoopback）、`programArguments` /
//! `installProgramArguments`（读 isSea / execPath / argv，SEA 时先装二进制）。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::term_env::js;

pub const LAUNCHD_LABEL: &str = "com.falcon.server";
pub const SYSTEMD_NAME: &str = "falcon";
pub const SERVICE_BIN_NAME: &str = "falcon";

/// `falcon service` 认的子命令
pub const SERVICE_COMMANDS: [&str; 6] = ["install", "uninstall", "start", "stop", "restart", "status"];

/// [`parse_service_cli`] 的判定
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceCli<'a> {
    /// 打印用法（stdout）后以 `exit_code` 退出：没给子命令是 0，给了认不出的是 1
    Usage { text: String, exit_code: i32 },
    /// 打印到 stderr 后以 1 退出
    Error(String),
    /// 照子命令执行（按平台分到 launchd / systemd，S7）
    Run { cmd: &'a str, server_args: &'a [String] },
}

pub fn service_usage() -> String {
    format!(
        "用法: falcon service <{}> [--host ..] [--port ..] [--data-dir ..]\n启动参数仅 install 时生效，会原样写进服务配置。",
        SERVICE_COMMANDS.join("|")
    )
}

/// `runServiceCli` 里与平台、进程无关的那段判定：argv 是 `service` 之后的部分
pub fn parse_service_cli(argv: &[String]) -> ServiceCli<'_> {
    let cmd = argv.first().map(String::as_str);
    let server_args = argv.get(1..).unwrap_or(&[]);
    let Some(cmd) = cmd.filter(|c| !c.is_empty() && SERVICE_COMMANDS.contains(c)) else {
        // TS 的 `process.exit(cmd ? 1 : 0)`：空串与没给一样算 0
        let exit_code = if cmd.is_some_and(|c| !c.is_empty()) { 1 } else { 0 };
        return ServiceCli::Usage { text: service_usage(), exit_code };
    };
    if cmd != "install" && !server_args.is_empty() {
        return ServiceCli::Error(format!("service {cmd} 不接受额外参数（启动参数在 install 时指定）"));
    }
    ServiceCli::Run { cmd, server_args }
}

/// 服务进程的固定入口：`<dataDir>/bin/falcon`
pub fn service_bin_path(data_dir: impl AsRef<Path>) -> PathBuf {
    data_dir.as_ref().join("bin").join(SERVICE_BIN_NAME)
}

/// 把当前 SEA 二进制拷到 dest（固定名为 falcon）。
///
/// 发布产物叫 `falcon-v0.1.0-darwin-arm64` 这类带版本的文件名，launchd / ps /
/// 活动监视器都拿文件名当进程名。拷到固定路径后进程就叫 falcon，升级也只是
/// 覆盖同一路径。用临时文件 + rename：覆盖正在跑的同路径不会 ETXTBSY，也不会
/// 让 macOS 因改了正在映射的签名文件而 SIGKILL。
pub fn install_service_binary(src: &Path, dest: &Path) -> io::Result<PathBuf> {
    if same_path(src, dest) {
        return Ok(dest.to_path_buf());
    }
    if let Some(dir) = dest.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut tmp = dest.as_os_str().to_owned();
    tmp.push(format!(".new-{}", std::process::id()));
    let tmp = PathBuf::from(tmp);
    let result = (|| {
        fs::copy(src, &tmp)?;
        set_executable(&tmp)?;
        fs::rename(&tmp, dest)
    })();
    if let Err(err) = result {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    Ok(dest.to_path_buf())
}

#[cfg(unix)]
fn set_executable(p: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(p, fs::Permissions::from_mode(0o755))
}

/// Windows 上 Node 的 chmod 只管只读位，可执行与否看扩展名，这里什么都不用做
#[cfg(not(unix))]
fn set_executable(_p: &Path) -> io::Result<()> {
    Ok(())
}

/// 两边都 realpath 得出来就比真实路径；任一不存在就退回比绝对路径（TS 的 `path.resolve`）
fn same_path(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => match (std::path::absolute(a), std::path::absolute(b)) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        },
    }
}

// ---- macOS: launchd（用户级 LaunchAgent）----

/// `~/Library/LaunchAgents/com.falcon.server.plist`
pub fn launchd_plist_path(home: impl AsRef<Path>) -> PathBuf {
    home.as_ref().join("Library").join("LaunchAgents").join(format!("{LAUNCHD_LABEL}.plist"))
}

/// launchctl 的域：`gui/<uid>`
pub fn launchd_domain(uid: u32) -> String {
    format!("gui/{uid}")
}

/// launchctl 的服务目标：`gui/<uid>/com.falcon.server`
pub fn launchd_target(domain: &str) -> String {
    format!("{domain}/{LAUNCHD_LABEL}")
}

/// plist 里的文本转义：只转 `&` `<` `>`（与 TS 一致，引号不转——都在元素内容里）
pub fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// LaunchAgent 的 plist 全文。`program_args` 是被守护进程的完整 argv（程序路径 + 启动参数），
/// `log_file` 是 `<dataDir>/logs/falcon.log`（launchd 不会替我们建日志目录，调用方先建）。
pub fn launchd_plist(program_args: &[String], log_file: &str) -> String {
    let args =
        program_args.iter().map(|a| format!("    <string>{}</string>", xml_escape(a))).collect::<Vec<_>>().join("\n");
    let log = xml_escape(log_file);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LAUNCHD_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
{args}
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>{log}</string>
  <key>StandardErrorPath</key><string>{log}</string>
</dict>
</plist>
"#
    )
}

/// `service status` 在 launchd 上的输出。`print_output` 是 `launchctl print <target>` 的 stdout，
/// 失败（没在跑）时是空串。
///
/// launchctl print 输出很长且子段落里键会重复，只留每个键的首次出现
pub fn launchd_status_report(print_output: &str) -> String {
    if print_output.is_empty() {
        return "已安装，未在运行。".into();
    }
    let mut seen: Vec<String> = Vec::new();
    let mut lines = vec!["运行中。".to_string()];
    for l in print_output.split('\n').map(js::trim) {
        if !is_status_line(l) {
            continue;
        }
        let key = js::trim(l.split('=').next().unwrap_or("")).to_string();
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        lines.push(l.to_string());
    }
    lines.join("\n")
}

/// `/^(state|pid|last exit code)\s*=/`（`\s` 是 JS 的空白集合）
fn is_status_line(l: &str) -> bool {
    ["state", "pid", "last exit code"]
        .iter()
        .any(|k| l.strip_prefix(k).is_some_and(|rest| rest.trim_start_matches(js::is_whitespace).starts_with('=')))
}

// ---- Linux: systemd（root 装 system 级，普通用户装 user 级）----

/// unit 文件位置：root 装到 `/etc/systemd/system/falcon.service`，普通用户装到
/// `${XDG_CONFIG_HOME:-~/.config}/systemd/user/falcon.service`。
///
/// 与 TS 一样用 `??`：XDG_CONFIG_HOME 设成空串时不回退（得到相对路径 `systemd/user/…`）。
pub fn systemd_unit_path(root_mode: bool, xdg_config_home: Option<&str>, home: impl AsRef<Path>) -> PathBuf {
    if root_mode {
        return PathBuf::from(format!("/etc/systemd/system/{SYSTEMD_NAME}.service"));
    }
    let base = match xdg_config_home {
        Some(x) => PathBuf::from(x),
        None => home.as_ref().join(".config"),
    };
    base.join("systemd").join("user").join(format!("{SYSTEMD_NAME}.service"))
}

/// systemctl 的 argv：root 走 system 实例，普通用户加 `--user`
pub fn systemctl_argv(root_mode: bool, args: &[&str]) -> Vec<String> {
    let mut argv = vec!["systemctl".to_string()];
    if !root_mode {
        argv.push("--user".into());
    }
    argv.extend(args.iter().map(|a| a.to_string()));
    argv
}

/// systemd 的 ExecStart 按空白切词，含空格/引号的路径必须加双引号转义
pub fn systemd_quote(a: &str) -> String {
    if a.chars().any(|c| js::is_whitespace(c) || c == '"' || c == '\\') {
        format!("\"{}\"", a.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        a.to_string()
    }
}

/// unit 文件全文。`program_args` 同 [`launchd_plist`]。
///
/// TS 的 `systemdUnit(serverArgs, rootMode)` 在里面调 `installProgramArguments`（SEA 时会顺手
/// 把二进制装到 `<dataDir>/bin/falcon`）；这里把算 argv 的那步交给调用方，渲染保持纯。
pub fn systemd_unit(program_args: &[String], root_mode: bool) -> String {
    let exec = program_args.iter().map(|a| systemd_quote(a)).collect::<Vec<_>>().join(" ");
    let wanted_by = if root_mode { "multi-user.target" } else { "default.target" };
    format!(
        "[Unit]
Description=falcon terminal server
After=network.target

[Service]
ExecStart={exec}
Restart=always
RestartSec=2

[Install]
WantedBy={wanted_by}
"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    /// 期望值是 JSON 字符串字面量（从 TS 实跑输出原样贴来）
    fn from_json(s: &str) -> String {
        serde_json::from_str(s).expect("json string")
    }

    // describe("serviceBinPath")

    #[test]
    fn service_bin_path_is_always_data_dir_bin_falcon() {
        // never the versioned download name
        assert_eq!(service_bin_path("/tmp/data"), Path::new("/tmp/data").join("bin").join("falcon"));
    }

    // describe("installServiceBinary")

    #[cfg(unix)]
    fn mode(p: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(p).unwrap().permissions().mode()
    }

    #[test]
    fn install_service_binary_copies_src_to_dest_named_falcon_and_makes_it_executable() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("falcon-v0.1.0-darwin-arm64");
        let dest = service_bin_path(dir.path().join("data"));
        fs::write(&src, "fake-bin").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&src, fs::Permissions::from_mode(0o644)).unwrap();
        }

        assert_eq!(install_service_binary(&src, &dest).unwrap(), dest);
        assert_eq!(dest.file_name().unwrap(), "falcon");
        assert_eq!(fs::read_to_string(&dest).unwrap(), "fake-bin");
        #[cfg(unix)]
        assert_eq!(mode(&dest) & 0o111, 0o111);
        assert!(src.exists());
    }

    #[test]
    fn install_service_binary_replaces_an_existing_dest_via_rename() {
        // so a running service can be upgraded
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("falcon-v0.2.0-darwin-arm64");
        let dest = service_bin_path(dir.path().join("data"));
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        fs::write(&dest, "old").unwrap();
        fs::write(&src, "new").unwrap();

        install_service_binary(&src, &dest).unwrap();
        assert_eq!(fs::read_to_string(&dest).unwrap(), "new");
        let names: Vec<String> = fs::read_dir(dest.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().any(|n| n == "falcon"));
        assert!(!names.iter().any(|n| n.starts_with("falcon.new-")));
    }

    #[test]
    fn install_service_binary_is_a_no_op_when_src_is_already_dest() {
        let dir = tempfile::tempdir().unwrap();
        let dest = service_bin_path(dir.path());
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        fs::write(&dest, "self").unwrap();
        let before = fs::metadata(&dest).unwrap().modified().unwrap();

        assert_eq!(install_service_binary(&dest, &dest).unwrap(), dest);
        assert_eq!(fs::read_to_string(&dest).unwrap(), "self");
        assert_eq!(fs::metadata(&dest).unwrap().modified().unwrap(), before);
    }

    // 以下不在 TS 测试里：渲染与判定（期望值由 TS 模板原文实跑得到）

    #[test]
    fn launchd_plist_matches_ts_byte_for_byte() {
        let args =
            strings(&["/Users/fay/.falcon/bin/falcon", "--port", "4924", "--data-dir", "/Users/fay/My Data & <x>"]);
        let expected = from_json(
            r#""<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n  <key>Label</key><string>com.falcon.server</string>\n  <key>ProgramArguments</key>\n  <array>\n    <string>/Users/fay/.falcon/bin/falcon</string>\n    <string>--port</string>\n    <string>4924</string>\n    <string>--data-dir</string>\n    <string>/Users/fay/My Data &amp; &lt;x&gt;</string>\n  </array>\n  <key>RunAtLoad</key><true/>\n  <key>KeepAlive</key><true/>\n  <key>StandardOutPath</key><string>/Users/fay/My Data &amp; &lt;x&gt;/logs/falcon.log</string>\n  <key>StandardErrorPath</key><string>/Users/fay/My Data &amp; &lt;x&gt;/logs/falcon.log</string>\n</dict>\n</plist>\n""#,
        );
        assert_eq!(launchd_plist(&args, "/Users/fay/My Data & <x>/logs/falcon.log"), expected);
    }

    #[test]
    fn systemd_unit_matches_ts_byte_for_byte() {
        let args =
            strings(&["/opt/falcon dir/bin/falcon", "--host", "0.0.0.0", "--data-dir", "/srv/a\"b\\c", "tab\there"]);
        let root = from_json(
            r#""[Unit]\nDescription=falcon terminal server\nAfter=network.target\n\n[Service]\nExecStart=\"/opt/falcon dir/bin/falcon\" --host 0.0.0.0 --data-dir \"/srv/a\\\"b\\\\c\" \"tab\there\"\nRestart=always\nRestartSec=2\n\n[Install]\nWantedBy=multi-user.target\n""#,
        );
        let user = from_json(
            r#""[Unit]\nDescription=falcon terminal server\nAfter=network.target\n\n[Service]\nExecStart=\"/opt/falcon dir/bin/falcon\" --host 0.0.0.0 --data-dir \"/srv/a\\\"b\\\\c\" \"tab\there\"\nRestart=always\nRestartSec=2\n\n[Install]\nWantedBy=default.target\n""#,
        );
        assert_eq!(systemd_unit(&args, true), root);
        assert_eq!(systemd_unit(&args, false), user);
        // 不含空白 / 引号 / 反斜杠的原样；JS 的 \s 含全角空格
        assert_eq!(systemd_quote("--port"), "--port");
        assert_eq!(systemd_quote("a\u{3000}b"), "\"a\u{3000}b\"");
    }

    #[test]
    fn service_cli_decisions() {
        assert_eq!(parse_service_cli(&[]), ServiceCli::Usage { text: service_usage(), exit_code: 0 });
        assert_eq!(parse_service_cli(&strings(&["bogus"])), ServiceCli::Usage { text: service_usage(), exit_code: 1 });
        assert_eq!(
            service_usage(),
            "用法: falcon service <install|uninstall|start|stop|restart|status> [--host ..] [--port ..] [--data-dir ..]\n启动参数仅 install 时生效，会原样写进服务配置。"
        );
        assert_eq!(
            parse_service_cli(&strings(&["start", "--port", "1"])),
            ServiceCli::Error("service start 不接受额外参数（启动参数在 install 时指定）".into())
        );
        let argv = strings(&["install", "--port", "4924"]);
        assert_eq!(parse_service_cli(&argv), ServiceCli::Run { cmd: "install", server_args: &argv[1..] });
        assert_eq!(parse_service_cli(&argv[..1]), ServiceCli::Run { cmd: "install", server_args: &[] });
    }

    #[test]
    fn launchd_status_keeps_first_occurrence_of_each_key() {
        assert_eq!(launchd_status_report(""), "已安装，未在运行。");
        let out = "gui/501/com.falcon.server = {\n\tstate = running\n\tpid = 123\n\tprogram = /x\n\tlast exit code = 0\n\tsubsection = {\n\t\tstate = waiting\n\t\tpid = 9\n\t}\n\tstatefoo = 1\n}\n";
        assert_eq!(launchd_status_report(out), "运行中。\nstate = running\npid = 123\nlast exit code = 0");
        // 没摘到任何键也算在跑
        assert_eq!(launchd_status_report("x\n"), "运行中。");
    }

    #[test]
    fn paths_and_argv() {
        assert_eq!(
            launchd_plist_path("/Users/fay"),
            PathBuf::from("/Users/fay/Library/LaunchAgents/com.falcon.server.plist")
        );
        assert_eq!(launchd_target(&launchd_domain(501)), "gui/501/com.falcon.server");
        assert_eq!(systemd_unit_path(true, None, "/root"), PathBuf::from("/etc/systemd/system/falcon.service"));
        assert_eq!(
            systemd_unit_path(false, None, "/home/fay"),
            PathBuf::from("/home/fay/.config/systemd/user/falcon.service")
        );
        assert_eq!(
            systemd_unit_path(false, Some("/xdg"), "/home/fay"),
            PathBuf::from("/xdg/systemd/user/falcon.service")
        );
        assert_eq!(systemd_unit_path(false, Some(""), "/home/fay"), PathBuf::from("systemd/user/falcon.service"));
        assert_eq!(systemctl_argv(true, &["daemon-reload"]), strings(&["systemctl", "daemon-reload"]));
        assert_eq!(
            systemctl_argv(false, &["restart", SYSTEMD_NAME]),
            strings(&["systemctl", "--user", "restart", "falcon"])
        );
    }
}
