//! `hc` 网关的持久化状态（`<root>/sessiond/hc/{core.json, terminals.json, docs.json}`）：
//! - **短地址编码**（`t1`、`t9`、`t10`、`t1b`、`t100`）：首字符固定 `'1'..='9'`（9 种），
//!   后续字符取 30 进制无元音字符集 `0123456789bcdfghjkmnpqrstvwxyz`；
//! - **终端注册表**（`terminals.json`）：把 falcon 的 16 位 `sid` 映射为稳定的单调递增 `seq`、
//!   可选自定义 `name`、`keep` 标记与已知元数据（会话退出后仍能按 `t<seq>` 查到）；
//! - **文档存储**（`docs.json`）：供 `herdr-app` 保存空间/布局文档，带乐观锁 `version`。

use std::collections::BTreeMap;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::hc::rpc::RpcError;
use crate::layout::{Layout, now_ms, write_atomic};

const ALPHABET: &[u8; 30] = b"0123456789bcdfghjkmnpqrstvwxyz";

/// 把从 1 开始的序号编码成 `hc` 的短地址（`1 -> t1`, `9 -> t9`, `10 -> t10`, `20 -> t1b`, `280 -> t100`）
pub fn format_addr(seq: u32) -> String {
    assert!(seq >= 1, "seq 从 1 起");
    let mut rem = (seq - 1) as u64;
    let mut tail_len = 0u32;
    let mut block = 9u64;
    while rem >= block {
        rem -= block;
        tail_len += 1;
        block *= 30;
    }
    let mut digits = vec![0u8; tail_len as usize];
    for i in (0..tail_len as usize).rev() {
        digits[i] = ALPHABET[(rem % 30) as usize];
        rem /= 30;
    }
    let first = b'1' + (rem as u8);
    let mut out = String::with_capacity(2 + digits.len());
    out.push('t');
    out.push(first as char);
    for d in digits {
        out.push(d as char);
    }
    out
}

/// 解析 `t<addr>` 为从 1 开始的序号
pub fn parse_addr(s: &str) -> Option<u32> {
    let body = s.strip_prefix('t')?;
    let mut bytes = body.bytes();
    let first = bytes.next()?;
    if !(b'1'..=b'9').contains(&first) {
        return None;
    }
    let first_idx = (first - b'1') as u64;
    let mut tail_val = 0u64;
    let mut tail_len = 0u32;
    let mut base_offset = 0u64;
    let mut block = 9u64;
    for b in bytes {
        let idx = ALPHABET.iter().position(|&c| c == b)? as u64;
        tail_val = tail_val.checked_mul(30)?.checked_add(idx)?;
        base_offset = base_offset.checked_add(block)?;
        block = block.checked_mul(30)?;
        tail_len += 1;
    }
    let power = 30u64.checked_pow(tail_len)?;
    let seq0 = base_offset.checked_add(first_idx.checked_mul(power)?)?.checked_add(tail_val)?;
    u32::try_from(seq0 + 1).ok()
}

/// 名字是否合法：`[a-z0-9-_.]{1,64}`，且不能撞上短地址形状 `t[1-9]...`
pub fn valid_terminal_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 64 {
        return false;
    }
    if parse_addr(name).is_some() {
        return false;
    }
    name.bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.'))
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CoreIdentity {
    /// 32 位小写十六进制核心 ID（`hc` 用它标识 machine 身份）
    pub core_id: String,
    /// 显示名（默认取宿主机名首段）
    #[serde(default = "default_host_name")]
    pub name: String,
    pub created_at: u64,
    /// 是否默认给 falcon 会话合成 `herdr.space` / `herdr.tab` 标签
    #[serde(default = "default_true")]
    pub synthesize_labels: bool,
}

fn default_true() -> bool {
    true
}

