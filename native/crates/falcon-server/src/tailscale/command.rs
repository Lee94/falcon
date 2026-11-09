//! 读 `tailscale status --json`：argv、安装位置、把状态 JSON 收成设备列表。纯函数，零 I/O。
//!
//! 只问后端本机的 Tailscale。SSH 从后端拨出去，界面那台机器的 tailnet 用不上。
//! 不走 `tailscale ssh`：那是另一套身份，falcon 的会话仍是普通 SSH（密钥 / 密码 / agent）。

use std::path::{Path, PathBuf};

use falcon_proto::{TailscaleDevice, TailscaleDevices, TailscaleUnavailable};
use serde::Deserialize;
use serde_json::{Map, Value};

/// `tailscale status` 通常几十毫秒就返回。守护进程卡住时这条 CLI 会一直挂着，到点就结束它
pub const STATUS_TIMEOUT_SECS: u64 = 8;

/// 一次状态查询的 argv，接在可执行文件后面。不经 shell
pub fn status_args() -> [&'static str; 2] {
    ["status", "--json"]
}

/// PATH 里没有时再试这些。launchd 拉起的后端 PATH 往往只剩 `/usr/bin:/bin:/usr/sbin:/sbin`，
/// macOS 上 Tailscale 的 CLI 在 `/usr/local/bin`，或者只住在 Tailscale.app 里
pub fn candidate_paths(windows: bool) -> Vec<String> {
    if windows {
        vec![r"C:\Program Files\Tailscale\tailscale.exe".into()]
    } else {
        vec![
            "/usr/local/bin/tailscale".into(),
            "/opt/homebrew/bin/tailscale".into(),
            "/usr/bin/tailscale".into(),
            "/Applications/Tailscale.app/Contents/MacOS/Tailscale".into(),
        ]
    }
}

/// `FALCON_TAILSCALE_BIN` 指向的文件真在就用它。设了但文件不在，就继续找 PATH，
/// 再找 [`candidate_paths`]，过期路径不会让后面的位置失效。
pub fn resolve_bin<'a>(
    override_bin: Option<&str>,
    path_hits: impl IntoIterator<Item = &'a Path>,
    candidates: impl IntoIterator<Item = &'a Path>,
    exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    if let Some(raw) = override_bin.map(str::trim).filter(|s| !s.is_empty()) {
        let path = PathBuf::from(raw);
        if exists(&path) {
            return Some(path);
        }
    }
    path_hits.into_iter().chain(candidates).find(|p| exists(p)).map(|p| p.to_path_buf())
}

/// 后端进程环境里的登录名。Windows 是 `USERNAME`，其余是 `USER`
pub fn suggested_username(env: &[(String, String)]) -> String {
    let key = if cfg!(windows) { "USERNAME" } else { "USER" };
    env.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_default()
}

/// CLI 的一次结局。退出码不在这里：`status --json` 在未登录时有的版本退出码是 1，
/// stdout 仍是完整 JSON，能不能用要看 `BackendState`
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusRun {
    NotFound,
    TimedOut,
    SpawnFailed(String),
    TooLarge,
    Output { stdout: String, stderr: String },
}

/// 把「找到没有二进制」和「跑出来的结果」收成 API 响应
pub fn assemble(found: bool, run: Option<&StatusRun>, suggested_username: &str) -> TailscaleDevices {
    let suggested_username = suggested_username.to_string();
    let empty = |reason: TailscaleUnavailable, message: Option<String>| TailscaleDevices {
        available: false,
        reason: Some(reason),
        message,
        suggested_username: suggested_username.clone(),
        devices: Vec::new(),
    };
    if !found {
        return empty(TailscaleUnavailable::NotInstalled, None);
    }
    let Some(run) = run else {
        return empty(TailscaleUnavailable::Failed, Some("没有执行 Tailscale".into()));
    };
    match run {
        StatusRun::NotFound => empty(TailscaleUnavailable::NotInstalled, None),
        StatusRun::TimedOut => {
            empty(TailscaleUnavailable::Failed, Some(format!("Tailscale 在 {STATUS_TIMEOUT_SECS} 秒内没有返回")))
        }
        StatusRun::SpawnFailed(err) => empty(TailscaleUnavailable::Failed, Some(clip(err, 400))),
        StatusRun::TooLarge => empty(TailscaleUnavailable::Failed, Some("Tailscale 的输出太大".into())),
        StatusRun::Output { stdout, stderr } => from_output(stdout, stderr, suggested_username),
    }
}

