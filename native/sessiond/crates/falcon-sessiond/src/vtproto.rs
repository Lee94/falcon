//! holder 与 VT 进程之间的内部协议：同样的 `u8 kind | u32 LE len | payload` 帧，走 VT 进程的
//! stdin / stdout。只在同一个二进制的两个子命令之间说，但热升级时新旧 VT 进程的版本不同，
//! 所以照样只增不改、未知帧跳过。kind 从 0x81 起，和对外协议不重叠。

use std::io::Read;

use falcon_proto::sessiond::{Appearance, ExportFormat, Resize, encode_frame};
use falcon_proto::program_status::ProgramStatusReport;
use serde::{Deserialize, Serialize};

pub mod kind {
    pub const FEED: u8 = 0x81;
    pub const RESIZE: u8 = 0x82;
    pub const SNAPSHOT: u8 = 0x83;
    pub const EXPORT: u8 = 0x84;
    pub const APPEARANCE: u8 = 0x85;
    pub const ENCODE: u8 = 0x86;
    pub const RESTORE: u8 = 0x87;

    pub const READY: u8 = 0xc1;
    pub const PTY_WRITE: u8 = 0xc2;
    pub const STATUS: u8 = 0xc3;
    pub const PROMPT: u8 = 0xc4;
    pub const TITLE: u8 = 0xc5;
    pub const RESET: u8 = 0xc7;
    pub const SNAPSHOTTED: u8 = 0xc8;
    pub const EXPORTED: u8 = 0xc9;
    pub const ENCODED: u8 = 0xca;
    pub const RESTORED: u8 = 0xcb;
}

/// 内部帧的上限：快照与二进制快照可能有几十 MB（1 万行满屏样式），给足
const MAX_PAYLOAD: usize = 256 << 20;

/// holder → VT 进程
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToVt {
    /// `replay`：这是重建用的旧输出（VT 进程重启后喂原始尾巴）。期间不回答查询、不报事件——
    /// 那些查询早就有人答过了，再答一遍就成了程序收到的多余输入
    Feed { replay: bool, bytes: Vec<u8> },
    Resize(Resize),
    /// 导出接回用的 VT 快照（设计文档 6.2）
    Snapshot { req: u32 },
    Export { req: u32, format: ExportFormat },
    Appearance(Appearance),
    /// 二进制快照（热升级交接用）
    Encode { req: u32 },
    /// 用二进制快照恢复（新 VT 进程收到的第一条）
    Restore(Vec<u8>),
    Unknown(u8),
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Ready {
    pub ghostty: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Prompt {
    /// "A" / "B" / "C" / "D"
    pub kind: String,
    #[serde(default)]
    pub exit_code: Option<i32>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Restored {
    pub ok: bool,
    #[serde(default)]
    pub message: String,
}

/// VT 进程 → holder
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FromVt {
    Ready(Ready),
    PtyWrite(Vec<u8>),
    Status(ProgramStatusReport),
    Prompt(Prompt),
    Title(String),
    Reset,
    Snapshotted { req: u32, snapshot: Vec<u8>, continuation: Vec<u8> },
    Exported { req: u32, bytes: Vec<u8> },
    Encoded { req: u32, bytes: Vec<u8> },
    Restored(Restored),
    Unknown(u8),
}

fn json<T: Serialize>(v: &T) -> Vec<u8> {
    serde_json::to_vec(v).expect("能序列化")
}

fn req_of(p: &[u8]) -> anyhow::Result<u32> {
    Ok(u32::from_le_bytes(p.get(..4).and_then(|s| s.try_into().ok()).ok_or_else(|| anyhow::anyhow!("帧太短"))?))
}

fn with_req(req: u32, body: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(4 + body.len());
    p.extend_from_slice(&req.to_le_bytes());
    p.extend_from_slice(body);
    p
}

impl ToVt {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            ToVt::Feed { replay, bytes } => {
                let mut p = Vec::with_capacity(1 + bytes.len());
                p.push(u8::from(*replay));
                p.extend_from_slice(bytes);
                encode_frame(kind::FEED, &p)
            }
            ToVt::Resize(r) => encode_frame(kind::RESIZE, &json(r)),
            ToVt::Snapshot { req } => encode_frame(kind::SNAPSHOT, &req.to_le_bytes()),
            ToVt::Export { req, format } => encode_frame(kind::EXPORT, &with_req(*req, &json(format))),
            ToVt::Appearance(a) => encode_frame(kind::APPEARANCE, &json(a)),
            ToVt::Encode { req } => encode_frame(kind::ENCODE, &req.to_le_bytes()),
            ToVt::Restore(b) => encode_frame(kind::RESTORE, b),
            ToVt::Unknown(k) => encode_frame(*k, &[]),
        }
    }

    pub fn decode(k: u8, p: Vec<u8>) -> anyhow::Result<Self> {
        Ok(match k {
            kind::FEED => {
                anyhow::ensure!(!p.is_empty(), "FEED 帧缺标志位");
                ToVt::Feed { replay: p[0] != 0, bytes: p[1..].to_vec() }
            }
            kind::RESIZE => ToVt::Resize(serde_json::from_slice(&p)?),
            kind::SNAPSHOT => ToVt::Snapshot { req: req_of(&p)? },
            kind::EXPORT => ToVt::Export { req: req_of(&p)?, format: serde_json::from_slice(&p[4..])? },
            kind::APPEARANCE => ToVt::Appearance(serde_json::from_slice(&p)?),
            kind::ENCODE => ToVt::Encode { req: req_of(&p)? },
            kind::RESTORE => ToVt::Restore(p),
            other => ToVt::Unknown(other),
        })
    }
}

impl FromVt {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            FromVt::Ready(r) => encode_frame(kind::READY, &json(r)),
            FromVt::PtyWrite(b) => encode_frame(kind::PTY_WRITE, b),
            FromVt::Status(s) => encode_frame(kind::STATUS, &json(s)),
            FromVt::Prompt(p) => encode_frame(kind::PROMPT, &json(p)),
            FromVt::Title(t) => encode_frame(kind::TITLE, &json(t)),
            FromVt::Reset => encode_frame(kind::RESET, &[]),
            FromVt::Snapshotted { req, snapshot, continuation } => {
                let mut p = Vec::with_capacity(8 + snapshot.len() + continuation.len());
                p.extend_from_slice(&req.to_le_bytes());
                p.extend_from_slice(&(snapshot.len() as u32).to_le_bytes());
                p.extend_from_slice(snapshot);
                p.extend_from_slice(continuation);
                encode_frame(kind::SNAPSHOTTED, &p)
            }
            FromVt::Exported { req, bytes } => encode_frame(kind::EXPORTED, &with_req(*req, bytes)),
            FromVt::Encoded { req, bytes } => encode_frame(kind::ENCODED, &with_req(*req, bytes)),
            FromVt::Restored(r) => encode_frame(kind::RESTORED, &json(r)),
            FromVt::Unknown(k) => encode_frame(*k, &[]),
        }
    }

    pub fn decode(k: u8, p: Vec<u8>) -> anyhow::Result<Self> {
        Ok(match k {
            kind::READY => FromVt::Ready(serde_json::from_slice(&p)?),
            kind::PTY_WRITE => FromVt::PtyWrite(p),
            kind::STATUS => FromVt::Status(serde_json::from_slice(&p)?),
            kind::PROMPT => FromVt::Prompt(serde_json::from_slice(&p)?),
            kind::TITLE => FromVt::Title(serde_json::from_slice(&p)?),
            kind::RESET => FromVt::Reset,
            kind::SNAPSHOTTED => {
                let req = req_of(&p)?;
                let len = req_of(&p[4..])? as usize;
                let body = &p[8..];
                anyhow::ensure!(len <= body.len(), "快照长度越界");
                FromVt::Snapshotted { req, snapshot: body[..len].to_vec(), continuation: body[len..].to_vec() }
            }
            kind::EXPORTED => FromVt::Exported { req: req_of(&p)?, bytes: p[4..].to_vec() },
            kind::ENCODED => FromVt::Encoded { req: req_of(&p)?, bytes: p[4..].to_vec() },
            kind::RESTORED => FromVt::Restored(serde_json::from_slice(&p)?),
            other => FromVt::Unknown(other),
        })
    }
}

