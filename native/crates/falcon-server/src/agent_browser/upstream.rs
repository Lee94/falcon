//! agent-browser 上游 stream 协议解析、坐标换算与出站命令编码（ADR 0029 决定一 / 决定六）。
//!
//! agent-browser 的协议细节只放在本文件一处，客户端不感知底层字段。

use base64::Engine as _;
use falcon_proto::{
    BrowserEventKind, BrowserFrameHeader, BrowserStatus, BrowserTab, KeyKind, PointerButton, PointerKind,
};
use serde_json::{Value, json};

/// 上游 stream URL：`ws://127.0.0.1:<port>/?pacing=ack&maxFps=10`。
pub fn upstream_url(port: u16) -> String {
    format!("ws://127.0.0.1:{port}/?pacing=ack&maxFps=10")
}

/// 单帧的页面几何元数据，用于把客户端发来的归一化坐标（0..1）换算成页面坐标。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameMeta {
    pub seq: u64,
    pub device_width: u32,
    pub device_height: u32,
    pub page_scale_factor: f32,
    pub offset_top: f32,
}

impl FrameMeta {
    pub fn to_header(self, image_width: u32, image_height: u32) -> BrowserFrameHeader {
        BrowserFrameHeader {
            seq: self.seq,
            device_width: self.device_width,
            device_height: self.device_height,
            image_width,
            image_height,
            page_scale_factor: self.page_scale_factor,
            offset_top: self.offset_top,
        }
    }
}

/// 归一化坐标（0..1）→ agent-browser 页面坐标（CSS 像素）。
pub fn translate_normalized_coords(norm_x: f64, norm_y: f64, meta: FrameMeta) -> (f64, f64) {
    let scale = if meta.page_scale_factor.is_finite() && meta.page_scale_factor > 0.0 {
        f64::from(meta.page_scale_factor)
    } else {
        1.0
    };
    let offset_top = if meta.offset_top.is_finite() { f64::from(meta.offset_top) } else { 0.0 };
    let x = (norm_x.clamp(0.0, 1.0) * f64::from(meta.device_width)) / scale;
    let y = ((norm_y.clamp(0.0, 1.0) * f64::from(meta.device_height)) - offset_top).max(0.0) / scale;
    (x, y)
}

/// 从上游 stream 收到的一条已解析消息。
#[derive(Debug)]
pub enum UpstreamMessage {
    Frame {
        meta: FrameMeta,
        image_width: u32,
        image_height: u32,
        jpeg: Vec<u8>,
    },
    Status(BrowserStatus),
    Url {
        url: String,
        title: Option<String>,
    },
    Tabs(Vec<BrowserTab>),
    Event(BrowserEventKind),
}