fn from_output(stdout: &str, stderr: &str, suggested_username: String) -> TailscaleDevices {
    let base = |available, reason, message, devices| TailscaleDevices {
        available,
        reason,
        message,
        suggested_username: suggested_username.clone(),
        devices,
    };
    let Some(parsed) = parse_status(stdout) else {
        let reason = sniff(stderr, stdout);
        let message = (reason == TailscaleUnavailable::Failed).then(|| clip_either(stderr, stdout)).filter(|s| !s.is_empty());
        return base(false, Some(reason), message, Vec::new());
    };
    match reason_for_state(&parsed.state) {
        Some(reason) => base(false, Some(reason), None, Vec::new()),
        None => base(true, None, None, parsed.devices),
    }
}

struct Parsed {
    state: String,
    devices: Vec<TailscaleDevice>,
}

/// `Running`，或者老版本没写 BackendState 但已经带了 Peer，都当能用。
/// 其余状态（NeedsLogin / Stopped / Starting …）不把可能过期的节点交出去
fn reason_for_state(state: &str) -> Option<TailscaleUnavailable> {
    match state {
        "Running" | "" => None,
        "NeedsLogin" | "NeedsMachineAuth" => Some(TailscaleUnavailable::NeedsLogin),
        _ => Some(TailscaleUnavailable::NotRunning),
    }
}

fn parse_status(stdout: &str) -> Option<Parsed> {
    let root = json_value(stdout)?;
    let obj = root.as_object()?;
    let state = str_field(obj, "BackendState");
    // 空状态又没有任何节点：当没在跑，而不是「网络里没有别人」
    if state.is_empty() && !obj.contains_key("Peer") && !obj.contains_key("Self") {
        return None;
    }
    if reason_for_state(&state).is_some() && !state.is_empty() {
        return Some(Parsed { state, devices: Vec::new() });
    }
    let magic = obj
        .get("CurrentTailnet")
        .and_then(|v| v.as_object())
        .and_then(|t| t.get("MagicDNSEnabled"))
        .and_then(Value::as_bool);
    let users = obj.get("User").and_then(Value::as_object);
    let self_id = obj.get("Self").and_then(peer_id).unwrap_or_default();
    let mut devices = Vec::new();
    if let Some(peers) = obj.get("Peer").and_then(Value::as_object) {
        for (key, value) in peers {
            let Some(raw) = peer_from(value) else { continue };
            let id = if raw.id.is_empty() { key.clone() } else { raw.id.clone() };
            if !self_id.is_empty() && id == self_id {
                continue;
            }
            let Some(device) = into_device(id, raw, magic, users) else { continue };
            devices.push(device);
        }
    }
    devices.sort_by(|a, b| {
        b.online
            .cmp(&a.online)
            .then(mobile(a.os.as_deref()).cmp(&mobile(b.os.as_deref())))
            .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then(a.id.cmp(&b.id))
    });
    Some(Parsed { state: if state.is_empty() { "Running".into() } else { state }, devices })
}

struct RawPeer {
    id: String,
    host_name: String,
    dns_name: String,
    os: String,
    user_id: Option<String>,
    ips: Vec<String>,
    online: bool,
}

fn peer_id(v: &Value) -> Option<String> {
    let id = str_field(v.as_object()?, "ID");
    if id.is_empty() { None } else { Some(id) }
}

fn peer_from(v: &Value) -> Option<RawPeer> {
    let obj = v.as_object()?;
    Some(RawPeer {
        id: str_field(obj, "ID"),
        host_name: str_field(obj, "HostName"),
        dns_name: str_field(obj, "DNSName"),
        os: str_field(obj, "OS"),
        user_id: obj.get("UserID").and_then(id_key),
        ips: string_list(obj.get("TailscaleIPs")),
        online: obj.get("Online").and_then(Value::as_bool).unwrap_or(false),
    })
}

