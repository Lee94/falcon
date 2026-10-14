//! falcon-server（以及人手 `falcon-sessiond attach`）与宿主机上会话 holder 之间的协议
//! （ADR 0020 决定四，设计文档 `docs/design/session-daemon.md` 第 4、5 节）。
//!
//! holder、VT 进程、falcon-server 共用这一份。holder 的协议**只增不改**：二进制按版本命名、
//! 旧版不删，新版 falcon-server 必须能和它可能遇到的所有旧 holder 说话——字段只加不删，
//! 未知字段忽略，未知帧按长度跳过。
//!
//! # 帧
//!
//! `u8 kind | u32 LE len | payload`。字节类的帧（输入、输出、快照）载荷是原始字节加几个定长
//! 头字段；控制类的帧载荷是 JSON。
//!
//! # 快照与增量怎么接上（第 5.5 节）
//!
//! 输出帧带它在会话输出流里的起始偏移。连接发 `Sync` 后，holder 把**这条连接**的输出先扣下，
//! 等 VT 进程交回反映 `[0, N)` 的快照，发出 `Synced{offset: N}`，再把扣下的（全都 `≥ N`）
//! 补发出去。于是在这条连接上，`Synced` 之前的输出都 `< N`、之后的都 `≥ N`，接收方不用拼接：
//! 回放 = 快照 + continuation（解析器停在半截序列时要补的那几个字节），此后照常接输出。

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::program_status::{ProgramStatusRecord, ProgramStatusReport};

/// falcon 的发布版本号（native 工作区的版本）。守护进程的二进制按它命名，`version` 也报它
pub const FALCON_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 本端支持的协议版本区间。握手取双方区间交集的最大值
pub const PROTO_MIN: u32 = 1;
pub const PROTO_MAX: u32 = 1;

/// `bridge` 连上 holder 后先往 stdout 写这一串，再开始透传帧。
///
/// SSH 上 bridge 由登录 shell 拉起（holder 要继承登录环境），用户的 rc 文件可能往 stdout
/// 打字（motd、echo、`conda activate` 的提示）；服务端读到这一串之前的字节一律丢掉，
/// 帧流就不会被这些字污染。以 NUL 开头：正常的 shell 输出里不会出现
pub const BRIDGE_PREAMBLE: &[u8] = b"\0falcon-sessiond-bridge/1\n";

/// `bridge` 的退出码：会话不在了（没有 meta、或进程已死、或已经退出）。其余非零都按链路问题处理
pub const BRIDGE_EXIT_GONE: i32 = 3;
/// `bridge` 的退出码：进程还在但连不上它的 socket（刚起来 / 卡住），稍后再试
pub const BRIDGE_EXIT_UNAVAILABLE: i32 = 4;
/// `bridge` 的退出码：会话已经正常结束（shell 退出了，留下了 `.exit`）。与"没了"分开，
/// 服务端据此标 dead(Exited) 而不是 dead(SessionGone)
pub const BRIDGE_EXIT_EXITED: i32 = 5;

