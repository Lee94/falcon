//! 内置 agent 的工具（docs/design/ai-assistant.md 6.3）：定义、系统提示词、参数校验、文件读写。
//!
//! 只给四个：`run_command`（打进对话终端的 shell，manager/machine_run.rs）、`read_file`、
//! `write_file`、`edit_file`。文件操作单独成工具而不是让模型 `cat` / `sed`：卡片能画 diff、审批
//! 能分开（读文件自动放行，写要问）——这正是 claude-api skill 说的「要拦、要渲染的动作升格成专用
//! 工具」。
//!
//! 文件工具作用于这段对话绑定的机器：本机直接读写，SSH POSIX 主机走 exec 通道；Windows 宿主机
//! 的文件工具 v1 不做（报错让模型改用 run_command）。相对路径按 shell 的当前目录（OSC 7）解析。
//! 写文件**原地覆盖内容**（`cat 临时文件 > 目标`），不换 inode：脚本的执行位、文件的属主都留着。

use std::rc::Rc;

use falcon_proto::assistant::{ToolContent, ToolKind};
use serde_json::{Value, json};

use super::claude::tool;
use crate::exec::Exec as _;
use crate::host::{HostKind, quote_posix};
use crate::sessions::ssh::SshLink;

/// 读文件的大小上限：再大就不是该整个读进上下文的东西了
const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;
/// read_file 缺省给多少行
const DEFAULT_READ_LINES: usize = 2000;
/// 给模型的命令输出最多这么多字符（掐中间）
pub const MAX_TOOL_OUTPUT: usize = 30_000;

pub fn definitions() -> Vec<Value> {
    vec![
        tool(
            "run_command",
            "Run a shell command on this machine and return its output and exit code. The command is typed into \
             the user's own interactive shell - the terminal of this conversation - so the working directory \
             and environment persist between commands (a `cd` stays in effect) and the user watches the output \
             live. Stops waiting after timeout_secs (default 120, max 600); a command that is still running then \
             keeps running in the user's terminal.",
            json!({
                "command": { "type": "string", "description": "The command line. Prefer a single line." },
                "description": {
                    "type": "string",
                    "description": "One short sentence, in the user's language, saying what this command does and why. \
                                    Shown to the user above the command while they decide whether to run it."
                },
                "timeout_secs": { "type": "integer", "description": "How long to wait for it, in seconds." }
            }),
            &["command", "description"],
        ),
        tool(
            "read_file",
            "Read a text file on this machine. Returns numbered lines. Relative paths resolve against the shell's \
             current directory.",
            json!({
                "path": { "type": "string" },
                "offset": { "type": "integer", "description": "First line to return, 1-based." },
                "limit": { "type": "integer", "description": "How many lines to return (default 2000)." }
            }),
            &["path"],
        ),
        tool(
            "write_file",
            "Create a text file or replace its entire content. Parent directories are created as needed.",
            json!({
                "path": { "type": "string" },
                "content": { "type": "string", "description": "The full new content of the file." }
            }),
            &["path", "content"],
        ),
        tool(
            "edit_file",
            "Replace an exact substring in a text file. old_string must occur exactly once unless replace_all is \
             true - include enough surrounding lines to make it unique.",
            json!({
                "path": { "type": "string" },
                "old_string": { "type": "string" },
                "new_string": { "type": "string" },
                "replace_all": { "type": "boolean" }
            }),
            &["path", "old_string", "new_string"],
        ),
    ]
}

/// 这台机器的事实（建对话时探一次，写进系统提示词）
#[derive(Debug, Clone, Default)]
pub struct MachineFacts {
    /// 「本机」或 SSH Host 的显示名
    pub label: String,
    pub os: String,
    pub shell: String,
    pub user: Option<String>,
    pub home: Option<String>,
    pub windows: bool,
}

