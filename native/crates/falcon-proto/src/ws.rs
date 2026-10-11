//! WebSocket 协议：会话通道 `/ws/sessions/:id` 与 Zellij 安装通道 `/ws/install/:projectId`。
//!
//! 会话通道是混合协议：**终端字节走二进制帧**（1 字节类型头 + UTF-8 载荷，见
//! [`decode_term_frame`]），state / reconnecting / error / title / askpass 等控制消息
//! 走 JSON 文本帧（[`ServerMessage`]）。客户端发的一律是 JSON 文本帧（[`ClientMessage`]）。
//!
//! 连接时序（设计文档 §3.2）：每条新 socket 都**强制**发一次 `resize`（服务端拿它当持久会话
//! 懒惰接回的信号），以及 `appearance`（服务端的 OscColorGate 拿到它才代答 OSC 10/11/12）。
//!
//! 客户端先发 `appearance` 再发 `resize`，与旧 React 版（`TerminalView.tsx` 先 resize）相反，
//! 是有意的：resize 触发懒惰接回，zellij 一 attach 就查 OSC 11；React 版的 xterm.js 会自己兜底
//! 回答，现在的客户端不答颜色查询（多个 Viewer 会各答一遍），所以外观必须赶在接回之前到服务端。

use serde::{Deserialize, Serialize};

use crate::session::{DeadReason, SessionState};
use crate::term_env::TermAppearance;
use crate::zellij::{NonDurableReason, ZellijInstallStage};

// ---------------- 二进制帧 ----------------

/// 终端数据走二进制帧，不走 JSON：1 字节类型头 + UTF-8 载荷。
/// JSON 文本帧对 ANSI 密集数据的转义（`\x1b` → `\u001b`）会把线上字节膨胀
/// 1.3~1.7 倍，且每帧多一次全量转义扫描；二进制帧还让客户端能把字节直接喂给
/// VT 解析器，不必先解成 JSON 字符串（当初 React 版省的是一轮 UTF-8 → UTF-16 → UTF-8）。
///
/// 输出帧：服务端已按 16ms 合并，单帧很小。
pub const TERM_FRAME_OUTPUT: u8 = 0x01;
/// 回放帧：整份 Scrollback 快照（最大 4MB 的 RingBuffer），客户端整体替换终端状态
/// （falcon-term 在新 Term 上解析完再换上，等同先 reset 再写入）。可能在连接中途任意
/// 时刻到达——服务端背压重同步（慢 Viewer 排空后整体重放）也走这条。
pub const TERM_FRAME_REPLAY: u8 = 0x02;

/// 未认证时服务端关闭 WS 用的关闭码（`ws.ts` 的 `socket.close(4401, …)`）。
/// 收到它不要重连——重试只会无限 4401，应当走重新登录。
///
/// **shared 里没有这个常量**，照 `packages/server/src/ws.ts` 补上。
pub const WS_CLOSE_UNAUTHORIZED: u16 = 4401;

/// 服务端 WS 的单帧上限（`@fastify/websocket` 的 `maxPayload`）。超过的帧会被
/// 服务端直接断开连接，所以大段 `input`（粘贴）要分片发。注意这是整帧 JSON 的
/// 字节数，不是 `data` 的字节数——控制字符转义后会膨胀。
///
/// **shared 里没有这个常量**，照 `packages/server/src/index.ts` 补上。
pub const WS_MAX_PAYLOAD_BYTES: usize = 1024 * 1024;

/// 二进制帧的类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TermFrameKind {
    /// [`TERM_FRAME_OUTPUT`]：增量输出，直接喂给 VT 解析器
    Output,
    /// [`TERM_FRAME_REPLAY`]：整份回放，先 reset（或新建一个 Term 离锁解析后整体换上）再写
    Replay,
}

impl TermFrameKind {
    /// 帧头字节。
    pub const fn byte(self) -> u8 {
        match self {
            TermFrameKind::Output => TERM_FRAME_OUTPUT,
            TermFrameKind::Replay => TERM_FRAME_REPLAY,
        }
    }

    /// 按帧头字节取类型；认不出返回 `None`。
    pub const fn from_byte(b: u8) -> Option<Self> {
        match b {
            TERM_FRAME_OUTPUT => Some(TermFrameKind::Output),
            TERM_FRAME_REPLAY => Some(TermFrameKind::Replay),
            _ => None,
        }
    }
}

/// 解出来的一帧：类型 + 载荷切片（借用原帧，不拷贝）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TermFrame<'a> {
    pub kind: TermFrameKind,
    /// 终端字节流。服务端从 JS 字符串编码而来，每帧都是完整的 UTF-8；但客户端
    /// 不该依赖这一点做校验——直接把字节喂给 VT 解析器，它自己处理。
    pub payload: &'a [u8],
}

