//! 列表里显示的名字。会话的自动标题规则在 falcon-core 的 session_title（与 web 的
//! `lib/sessionTitle.ts` 同一份逻辑、同一组测试向量）：侧栏、总览、命令面板必须叫同一个名字，
//! 而且要与 web 叫同一个名字。窗口标题栏例外——没标题时那一格空着，不编占位名。

use falcon_proto::{Project, SessionWithProject};
use rust_i18n::t;

/// 列表里用的名字（有兜底：shell 命令名）
pub fn session_label(s: &SessionWithProject, projects: &[Project]) -> String {
    falcon_core::session_title::session_label(&s.session, projects)
}

/// 标题栏用的名字：可能为空
pub fn session_title(s: &SessionWithProject) -> Option<String> {
    falcon_core::session_title::session_title(&s.session)
}

/// 相对空闲时间（web useActions 的 idleText）
pub fn idle_text(last_active_at: i64) -> String {
    let now = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let min = (now - last_active_at).max(0) / 60_000;
    if min < 1 {
        return t!("overview.idleNow").to_string();
    }
    if min < 60 {
        return t!("overview.idleMin", n = min).to_string();
    }
    let hours = min / 60;
    if hours < 24 {
        return t!("overview.idleHour", n = hours).to_string();
    }
    t!("overview.idleDay", n = hours / 24).to_string()
}

/// 本机项目的简称（"本机"）
pub fn local_word() -> String {
    t!("project.typeLocalShort").to_string()
}