/// 解析一条来自 agent-browser 守护进程的 JSON 文本消息。不认识的类型返回 `None`。
pub fn parse_upstream_message(text: &str, fallback_seq: u64) -> Option<UpstreamMessage> {
    let val: Value = serde_json::from_str(text).ok()?;
    let kind = val.get("type").and_then(Value::as_str)?;
    match kind {
        "frame" => parse_frame(&val, fallback_seq),
        "status" => Some(UpstreamMessage::Status(BrowserStatus {
            connected: val.get("connected").and_then(Value::as_bool).unwrap_or(true),
            screencasting: val.get("screencasting").and_then(Value::as_bool).unwrap_or(false),
            viewport_width: val.get("viewportWidth").and_then(Value::as_u64).unwrap_or(0) as u32,
            viewport_height: val.get("viewportHeight").and_then(Value::as_u64).unwrap_or(0) as u32,
        })),
        "url" => {
            let url = val.get("url").and_then(Value::as_str)?.to_string();
            let title = val.get("title").and_then(Value::as_str).map(str::to_string);
            Some(UpstreamMessage::Url { url, title })
        }
        "tabs" => {
            let arr = val.get("tabs").and_then(Value::as_array)?;
            let tabs = arr
                .iter()
                .enumerate()
                .map(|(i, item)| BrowserTab {
                    index: item.get("index").and_then(Value::as_u64).unwrap_or(i as u64) as u32,
                    url: item.get("url").and_then(Value::as_str).unwrap_or_default().to_string(),
                    title: item.get("title").and_then(Value::as_str).unwrap_or_default().to_string(),
                    active: item.get("active").and_then(Value::as_bool).unwrap_or(false),
                })
                .collect();
            Some(UpstreamMessage::Tabs(tabs))
        }
        "command" => {
            let action = val
                .get("action")
                .or_else(|| val.get("command"))
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let command_id = val.get("id").and_then(Value::as_str).map(str::to_string);
            let (summary, sensitive_text) = summarize_command(&action, &val);
            Some(UpstreamMessage::Event(BrowserEventKind::Command {
                action,
                summary,
                sensitive_text,
                command_id,
            }))
        }
        "result" => {
            let action = val.get("action").and_then(Value::as_str).map(str::to_string);
            let ok = val
                .get("success")
                .or_else(|| val.get("ok"))
                .and_then(Value::as_bool)
                .unwrap_or_else(|| val.get("error").is_none_or(Value::is_null));
            let duration_ms = val
                .get("duration_ms")
                .or_else(|| val.get("durationMs"))
                .or_else(|| val.get("duration"))
                .and_then(Value::as_u64);
            let error = val.get("error").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
            let command_id = val.get("id").and_then(Value::as_str).map(str::to_string);
            Some(UpstreamMessage::Event(BrowserEventKind::Result {
                action,
                ok,
                duration_ms,
                error,
                command_id,
            }))
        }
        "console" => {
            let raw_level = val.get("level").or_else(|| val.get("kind")).and_then(Value::as_str).unwrap_or("");
            let level = match raw_level.to_ascii_lowercase().as_str() {
                "warn" | "warning" => "warn",
                "error" => "error",
                _ => return None,
            };
            let text = val
                .get("text")
                .or_else(|| val.get("message"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if text.is_empty() {
                return None;
            }
            Some(UpstreamMessage::Event(BrowserEventKind::Console {
                level: level.to_string(),
                text,
            }))
        }
        "page_error" | "pageError" => {
            let message = val
                .get("text")
                .or_else(|| val.get("message"))
                .or_else(|| val.get("error"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if message.is_empty() {
                return None;
            }
            Some(UpstreamMessage::Event(BrowserEventKind::PageError { message }))
        }
        _ => None,
    }
}

fn parse_frame(val: &Value, fallback_seq: u64) -> Option<UpstreamMessage> {
    let data = val.get("data").and_then(Value::as_str)?;
    let b64 = data.split_once(',').map_or(data, |(_, rest)| rest);
    let jpeg = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    if jpeg.is_empty() {
        return None;
    }
    let md = val.get("metadata").unwrap_or(val);
    let seq = val
        .get("seq")
        .or_else(|| md.get("seq"))
        .and_then(Value::as_u64)
        .unwrap_or(fallback_seq);
    let device_width = md
        .get("deviceWidth")
        .or_else(|| val.get("width"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let device_height = md
        .get("deviceHeight")
        .or_else(|| val.get("height"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let page_scale_factor = md
        .get("pageScaleFactor")
        .and_then(Value::as_f64)
        .map_or(1.0, |v| v as f32);
    let offset_top = md.get("offsetTop").and_then(Value::as_f64).map_or(0.0, |v| v as f32);
    let (img_w, img_h) = jpeg_dimensions(&jpeg).unwrap_or((device_width, device_height));
    Some(UpstreamMessage::Frame {
        meta: FrameMeta {
            seq,
            device_width: if device_width > 0 { device_width } else { img_w },
            device_height: if device_height > 0 { device_height } else { img_h },
            page_scale_factor,
            offset_top,
        },
        image_width: img_w,
        image_height: img_h,
        jpeg,
    })
}

/// 从 JPEG SOF 标记解析真实像素宽高（用于没有给 metadata 时的兜底与画面比例）。
fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 4 || bytes[0] != 0xff || bytes[1] != 0xd8 {
        return None;
    }
    let mut i = 2usize;
    while i + 3 < bytes.len() {
        if bytes[i] != 0xff {
            i += 1;
            continue;
        }
        while i < bytes.len() && bytes[i] == 0xff {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let marker = bytes[i];
        i += 1;
        if marker == 0xd9 || marker == 0xda {
            break;
        }
        if i + 1 >= bytes.len() {
            break;
        }
        let len = u16::from_be_bytes([bytes[i], bytes[i + 1]]) as usize;
        if len < 2 || i + len > bytes.len() {
            break;
        }
        let is_sof = matches!(
            marker,
            0xc0 | 0xc1 | 0xc2 | 0xc3 | 0xc5 | 0xc6 | 0xc7 | 0xc9 | 0xca | 0xcb | 0xcd | 0xce | 0xcf
        );
        if is_sof && len >= 7 {
            let h = u16::from_be_bytes([bytes[i + 3], bytes[i + 4]]) as u32;
            let w = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]) as u32;
            return (w > 0 && h > 0).then_some((w, h));
        }
        i += len;
    }
    None
}

/// 提取命令摘要；`fill` / `type` 的输入文本拆进 `sensitive_text`（界面默认打码）。
fn summarize_command(action: &str, val: &Value) -> (Option<String>, Option<String>) {
    let params = val.get("params").or_else(|| val.get("args")).unwrap_or(val);
    let sensitive = matches!(action.to_ascii_lowercase().as_str(), "fill" | "type");
    let mut sensitive_text = None;
    let mut parts: Vec<String> = Vec::new();

    if let Some(obj) = params.as_object() {
        for key in ["selector", "ref", "target", "url", "key", "direction", "name"] {
            if let Some(s) = obj.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()) {
                parts.push(truncate_str(s, 120));
            }
        }
        if sensitive {
            for key in ["text", "value"] {
                if let Some(s) = obj.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()) {
                    sensitive_text = Some(s.to_string());
                    break;
                }
            }
        } else {
            for key in ["text", "value"] {
                if let Some(s) = obj.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()) {
                    parts.push(truncate_str(s, 120));
                }
            }
        }
    } else if let Some(arr) = params.as_array() {
        if sensitive && arr.len() >= 2 {
            if let Some(s) = arr[0].as_str() {
                parts.push(truncate_str(s, 120));
            }
            if let Some(s) = arr[1].as_str() {
                sensitive_text = Some(s.to_string());
            }
        } else if sensitive && arr.len() == 1 {
            if let Some(s) = arr[0].as_str() {
                sensitive_text = Some(s.to_string());
            }
        } else {
            for item in arr.iter().take(3) {
                if let Some(s) = item.as_str().filter(|s| !s.is_empty()) {
                    parts.push(truncate_str(s, 120));
                }
            }
        }
    }

    let summary = (!parts.is_empty()).then(|| parts.join(" "));
    (summary, sensitive_text)
}

fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max_chars).collect();
        out.push('…');
        out
    }
}

