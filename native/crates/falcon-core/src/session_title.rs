//! 会话在界面上显示成什么。对应 web 的 `lib/sessionTitle.ts`。
//!
//! 侧栏、命令面板、会话总览、各种确认框必须叫同一个名字——现在还要与 web 叫同一个
//! 名字（设计文档 §4.3，`tests/vectors/sessionTitle.json` 兜住）。窗口标题栏例外：它右边
//! 就是完整工作目录，没标题时那一格空着，别编占位名填进去。

use falcon_proto::{Project, Session, SessionAgent};

use crate::js::js_trim;

/// 会话的自动标题。
///
/// 优先级：手起的名字 > 前台命令（后端探的自动标题）> agent 的 CLI 名。
/// 三样都没有就返回 `None`——**这时不要编一个占位名出来**，调用方各自兜底：
/// 窗口标题栏右边紧跟着完整工作目录，什么都不显示才对；侧栏只有一个文本槽，
/// 落到 [`shell_label`]。
///
/// agent 兜底取 CLI 名小写（claude / codex / grok），与前台命令同形：CLI 正跑着时
/// 探到的前台命令本来就是同一个词，它退出回到 shell 也不会跳成另一种写法。
pub fn session_title(session: &Session) -> Option<String> {
    title_of(&session.name, session.title.as_deref(), session.agent)
}

/// [`session_title`] 的拆开版：只要这三样（TS 的 `Pick<Session, "name" | "title" | "agent">`）
pub fn title_of(name: &str, title: Option<&str>, agent: Option<SessionAgent>) -> Option<String> {
    let name = js_trim(name);
    if !name.is_empty() {
        return Some(name.to_string());
    }
    // 全空白的标题等同于没有，不能让一行空白顶掉后面的兜底
    if let Some(title) = title.map(js_trim).filter(|t| !t.is_empty()) {
        return Some(title.to_string());
    }
    agent.map(|a| a.as_str().to_string())
}

/// [`session_label`] 查 shell 用的最小面：项目 id 与它覆盖的 shell。
/// TS 那边收 `readonly { id: string; shell?: string }[]`，这里用 trait 表达同一件事。
pub trait ProjectShell {
    fn project_id(&self) -> &str;
    fn shell(&self) -> Option<&str>;
}

impl ProjectShell for Project {
    fn project_id(&self) -> &str {
        &self.id
    }
    fn shell(&self) -> Option<&str> {
        self.shell.as_deref()
    }
}

/// 会话在**列表**里显示成什么：自动标题，空闲 shell 落到 shell 的命令名。
///
/// 侧栏、会话总览、命令面板、各种确认框共用一份——同一个会话在这些地方必须叫同一个
/// 名字，否则用户没法把它们对应起来。
pub fn session_label<P: ProjectShell>(session: &Session, projects: &[P]) -> String {
    session_title(session).unwrap_or_else(|| {
        let shell = projects.iter().find(|p| p.project_id() == session.project_id).and_then(|p| p.shell());
        shell_label(shell)
    })
}

/// 空闲会话在侧栏顶上的那一格：shell 的命令名（zsh / bash / powershell）。
///
/// 用命令名而不是"终端 3"这类标签，是为了跟前台命令同一形态——同一行里这一格
/// 要么是 `pnpm dev`，要么是 `zsh`，读的人不用分辨"这是名字还是在跑的东西"。
///
/// 路径两种分隔符都切：`project.shell` 可能是远端 Windows 上的 `C:\...\pwsh.exe`，
/// 而这里不能用 `std::path`（客户端平台 ≠ 宿主机平台）。
pub fn shell_label(shell: Option<&str>) -> String {
    let leaf = shell.unwrap_or("").rsplit(['/', '\\']).next().unwrap_or("");
    let bare = strip_exe_suffix(leaf);
    if bare.is_empty() { "shell".to_string() } else { bare.to_string() }
}

/// `/\.(exe|cmd|bat)$/i`
fn strip_exe_suffix(leaf: &str) -> &str {
    if leaf.len() >= 4 && leaf.is_char_boundary(leaf.len() - 4) {
        let (head, tail) = leaf.split_at(leaf.len() - 4);
        if [".exe", ".cmd", ".bat"].iter().any(|s| tail.eq_ignore_ascii_case(s)) {
            return head;
        }
    }
    leaf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suffix_is_case_insensitive_and_only_at_the_end() {
        assert_eq!(shell_label(Some(r"C:\Windows\System32\CMD.EXE")), "CMD");
        assert_eq!(shell_label(Some("/usr/bin/bash.exe.bak")), "bash.exe.bak");
        assert_eq!(shell_label(Some(".exe")), "shell");
        assert_eq!(shell_label(Some("/opt/中文壳")), "中文壳");
    }

    #[test]
    fn agent_falls_back_to_its_wire_literal() {
        assert_eq!(title_of("", None, Some(SessionAgent::Grok)).as_deref(), Some("grok"));
        // 已知出入：新服务端的 agent（比如 "gemini"）在 falcon-proto 里落成 Unknown，
        // 原值已丢，这里只能叫 "unknown"；web 会原样显示 "gemini"
        assert_eq!(title_of("", None, Some(SessionAgent::Unknown)).as_deref(), Some("unknown"));
    }
}
