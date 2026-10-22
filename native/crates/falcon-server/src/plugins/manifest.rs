//! 命令插件的清单（`<数据目录>/plugins/<id>/plugin.json`）与 id / 参数的白名单。纯函数，零 I/O。
//!
//! 清单是手写的 JSON，读法照 `api/input.rs` 的口径逐字段取，不先反序列化成强类型：某个可选字段
//! 写错了类型，退回缺省就好，不至于让整个插件"清单不合法"。真正读不下去的只有三种——不是 JSON
//! 对象、没有能跑的 `command`、`api` 比这版 falcon 新——这些插件照样出现在设置页里，带着原因。

use std::path::{Path, PathBuf};
use std::time::Duration;

use falcon_proto::{PLUGIN_API, PluginScope};
use serde_json::Value;

/// 内置插件占用的 id：同名目录不当命令插件读（设置页里报"与内置插件重名"）
pub const BUILTIN_IDS: &[&str] = &[super::sysmon::ID, super::docker::ID];

/// 插件进程的缺省时限与上限。面板是开着就重画的，一次比十秒还慢的插件该自己缓存
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
pub const MAX_TIMEOUT: Duration = Duration::from_secs(120);
/// 自动重画的间隔下限 / 上限（秒）。下限挡住"refresh: 0.1"把后端打满
pub const MIN_REFRESH: f64 = 1.0;
pub const MAX_REFRESH: f64 = 3600.0;
/// 清单文件本身的大小上限：它只该有几行
pub const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
/// 插件目录里最多认多少个插件（多了只列前这么多，按 id 排）
pub const MAX_PLUGINS: usize = 64;

/// 插件的 stdout 怎么读
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    /// 一份 `PluginView` 的 JSON
    Json,
    /// 纯文字，原样贴成一块等宽文字（最省事的插件：`["uptime"]`）
    Text,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    pub name: String,
    pub description: Option<String>,
    pub icon: String,
    /// argv，不经 shell。`[0]` 是相对路径（含路径分隔符）时相对插件目录，裸名字走 PATH
    pub command: Vec<String>,
    pub output: Output,
    pub scope: PluginScope,
    pub refresh: Option<f64>,
    pub timeout: Duration,
}

/// 命令插件的 id 就是目录名：小写字母数字开头，后面可以有 `-` `_`，最长 48。
/// 它会进 URL、环境变量与数据目录的路径，所以只放这么窄
pub fn valid_id(id: &str) -> bool {
    let b = id.as_bytes();
    !b.is_empty()
        && b.len() <= 48
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-' || *c == b'_')
}