pub fn encode_ack(seq: u64) -> String {
    json!({ "type": "ack", "seq": seq }).to_string()
}

pub fn encode_pointer(
    kind: PointerKind,
    x: f64,
    y: f64,
    button: PointerButton,
    clicks: u8,
    modifiers: u8,
) -> Option<String> {
    let event_type = match kind {
        PointerKind::Move => "mouseMoved",
        PointerKind::Down => "mousePressed",
        PointerKind::Up => "mouseReleased",
        PointerKind::Unknown => return None,
    };
    let btn = match button {
        PointerButton::Left => "left",
        PointerButton::Right => "right",
        PointerButton::Middle => "middle",
        PointerButton::None | PointerButton::Unknown => "none",
    };
    Some(
        json!({
            "type": "input_mouse",
            "eventType": event_type,
            "x": x,
            "y": y,
            "button": btn,
            "clickCount": clicks,
            "modifiers": modifiers,
        })
        .to_string(),
    )
}

pub fn encode_wheel(x: f64, y: f64, dx: f64, dy: f64) -> String {
    json!({
        "type": "input_mouse",
        "eventType": "mouseWheel",
        "x": x,
        "y": y,
        "deltaX": dx,
        "deltaY": dy,
    })
    .to_string()
}

pub fn encode_key(kind: KeyKind, key: &str, code: &str, modifiers: u8) -> Option<String> {
    let event_type = match kind {
        KeyKind::Down => "keyDown",
        KeyKind::Up => "keyUp",
        KeyKind::Unknown => return None,
    };
    Some(
        json!({
            "type": "input_keyboard",
            "eventType": event_type,
            "key": key,
            "code": code,
            "modifiers": modifiers,
        })
        .to_string(),
    )
}

pub fn encode_text(text: &str) -> String {
    json!({
        "type": "input_keyboard",
        "eventType": "char",
        "text": text,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_normalized_coords_with_scale_and_offset() {
        let meta = FrameMeta {
            seq: 5,
            device_width: 1280,
            device_height: 720,
            page_scale_factor: 2.0,
            offset_top: 20.0,
        };
        let (x, y) = translate_normalized_coords(0.5, 0.5, meta);
        assert!((x - 320.0).abs() < 1e-6);
        assert!((y - 170.0).abs() < 1e-6);
    }

    #[test]
    fn parses_frame_and_command_with_sensitive_text() {
        let b64 = base64::engine::general_purpose::STANDARD.encode([0xff, 0xd8, 0xff, 0xd9]);
        let frame_json = json!({
            "type": "frame",
            "seq": 9,
            "data": b64,
            "metadata": {
                "deviceWidth": 1280,
                "deviceHeight": 800,
                "pageScaleFactor": 1.0,
                "offsetTop": 0.0
            }
        })
        .to_string();
        match parse_upstream_message(&frame_json, 1).unwrap() {
            UpstreamMessage::Frame { meta, jpeg, .. } => {
                assert_eq!(meta.seq, 9);
                assert_eq!(meta.device_width, 1280);
                assert_eq!(jpeg, vec![0xff, 0xd8, 0xff, 0xd9]);
            }
            other => panic!("unexpected: {other:?}"),
        }

        let cmd_json = json!({
            "type": "command",
            "id": "cmd-1",
            "action": "fill",
            "params": { "selector": "#password", "value": "hunter2" }
        })
        .to_string();
        match parse_upstream_message(&cmd_json, 1).unwrap() {
            UpstreamMessage::Event(BrowserEventKind::Command {
                action,
                summary,
                sensitive_text,
                command_id,
            }) => {
                assert_eq!(action, "fill");
                assert_eq!(summary.as_deref(), Some("#password"));
                assert_eq!(sensitive_text.as_deref(), Some("hunter2"));
                assert_eq!(command_id.as_deref(), Some("cmd-1"));
            }
            other => panic!("unexpected: {other:?}"),
        }

        // console.log 被忽略，只保留 warn / error
        assert!(parse_upstream_message(r#"{"type":"console","level":"log","text":"hi"}"#, 1).is_none());
        assert!(parse_upstream_message(r#"{"type":"console","level":"warn","text":"careful"}"#, 1).is_some());
    }
}