/// 开放的字符串枚举（认不出的值落到 `Unknown`），协议里只有 [`Role`] 一个，不走 wire_enum!：
/// 那个宏给的是带 `as_str` / `ALL` 的全套，这里用不上
macro_rules! wire_open_str {
    (
        $(#[$meta:meta])*
        pub enum $name:ident { $( $variant:ident => $wire:literal ),+ $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
        pub enum $name {
            $( #[serde(rename = $wire)] $variant, )+
            #[serde(other)]
            Unknown,
        }
    };
}

// ---------------------------------------------------------------- 帧

pub const HEADER_LEN: usize = 5;

/// 帧类型。客户端 → holder 在 0x01–0x3f，holder → 客户端在 0x41–0x7f；VT 进程的内部协议
/// 用 0x81 起（在 falcon-sessiond 里定义），三者不重叠，抓包时一眼看得出方向。
pub mod kind {
    pub const HELLO: u8 = 0x01;
    pub const INPUT: u8 = 0x02;
    pub const RESIZE: u8 = 0x03;
    pub const SYNC: u8 = 0x04;
    pub const EXPORT: u8 = 0x05;
    pub const APPEARANCE: u8 = 0x06;
    pub const FOREGROUND: u8 = 0x07;
    pub const STATUS_SEEN: u8 = 0x08;
    pub const TERMINATE: u8 = 0x09;
    pub const PING: u8 = 0x0a;
    /// 让 holder 把 VT 进程换成另一个二进制（热升级 ghostty，设计文档 2.3）
    pub const UPGRADE_VT: u8 = 0x0b;

    pub const WELCOME: u8 = 0x41;
    pub const OUTPUT: u8 = 0x42;
    pub const SYNCED: u8 = 0x43;
    pub const EXPORTED: u8 = 0x44;
    pub const STATUS: u8 = 0x45;
    pub const FOREGROUND_IS: u8 = 0x46;
    pub const EVENT: u8 = 0x47;
    pub const EXIT: u8 = 0x48;
    pub const PONG: u8 = 0x49;
    pub const ERROR: u8 = 0x4a;
    pub const UPGRADED: u8 = 0x4b;
}

/// 单帧载荷的上限。超限即视为对端坏了、断开连接——宁可断开重连，也不能让一个畸形的
/// 长度字段把内存吃光。
pub fn max_payload(kind: u8) -> usize {
    match kind {
        // 快照 / 导出：1 万行带颜色的历史约 120 KB，满屏宽字符加满样式也远到不了这里
        kind::SYNCED | kind::EXPORTED => 64 << 20,
        kind::INPUT | kind::OUTPUT => (1 << 20) + 16,
        // 程序状态全集最多 256 条，每条正文按规范上限约 2.5 KB
        _ => 1 << 20,
    }
}

pub fn encode_frame(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.push(kind);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameTooLarge {
    pub kind: u8,
    pub len: usize,
}

impl std::fmt::Display for FrameTooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "帧 0x{:02x} 的长度 {} 超出上限", self.kind, self.len)
    }
}

impl std::error::Error for FrameTooLarge {}

/// 流式拆帧：读到多少喂多少，攒够一帧吐一帧
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buf: Vec<u8>,
    start: usize,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, bytes: &[u8]) {
        // 前面吃掉的部分攒多了再挪，避免每帧都 memmove
        if self.start > 0 && self.start >= self.buf.len() / 2 {
            self.buf.drain(..self.start);
            self.start = 0;
        }
        self.buf.extend_from_slice(bytes);
    }

    /// 下一帧；不够一帧时 `Ok(None)`
    pub fn next_frame(&mut self) -> Result<Option<(u8, Vec<u8>)>, FrameTooLarge> {
        let avail = &self.buf[self.start..];
        if avail.len() < HEADER_LEN {
            return Ok(None);
        }
        let kind = avail[0];
        let len = u32::from_le_bytes([avail[1], avail[2], avail[3], avail[4]]) as usize;
        if len > max_payload(kind) {
            return Err(FrameTooLarge { kind, len });
        }
        if avail.len() < HEADER_LEN + len {
            return Ok(None);
        }
        let payload = avail[HEADER_LEN..HEADER_LEN + len].to_vec();
        self.start += HEADER_LEN + len;
        if self.start == self.buf.len() {
            self.buf.clear();
            self.start = 0;
        }
        Ok(Some((kind, payload)))
    }
}

/// 阻塞地从 `Read` 里读一帧。`Ok(None)` = 对端在帧边界上正常关闭
pub fn read_frame(r: &mut impl std::io::Read) -> std::io::Result<Option<(u8, Vec<u8>)>> {
    let mut head = [0u8; HEADER_LEN];
    let mut got = 0;
    while got < HEADER_LEN {
        let n = r.read(&mut head[got..])?;
        if n == 0 {
            return if got == 0 { Ok(None) } else { Err(std::io::ErrorKind::UnexpectedEof.into()) };
        }
        got += n;
    }
    let kind = head[0];
    let len = u32::from_le_bytes([head[1], head[2], head[3], head[4]]) as usize;
    if len > max_payload(kind) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, FrameTooLarge { kind, len }));
    }
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload)?;
    Ok(Some((kind, payload)))
}