fn into_device(id: String, raw: RawPeer, magic_dns: Option<bool>, users: Option<&Map<String, Value>>) -> Option<TailscaleDevice> {
    let dns = normalize_dns(&raw.dns_name);
    let ipv4 = raw.ips.iter().find(|ip| ip.contains('.') && !ip.contains(':')).cloned();
    let ipv6 = raw.ips.iter().find(|ip| ip.contains(':')).cloned();
    // MagicDNS 关着时名字解析不了，地址用 Tailscale IPv4（它跟节点绑定，不会因改名变掉）。
    // 字段缺省时按开着处理：老 CLI 不报这个字段，而 MagicDNS 默认是开的
    let host = if magic_dns.unwrap_or(true) {
        dns.clone().or(ipv4.clone()).or(ipv6.clone())
    } else {
        ipv4.clone().or(dns.clone()).or(ipv6.clone())
    }?;
    let name = device_name(&raw.host_name, dns.as_deref(), ipv4.as_deref(), &id);
    let owner = raw.user_id.as_deref().and_then(|uid| users.and_then(|users| owner_name(users, uid)));
    Some(TailscaleDevice {
        id,
        name,
        host,
        dns_name: dns,
        ipv4,
        os: (!raw.os.is_empty()).then_some(raw.os),
        online: raw.online,
        owner,
    })
}

fn device_name(host_name: &str, dns: Option<&str>, ipv4: Option<&str>, id: &str) -> String {
    if !host_name.is_empty() {
        return host_name.to_string();
    }
    if let Some(label) = dns.and_then(|d| d.split('.').next()).filter(|s| !s.is_empty()) {
        return label.to_string();
    }
    if let Some(ip) = ipv4 {
        return ip.to_string();
    }
    if !id.is_empty() {
        return id.to_string();
    }
    "tailscale".into()
}

fn owner_name(users: &Map<String, Value>, user_id: &str) -> Option<String> {
    let obj = users.get(user_id)?.as_object()?;
    let display = str_field(obj, "DisplayName");
    let login = str_field(obj, "LoginName");
    let name = if !display.is_empty() { display } else { login };
    if name.is_empty() { None } else { Some(name) }
}

fn normalize_dns(raw: &str) -> Option<String> {
    // status JSON 的 DNSName 带根点：`work.tail.ts.net.`。SSH 主机字段不要这个点
    let s = raw.trim().trim_end_matches('.').trim();
    if s.is_empty() { None } else { Some(s.to_string()) }
}

fn mobile(os: Option<&str>) -> u8 {
    matches!(os.unwrap_or("").to_ascii_lowercase().as_str(), "ios" | "android" | "tvos") as u8
}

fn sniff(stderr: &str, stdout: &str) -> TailscaleUnavailable {
    let blob = format!("{stderr}\n{stdout}").to_ascii_lowercase();
    if blob.contains("not logged in") || blob.contains("needslogin") || blob.contains("needs login") {
        TailscaleUnavailable::NeedsLogin
    } else if blob.contains("stopped")
        || blob.contains("failed to connect")
        || (blob.contains("tailscaled") && blob.contains("not running"))
    {
        TailscaleUnavailable::NotRunning
    } else if blob.contains("command not found") || blob.contains("no such file") {
        TailscaleUnavailable::NotInstalled
    } else {
        TailscaleUnavailable::Failed
    }
}

fn clip_either(stderr: &str, stdout: &str) -> String {
    let stderr = stderr.trim();
    if !stderr.is_empty() {
        clip(stderr, 400)
    } else {
        clip(stdout.trim(), 400)
    }
}

fn clip(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max { s.to_string() } else { format!("{}…", s.chars().take(max).collect::<String>()) }
}

fn json_value(stdout: &str) -> Option<Value> {
    let s = stdout.trim();
    let start = s.find('{')?;
    let mut de = serde_json::Deserializer::from_str(&s[start..]);
    Value::deserialize(&mut de).ok()
}

fn str_field(obj: &Map<String, Value>, key: &str) -> String {
    obj.get(key).and_then(Value::as_str).unwrap_or("").trim().to_string()
}

