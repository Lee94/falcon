//! Agent 的身份图标与会话状态分开：各入口共用同一图形，颜色由调用方的主题决定。
//! 图形按 24px 网格绘制，在侧栏的 14px 下也能靠轮廓区分，不依赖品牌色。

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