// ---------------------------------------------------------------- 消息

wire_open_str! {
    /// 连上 holder 的是谁。`attach` 是人手的逃生通道，`server` 是 falcon-server
    pub enum Role { Server => "server", Attach => "attach" }
}

/// 输出 `Synced` / `Exported` 的格式
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ExportFormat {
    Vt,
    Plain,
    Html,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Hello {
    pub proto_min: u32,
    pub proto_max: u32,
    pub role: Role,
    /// 自报家门，只进日志：`falcon-server/0.3.0`
    #[serde(default)]
    pub client: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Welcome {
    pub proto: u32,
    /// holder 的 falcon 版本
    pub version: String,
    /// VT 进程跑的 ghostty commit；VT 进程没起来时为空
    #[serde(default)]
    pub vt_version: String,
    /// VT 进程降级了（反复崩溃，只剩纯转发）
    #[serde(default)]
    pub vt_degraded: bool,
    pub cols: u16,
    pub rows: u16,
    /// shell 的 pid
    pub pid: u32,
    pub started_at: i64,
    /// 会话 id（完整的 uuid），核对用：连错 socket 时一眼看得出
    #[serde(default)]
    pub session_id: String,
    /// 截至握手时会话输出流的总字节数
    #[serde(default)]
    pub offset: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resize {
    pub cols: u16,
    pub rows: u16,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Scheme {
    Light,
    Dark,
}

/// 外观：VT 进程按它回答 OSC 10/11/12 与 `CSI ? 996 n`；scheme 翻转且程序订了 2031 时
/// holder 往 PTY 写 `CSI ? 997 ; 1|2 n`
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Appearance {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheme: Option<Scheme>,
    /// `#rrggbb`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreground: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// holder 的事件（不影响会话存活的那一类）
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Event {
    /// VT 进程挂了，已经用原始尾巴重建；在这之前接回的历史可能比现在长
    #[serde(rename_all = "camelCase")]
    VtRestarted { at_offset: u64 },
    /// 反复崩溃，不再重启：快照退化成原始尾巴，终端查询没人答
    VtDegraded,
    /// 程序设的终端标题（OSC 0 / 2）；空串 = 没有。变了才发，握手后紧跟 `Status` 先发一次当前的
    /// （0.3.2 及之前的 holder 一次都不发）。holder 只管存与转，算不算数由服务端定
    Title { title: String },
    Bell,
    /// OSC 133：A 提示符开始、B 输入开始、C 命令开始输出、D 命令结束
    #[serde(rename_all = "camelCase")]
    Prompt { kind: String, #[serde(default, skip_serializing_if = "Option::is_none")] exit_code: Option<i32> },
    #[serde(other)]
    Unknown,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExitStatus {
    /// 正常退出的退出码；被信号杀死时为 None
    #[serde(default)]
    pub code: Option<i32>,
    #[serde(default)]
    pub signal: Option<i32>,
}

/// 客户端 → holder
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientMsg {
    Hello(Hello),
    Input(Vec<u8>),
    Resize(Resize),
    Sync { req: u32 },
    Export { req: u32, format: ExportFormat },
    Appearance(Appearance),
    Foreground { req: u32 },
    StatusSeen { ids: Vec<String> },
    Terminate,
    Ping,
    /// 新 VT 二进制的绝对路径（宿主机上的）
    UpgradeVt { req: u32, binary: String },
    Unknown(u8),
}

/// holder → 客户端
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HolderMsg {
    Welcome(Welcome),
    Output { offset: u64, bytes: Vec<u8> },
    /// `snapshot` 是反映 `[0, offset)` 的 VT 字节；`continuation` 是解析器停在半截序列时
    /// 要补在后面的原始字节（多半为空）
    Synced { req: u32, offset: u64, snapshot: Vec<u8>, continuation: Vec<u8> },
    Exported { req: u32, bytes: Vec<u8> },
    Status { records: Vec<ProgramStatusRecord> },
    ForegroundIs { req: u32, name: Option<String> },
    Event(Event),
    Exit(ExitStatus),
    Pong,
    Error { message: String },
    Upgraded { req: u32, ok: bool, message: String },
    Unknown(u8),
}

#[derive(Serialize, Deserialize)]
struct ReqJson {
    req: u32,
}

#[derive(Serialize, Deserialize)]
struct ExportJson {
    req: u32,
    format: ExportFormat,
}

#[derive(Serialize, Deserialize)]
struct IdsJson {
    ids: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct UpgradeJson {
    req: u32,
    binary: String,
}

#[derive(Serialize, Deserialize)]
struct RecordsJson {
    records: Vec<ProgramStatusRecord>,
}

#[derive(Serialize, Deserialize)]
struct ForegroundJson {
    req: u32,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct MessageJson {
    message: String,
}

#[derive(Serialize, Deserialize)]
struct UpgradedJson {
    req: u32,
    ok: bool,
    #[serde(default)]
    message: String,
}

fn json<T: Serialize>(v: &T) -> Vec<u8> {
    serde_json::to_vec(v).expect("协议类型都能序列化")
}

fn parse<T: for<'de> Deserialize<'de>>(payload: &[u8]) -> anyhow::Result<T> {
    Ok(serde_json::from_slice(payload)?)
}

fn take<const N: usize>(p: &[u8], at: usize) -> anyhow::Result<[u8; N]> {
    p.get(at..at + N).and_then(|s| s.try_into().ok()).ok_or_else(|| anyhow::anyhow!("帧太短"))
}

impl ClientMsg {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            ClientMsg::Hello(h) => encode_frame(kind::HELLO, &json(h)),
            ClientMsg::Input(b) => encode_frame(kind::INPUT, b),
            ClientMsg::Resize(r) => encode_frame(kind::RESIZE, &json(r)),
            ClientMsg::Sync { req } => encode_frame(kind::SYNC, &json(&ReqJson { req: *req })),
            ClientMsg::Export { req, format } => encode_frame(kind::EXPORT, &json(&ExportJson { req: *req, format: *format })),
            ClientMsg::Appearance(a) => encode_frame(kind::APPEARANCE, &json(a)),
            ClientMsg::Foreground { req } => encode_frame(kind::FOREGROUND, &json(&ReqJson { req: *req })),
            ClientMsg::StatusSeen { ids } => encode_frame(kind::STATUS_SEEN, &json(&IdsJson { ids: ids.clone() })),
            ClientMsg::Terminate => encode_frame(kind::TERMINATE, b"{}"),
            ClientMsg::Ping => encode_frame(kind::PING, b"{}"),
            ClientMsg::UpgradeVt { req, binary } => {
                encode_frame(kind::UPGRADE_VT, &json(&UpgradeJson { req: *req, binary: binary.clone() }))
            }
            ClientMsg::Unknown(k) => encode_frame(*k, &[]),
        }
    }

    pub fn decode(k: u8, p: Vec<u8>) -> anyhow::Result<Self> {
        Ok(match k {
            kind::HELLO => ClientMsg::Hello(parse(&p)?),
            kind::INPUT => ClientMsg::Input(p),
            kind::RESIZE => ClientMsg::Resize(parse(&p)?),
            kind::SYNC => ClientMsg::Sync { req: parse::<ReqJson>(&p)?.req },
            kind::EXPORT => {
                let e: ExportJson = parse(&p)?;
                ClientMsg::Export { req: e.req, format: e.format }
            }
            kind::APPEARANCE => ClientMsg::Appearance(parse(&p)?),
            kind::FOREGROUND => ClientMsg::Foreground { req: parse::<ReqJson>(&p)?.req },
            kind::STATUS_SEEN => ClientMsg::StatusSeen { ids: parse::<IdsJson>(&p)?.ids },
            kind::TERMINATE => ClientMsg::Terminate,
            kind::PING => ClientMsg::Ping,
            kind::UPGRADE_VT => {
                let u: UpgradeJson = parse(&p)?;
                ClientMsg::UpgradeVt { req: u.req, binary: u.binary }
            }
            other => ClientMsg::Unknown(other),
        })
    }
}

impl HolderMsg {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            HolderMsg::Welcome(w) => encode_frame(kind::WELCOME, &json(w)),
            HolderMsg::Output { offset, bytes } => {
                let mut p = Vec::with_capacity(8 + bytes.len());
                p.extend_from_slice(&offset.to_le_bytes());
                p.extend_from_slice(bytes);
                encode_frame(kind::OUTPUT, &p)
            }
            HolderMsg::Synced { req, offset, snapshot, continuation } => {
                let mut p = Vec::with_capacity(16 + snapshot.len() + continuation.len());
                p.extend_from_slice(&req.to_le_bytes());
                p.extend_from_slice(&offset.to_le_bytes());
                p.extend_from_slice(&(snapshot.len() as u32).to_le_bytes());
                p.extend_from_slice(snapshot);
                p.extend_from_slice(continuation);
                encode_frame(kind::SYNCED, &p)
            }
            HolderMsg::Exported { req, bytes } => {
                let mut p = Vec::with_capacity(4 + bytes.len());
                p.extend_from_slice(&req.to_le_bytes());
                p.extend_from_slice(bytes);
                encode_frame(kind::EXPORTED, &p)
            }
            HolderMsg::Status { records } => encode_frame(kind::STATUS, &json(&RecordsJson { records: records.clone() })),
            HolderMsg::ForegroundIs { req, name } => {
                encode_frame(kind::FOREGROUND_IS, &json(&ForegroundJson { req: *req, name: name.clone() }))
            }
            HolderMsg::Event(e) => encode_frame(kind::EVENT, &json(e)),
            HolderMsg::Exit(s) => encode_frame(kind::EXIT, &json(s)),
            HolderMsg::Pong => encode_frame(kind::PONG, b"{}"),
            HolderMsg::Error { message } => encode_frame(kind::ERROR, &json(&MessageJson { message: message.clone() })),
            HolderMsg::Upgraded { req, ok, message } => {
                encode_frame(kind::UPGRADED, &json(&UpgradedJson { req: *req, ok: *ok, message: message.clone() }))
            }
            HolderMsg::Unknown(k) => encode_frame(*k, &[]),
        }
    }

    pub fn decode(k: u8, p: Vec<u8>) -> anyhow::Result<Self> {
        Ok(match k {
            kind::WELCOME => HolderMsg::Welcome(parse(&p)?),
            kind::OUTPUT => {
                let offset = u64::from_le_bytes(take::<8>(&p, 0)?);
                HolderMsg::Output { offset, bytes: p[8..].to_vec() }
            }
            kind::SYNCED => {
                let req = u32::from_le_bytes(take::<4>(&p, 0)?);
                let offset = u64::from_le_bytes(take::<8>(&p, 4)?);
                let len = u32::from_le_bytes(take::<4>(&p, 12)?) as usize;
                let body = &p[16..];
                anyhow::ensure!(len <= body.len(), "快照长度越界");
                HolderMsg::Synced { req, offset, snapshot: body[..len].to_vec(), continuation: body[len..].to_vec() }
            }
            kind::EXPORTED => {
                let req = u32::from_le_bytes(take::<4>(&p, 0)?);
                HolderMsg::Exported { req, bytes: p[4..].to_vec() }
            }
            kind::STATUS => HolderMsg::Status { records: parse::<RecordsJson>(&p)?.records },
            kind::FOREGROUND_IS => {
                let f: ForegroundJson = parse(&p)?;
                HolderMsg::ForegroundIs { req: f.req, name: f.name }
            }
            kind::EVENT => HolderMsg::Event(parse(&p)?),
            kind::EXIT => HolderMsg::Exit(parse(&p)?),
            kind::PONG => HolderMsg::Pong,
            kind::ERROR => HolderMsg::Error { message: parse::<MessageJson>(&p)?.message },
            kind::UPGRADED => {
                let u: UpgradedJson = parse(&p)?;
                HolderMsg::Upgraded { req: u.req, ok: u.ok, message: u.message }
            }
            other => HolderMsg::Unknown(other),
        })
    }
}

/// 握手：双方区间交集的最大值；没有交集为 None
pub fn negotiate(ours: (u32, u32), theirs: (u32, u32)) -> Option<u32> {
    let lo = ours.0.max(theirs.0);
    let hi = ours.1.min(theirs.1);
    (lo <= hi).then_some(hi)
}

// ---------------------------------------------------------------- 启动参数

/// `spawn --spec` / `bridge --create` 的参数：起一个会话要的全部东西。
///
/// env 是有序列表（同 falcon-server 的 `merge_env`：发到宿主机的东西逐字节可复现）。
/// `env_clear`：true = 子进程只拿这份 env（本机，服务端已经解析好整份登录环境）；
/// false = 在 holder 自己的环境上叠加（SSH，holder 由登录 shell 拉起，继承的就是登录环境）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct SpawnSpec {
    pub session_id: String,
    pub argv: Vec<String>,
    #[serde(default)]
    pub env: Vec<(String, String)>,
    #[serde(default)]
    pub env_clear: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub cols: u16,
    pub rows: u16,
    /// VT 进程保留的历史行数
    #[serde(default = "default_scrollback")]
    pub scrollback_lines: u32,
    /// 建会话的 falcon 实例 id（孤儿清理只动自己的）
    #[serde(default)]
    pub owner: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appearance: Option<Appearance>,
}

fn default_scrollback() -> u32 {
    10_000
}

impl SpawnSpec {
    /// base64url（无填充）的 JSON：命令行上任何一层 shell 都不会改写它
    pub fn encode(&self) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json(self))
    }

    pub fn decode(s: &str) -> anyhow::Result<Self> {
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s.trim())?;
        Ok(serde_json::from_slice(&bytes)?)
    }
}

// ---------------------------------------------------------------- 宿主机上的文件

/// 会话在宿主机上的短 id：会话 uuid 去掉横线后的前 16 个 hex（与 Zellij 时代的 `mj-<16hex>` 同源）
pub fn sid_of(session_id: &str) -> String {
    session_id.chars().filter(|c| c.is_ascii_hexdigit()).take(16).collect::<String>().to_ascii_lowercase()
}

/// 只认 16 位小写 hex：它要拼进路径，不能带分隔符或 `..`
pub fn valid_sid(sid: &str) -> bool {
    sid.len() == 16 && sid.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// 根目录下守护进程的子目录名
pub const SESSIOND_DIR: &str = "sessiond";

/// socket 路径上限：macOS 104 字节、Linux 108 字节，留点余量
pub const SOCKET_PATH_MAX: usize = 100;

/// FNV-1a 64：只用来给根目录起一个短名字，区分同一用户下的多个 falcon 根，不需要抗碰撞
fn short_hash(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{:08x}", (h >> 32) as u32)
}

/// POSIX 宿主机上会话的 socket 路径（纯字符串运算，`root` 是宿主机上的绝对路径）。
///
/// `<root>/sessiond/run/<sid>.sock` 不超过 [`SOCKET_PATH_MAX`] 就用它；本机的 root 是用户给的
/// `--data-dir`，可能很长，超了就落到 `/tmp/falcon-sd-<uid>-<root 的短哈希>/<sid>.sock`
/// （目录 0700，创建时校验属主）。
pub fn socket_path(root: &str, sid: &str, uid: u32) -> String {
    let root = root.trim_end_matches('/');
    let preferred = format!("{root}/{SESSIOND_DIR}/run/{sid}.sock");
    if preferred.len() <= SOCKET_PATH_MAX {
        return preferred;
    }
    format!("{}/{sid}.sock", fallback_socket_dir(root, uid))
}

/// [`socket_path`] 超长时用的目录
pub fn fallback_socket_dir(root: &str, uid: u32) -> String {
    format!("/tmp/falcon-sd-{uid}-{}", short_hash(root.trim_end_matches('/')))
}

/// Windows 宿主机上会话的命名管道名
pub fn pipe_name(root: &str, sid: &str) -> String {
    format!(r"\\.\pipe\falcon-sd-{}-{sid}", short_hash(root.trim_end_matches(['/', '\\'])))
}

/// `sessions/<sid>.json`：holder 起来后写入，`list` 与服务端都以它为准
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct SessionMeta {
    pub sid: String,
    pub session_id: String,
    /// holder 的 pid
    pub pid: u32,
    /// shell 的 pid
    pub shell_pid: u32,
    pub version: String,
    #[serde(default)]
    pub owner: String,
    /// 实际在用的 socket 路径 / 管道名
    pub endpoint: String,
    pub created_at: i64,
    #[serde(default)]
    pub argv: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
}

/// `sessions/<sid>.exit`：shell 退出后写入
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExitRecord {
    #[serde(flatten)]
    pub status: ExitStatus,
    pub at: i64,
}

/// `version --json`
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VersionInfo {
    pub version: String,
    pub proto_min: u32,
    pub proto_max: u32,
    /// libghostty-vt 的 commit
    pub ghostty: String,
    /// 编出这个二进制的 Rust target 三元组
    pub target: String,
}

/// `list --json` 的一项
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ListEntry {
    pub sid: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub owner: String,
    pub pid: u32,
    /// holder 还活着
    pub alive: bool,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exited: Option<ExitRecord>,
}

/// `spawn` 的 stdout
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SpawnResult {
    pub ok: bool,
    #[serde(default)]
    pub pid: u32,
    /// 会话早就在了，这次没有新起
    #[serde(default)]
    pub existing: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// VT 进程转给 holder 的程序状态报告就是 [`ProgramStatusReport`]，在这里再导出一次，
/// 免得 falcon-sessiond 去够 program_status 模块的路径
pub type StatusReport = ProgramStatusReport;


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_through_a_byte_at_a_time_decoder() {
        let msgs = vec![
            HolderMsg::Welcome(Welcome {
                proto: 1,
                version: "0.3.0".into(),
                vt_version: "05dcf88".into(),
                vt_degraded: false,
                cols: 80,
                rows: 24,
                pid: 42,
                started_at: 7,
                session_id: "s".into(),
                offset: 3,
            }),
            HolderMsg::Output { offset: 9, bytes: b"\x1b[31mhi\xe4\xb8".to_vec() },
            HolderMsg::Synced { req: 2, offset: 11, snapshot: b"snap".to_vec(), continuation: b"\x1b]0;ti".to_vec() },
            HolderMsg::Exported { req: 3, bytes: vec![] },
            HolderMsg::Event(Event::VtRestarted { at_offset: 5 }),
            HolderMsg::Exit(ExitStatus { code: Some(0), signal: None }),
            HolderMsg::ForegroundIs { req: 1, name: Some("vim".into()) },
        ];
        let bytes: Vec<u8> = msgs.iter().flat_map(HolderMsg::encode).collect();
        let mut dec = FrameDecoder::new();
        let mut out = Vec::new();
        for b in bytes {
            dec.push(&[b]);
            while let Some((k, p)) = dec.next_frame().unwrap() {
                out.push(HolderMsg::decode(k, p).unwrap());
            }
        }
        assert_eq!(out, msgs);
    }

    #[test]
    fn client_messages_round_trip_and_unknown_kinds_are_skipped() {
        let msgs = vec![
            ClientMsg::Hello(Hello { proto_min: 1, proto_max: 2, role: Role::Server, client: "falcon-server/x".into() }),
            ClientMsg::Input(b"ls\r".to_vec()),
            ClientMsg::Resize(Resize { cols: 100, rows: 30 }),
            ClientMsg::Sync { req: 1 },
            ClientMsg::Export { req: 2, format: ExportFormat::Html },
            ClientMsg::Appearance(Appearance { scheme: Some(Scheme::Light), foreground: Some("#000000".into()), ..Default::default() }),
            ClientMsg::StatusSeen { ids: vec!["a".into()] },
            ClientMsg::Terminate,
            ClientMsg::UpgradeVt { req: 4, binary: "/x/falcon-sessiond".into() },
        ];
        let mut bytes: Vec<u8> = msgs.iter().flat_map(ClientMsg::encode).collect();
        // 新版本才有的帧：旧端按长度跳过
        bytes.extend(encode_frame(0x3e, b"{\"future\":true}"));
        let mut cur = std::io::Cursor::new(bytes);
        let mut out = Vec::new();
        while let Some((k, p)) = read_frame(&mut cur).unwrap() {
            out.push(ClientMsg::decode(k, p).unwrap());
        }
        assert_eq!(out.last(), Some(&ClientMsg::Unknown(0x3e)));
        out.pop();
        assert_eq!(out, msgs);
    }

    #[test]
    fn oversized_frames_are_rejected_before_buffering() {
        let mut dec = FrameDecoder::new();
        let mut head = vec![kind::RESIZE];
        head.extend_from_slice(&u32::MAX.to_le_bytes());
        dec.push(&head);
        assert!(dec.next_frame().is_err());
    }

    #[test]
    fn unknown_json_fields_and_roles_are_tolerated() {
        let h: Hello = serde_json::from_str(r#"{"protoMin":1,"protoMax":9,"role":"robot","client":"x","extra":1}"#).unwrap();
        assert_eq!(h.role, Role::Unknown);
        let e: Event = serde_json::from_str(r#"{"type":"somethingNew","x":1}"#).unwrap();
        assert_eq!(e, Event::Unknown);
    }

    #[test]
    fn negotiation_takes_the_highest_common_version() {
        assert_eq!(negotiate((1, 3), (2, 5)), Some(3));
        assert_eq!(negotiate((1, 1), (1, 1)), Some(1));
        assert_eq!(negotiate((1, 1), (2, 2)), None);
    }

    #[test]
    fn spec_survives_base64() {
        let spec = SpawnSpec {
            session_id: "0a1b2c3d-4e5f-6789-abcd-ef0123456789".into(),
            argv: vec!["/bin/zsh".into()],
            env: vec![("TERM".into(), "xterm-256color".into()), ("A".into(), "中 '\"$x".into())],
            env_clear: true,
            cwd: Some("/tmp/x y".into()),
            cols: 120,
            rows: 40,
            scrollback_lines: 10_000,
            owner: "inst".into(),
            appearance: None,
        };
        let s = spec.encode();
        assert!(s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'), "{s}");
        assert_eq!(SpawnSpec::decode(&s).unwrap(), spec);
    }

    #[test]
    fn sid_is_the_first_sixteen_hex_digits() {
        let sid = sid_of("0A1B2C3D-4E5F-6789-abcd-ef0123456789");
        assert_eq!(sid, "0a1b2c3d4e5f6789");
        assert!(valid_sid(&sid));
        assert!(!valid_sid("../../etc/passwd"));
        assert!(!valid_sid("0A1B2C3D4E5F6789"));
    }

    #[test]
    fn socket_path_falls_back_to_tmp_when_too_long() {
        assert_eq!(socket_path("/home/u/.falcon/", "0123456789abcdef", 501), "/home/u/.falcon/sessiond/run/0123456789abcdef.sock");
        let long = format!("/Users/someone/{}", "very-long-directory-name/".repeat(4));
        let p = socket_path(&long, "0123456789abcdef", 501);
        assert!(p.starts_with("/tmp/falcon-sd-501-"), "{p}");
        assert!(p.len() <= SOCKET_PATH_MAX);
        // 同一个根总是落到同一个目录，不同的根不撞
        assert_eq!(p, socket_path(&long, "0123456789abcdef", 501));
        assert_ne!(fallback_socket_dir(&long, 501), fallback_socket_dir("/other", 501));
    }

    #[test]
    fn pipe_names_differ_per_root() {
        let a = pipe_name(r"C:\Users\a\.falcon", "0123456789abcdef");
        assert!(a.starts_with(r"\\.\pipe\falcon-sd-"), "{a}");
        assert_eq!(a, pipe_name(r"C:\Users\a\.falcon\", "0123456789abcdef"));
        assert_ne!(a, pipe_name(r"C:\Users\b\.falcon", "0123456789abcdef"));
    }
}
