//! Agent 的身份图标与会话状态分开：各入口共用同一图形。
//! GPUI 把 SVG 画成单色蒙版，再铺调用方的文字色，所以用各家已发布标志的外形
//! （`fill="currentColor"`），不另画一套。出处写在各个 SVG 里。
//! 铺满画布的底色会让蒙版变成实心方块；画布留白过多时，14px 的侧栏里标志会缩得很小。
//! 这两种情况都按标志本身裁过。

use falcon_proto::SessionAgent;
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;

/// 普通 shell 保留终端图标；新服务端传来的未知 agent 使用通用机器人。
pub fn session_icon(agent: Option<SessionAgent>) -> Icon {
    let data: &[u8] = match agent {
        Some(SessionAgent::Claude) => include_bytes!("../assets/agents/claude.svg"),
        Some(SessionAgent::Codex) => include_bytes!("../assets/agents/codex.svg"),
        Some(SessionAgent::Grok) => include_bytes!("../assets/agents/grok.svg"),
        Some(SessionAgent::Devin) => include_bytes!("../assets/agents/devin.svg"),
        Some(SessionAgent::Gemini) => include_bytes!("../assets/agents/gemini.svg"),
        Some(SessionAgent::OpenCode) => include_bytes!("../assets/agents/opencode.svg"),
        Some(SessionAgent::Cursor) => include_bytes!("../assets/agents/cursor-agent.svg"),
        Some(SessionAgent::Copilot) => include_bytes!("../assets/agents/copilot.svg"),
        Some(SessionAgent::Aider) => include_bytes!("../assets/agents/aider.svg"),
        Some(SessionAgent::Amp) => include_bytes!("../assets/agents/amp.svg"),
        Some(SessionAgent::Qwen) => include_bytes!("../assets/agents/qwen.svg"),
        Some(SessionAgent::Droid) => include_bytes!("../assets/agents/droid.svg"),
        Some(SessionAgent::Agy) => include_bytes!("../assets/agents/agy.svg"),
        Some(SessionAgent::Omp) => include_bytes!("../assets/agents/omp.svg"),
        Some(_) => return Icon::new(IconName::Bot),
        None => return Icon::new(IconName::SquareTerminal),
    };
    Icon::default().data(data)
}