/// 解一帧二进制消息。空帧与认不出的类型头返回 `None`——照 React 版的做法，直接丢掉，
/// 不当成错误（新服务端加的帧类型，老客户端看不懂就不看）。
pub fn decode_term_frame(frame: &[u8]) -> Option<TermFrame<'_>> {
    let (&head, payload) = frame.split_first()?;
    let kind = TermFrameKind::from_byte(head)?;
    Some(TermFrame { kind, payload })
}

/// 编一帧二进制消息（服务端的 `encodeTermFrame`）。客户端只收不发，这个主要给测试
/// 与本地模拟服务端用。
pub fn encode_term_frame(kind: TermFrameKind, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(1 + payload.len());
    frame.push(kind.byte());
    frame.extend_from_slice(payload);
    frame
}

// ---------------- 会话通道的 JSON 控制消息 ----------------

/// 客户端 → 服务端（会话通道）。客户端写出的值全是自己构造的，不加 `Unknown` 兜底。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum ClientMessage {
    /// 键盘 / 粘贴 / IME 上屏的输入。整帧超过 [`WS_MAX_PAYLOAD_BYTES`] 要分片。
    #[serde(rename = "input")]
    Input { data: String },
    /// 终端格子数。服务端只收正整数；新 socket 必须强发一次（懒惰接回的信号），
    /// 之后格子数没变就别发（服务端每条 resize 都有落库逻辑）。
    #[serde(rename = "resize")]
    Resize { cols: u16, rows: u16 },
    /// 当前 Viewer 的终端深浅；主题切换时再推一次，供 OSC 10/11/12 答复
    #[serde(rename = "appearance")]
    Appearance {
        appearance: TermAppearance,
        /// `#rrggbb`
        #[serde(default, skip_serializing_if = "Option::is_none")]
        background: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        foreground: Option<String>,
    },
    /// 滚动条（ADR 0019）：`seek` 缺省是问一次滚动位置，给了是让 zellij 滚到
    /// 「视口下方还剩 seek 行」处。回话是广播的 [`ServerMessage::Scroll`]；会话不支持
    /// （非持久、升级前用老配置建的）时没有回话。
    #[serde(rename = "scroll")]
    Scroll {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seek: Option<u32>,
    },
    /// 应用层心跳：服务端回一条 [`ServerMessage::Pong`]。浏览器的 WebSocket 发不了 ping 帧，
    /// 浏览器版拿它代替（心跳与唤醒后的探活，falcon-client 的 `ws/web.rs`）；原生仍发 ping 帧。
    /// 浏览器版总由同一个服务端二进制托管，不会遇上不认它的旧服务端
    #[serde(rename = "ping")]
    Ping,
}

/// 服务端 → 客户端（会话通道）的控制消息。output / replay 见 `TERM_FRAME_*` 二进制帧。
///
/// Rust 侧加了 `Unknown` 兜底：新服务端加一种控制消息时，客户端能把"看不懂的消息"
/// 与"坏掉的 JSON"分开——前者照 React 版 `switch` 无 default 分支那样安静忽略。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum ServerMessage {
    /// 会话状态变化。连上时服务端也会先推一次当前状态（等 resize 的持久会话推
    /// unverified，已死的会话推 dead + 原因后就不再有下文）。
    #[serde(rename = "state")]
    State {
        state: SessionState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dead_reason: Option<DeadReason>,
    },
    /// SSH 断线自动重连中
    #[serde(rename = "reconnecting")]
    Reconnecting { attempt: u32 },
    /// 如"会话不存在"、接回失败的原因；一句可以直接展示的话
    #[serde(rename = "error")]
    Error { message: String },
    /// 前台命令变了（含变空：`null`），UI 据此换会话的自动标题
    #[serde(rename = "title")]
    Title {
        #[serde(default)]
        title: Option<String>,
    },
    /// sudo / SSH askpass：helper 在等密码，弹对话框；答复走
    /// `POST /api/askpass/:id/answer`
    #[serde(rename = "askpass")]
    Askpass { id: String, prompt: String },
    /// 滚动位置（ADR 0019），单位都是 zellij 的显示行。position = 视口下方的行数
    /// （0 = 在底部）；length = 视口上方 + 下方，0 = 没有可滚的历史；rows = 视口高度。
    #[serde(rename = "scroll")]
    Scroll { position: u32, length: u32, rows: u32 },
    /// [`ClientMessage::Ping`] 的回话，只发给问的那个 Viewer；客户端收到任何消息都算连接活着，
    /// 这条本身没有内容
    #[serde(rename = "pong")]
    Pong,
    /// 本版本不认识的控制消息（服务端比客户端新）。只在反序列化时出现，忽略即可。
    #[serde(rename = "unknown", other)]
    Unknown,
}

