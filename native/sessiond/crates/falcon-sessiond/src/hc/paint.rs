//! `terminal.attach` 的首帧拼装（First Paint，设计文档 3.5 节）：
//! 1. `0x18`（CAN）取消半截序列；
//! 2. 复位前导：SGR 复位、光标可见、正常光标键与小键盘、关全部鼠标上报、关 focus 与 bracketed paste、
//!    关 modifyOtherKeys 与 kitty 键盘栈、重置滚动区、退出备用屏、清屏与历史、光标归位；
//! 3. Holder 的 `Synced.snapshot`（libghostty-vt 导出的主屏历史 + 当前屏 + 模式 + 光标）；
//! 4. 当前窗口标题（`\x1b]2;<title>\x07`）与 `\x1b[?9001l`；
//! 5. 当前活跃的 `ProgramStatusRecord` 重编码为 `\x1b]7501;...\x07`（供下方终端感知状态）；
//! 6. Holder 的 `Synced.continuation`（补齐半截序列）。

use base64::Engine as _;
use falcon_proto::program_status::{ProgramBlockKind, ProgramState, ProgramStatusRecord};

const RESET_PREAMBLE: &[u8] = b"\x18\x1b[0m\x1b[?25h\x1b[?1l\x1b>\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l\x1b[?1015l\x1b[?1004l\x1b[?2004l\x1b[>4;0m\x1b[<u\x1b[r\x1b[?1049l\x1b[2J\x1b[3J\x1b[H";

/// 若 `history == Some(0)`，并且快照里包含主屏历史 + `\x1b[?1049h\x1b[H\x1b[2J` 备用屏切换，
/// 可以直接从备用屏切换处截取；主屏下若 `history == Some(max_lines)`，只保留最后 `rows + max_lines` 行。
pub fn trim_snapshot_history(snapshot: &[u8], rows: u16, history: Option<u32>) -> Vec<u8> {
    let Some(max_hist) = history else {
        return snapshot.to_vec();
    };
    if max_hist == 0 {
        let alt_marker = b"\x1b[?1049h\x1b[H\x1b[2J";
        if let Some(pos) = snapshot.windows(alt_marker.len()).position(|w| w == alt_marker) {
            return snapshot[pos..].to_vec();
        }
    }
    let keep_lines = (rows.max(1) as usize).saturating_add(max_hist as usize);
    let newline_positions: Vec<usize> = snapshot
        .iter()
        .enumerate()
        .filter_map(|(i, &b)| (b == b'\n').then_some(i))
        .collect();
    if newline_positions.len() <= keep_lines {
        return snapshot.to_vec();
    }
    // 保留开头的模式前导（第一个 CSI / ESC 段），剪掉中间过早的历史行
    let drop_until = newline_positions[newline_positions.len() - keep_lines] + 1;
    let mut out = Vec::new();
    // 提取开头连续的 `ESC [` 模式设置序列，防止剪掉首行的模式前缀
    let prefix_len = leading_mode_sequences_len(snapshot);
    if prefix_len > 0 && prefix_len < drop_until {
        out.extend_from_slice(&snapshot[..prefix_len]);
    }
    out.extend_from_slice(b"\x1b[0m");
    out.extend_from_slice(&snapshot[drop_until..]);
    out
}

fn leading_mode_sequences_len(bytes: &[u8]) -> usize {
    let mut i = 0;
    while i + 2 < bytes.len() && bytes[i] == 0x1b && bytes[i + 1] == b'[' {
        let mut j = i + 2;
        while j < bytes.len() && !(0x40..=0x7e).contains(&bytes[j]) {
            j += 1;
        }
        if j < bytes.len() && matches!(bytes[j], b'h' | b'l' | b'r' | b'H') {
            i = j + 1;
        } else {
            break;
        }
    }
    i
}

pub fn encode_osc7501(rec: &ProgramStatusRecord) -> Vec<u8> {
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let state = match rec.state {
        ProgramState::Idle => "idle",
        ProgramState::Working => "working",
        ProgramState::Done => "done",
        ProgramState::Blocked => "blocked",
        ProgramState::Error => "error",
        ProgramState::Unknown => return Vec::new(),
    };
    let mut parts = Vec::new();
    if !rec.id.is_empty() && rec.id != "default" {
        parts.push(format!("id={}", rec.id));
    }
    parts.push(format!("state={state}"));
    if let Some(kind) = &rec.kind {
        let k = match kind {
            ProgramBlockKind::Permission => Some("permission"),
            ProgramBlockKind::Question => Some("question"),
            ProgramBlockKind::Auth => Some("auth"),
            ProgramBlockKind::Unknown => None,
        };
        if let Some(k) = k {
            parts.push(format!("kind={k}"));
        }
    }
    if let Some(p) = rec.progress {
        parts.push(format!("progress={}", p.min(100)));
    }
    if let Some(app) = &rec.app
        && !app.is_empty()
    {
        parts.push(format!("app={app}"));
    }
    if let Some(title) = &rec.title
        && !title.is_empty()
    {
        parts.push(format!("title={}", b64.encode(title.as_bytes())));
    }
    if let Some(msg) = &rec.message
        && !msg.is_empty()
    {
        parts.push(format!("msg={}", b64.encode(msg.as_bytes())));
    }
    format!("\x1b]7501;{}\x07", parts.join(":")).into_bytes()
}

pub fn sanitize_osc_text(s: &str) -> String {
    s.chars().filter(|&c| c >= ' ' && c != '\x7f').collect()
}

pub fn first_paint(
    snapshot: &[u8],
    continuation: &[u8],
    title: &str,
    records: &[ProgramStatusRecord],
    rows: u16,
    history: Option<u32>,
) -> Vec<u8> {
    let snap = trim_snapshot_history(snapshot, rows, history);
    let mut out = Vec::with_capacity(RESET_PREAMBLE.len() + snap.len() + continuation.len() + 256);
    out.extend_from_slice(RESET_PREAMBLE);
    out.extend_from_slice(&snap);
    let clean_title = sanitize_osc_text(title);
    out.extend_from_slice(format!("\x1b]2;{clean_title}\x07").as_bytes());
    out.extend_from_slice(b"\x1b[?9001l");
    for rec in records {
        out.extend_from_slice(&encode_osc7501(rec));
    }
    out.extend_from_slice(continuation);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_paint_contains_reset_title_and_status() {
        let rec = ProgramStatusRecord {
            id: "default".into(),
            state: ProgramState::Working,
            kind: None,
            progress: Some(50),
            app: Some("claude-code".into()),
            title: Some("Fixing bug".into()),
            message: None,
            updated_at: 1,
        };
        let bytes = first_paint(b"hello\r\n", b"\x1b[31", "my-title", &[rec], 24, None);
        let s = String::from_utf8_lossy(&bytes);
        assert!(bytes.starts_with(b"\x18\x1b[0m"));
        assert!(s.contains("hello\r\n"));
        assert!(s.contains("\x1b]2;my-title\x07"));
        assert!(s.contains("\x1b[?9001l"));
        assert!(s.contains("\x1b]7501;state=working:progress=50:app=claude-code:title="));
        assert!(bytes.ends_with(b"\x1b[31"));
    }
}