/// Lucide 图标名（kebab-case）。客户端按名字找图标，认不出就用拼图块，这里只挡奇怪的字符
pub fn valid_icon(name: &str) -> bool {
    !name.is_empty() && name.len() <= 48 && name.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

/// 面板参数名：它会变成环境变量 `FALCON_PARAM_<大写>`，所以只要小写字母开头的 `[a-z0-9_]`
pub fn valid_param(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b.len() <= 32
        && b[0].is_ascii_lowercase()
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

/// 按钮上的动作 id：插件自己起的名字，原样交回插件（环境变量 + stdin），放宽到常见的标识符字符
pub fn valid_action(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.' | b':'))
}

/// 自动重画间隔：≤ 0 / 不是数 = 不自动重画；其余夹到 [`MIN_REFRESH`]–[`MAX_REFRESH`]
pub fn clamp_refresh(v: f64) -> Option<f64> {
    (v.is_finite() && v > 0.0).then(|| v.clamp(MIN_REFRESH, MAX_REFRESH))
}

fn non_empty(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// 解析清单。`id` 只用来给 `name` 兜底
pub fn parse_manifest(id: &str, text: &str) -> Result<Manifest, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("plugin.json 不是合法的 JSON：{e}"))?;
    let Value::Object(m) = v else { return Err("plugin.json 应该是一个 JSON 对象".into()) };
    if let Some(api) = m.get("api").and_then(Value::as_f64)
        && api > PLUGIN_API as f64
    {
        return Err(format!("插件要求协议 v{api}，这版 falcon 只认 v{PLUGIN_API}，先升级 falcon"));
    }
    // command 可以是数组（argv），也可以是一个字符串——那是**一个**程序，不按空格拆：
    // 拆了就得管引号，管引号就是在实现半个 shell
    let command: Vec<String> = match m.get("command") {
        Some(Value::String(s)) if !s.trim().is_empty() => vec![s.trim().to_string()],
        Some(Value::Array(items)) => {
            let argv: Option<Vec<String>> = items.iter().map(|a| a.as_str().map(str::to_string)).collect();
            match argv {
                Some(argv) if argv.first().is_some_and(|p| !p.trim().is_empty()) => argv,
                Some(_) => return Err("command 的第一项（要跑的程序）是空的".into()),
                None => return Err("command 数组里只能放字符串".into()),
            }
        }
        Some(_) => return Err("command 要么是字符串，要么是字符串数组".into()),
        None => return Err("plugin.json 缺少 command（要跑的程序）".into()),
    };
    let output = match m.get("output").and_then(Value::as_str) {
        None | Some("json") => Output::Json,
        Some("text") => Output::Text,
        Some(other) => return Err(format!("output 只能是 json 或 text，写的是 {other}")),
    };
    let scope = match m.get("scope").and_then(Value::as_str) {
        Some("project") => PluginScope::Project,
        _ => PluginScope::Global,
    };
    let icon = non_empty(m.get("icon")).filter(|i| valid_icon(i)).unwrap_or_else(|| "puzzle".into());
    let timeout = match m.get("timeout").and_then(Value::as_f64) {
        Some(s) if s.is_finite() && s > 0.0 => Duration::from_secs_f64(s).min(MAX_TIMEOUT).max(Duration::from_secs(1)),
        _ => DEFAULT_TIMEOUT,
    };
    Ok(Manifest {
        name: non_empty(m.get("name")).unwrap_or_else(|| id.to_string()),
        description: non_empty(m.get("description")),
        icon,
        command,
        output,
        scope,
        refresh: m.get("refresh").and_then(Value::as_f64).and_then(clamp_refresh),
        timeout,
    })
}

/// `command[0]` → 实际 spawn 的程序。带路径分隔符的（`./run.sh`、`bin/x`）相对插件目录——
/// spawn 时不 chdir 也一样找得到；绝对路径原样；裸名字原样交给 spawn 走 PATH（插件进程的 PATH
/// 是登录环境的那份，与终端里一致）
pub fn resolve_program(dir: &Path, program: &str) -> PathBuf {
    let p = Path::new(program);
    if p.is_absolute() {
        return p.to_path_buf();
    }
    let has_sep = program.contains('/') || (cfg!(windows) && program.contains('\\'));
    if has_sep { dir.join(p) } else { PathBuf::from(program) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_names() {
        assert!(valid_id("hello"));
        assert!(valid_id("disk-usage_2"));
        assert!(valid_id("0x"));
        for bad in ["", "Hello", "-x", "_x", "a.b", "a b", "中文", &"a".repeat(49)] {
            assert!(!valid_id(bad), "{bad}");
        }
        assert!(valid_icon("hard-drive"));
        assert!(!valid_icon("HardDrive"));
        assert!(!valid_icon("../x"));
        assert!(valid_param("machine"));
        assert!(valid_param("m_2"));
        assert!(!valid_param("2m"));
        assert!(!valid_param("Machine"));
        assert!(!valid_param("a-b"));
        assert!(valid_action("restart"));
        assert!(valid_action("svc:web.restart"));
        assert!(!valid_action("a b"));
        assert!(!valid_action(""));
    }

    #[test]
    fn refresh_is_clamped() {
        assert_eq!(clamp_refresh(0.0), None);
        assert_eq!(clamp_refresh(-3.0), None);
        assert_eq!(clamp_refresh(f64::NAN), None);
        assert_eq!(clamp_refresh(0.1), Some(MIN_REFRESH));
        assert_eq!(clamp_refresh(5.0), Some(5.0));
        assert_eq!(clamp_refresh(1e9), Some(MAX_REFRESH));
    }

    #[test]
    fn minimal_and_full_manifests() {
        let m = parse_manifest("up", r#"{"command":"uptime","output":"text"}"#).unwrap();
        assert_eq!(m.name, "up");
        assert_eq!(m.command, vec!["uptime"]);
        assert_eq!(m.output, Output::Text);
        assert_eq!(m.icon, "puzzle");
        assert_eq!(m.scope, PluginScope::Global);
        assert_eq!(m.refresh, None);
        assert_eq!(m.timeout, DEFAULT_TIMEOUT);

        let m = parse_manifest(
            "hello",
            r#"{"api":1,"name":" 你好 ","description":"示例","icon":"hand","command":["./run.sh","--x"],
                "scope":"project","refresh":0.2,"timeout":999}"#,
        )
        .unwrap();
        assert_eq!(m.name, "你好");
        assert_eq!(m.description.as_deref(), Some("示例"));
        assert_eq!(m.icon, "hand");
        assert_eq!(m.command, vec!["./run.sh", "--x"]);
        assert_eq!(m.output, Output::Json);
        assert_eq!(m.scope, PluginScope::Project);
        assert_eq!(m.refresh, Some(MIN_REFRESH));
        assert_eq!(m.timeout, MAX_TIMEOUT);
    }

    #[test]
    fn sloppy_optional_fields_fall_back() {
        let m = parse_manifest("x", r#"{"command":["x"],"name":3,"icon":"Bad Icon","refresh":"5","timeout":-1}"#).unwrap();
        assert_eq!(m.name, "x");
        assert_eq!(m.icon, "puzzle");
        assert_eq!(m.refresh, None);
        assert_eq!(m.timeout, DEFAULT_TIMEOUT);
    }

    #[test]
    fn broken_manifests_say_why() {
        let err = |t: &str| parse_manifest("x", t).unwrap_err();
        assert!(err("{").contains("不是合法的 JSON"));
        assert!(err("[]").contains("JSON 对象"));
        assert!(err("{}").contains("缺少 command"));
        assert!(err(r#"{"command":""}"#).contains("要么是字符串"));
        assert!(err(r#"{"command":[]}"#).contains("第一项"));
        assert!(err(r#"{"command":[1]}"#).contains("只能放字符串"));
        assert!(err(r#"{"command":"x","output":"html"}"#).contains("json 或 text"));
        assert!(err(r#"{"command":"x","api":2}"#).contains("先升级 falcon"));
    }

    #[test]
    fn program_resolution() {
        let dir = Path::new("/p/hello");
        assert_eq!(resolve_program(dir, "./run.sh"), PathBuf::from("/p/hello/./run.sh"));
        assert_eq!(resolve_program(dir, "bin/x"), PathBuf::from("/p/hello/bin/x"));
        assert_eq!(resolve_program(dir, "python3"), PathBuf::from("python3"));
        #[cfg(unix)]
        assert_eq!(resolve_program(dir, "/usr/bin/env"), PathBuf::from("/usr/bin/env"));
    }
}
