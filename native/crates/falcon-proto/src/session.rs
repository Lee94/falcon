//! 终端会话（Terminal Session）。
//!
//! 状态机 active / unverified / dead 与 Detach / Terminate 的区分照 CONTEXT.md，
//! 客户端不自己发明语义：关闭终端窗口 = Terminate，Shift+关闭 / 退出 App = Detach。

use serde::{Deserialize, Serialize};

use crate::project::ProjectType;
use crate::term_env::TermAppearance;
use crate::wire::wire_enum;
use crate::zellij::NonDurableReason;

wire_enum! {
    /// 会话状态（CONTEXT.md「会话状态」）：
    /// - active：后端持有 PTY / 连接、确认存活。有无 Viewer 在看不影响此状态。
    /// - unverified（待接回）：链路中断或后端重启后，持久会话理论上仍存活但尚未验证。
    /// - dead（已丢失）：确认死亡，保留在列表中标注原因，由用户手动清除。
    ///
    /// Rust 侧加了 `Unknown` 兜底：状态类，而且会话列表每 5s 轮询一次，一个新状态
    /// 不该让整张列表解不出来。客户端对 `Unknown` 的合理处理是当成 unverified 显示。
    pub enum SessionState open {
        Active => "active",
        Unverified => "unverified",
        Dead => "dead",
    }
}

wire_enum! {
    /// 会话为什么死了。
    ///
    /// Rust 侧加了 `Unknown` 兜底：原因类，服务端会随新的故障模式加值。
    pub enum DeadReason open {
        /// shell 退出（持久会话则是 Zellij 会话在宿主机上也没了）
        Exited => "exited",
        /// 非持久会话遭遇后端重启
        BackendRestart => "backend-restart",
        /// 非持久的 SSH 会话，SSH 链路断了（持久会话此时是 unverified，不会死）
        LinkLost => "link-lost",
        /// 接回时发现 Zellij 会话已经不存在
        SessionGone => "session-gone",
    }
}

wire_enum! {
    /// 会话开出来直接跑的 CLI（ADR 0013）。缺省（`None`）就是普通 shell。
    ///
    /// 只是"用什么命令开场"，不改变会话的其它性质：CLI 退出后落回登录 shell，
    /// 会话还在（见 server/sessions/agent.ts 的启动脚本）。
    ///
    /// Rust 侧加了 `Unknown` 兜底：`SESSION_AGENTS` 注定会加新 CLI，新服务端上开的
    /// agent 会话不该让老客户端的会话列表解不出来。
    pub enum SessionAgent open {
        Claude => "claude",
        Codex => "codex",
        Grok => "grok",
    }
}

/// TS 的 `SESSION_AGENTS`：＋ 菜单里按这个顺序列各家 CLI。
pub const SESSION_AGENTS: &[SessionAgent] = SessionAgent::ALL;

/// `POST /api/projects/:id/sessions`、`POST /api/sessions/:id/reattach` 的响应。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: String,
    pub project_id: String,
    /// 用户手起的名字。**空串 = 没起过名**，此时 UI 显示 title / agent / 工作目录，
    /// 不再摆 "Terminal 3" 这种占位名：会话挂在侧栏树的哪个检出下面、窗口标题栏右边
    /// 的工作目录，已经把"这是谁"说完了，序号只是噪音。
    pub name: String,
    /// 自动标题：宿主机上这个会话**此刻**的前台命令（`pnpm dev`、`vim x.ts`），
    /// 空闲在 shell 里时为空。后端探测（manager.foreground）得来，不入库。
    ///
    /// 只在有 Viewer 看着时刷新，所以后台会话上可能是陈旧值——见 manager 的
    /// scheduleTitleProbe 注释。name 非空时 UI 一律以 name 为准。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub state: SessionState,
    /// 开场跑的 CLI；缺省是普通 shell
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<SessionAgent>,
    /// 持久会话 = Zellij 包装，可在断链 / 后端重启后接回
    pub durable: bool,
    /// durable=false 时的具体原因，UI 据此标注"为什么不持久"而非笼统的"非持久"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub non_durable_reason: Option<NonDurableReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dead_reason: Option<DeadReason>,
    /// unix 毫秒
    pub created_at: i64,
    /// unix 毫秒
    pub last_active_at: i64,
}

/// `GET /api/sessions` 的元素。TS 里是 `extends Session`，这里用 flatten 摊平，
/// 线上形状不变；`Deref` 到 [`Session`]，读字段不用多写一层。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionWithProject {
    #[serde(flatten)]
    pub session: Session,
    /// 项目行已被删时服务端给 `"?"`
    pub project_name: String,
    pub project_type: ProjectType,
}

impl std::ops::Deref for SessionWithProject {
    type Target = Session;
    fn deref(&self) -> &Session {
        &self.session
    }
}

/// `GET /api/sessions/:id/foreground` 的返回：关窗口前问"有没有程序在跑"
/// （设计文档 §4.3：2s 超时）。
///
/// 侦测不到（非持久 SSH、链路不通、探测失败）时 busy 恒为 false——
/// 它是道保险，自己坏了不能把关窗口拦下来。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionForeground {
    pub busy: bool,
    /// busy=true 时正在跑的命令行，供确认框展示
    #[serde(default)]
    pub command: Option<String>,
}

/// 粘贴图片的大小上限。Retina 全屏截图的 PNG 能到 10 MB 上下，取 20 MB；
/// 前端超限时不发请求（React 版当初还直接提示，falcon-ui 眼下只记一条日志），
/// 后端的 bodyLimit 用同一个数兜底。
pub const PASTE_IMAGE_MAX_BYTES: u64 = 20 * 1024 * 1024;