fn string_list(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

fn id_key(v: &Value) -> Option<String> {
    match v {
        Value::Number(n) => Some(n.to_string()),
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> &'static str {
        r#"{
          "BackendState": "Running",
          "CurrentTailnet": {"Name": "t", "MagicDNSSuffix": "tail.ts.net", "MagicDNSEnabled": true},
          "Self": {"ID": "nself", "HostName": "this-mac", "DNSName": "this-mac.tail.ts.net.", "OS": "macOS", "Online": true, "TailscaleIPs": ["100.1.1.1"], "UserID": 7},
          "User": {
            "7": {"ID": 7, "LoginName": "ada@example.com", "DisplayName": "Ada"},
            "9": {"LoginName": "bob@example.com", "DisplayName": ""}
          },
          "Peer": {
            "nodekey:a": {"ID": "na", "HostName": "work", "DNSName": "work.tail.ts.net.", "OS": "linux", "Online": true, "TailscaleIPs": ["100.2.2.2", "fd7a::1"], "UserID": 7},
            "nodekey:b": {"ID": "nb", "HostName": "phone", "DNSName": "phone.tail.ts.net.", "OS": "iOS", "Online": true, "TailscaleIPs": ["100.3.3.3"], "UserID": 9},
            "nodekey:c": {"ID": "nc", "HostName": "asleep", "DNSName": "asleep.tail.ts.net.", "OS": "windows", "Online": false, "TailscaleIPs": ["100.4.4.4"], "UserID": 7},
            "nodekey:d": {"ID": "nd", "HostName": "", "DNSName": "", "OS": "linux", "Online": true, "TailscaleIPs": []},
            "nodekey:e": {"ID": "ne", "HostName": "v6only", "DNSName": "", "OS": "linux", "Online": true, "TailscaleIPs": ["fd7a::9"]},
            "nodekey:self": {"ID": "nself", "HostName": "this-mac", "DNSName": "this-mac.tail.ts.net.", "OS": "macOS", "Online": true, "TailscaleIPs": ["100.1.1.1"], "UserID": 7}
          }
        }"#
    }

    fn listed(stdout: &str) -> TailscaleDevices {
        assemble(true, Some(&StatusRun::Output { stdout: stdout.into(), stderr: String::new() }), "ada")
    }

    #[test]
    fn running_status_drops_self_and_unaddressed_and_sorts() {
        let list = listed(sample());
        assert!(list.available);
        assert_eq!(list.suggested_username, "ada");
        let names: Vec<_> = list.devices.iter().map(|d| d.name.as_str()).collect();
        // 在线的电脑在前，手机其次，离线的在最后；自己和无地址的节点不出现
        assert_eq!(names, ["v6only", "work", "phone", "asleep"]);
        let work = list.devices.iter().find(|d| d.name == "work").unwrap();
        assert_eq!(work.host, "work.tail.ts.net");
        assert_eq!(work.dns_name.as_deref(), Some("work.tail.ts.net"));
        assert_eq!(work.ipv4.as_deref(), Some("100.2.2.2"));
        assert_eq!(work.owner.as_deref(), Some("Ada"));
        assert!(work.online);
        let phone = list.devices.iter().find(|d| d.name == "phone").unwrap();
        assert_eq!(phone.owner.as_deref(), Some("bob@example.com"));
        let v6 = list.devices.iter().find(|d| d.name == "v6only").unwrap();
        assert_eq!(v6.host, "fd7a::9");
        assert!(v6.ipv4.is_none());
    }

    #[test]
    fn magic_dns_off_prefers_ipv4_but_keeps_the_name() {
        let raw = sample().replace(r#""MagicDNSEnabled": true"#, r#""MagicDNSEnabled": false"#);
        let list = listed(&raw);
        let work = list.devices.iter().find(|d| d.name == "work").unwrap();
        assert_eq!(work.host, "100.2.2.2");
        assert_eq!(work.dns_name.as_deref(), Some("work.tail.ts.net"));
    }

    #[test]
    fn needs_login_and_stopped_hide_peers() {
        let login = listed(r#"{"BackendState":"NeedsLogin","Peer":{"k":{"ID":"na","HostName":"work","DNSName":"work.tail.ts.net.","TailscaleIPs":["100.1.1.1"],"Online":true}}}"#);
        assert!(!login.available);
        assert_eq!(login.reason, Some(TailscaleUnavailable::NeedsLogin));
        assert!(login.devices.is_empty());
        let stopped = listed(r#"{"BackendState":"Stopped"}"#);
        assert_eq!(stopped.reason, Some(TailscaleUnavailable::NotRunning));
    }

    #[test]
    fn running_with_no_peers_is_an_empty_tailnet() {
        let list = listed(r#"{"BackendState":"Running","Peer":null}"#);
        assert!(list.available);
        assert!(list.devices.is_empty());
        assert!(list.reason.is_none());
    }

    #[test]
    fn warning_before_json_is_ignored() {
        let stdout = format!("warning: hi\n{}", r#"{"BackendState":"Running","Peer":{}}"#);
        let list = listed(&stdout);
        assert!(list.available);
        assert!(list.devices.is_empty());
    }

    #[test]
    fn stderr_classifies_daemon_and_login_failures() {
        let down = assemble(
            true,
            Some(&StatusRun::Output { stdout: String::new(), stderr: "failed to connect to local Tailscale service".into() }),
            "ada",
        );
        assert_eq!(down.reason, Some(TailscaleUnavailable::NotRunning));
        assert!(down.message.is_none());
        let login = assemble(
            true,
            Some(&StatusRun::Output { stdout: "nope".into(), stderr: "not logged in".into() }),
            "ada",
        );
        assert_eq!(login.reason, Some(TailscaleUnavailable::NeedsLogin));
        let broke = assemble(
            true,
            Some(&StatusRun::Output { stdout: String::new(), stderr: "something broke".into() }),
            "ada",
        );
        assert_eq!(broke.reason, Some(TailscaleUnavailable::Failed));
        assert_eq!(broke.message.as_deref(), Some("something broke"));
    }

    #[test]
    fn missing_binary_is_not_installed() {
        let list = assemble(false, None, "ada");
        assert_eq!(list.reason, Some(TailscaleUnavailable::NotInstalled));
        assert_eq!(list.suggested_username, "ada");
        let timed = assemble(true, Some(&StatusRun::TimedOut), "ada");
        assert_eq!(timed.reason, Some(TailscaleUnavailable::Failed));
        assert!(timed.message.unwrap().contains("8"));
    }

    #[test]
    fn large_user_id_matches_the_user_map_key() {
        let stdout = r#"{"BackendState":"Running","User":{"5345947841186460":{"DisplayName":"Ada","LoginName":"a@b.c"}},"Peer":{"k":{"ID":"na","HostName":"work","DNSName":"work.tail.ts.net.","Online":true,"TailscaleIPs":["100.2.2.2"],"UserID":5345947841186460}}}"#;
        let list = listed(stdout);
        assert_eq!(list.devices[0].owner.as_deref(), Some("Ada"));
    }

    #[test]
    fn hostname_falls_back_to_the_dns_label() {
        let stdout = r#"{"BackendState":"Running","Peer":{"k":{"ID":"na","HostName":"","DNSName":"box.tail.ts.net.","Online":true,"TailscaleIPs":["100.9.9.9"]}}}"#;
        let list = listed(stdout);
        assert_eq!(list.devices[0].name, "box");
        assert_eq!(list.devices[0].host, "box.tail.ts.net");
    }

    #[test]
    fn resolve_bin_prefers_an_existing_override_then_path_then_candidates() {
        let app = Path::new("/Applications/Tailscale.app/Contents/MacOS/Tailscale");
        let path_hit = Path::new("/usr/local/bin/tailscale");
        assert_eq!(
            resolve_bin(Some(" /opt/ts "), [path_hit], [app], |p| p == Path::new("/opt/ts")).unwrap(),
            PathBuf::from("/opt/ts")
        );
        assert_eq!(resolve_bin(Some("/missing"), [path_hit], [app], |p| p == path_hit).unwrap(), path_hit);
        assert_eq!(resolve_bin(None, [Path::new("/usr/bin/tailscale")], [app], |p| p == app).unwrap(), app);
        assert!(resolve_bin(None, [Path::new("/usr/bin/tailscale")], [app], |_| false).is_none());
    }

    #[test]
    fn candidates_cover_the_usual_installs() {
        let v = candidate_paths(false);
        assert!(v.iter().any(|p| p.ends_with("Tailscale")));
        assert!(v.iter().any(|p| p.contains("homebrew")));
        assert!(candidate_paths(true)[0].ends_with("tailscale.exe"));
    }

    #[test]
    fn suggested_username_reads_the_login_name() {
        assert_eq!(suggested_username(&[("PATH".into(), "/bin".into()), ("USER".into(), " ada ".into())]), "ada");
        assert_eq!(suggested_username(&[("USERNAME".into(), "win".into())]), if cfg!(windows) { "win".into() } else { String::new() });
    }
}
