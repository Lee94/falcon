//! Agent 浏览器协议（ADR 0029）：`/ws/browser/:sessionId` 与会话摘要里的在场信息。

use serde::{Deserialize, Serialize};

use crate::wire::wire_enum;

/// 支持的最低 agent-browser 版本（以 stream 协议带 `seq` / ack pacing 为准）。
pub const MIN_AGENT_BROWSER_VERSION: (u32, u32, u32) = (0, 43, 0);

/// 二进制画面帧版本号与固定头长度（1 + 8 + 4*4 + 4*2 = 33 字节）。
pub const BROWSER_FRAME_VERSION: u8 = 0x01;
pub const BROWSER_FRAME_HEADER_LEN: usize = 33;

/// 二进制帧头：seq + 编码帧与页面视口尺寸（用于展示比例与坐标换算）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BrowserFrameHeader {
    pub seq: u64,
    pub device_width: u32,
    pub device_height: u32,
    pub image_width: u32,
    pub image_height: u32,
    pub page_scale_factor: f32,
    pub offset_top: f32,
}

/// 编码二进制画面帧：固定 33 字节大端头 + 原始 JPEG 字节。
pub fn encode_browser_frame(header: &BrowserFrameHeader, jpeg: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(BROWSER_FRAME_HEADER_LEN + jpeg.len());
    out.push(BROWSER_FRAME_VERSION);
    out.extend_from_slice(&header.seq.to_be_bytes());
    out.extend_from_slice(&header.device_width.to_be_bytes());
    out.extend_from_slice(&header.device_height.to_be_bytes());
    out.extend_from_slice(&header.image_width.to_be_bytes());
    out.extend_from_slice(&header.image_height.to_be_bytes());
    out.extend_from_slice(&header.page_scale_factor.to_be_bytes());
    out.extend_from_slice(&header.offset_top.to_be_bytes());
    out.extend_from_slice(jpeg);
    out
}

/// 解码二进制画面帧；版本不对或长度不足返回 `None`。
pub fn decode_browser_frame(raw: &[u8]) -> Option<(BrowserFrameHeader, &[u8])> {
    if raw.len() <= BROWSER_FRAME_HEADER_LEN || raw[0] != BROWSER_FRAME_VERSION {
        return None;
    }
    let header = BrowserFrameHeader {
        seq: u64::from_be_bytes(raw[1..9].try_into().ok()?),
        device_width: u32::from_be_bytes(raw[9..13].try_into().ok()?),
        device_height: u32::from_be_bytes(raw[13..17].try_into().ok()?),
        image_width: u32::from_be_bytes(raw[17..21].try_into().ok()?),
        image_height: u32::from_be_bytes(raw[21..25].try_into().ok()?),
        page_scale_factor: f32::from_be_bytes(raw[25..29].try_into().ok()?),
        offset_top: f32::from_be_bytes(raw[29..33].try_into().ok()?),
    };
    Some((header, &raw[BROWSER_FRAME_HEADER_LEN..]))
}

wire_enum! {
    /// 当前控制权归谁。
    pub enum BrowserControlHolder open {
        Agent => "agent",
        Human => "human",
    }
}

fn default_true() -> bool {
    true
}

/// 当前控制权状态。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BrowserControl {
    pub holder: BrowserControlHolder,
    /// unix 毫秒；Human 接管期间有值，用于顶条显示「你在操作 · N 分钟」
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<i64>,
    /// action policy 锁是否有效；失效时界面提示「agent 可能同时在操作」
    #[serde(default = "default_true")]
    pub policy_enforced: bool,
}

impl Default for BrowserControl {
    fn default() -> Self {
        Self {
            holder: BrowserControlHolder::Agent,
            since: None,
            policy_enforced: true,
        }
    }
}

/// 会话列表里的浏览器在场摘要（终端标题栏标记与首次自动弹出窗口都靠它）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BrowserBrief {
    /// agent-browser 的独立会话名。默认实例和旧服务端省略此字段，客户端使用无参数路由。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub browser_session: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub title: String,
    pub control: BrowserControlHolder,
}

/// 浏览器标签页摘要。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BrowserTab {
    #[serde(default)]
    pub index: u32,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub active: bool,
}

/// 浏览器流连接状态。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct BrowserStatus {
    #[serde(default)]
    pub connected: bool,
    #[serde(default)]
    pub screencasting: bool,
    #[serde(default)]
    pub viewport_width: u32,
    #[serde(default)]
    pub viewport_height: u32,
}