/// 系统提示词。只放不变的东西（缓存的前缀）；当前目录每轮随用户消息带（[`turn_preamble`]）
pub fn system_prompt(facts: &MachineFacts) -> String {
    let user = facts.user.as_deref().unwrap_or("unknown");
    let home = facts.home.as_deref().unwrap_or("unknown");
    format!(
        "You are the AI assistant built into Falcon, a terminal workbench. You work on one machine on the \
         user's behalf.\n\
         \n\
         Machine: {label}\nOS: {os}\nShell: {shell}\nUser: {user}\nHome: {home}\n\
         \n\
         How to work:\n\
         - run_command types the command into the user's own shell, the terminal of this conversation. \
         The user watches it run; the working directory and environment carry over between commands.\n\
         - Commands must not wait for keyboard input. Use non-interactive flags (-y, --no-pager, GIT_PAGER=cat, \
         PAGER=cat) and avoid full-screen programs (vim, less, top). If something needs sudo or a password, say \
         so and let the user type it in the terminal themselves.\n\
         - Prefer one-line commands. Do not start servers, watchers or other long-running programs unless the \
         user asks for one; they would occupy the user's shell.\n\
         - Use read_file, write_file and edit_file for files rather than cat, heredocs or sed.\n\
         - The user approves each command and file change. Before anything destructive or hard to undo \
         (deleting files, force-pushing, killing processes, changing system configuration), say what you are \
         about to do and why.\n\
         - Reply in the language the user writes in. Keep replies short: the user can already see command \
         output.",
        label = facts.label,
        os = facts.os,
        shell = facts.shell,
    )
}

/// 每条用户消息前面带的一小段：shell 的当前目录会变，放在这里不打穿系统提示词的缓存
pub fn turn_preamble(cwd: Option<&str>) -> Option<String> {
    cwd.map(|c| format!("<environment>Shell working directory: {c}</environment>"))
}

// ---------------------------------------------------------------- 参数

#[derive(Debug, Clone, PartialEq)]
pub enum ToolInput {
    /// `description`：这一步要做什么（给人看，卡片上面那一行）。schema 里是必填，这里宽松：没给不算错
    Run { command: String, timeout_secs: u64, description: Option<String> },
    Read { path: String, offset: usize, limit: usize },
    Write { path: String, content: String },
    Edit { path: String, old_string: String, new_string: String, replace_all: bool },
}

impl ToolInput {
    /// 按 schema 严格校验（eager 模式下服务端不替我们校验）
    pub fn parse(name: &str, input: &Value) -> Result<ToolInput, String> {
        let str_field = |k: &str| -> Result<String, String> {
            input.get(k).and_then(Value::as_str).map(str::to_string).ok_or_else(|| format!("missing string field `{k}`"))
        };
        let uint = |k: &str| -> Result<Option<u64>, String> {
            match input.get(k) {
                None | Some(Value::Null) => Ok(None),
                Some(v) => v.as_u64().map(Some).ok_or_else(|| format!("`{k}` must be a non-negative integer")),
            }
        };
        match name {
            "run_command" => {
                let command = str_field("command")?;
                if command.trim().is_empty() {
                    return Err("`command` is empty".into());
                }
                let description = input
                    .get("description")
                    .and_then(Value::as_str)
                    .map(|d| d.trim().to_string())
                    .filter(|d| !d.is_empty());
                Ok(ToolInput::Run { command, timeout_secs: uint("timeout_secs")?.unwrap_or(120).clamp(1, 600), description })
            }
            "read_file" => Ok(ToolInput::Read {
                path: str_field("path")?,
                offset: uint("offset")?.unwrap_or(1).max(1) as usize,
                limit: uint("limit")?.map(|l| l as usize).unwrap_or(DEFAULT_READ_LINES).max(1),
            }),
            "write_file" => Ok(ToolInput::Write { path: str_field("path")?, content: str_field("content")? }),
            "edit_file" => {
                let old_string = str_field("old_string")?;
                if old_string.is_empty() {
                    return Err("`old_string` is empty; use write_file to create a file".into());
                }
                Ok(ToolInput::Edit {
                    path: str_field("path")?,
                    old_string,
                    new_string: str_field("new_string")?,
                    replace_all: input.get("replace_all").and_then(Value::as_bool).unwrap_or(false),
                })
            }
            other => Err(format!("unknown tool `{other}`")),
        }
    }

    pub fn kind(&self) -> ToolKind {
        match self {
            ToolInput::Run { .. } => ToolKind::Execute,
            ToolInput::Read { .. } => ToolKind::Read,
            ToolInput::Write { .. } | ToolInput::Edit { .. } => ToolKind::Edit,
        }
    }