/// `POST /api/sessions/:id/paste-image` 的返回：图片在会话宿主机上的绝对路径
/// （CONTEXT.md「Image Paste」：把这个路径粘进终端输入，图片本身永远不进 PTY）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PasteImageResult {
    pub path: String,
}

/// 创建会话（`POST /api/projects/:id/sessions`）。appearance 是当前终端配色的深浅，
/// 不是界面主题——用户可以浅色 UI + Solarized Dark。后端据此写 COLORFGBG / GROK_APPEARANCE。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct CreateSessionRequest {
    /// 留空 = 不起名，UI 走自动标题（见 Session.name）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 开场跑的 CLI（claude / codex / grok）；缺省是普通 shell。
    /// 服务端认不出的一律当普通终端，不 400。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<SessionAgent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appearance: Option<TermAppearance>,
    /// #rrggbb，给 OSC 11 用；缺省按 appearance 给黑 / 白
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreground: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::roundtrip;

    #[test]
    fn agents_match_ts() {
        let names: Vec<_> = SESSION_AGENTS.iter().map(|a| a.as_str()).collect();
        assert_eq!(names, ["claude", "codex", "grok"]);
        assert_eq!(SessionAgent::from_wire("codex"), Some(SessionAgent::Codex));
        assert_eq!(SessionAgent::from_wire("unknown"), None);
        assert_eq!(SessionAgent::from_wire("gemini"), None);
    }

    #[test]
    fn session_active_agent() {
        let s = roundtrip::<Session>(
            r#"{"id":"s1","projectId":"p1","name":"","state":"active","durable":true,
                "agent":"claude","createdAt":1758700000000,"lastActiveAt":1758700100000}"#,
        );
        assert_eq!(s.agent, Some(SessionAgent::Claude));
        assert!(s.name.is_empty());
    }

    #[test]
    fn session_dead_non_durable() {
        let s = roundtrip::<Session>(
            r#"{"id":"s2","projectId":"p2","name":"build","state":"dead","durable":false,
                "nonDurableReason":"not-authorized","deadReason":"backend-restart",
                "createdAt":1758700000000,"lastActiveAt":1758700000000}"#,
        );
        assert_eq!(s.dead_reason, Some(DeadReason::BackendRestart));
        assert_eq!(s.non_durable_reason, Some(NonDurableReason::NotAuthorized));
    }

    #[test]
    fn session_list_with_project() {
        let list = roundtrip::<Vec<SessionWithProject>>(
            r#"[{"id":"s1","projectId":"p1","name":"","title":"pnpm dev","state":"active","durable":true,
                 "createdAt":1758700000000,"lastActiveAt":1758700100000,"projectName":"mojito","projectType":"local"},
                {"id":"s3","projectId":"gone","name":"x","state":"unverified","durable":true,
                 "createdAt":1758700000000,"lastActiveAt":1758700000000,"projectName":"?","projectType":"ssh"}]"#,
        );
        assert_eq!(list[0].title.as_deref(), Some("pnpm dev")); // 经 Deref
        assert_eq!(list[1].project_type, ProjectType::Ssh);
    }

    #[test]
    fn newer_server_values_do_not_break_the_list() {
        // 新服务端多了状态、原因、agent：列表照样解得出来，认不出的落到 Unknown
        let list: Vec<SessionWithProject> = serde_json::from_str(
            r#"[{"id":"s1","projectId":"p1","name":"","state":"hibernating","durable":true,
                 "agent":"gemini","createdAt":0,"lastActiveAt":0,"projectName":"a","projectType":"local"},
                {"id":"s2","projectId":"p1","name":"","state":"dead","deadReason":"oom-killed","durable":false,
                 "nonDurableReason":"rootless","createdAt":0,"lastActiveAt":0,"projectName":"a","projectType":"local"}]"#,
        )
        .unwrap();
        assert_eq!(list[0].state, SessionState::Unknown);
        assert_eq!(list[0].agent, Some(SessionAgent::Unknown));
        assert_eq!(list[1].dead_reason, Some(DeadReason::Unknown));
        assert_eq!(list[1].non_durable_reason, Some(NonDurableReason::Unknown));
        // 原值已丢；写回的是 "unknown"，客户端不该把它再发回服务端
        assert_eq!(serde_json::to_string(&SessionState::Unknown).unwrap(), r#""unknown""#);
        assert_eq!(SessionState::Unknown.as_str(), "unknown");
        assert_eq!(SessionState::from_wire("unknown"), None);
    }

    #[test]
    fn foreground_and_paste() {
        roundtrip::<SessionForeground>(r#"{"busy":true,"command":"vim src/main.rs"}"#);
        let idle = roundtrip::<SessionForeground>(r#"{"busy":false,"command":null}"#);
        assert_eq!(idle.command, None);
        roundtrip::<PasteImageResult>(
            r#"{"path":"/Users/fay/.falcon/paste/20260924-101010-ab12.png"}"#,
        );
        assert_eq!(PASTE_IMAGE_MAX_BYTES, 20_971_520);
    }

    #[test]
    fn create_request() {
        roundtrip::<CreateSessionRequest>("{}");
        let r = roundtrip::<CreateSessionRequest>(
            r##"{"agent":"codex","appearance":"light","background":"#ffffff","foreground":"#171717"}"##,
        );
        assert_eq!(r.appearance, Some(TermAppearance::Light));
        roundtrip::<CreateSessionRequest>(r#"{"name":"deploy"}"#);
    }
}