/// 阻塞读一帧（内部协议的上限比对外的宽）。`Ok(None)` = 对端在帧边界上关了
pub fn read_frame(r: &mut impl Read) -> std::io::Result<Option<(u8, Vec<u8>)>> {
    let mut head = [0u8; 5];
    let mut got = 0;
    while got < head.len() {
        let n = r.read(&mut head[got..])?;
        if n == 0 {
            return if got == 0 { Ok(None) } else { Err(std::io::ErrorKind::UnexpectedEof.into()) };
        }
        got += n;
    }
    let len = u32::from_le_bytes([head[1], head[2], head[3], head[4]]) as usize;
    if len > MAX_PAYLOAD {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "内部帧超长"));
    }
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload)?;
    Ok(Some((head[0], payload)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let to = vec![
            ToVt::Feed { replay: true, bytes: b"abc".to_vec() },
            ToVt::Resize(Resize { cols: 3, rows: 4 }),
            ToVt::Snapshot { req: 7 },
            ToVt::Export { req: 8, format: ExportFormat::Plain },
            ToVt::Encode { req: 9 },
            ToVt::Restore(vec![1, 2, 3]),
        ];
        let mut cur = std::io::Cursor::new(to.iter().flat_map(ToVt::encode).collect::<Vec<u8>>());
        let mut got = Vec::new();
        while let Some((k, p)) = read_frame(&mut cur).unwrap() {
            got.push(ToVt::decode(k, p).unwrap());
        }
        assert_eq!(got, to);

        let from = vec![
            FromVt::PtyWrite(b"\x1b[?62;22c".to_vec()),
            FromVt::Snapshotted { req: 1, snapshot: b"s".to_vec(), continuation: b"\x1b[".to_vec() },
            FromVt::Encoded { req: 2, bytes: vec![] },
            FromVt::Prompt(Prompt { kind: "A".into(), exit_code: None }),
            FromVt::Reset,
        ];
        let mut cur = std::io::Cursor::new(from.iter().flat_map(FromVt::encode).collect::<Vec<u8>>());
        let mut got = Vec::new();
        while let Some((k, p)) = read_frame(&mut cur).unwrap() {
            got.push(FromVt::decode(k, p).unwrap());
        }
        assert_eq!(got, from);
    }
}