    /// 卡片标题：命令本身（第一行），或路径。动词由界面按 kind 配（界面文案不在服务端写死）
    pub fn title(&self) -> String {
        match self {
            ToolInput::Run { command, .. } => {
                let first = command.lines().next().unwrap_or("");
                let more = if command.lines().nth(1).is_some() { " …" } else { "" };
                format!("{}{more}", first.chars().take(200).collect::<String>())
            }
            ToolInput::Read { path, .. } | ToolInput::Write { path, .. } | ToolInput::Edit { path, .. } => path.clone(),
        }
    }

    /// 卡片上面那一行「这一步要做什么」：只有命令带（文件工具的路径本身就说明了）
    pub fn description(&self) -> Option<String> {
        match self {
            ToolInput::Run { description, .. } => description.clone(),
            _ => None,
        }
    }

    /// 要不要你批准：读文件不用问，命令与写文件要问
    pub fn needs_approval(&self) -> bool {
        !matches!(self, ToolInput::Read { .. })
    }

    /// 「本对话内总是允许」按工具记
    pub fn approval_key(&self) -> &'static str {
        match self {
            ToolInput::Run { .. } => "run_command",
            ToolInput::Read { .. } => "read_file",
            ToolInput::Write { .. } => "write_file",
            ToolInput::Edit { .. } => "edit_file",
        }
    }

    /// 等批准时卡片上先画出来的内容：写 / 改文件给 diff（改之前的全文要等真的读到才有，
    /// 这里先只给新内容 / 片段）
    pub fn preview(&self) -> Vec<ToolContent> {
        match self {
            ToolInput::Write { path, content } => {
                vec![ToolContent::Diff { path: path.clone(), old_text: None, new_text: content.clone() }]
            }
            ToolInput::Edit { path, old_string, new_string, .. } => vec![ToolContent::Diff {
                path: path.clone(),
                old_text: Some(old_string.clone()),
                new_text: new_string.clone(),
            }],
            _ => Vec::new(),
        }
    }
}

// ---------------------------------------------------------------- 路径

/// 模型给的路径 → 这台机器上的绝对路径。`~` 是家目录，相对路径按 shell 的当前目录（没有就家目录）
pub fn resolve_path(path: &str, cwd: Option<&str>, home: Option<&str>) -> Result<String, String> {
    let path = path.trim();
    if path.is_empty() {
        return Err("path is empty".into());
    }
    if path.starts_with('/') {
        return Ok(path.to_string());
    }
    let home = home.filter(|h| !h.is_empty());
    if path == "~" {
        return home.map(str::to_string).ok_or("home directory unknown".into());
    }
    if let Some(rest) = path.strip_prefix("~/") {
        let home = home.ok_or("home directory unknown")?;
        return Ok(format!("{}/{rest}", home.trim_end_matches('/')));
    }
    let base = cwd.filter(|c| c.starts_with('/')).or(home).ok_or("cannot resolve a relative path: current directory unknown")?;
    Ok(format!("{}/{path}", base.trim_end_matches('/')))
}

// ---------------------------------------------------------------- 文件

/// 文件工具落到哪台机器
pub enum FileHost {
    Local,
    Remote(Rc<SshLink>),
    /// Windows 宿主机：v1 不做
    Unsupported,
}

fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8192).any(|b| *b == 0)
}