/// 单条浏览器操作 / 页面记录（落盘 jsonl 并经 WS 增量推送）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BrowserEvent {
    pub id: u64,
    /// unix 毫秒
    pub ts: i64,
    #[serde(flatten)]
    pub kind: BrowserEventKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumb_id: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum BrowserEventKind {
    Command {
        action: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
        /// `fill` / `type` 的输入文本；界面默认打码，点开才显示
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sensitive_text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command_id: Option<String>,
    },
    Result {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action: Option<String>,
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command_id: Option<String>,
    },
    Console {
        level: String,
        text: String,
    },
    PageError {
        message: String,
    },
    Url {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    Control {
        holder: BrowserControlHolder,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

/// `GET /api/sessions/:id/browser/log` 的响应。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BrowserLogPage {
    pub events: Vec<BrowserEvent>,
    pub has_more: bool,
}

wire_enum! {
    pub enum PointerKind open {
        Move => "move",
        Down => "down",
        Up => "up",
    }
}

wire_enum! {
    pub enum PointerButton open {
        None => "none",
        Left => "left",
        Middle => "middle",
        Right => "right",
    }
}

wire_enum! {
    pub enum KeyKind open {
        Down => "down",
        Up => "up",
    }
}

/// `/ws/browser/:sessionId` 服务端 → 客户端 JSON 文本消息。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum BrowserServerMessage {
    Hello {
        control: BrowserControl,
        status: BrowserStatus,
        #[serde(default)]
        url: String,
        #[serde(default)]
        title: String,
        #[serde(default)]
        tabs: Vec<BrowserTab>,
        #[serde(default)]
        gone: bool,
    },
    Event {
        event: BrowserEvent,
    },
    Thumb {
        event_id: u64,
        thumb_id: String,
    },
    Control {
        holder: BrowserControlHolder,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        since: Option<i64>,
        #[serde(default = "default_true")]
        policy_enforced: bool,
    },
    Status {
        status: BrowserStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tabs: Option<Vec<BrowserTab>>,
    },
    Gone,
    #[serde(other)]
    Unknown,
}

/// `/ws/browser/:sessionId` 客户端 → 服务端 JSON 文本消息。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum BrowserClientMessage {
    Ack {
        seq: u64,
    },
    Visibility {
        visible: bool,
    },
    Takeover {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },
    Release,
    Pointer {
        kind: PointerKind,
        x: f64,
        y: f64,
        button: PointerButton,
        #[serde(default)]
        clicks: u8,
        #[serde(default)]
        modifiers: u8,
        #[serde(default)]
        seq: u64,
    },
    Wheel {
        x: f64,
        y: f64,
        dx: f64,
        dy: f64,
        #[serde(default)]
        seq: u64,
    },
    Key {
        kind: KeyKind,
        key: String,
        code: String,
        #[serde(default)]
        modifiers: u8,
    },
    Text {
        text: String,
    },
    #[serde(other)]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_codec_round_trip() {
        let header = BrowserFrameHeader {
            seq: 42,
            device_width: 1280,
            device_height: 720,
            image_width: 1280,
            image_height: 720,
            page_scale_factor: 1.5,
            offset_top: 12.5,
        };
        let jpeg = [0xff, 0xd8, 0xff, 0xd9];
        let encoded = encode_browser_frame(&header, &jpeg);
        assert_eq!(encoded.len(), BROWSER_FRAME_HEADER_LEN + jpeg.len());
        let (decoded, body) = decode_browser_frame(&encoded).expect("decode");
        assert_eq!(decoded, header);
        assert_eq!(body, &jpeg);
    }

    #[test]
    fn rejects_short_or_unknown_version_frame() {
        assert!(decode_browser_frame(&[0x01; 33]).is_none());
        let mut bad = vec![0x02; 40];
        bad[0] = 0x99;
        assert!(decode_browser_frame(&bad).is_none());
    }

    #[test]
    fn ignores_unknown_messages_both_ways() {
        let s: BrowserServerMessage = serde_json::from_str(r#"{"type":"futureThing","foo":1}"#).unwrap();
        assert_eq!(s, BrowserServerMessage::Unknown);
        let c: BrowserClientMessage = serde_json::from_str(r#"{"type":"futureCmd","bar":2}"#).unwrap();
        assert_eq!(c, BrowserClientMessage::Unknown);
    }

    #[test]
    fn event_round_trip_and_unknown_kind() {
        let ev = BrowserEvent {
            id: 7,
            ts: 1_700_000_000_000,
            kind: BrowserEventKind::Command {
                action: "fill".into(),
                summary: Some("@e3".into()),
                sensitive_text: Some("secret".into()),
                command_id: Some("c1".into()),
            },
            thumb_id: None,
        };
        let raw = serde_json::to_string(&ev).unwrap();
        let back: BrowserEvent = serde_json::from_str(&raw).unwrap();
        assert_eq!(back, ev);

        let unknown: BrowserEvent = serde_json::from_str(r#"{"id":8,"ts":1,"kind":"weirdNew"}"#).unwrap();
        assert_eq!(unknown.kind, BrowserEventKind::Unknown);
    }
}