// ---------------- Zellij 安装通道 ----------------

/// 客户端 → 服务端（`/ws/install/:projectId`）。取消只影响本次，不写入拒绝状态：
/// 取消按钮出现在进度条上，语境是"我不想等这次传输"，而不是"我撤销授权"。
/// 直接关掉 socket 也等于取消。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type")]
pub enum InstallClientMessage {
    #[serde(rename = "cancel")]
    Cancel,
}

/// 服务端 → 客户端（`/ws/install/:projectId`）。每次连上来服务端都当作用户显式发起
/// 的安装（首次或重试），推若干 `stage` 后以 `done` 或 `failed` 收尾并关闭 socket。
///
/// Rust 侧加了 `Unknown` 兜底，理由同 [`ServerMessage`]。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum InstallServerMessage {
    /// attempt 从 1 起；>1 表示后端在自动重试瞬时故障，UI 据此说明"为什么还在转"。
    /// command 是宿主机上正在跑的命令，界面把相邻重复的去掉后列在进度下面。
    #[serde(rename = "stage")]
    Stage {
        stage: ZellijInstallStage,
        attempt: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<String>,
    },
    #[serde(rename = "done")]
    Done,
    /// 失败原因用 NonDurableReason：除了安装本身失败，也可能是尚未授权。
    /// attempts 是后端已经自动试过的轮次——"已经替你试过 3 次"与"一次都没试"是
    /// 完全不同的处境，前者说明该去查网络而不是傻点重试。缺省按 1 算。
    #[serde(rename = "failed")]
    Failed {
        reason: NonDurableReason,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attempts: Option<u32>,
    },
    /// 本版本不认识的消息（服务端比客户端新）。只在反序列化时出现，忽略即可。
    #[serde(rename = "unknown", other)]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::roundtrip;

    #[test]
    fn frame_constants_match_ts() {
        assert_eq!(TERM_FRAME_OUTPUT, 0x01);
        assert_eq!(TERM_FRAME_REPLAY, 0x02);
    }

    #[test]
    fn frame_roundtrip() {
        for kind in [TermFrameKind::Output, TermFrameKind::Replay] {
            let payload = "\x1b[1;32m你好\x1b[0m 🦅\r\n".as_bytes();
            let frame = encode_term_frame(kind, payload);
            assert_eq!(frame[0], kind.byte());
            assert_eq!(frame.len(), payload.len() + 1);
            let decoded = decode_term_frame(&frame).unwrap();
            assert_eq!(decoded, TermFrame { kind, payload });
        }
    }

    #[test]
    fn frame_edge_cases() {
        // 只有类型头：合法的空载荷
        assert_eq!(
            decode_term_frame(&[TERM_FRAME_OUTPUT]),
            Some(TermFrame { kind: TermFrameKind::Output, payload: &[] })
        );
        // 空帧、未知类型头：丢掉
        assert_eq!(decode_term_frame(&[]), None);
        assert_eq!(decode_term_frame(&[0x00, b'x']), None);
        assert_eq!(decode_term_frame(&[0x03, b'x']), None);
        // 载荷不校验 UTF-8，原样交出
        let raw = [TERM_FRAME_REPLAY, 0xff, 0xfe];
        assert_eq!(decode_term_frame(&raw).unwrap().payload, &[0xff, 0xfe]);
    }

    #[test]
    fn client_messages_every_branch() {
        let i = roundtrip::<ClientMessage>(r#"{"type":"input","data":"ls -la\r"}"#);
        assert_eq!(i, ClientMessage::Input { data: "ls -la\r".into() });
        // 控制字符照 JSON 转义往返
        roundtrip::<ClientMessage>(r#"{"type":"input","data":"\u001b[A\u0003"}"#);
        let r = roundtrip::<ClientMessage>(r#"{"type":"resize","cols":120,"rows":40}"#);
        assert_eq!(r, ClientMessage::Resize { cols: 120, rows: 40 });
        roundtrip::<ClientMessage>(
            r##"{"type":"appearance","appearance":"dark","background":"#0a0a0a","foreground":"#fafafa"}"##,
        );
        roundtrip::<ClientMessage>(r#"{"type":"appearance","appearance":"light"}"#);
        let q = roundtrip::<ClientMessage>(r#"{"type":"scroll"}"#);
        assert_eq!(q, ClientMessage::Scroll { seek: None });
        let k = roundtrip::<ClientMessage>(r#"{"type":"scroll","seek":250}"#);
        assert_eq!(k, ClientMessage::Scroll { seek: Some(250) });
        assert_eq!(roundtrip::<ClientMessage>(r#"{"type":"ping"}"#), ClientMessage::Ping);
    }

    #[test]
    fn client_message_wire_shape() {
        // 客户端真正发出去的字节：type 在对象里、字段 camelCase、缺省字段不出现
        let s = serde_json::to_string(&ClientMessage::Appearance {
            appearance: TermAppearance::Light,
            background: Some("#ffffff".into()),
            foreground: None,
        })
        .unwrap();
        assert_eq!(s, r##"{"type":"appearance","appearance":"light","background":"#ffffff"}"##);
    }

    #[test]
    fn server_messages_every_branch() {
        let s = roundtrip::<ServerMessage>(r#"{"type":"state","state":"active"}"#);
        assert_eq!(s, ServerMessage::State { state: SessionState::Active, dead_reason: None });
        roundtrip::<ServerMessage>(r#"{"type":"state","state":"dead","deadReason":"exited"}"#);
        roundtrip::<ServerMessage>(r#"{"type":"state","state":"unverified"}"#);
        roundtrip::<ServerMessage>(r#"{"type":"reconnecting","attempt":3}"#);
        roundtrip::<ServerMessage>(r#"{"type":"error","message":"会话不存在"}"#);
        let t = roundtrip::<ServerMessage>(r#"{"type":"title","title":"pnpm dev"}"#);
        assert_eq!(t, ServerMessage::Title { title: Some("pnpm dev".into()) });
        // 标题变空是显式的 null，写回也得是 null（TS 的 `title: string | null` 是必填）
        let t = roundtrip::<ServerMessage>(r#"{"type":"title","title":null}"#);
        assert_eq!(t, ServerMessage::Title { title: None });
        roundtrip::<ServerMessage>(
            r#"{"type":"askpass","id":"ap-1","prompt":"fay@box's password:"}"#,
        );
        let sc = roundtrip::<ServerMessage>(r#"{"type":"scroll","position":97,"length":473,"rows":30}"#);
        assert_eq!(sc, ServerMessage::Scroll { position: 97, length: 473, rows: 30 });
        assert_eq!(roundtrip::<ServerMessage>(r#"{"type":"pong"}"#), ServerMessage::Pong);
    }

    #[test]
    fn server_message_unknown_type_is_not_an_error() {
        let m = serde_json::from_str::<ServerMessage>(r#"{"type":"bell","count":1}"#).unwrap();
        assert_eq!(m, ServerMessage::Unknown);
        // 坏 JSON / 缺字段仍然是错误
        assert!(serde_json::from_str::<ServerMessage>(r#"{"type":"state"}"#).is_err());
        assert!(serde_json::from_str::<ServerMessage>("not json").is_err());
    }

    #[test]
    fn install_messages_every_branch() {
        let c = roundtrip::<InstallClientMessage>(r#"{"type":"cancel"}"#);
        assert_eq!(c, InstallClientMessage::Cancel);

        roundtrip::<InstallServerMessage>(r#"{"type":"stage","stage":"probing","attempt":1}"#);
        let s = roundtrip::<InstallServerMessage>(
            r#"{"type":"stage","stage":"downloading","attempt":2,"command":"curl -fsSL https://github.com/zellij-org/zellij/releases/download/v0.44.0/zellij.tar.gz"}"#,
        );
        assert!(matches!(s, InstallServerMessage::Stage { attempt: 2, .. }));
        roundtrip::<InstallServerMessage>(r#"{"type":"stage","stage":"extracting","attempt":1}"#);
        roundtrip::<InstallServerMessage>(r#"{"type":"stage","stage":"verifying","attempt":1}"#);
        assert_eq!(roundtrip::<InstallServerMessage>(r#"{"type":"done"}"#), InstallServerMessage::Done);
        let f = roundtrip::<InstallServerMessage>(
            r#"{"type":"failed","reason":"download-failed","detail":"curl: (28) timeout","attempts":3}"#,
        );
        assert!(matches!(
            f,
            InstallServerMessage::Failed { reason: NonDurableReason::DownloadFailed, attempts: Some(3), .. }
        ));
        // 项目不存在时服务端只给 reason + detail
        roundtrip::<InstallServerMessage>(r#"{"type":"failed","reason":"verify-failed","detail":"项目不存在"}"#);
        roundtrip::<InstallServerMessage>(r#"{"type":"failed","reason":"not-authorized"}"#);
        assert_eq!(
            serde_json::from_str::<InstallServerMessage>(r#"{"type":"log","line":"x"}"#).unwrap(),
            InstallServerMessage::Unknown
        );
    }
}