pub fn default_host_name() -> String {
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        let r = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
        if r == 0 {
            let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            let raw = String::from_utf8_lossy(&buf[..end]);
            let first = raw.split('.').next().unwrap_or("falcon");
            let cleaned: String = first
                .chars()
                .map(|c| c.to_ascii_lowercase())
                .filter(|&c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'))
                .take(32)
                .collect();
            if !cleaned.is_empty() && !parse_addr(&cleaned).is_some() {
                return cleaned;
            }
        }
    }
    "falcon".into()
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct PersistedTerminal {
    pub sid: String,
    pub session_id: String,
    pub seq: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub keep: bool,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub argv: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default)]
    pub owner: String,
    #[serde(default)]
    pub turn: u64,
    /// 已通过 `terminal.rm` 删除的会话不再从残余 `.exit` 里复活
    #[serde(default)]
    pub removed: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct TerminalsFile {
    #[serde(default = "default_next_seq")]
    pub next_seq: u32,
    #[serde(default)]
    pub entries: BTreeMap<String, PersistedTerminal>,
}

fn default_next_seq() -> u32 {
    1
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DocRecord {
    pub data: Value,
    pub version: u64,
    pub updated_at: u64,
}

pub struct Registry {
    layout: Layout,
    pub identity: CoreIdentity,
    pub terminals: TerminalsFile,
    pub docs: BTreeMap<String, DocRecord>,
}

impl Registry {
    pub fn load(layout: Layout) -> Result<Self> {
        layout.ensure_dirs()?;
        let core_path = layout.hc_dir().join("core.json");
        let identity: CoreIdentity = match std::fs::read(&core_path).ok().and_then(|b| serde_json::from_slice(&b).ok()) {
            Some(id) => id,
            None => {
                let id = CoreIdentity {
                    core_id: gen_hex32(),
                    name: default_host_name(),
                    created_at: (now_ms().max(0) as u64) / 1000,
                    synthesize_labels: true,
                };
                let _ = write_atomic(&core_path, &serde_json::to_vec_pretty(&id)?);
                id
            }
        };
        let term_path = layout.hc_dir().join("terminals.json");
        let terminals: TerminalsFile = std::fs::read(&term_path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_else(|| TerminalsFile { next_seq: 1, entries: BTreeMap::new() });
        let docs_path = layout.hc_dir().join("docs.json");
        let docs: BTreeMap<String, DocRecord> = std::fs::read(&docs_path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Ok(Self { layout, identity, terminals, docs })
    }

    pub fn save_identity(&self) {
        let path = self.layout.hc_dir().join("core.json");
        if let Ok(bytes) = serde_json::to_vec_pretty(&self.identity) {
            let _ = write_atomic(&path, &bytes);
        }
    }

    pub fn save_terminals(&self) {
        let path = self.layout.hc_dir().join("terminals.json");
        if let Ok(bytes) = serde_json::to_vec_pretty(&self.terminals) {
            let _ = write_atomic(&path, &bytes);
        }
    }

    pub fn save_docs(&self) {
        let path = self.layout.hc_dir().join("docs.json");
        if let Ok(bytes) = serde_json::to_vec_pretty(&self.docs) {
            let _ = write_atomic(&path, &bytes);
        }
    }

    /// 确保 `sid` 在注册表里有一条记录，返回是否新建或更新了持久化字段
    pub fn ensure_terminal(
        &mut self,
        sid: &str,
        session_id: &str,
        created_at_ms: i64,
        argv: &[String],
        cwd: Option<&str>,
        owner: &str,
        alive: bool,
    ) -> &mut PersistedTerminal {
        let next_seq = &mut self.terminals.next_seq;
        let entry = self.terminals.entries.entry(sid.to_string()).or_insert_with(|| {
            let seq = (*next_seq).max(1);
            *next_seq = seq + 1;
            PersistedTerminal {
                sid: sid.to_string(),
                session_id: if session_id.is_empty() { uuid_from_sid(sid) } else { session_id.to_string() },
                seq,
                name: None,
                keep: false,
                created_at: (created_at_ms.max(0) as u64) / 1000,
                argv: argv.to_vec(),
                cwd: cwd.map(str::to_string),
                owner: owner.to_string(),
                turn: 0,
                removed: false,
            }
        });
        if alive && entry.removed {
            entry.removed = false;
        }
        if !session_id.is_empty() && entry.session_id.is_empty() {
            entry.session_id = session_id.to_string();
        }
        if created_at_ms > 0 && entry.created_at == 0 {
            entry.created_at = (created_at_ms as u64) / 1000;
        }
        if !argv.is_empty() && entry.argv.is_empty() {
            entry.argv = argv.to_vec();
        }
        if cwd.is_some() && entry.cwd.is_none() {
            entry.cwd = cwd.map(str::to_string);
        }
        if !owner.is_empty() && entry.owner.is_empty() {
            entry.owner = owner.to_string();
        }
        entry
    }

    pub fn doc_get(&self, name: &str) -> std::result::Result<Value, RpcError> {
        if !valid_terminal_name(name) {
            return Err(RpcError::invalid_params(format!("invalid document name: {name}")));
        }
        match self.docs.get(name) {
            Some(r) => Ok(json!({
                "value": r.data,
                "version": r.version,
            })),
            None => Ok(json!({
                "value": Value::Null,
                "version": 0,
            })),
        }
    }

    pub fn doc_set(&mut self, name: &str, value: Value, expected_version: u64) -> std::result::Result<u64, RpcError> {
        if !valid_terminal_name(name) {
            return Err(RpcError::invalid_params(format!("invalid document name: {name}")));
        }
        let encoded = serde_json::to_vec(&value).map_err(|e| RpcError::invalid_params(e.to_string()))?;
        if encoded.len() > 1024 * 1024 {
            return Err(RpcError::invalid_params("document exceeds 1 MiB of JSON"));
        }
        let cur_ver = self.docs.get(name).map(|r| r.version).unwrap_or(0);
        if expected_version != cur_ver {
            return Err(RpcError::conflict(
                format!("document {name:?} is at version {cur_ver}, not {expected_version}"),
                Some(cur_ver),
            ));
        }
        let next_ver = cur_ver + 1;
        let now = (now_ms().max(0) as u64) / 1000;
        self.docs.insert(
            name.to_string(),
            DocRecord {
                data: value,
                version: next_ver,
                updated_at: now,
            },
        );
        self.save_docs();
        Ok(next_ver)
    }
}

/// 生成 32 位小写十六进制随机 ID（对齐 herdr-core `platform::random_hex()`）
pub fn gen_hex32() -> String {
    gen_uuid_v4().replace('-', "")
}

/// 仅有 16 位 sid（例如旧 `.exit` 文件没有 `.json`）时，派生一个确定性的 UUID 字符串
pub fn uuid_from_sid(sid: &str) -> String {
    if sid.len() == 16 {
        format!("{}-{}-4000-8000-000000000000", &sid[..8], &sid[8..12])
    } else {
        gen_uuid_v4()
    }
}

/// 生成随机的 UUID v4（读取 `/dev/urandom` 辅以时间戳与进程号，不引入额外依赖）
pub fn gen_uuid_v4() -> String {
    let mut b = [0u8; 16];
    #[cfg(unix)]
    {
        use std::io::Read as _;
        if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
            let _ = f.read_exact(&mut b);
        }
    }
    if b == [0u8; 16] {
        let seed = (now_ms() as u64) ^ ((std::process::id() as u64) << 32);
        let mut x = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
        for chunk in b.chunks_mut(8) {
            x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            x ^= x >> 31;
            chunk.copy_from_slice(&x.to_le_bytes());
        }
    }
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bijective_base30_matches_herdr_core_vectors() {
        assert_eq!(format_addr(1), "t1");
        assert_eq!(format_addr(9), "t9");
        assert_eq!(format_addr(10), "t10");
        assert_eq!(format_addr(20), "t1b");
        assert_eq!(format_addr(279), "t9z");
        assert_eq!(format_addr(280), "t100");

        for seq in [1, 2, 9, 10, 19, 20, 21, 279, 280, 281, 999, 8379, 8380, 50_000] {
            let a = format_addr(seq);
            assert_eq!(parse_addr(&a), Some(seq), "seq={seq} addr={a}");
        }
        assert_eq!(parse_addr("t0"), None);
        assert_eq!(parse_addr("ta"), None);
        assert_eq!(parse_addr("x1"), None);
    }

    #[test]
    fn terminal_name_validation() {
        assert!(valid_terminal_name("build-server.1"));
        assert!(valid_terminal_name("claude_code"));
        assert!(!valid_terminal_name("t1"));
        assert!(!valid_terminal_name("t1b"));
        assert!(!valid_terminal_name("BadName"));
        assert!(!valid_terminal_name(""));
    }
}