pub async fn read_text(host: &FileHost, path: &str) -> Result<Option<String>, String> {
    match host {
        FileHost::Unsupported => Err(unsupported()),
        FileHost::Local => {
            let meta = match tokio::fs::metadata(path).await {
                Ok(m) => m,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(format!("cannot stat {path}: {e}")),
            };
            if meta.is_dir() {
                return Err(format!("{path} is a directory; use run_command with ls"));
            }
            if meta.len() > MAX_FILE_BYTES {
                return Err(format!("{path} is {} bytes, too large to read whole", meta.len()));
            }
            let bytes = tokio::fs::read(path).await.map_err(|e| format!("cannot read {path}: {e}"))?;
            if looks_binary(&bytes) {
                return Err(format!("{path} looks like a binary file"));
            }
            String::from_utf8(bytes).map(Some).map_err(|_| format!("{path} is not valid UTF-8 text"))
        }
        FileHost::Remote(link) => {
            let q = quote_posix(path);
            let line = format!(
                "f={q}; if [ -d \"$f\" ]; then echo dir >&2; exit 5; fi; if [ ! -e \"$f\" ]; then exit 3; fi; \
                 s=$(wc -c < \"$f\") || exit 6; if [ \"$s\" -gt {MAX_FILE_BYTES} ]; then echo \"$s\" >&2; exit 4; fi; \
                 cat -- \"$f\""
            );
            let res = link.exec(&line, None).await.map_err(|e| format!("cannot reach the host: {e}"))?;
            match res.code {
                Some(0) => {
                    if res.stdout.contains('\0') || res.stdout.contains('\u{FFFD}') {
                        return Err(format!("{path} looks like a binary file"));
                    }
                    Ok(Some(res.stdout))
                }
                Some(3) => Ok(None),
                Some(4) => Err(format!("{path} is {} bytes, too large to read whole", res.stderr.trim())),
                Some(5) => Err(format!("{path} is a directory; use run_command with ls")),
                _ => Err(format!("cannot read {path}: {}", res.stderr.trim())),
            }
        }
    }
}

pub async fn write_text(host: &FileHost, path: &str, content: &str) -> Result<(), String> {
    match host {
        FileHost::Unsupported => Err(unsupported()),
        FileHost::Local => {
            let p = std::path::Path::new(path);
            if let Some(dir) = p.parent() {
                tokio::fs::create_dir_all(dir).await.map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
            }
            // 原地覆盖内容（truncate + write），不换 inode：执行位与属主都留着
            tokio::fs::write(p, content).await.map_err(|e| format!("cannot write {path}: {e}"))
        }
        FileHost::Remote(link) => {
            // 先完整落到临时文件、核对字节数，再拷进目标（原地覆盖，不 mv：执行位与属主留着）
            let q = quote_posix(path);
            let len = content.len();
            let line = format!(
                "f={q}; d=$(dirname -- \"$f\") && mkdir -p -- \"$d\" && t=\"$f.falcon-$$\" && cat > \"$t\" && \
                 [ \"$(wc -c < \"$t\" | tr -d ' ')\" = {len} ] && cat \"$t\" > \"$f\"; r=$?; rm -f \"$t\"; exit $r"
            );
            let res = link.exec_with_input(&line, content.as_bytes()).await.map_err(|e| format!("cannot reach the host: {e}"))?;
            if res.ok() { Ok(()) } else { Err(format!("cannot write {path}: {}", res.stderr.trim())) }
        }
    }
}

fn unsupported() -> String {
    "file tools are not available on Windows hosts yet; use run_command".into()
}

/// 带行号的片段（`cat -n` 的样子），超出部分说一声
pub fn numbered(content: &str, offset: usize, limit: usize) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();
    if total == 0 {
        return "(empty file)".into();
    }
    let start = offset.saturating_sub(1).min(total);
    let end = (start + limit).min(total);
    let mut out: String =
        lines[start..end].iter().enumerate().map(|(i, l)| format!("{:>6}\t{l}\n", start + i + 1)).collect();
    if start > 0 || end < total {
        out.push_str(&format!("(lines {}-{} of {total}; use offset/limit to read more)", start + 1, end));
    }
    out
}

/// edit_file 的替换：唯一命中才换（replace_all 时全换）
pub fn apply_edit(content: &str, old: &str, new: &str, replace_all: bool) -> Result<String, String> {
    let n = content.matches(old).count();
    match n {
        0 => Err("old_string was not found in the file".into()),
        1 => Ok(content.replacen(old, new, 1)),
        _ if replace_all => Ok(content.replace(old, new)),
        _ => Err(format!("old_string occurs {n} times; include more context or set replace_all")),
    }
}

pub fn file_host_for(kind: Option<HostKind>, link: Option<Rc<SshLink>>) -> FileHost {
    match (kind, link) {
        (Some(HostKind::Windows), _) => FileHost::Unsupported,
        (_, Some(link)) => FileHost::Remote(link),
        _ => FileHost::Local,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn definitions_are_strict_objects() {
        for t in definitions() {
            assert_eq!(t["input_schema"]["type"], "object");
            assert_eq!(t["input_schema"]["additionalProperties"], false);
            assert!(t.get("eager_input_streaming").is_none());
        }
    }

    #[test]
    fn inputs_are_validated() {
        assert_eq!(
            ToolInput::parse("run_command", &json!({"command": "ls"})).unwrap(),
            ToolInput::Run { command: "ls".into(), timeout_secs: 120, description: None }
        );
        let r = ToolInput::parse("run_command", &json!({"command": "free -h", "description": " 看看内存 "})).unwrap();
        assert_eq!(r.description().as_deref(), Some("看看内存"));
        assert_eq!(
            ToolInput::parse("run_command", &json!({"command": "x", "timeout_secs": 9999})).unwrap(),
            ToolInput::Run { command: "x".into(), timeout_secs: 600, description: None }
        );
        assert!(ToolInput::parse("run_command", &json!({"command": 3})).is_err());
        assert!(ToolInput::parse("run_command", &json!({"command": "  "})).is_err());
        assert!(ToolInput::parse("read_file", &json!({"path": "a", "limit": -1})).is_err());
        assert!(ToolInput::parse("edit_file", &json!({"path": "a", "old_string": "", "new_string": "b"})).is_err());
        assert!(ToolInput::parse("rm_rf", &json!({})).is_err());
        let r = ToolInput::parse("read_file", &json!({"path": "a"})).unwrap();
        assert!(!r.needs_approval());
        assert!(ToolInput::parse("write_file", &json!({"path": "a", "content": ""})).unwrap().needs_approval());
    }

    #[test]
    fn titles_are_the_command_or_path() {
        let multi = ToolInput::Run { command: "cd /x\nmake".into(), timeout_secs: 1, description: None };
        assert_eq!(multi.title(), "cd /x …");
        assert_eq!(ToolInput::Read { path: "/a/b".into(), offset: 1, limit: 1 }.title(), "/a/b");
    }

    #[test]
    fn paths_resolve_against_cwd_then_home() {
        let h = Some("/home/u");
        assert_eq!(resolve_path("/etc/hosts", Some("/x"), h).unwrap(), "/etc/hosts");
        assert_eq!(resolve_path("~", None, h).unwrap(), "/home/u");
        assert_eq!(resolve_path("~/a", None, h).unwrap(), "/home/u/a");
        assert_eq!(resolve_path("src/main.rs", Some("/repo/"), h).unwrap(), "/repo/src/main.rs");
        assert_eq!(resolve_path("notes", None, h).unwrap(), "/home/u/notes");
        assert!(resolve_path("x", None, None).is_err());
        assert!(resolve_path("", Some("/"), h).is_err());
    }

    #[test]
    fn numbered_slices_and_reports_ranges() {
        assert_eq!(numbered("a\nb\nc\n", 1, 10), "     1\ta\n     2\tb\n     3\tc\n");
        assert_eq!(numbered("a\nb\nc\n", 2, 1), "     2\tb\n(lines 2-2 of 3; use offset/limit to read more)");
        assert_eq!(numbered("", 1, 10), "(empty file)");
    }

    #[test]
    fn edits_need_a_unique_match() {
        assert_eq!(apply_edit("a b a", "b", "c", false).unwrap(), "a c a");
        assert!(apply_edit("a b a", "a", "c", false).unwrap_err().contains("2 times"));
        assert_eq!(apply_edit("a b a", "a", "c", true).unwrap(), "c b c");
        assert!(apply_edit("abc", "x", "y", false).is_err());
    }

    #[tokio::test]
    async fn local_files_roundtrip_and_keep_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/run.sh").to_string_lossy().into_owned();
        assert_eq!(read_text(&FileHost::Local, &path).await.unwrap(), None);
        write_text(&FileHost::Local, &path, "echo 1\n").await.unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            write_text(&FileHost::Local, &path, "echo 2\n").await.unwrap();
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o755);
        }
        assert_eq!(read_text(&FileHost::Local, &path).await.unwrap().as_deref(), Some("echo 2\n"));
        let bin = dir.path().join("b.bin");
        std::fs::write(&bin, [0u8, 1, 2]).unwrap();
        assert!(read_text(&FileHost::Local, &bin.to_string_lossy()).await.is_err());
        assert!(read_text(&FileHost::Local, &dir.path().to_string_lossy()).await.unwrap_err().contains("directory"));
    }
}
