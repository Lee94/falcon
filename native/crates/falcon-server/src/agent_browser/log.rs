//! 操作记录（jsonl）与缩略图存储（ADR 0029 决定七）。
//!
//! - 记录存放在 `<dataDir>/agent-browser/log/<sessionId>.jsonl`，权限 0600，最多保留 5000 条。
//! - 缩略图存放在 `<dataDir>/agent-browser/thumbs/<sessionId>/<eventId>.jpg`，权限 0600，每个会话最多 300 张。

use std::collections::{HashSet, VecDeque};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use falcon_proto::{BrowserEvent, BrowserEventKind, BrowserLogPage};

use super::{is_valid_session_id, log_dir, thumbs_dir};

/// 默认实例直接使用终端 id；命名实例使用 `<终端 id>.<64 位小写十六进制哈希>`。
/// 这里校验的是存储键，不是 agent-browser 的原始 session 名。
fn is_valid_storage_id(storage_id: &str) -> bool {
    let mut parts = storage_id.split('.');
    let Some(session_id) = parts.next() else { return false };
    if !is_valid_session_id(session_id) {
        return false;
    }
    match (parts.next(), parts.next()) {
        (None, None) => true,
        (Some(hash), None) => hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
        _ => false,
    }
}

pub const MAX_LOG_ENTRIES: usize = 5000;
const COMPACT_THRESHOLD: usize = 5500;
pub const MAX_THUMBS_PER_SESSION: usize = 300;
pub const DEFAULT_LOG_LIMIT: usize = 200;
pub const MAX_LOG_LIMIT: usize = 500;

pub fn log_file_path(data_dir: &Path, session_id: &str) -> PathBuf {
    log_dir(data_dir).join(format!("{session_id}.jsonl"))
}

pub fn session_thumbs_dir(data_dir: &Path, session_id: &str) -> PathBuf {
    thumbs_dir(data_dir).join(session_id)
}

/// 单个会话的记录内存缓存 + jsonl 追加器。
pub struct SessionLog {
    data_dir: PathBuf,
    session_id: String,
    entries: VecDeque<BrowserEvent>,
    next_id: u64,
    disk_lines: usize,
    thumb_ids: HashSet<u64>,
}

impl SessionLog {
    pub fn load(data_dir: &Path, session_id: &str) -> Self {
        let mut entries = VecDeque::new();
        let mut next_id = 1u64;
        let mut disk_lines = 0usize;
        let thumb_ids = existing_thumb_ids(data_dir, session_id);

        if is_valid_storage_id(session_id) {
            let path = log_file_path(data_dir, session_id);
            if let Ok(raw) = std::fs::read_to_string(&path) {
                for line in raw.lines() {
                    if line.trim().is_empty() {
                        continue;
                    }
                    disk_lines += 1;
                    if let Ok(mut ev) = serde_json::from_str::<BrowserEvent>(line) {
                        next_id = next_id.max(ev.id.saturating_add(1));
                        if matches!(ev.kind, BrowserEventKind::Result { .. }) && thumb_ids.contains(&ev.id) {
                            ev.thumb_id = Some(ev.id.to_string());
                        } else {
                            ev.thumb_id = None;
                        }
                        entries.push_back(ev);
                        if entries.len() > MAX_LOG_ENTRIES {
                            entries.pop_front();
                        }
                    }
                }
            }
        }

        Self {
            data_dir: data_dir.to_path_buf(),
            session_id: session_id.to_string(),
            entries,
            next_id,
            disk_lines,
            thumb_ids,
        }
    }

    pub fn append(&mut self, ts: i64, kind: BrowserEventKind) -> BrowserEvent {
        let id = self.next_id;
        self.next_id += 1;
        let ev = BrowserEvent { id, ts, kind, thumb_id: None };
        self.entries.push_back(ev.clone());
        if self.entries.len() > MAX_LOG_ENTRIES {
            self.entries.pop_front();
        }
        self.append_disk(&ev);
        ev
    }

    /// 保存某条 `Result` 事件对应的帧缩略图，并在超过 300 张时清理最早的缩略图。
    pub fn save_thumb(&mut self, event_id: u64, jpeg: &[u8]) -> Option<String> {
        if !is_valid_storage_id(&self.session_id) || jpeg.is_empty() {
            return None;
        }
        let dir = session_thumbs_dir(&self.data_dir, &self.session_id);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            log::warn!("创建浏览器缩略图目录失败：{e}");
            return None;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
        let path = dir.join(format!("{event_id}.jpg"));
        match std::fs::File::create(&path) {
            Ok(mut f) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt as _;
                    let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
                }
                if let Err(e) = f.write_all(jpeg) {
                    log::warn!("写入浏览器缩略图失败：{e}");
                    let _ = std::fs::remove_file(&path);
                    return None;
                }
            }
            Err(e) => {
                log::warn!("创建浏览器缩略图文件失败：{e}");
                return None;
            }
        }
        self.thumb_ids.insert(event_id);
        let thumb_id = event_id.to_string();
        if let Some(ev) = self.entries.iter_mut().rev().find(|e| e.id == event_id) {
            ev.thumb_id = Some(thumb_id.clone());
        }
        self.prune_thumbs(&dir);
        Some(thumb_id)
    }

    /// 按 `before`（严格小于该 event id）和 `limit` 分页查询记录，返回时间正序。
    pub fn query(&self, before: Option<u64>, limit: usize) -> BrowserLogPage {
        let limit = limit.clamp(1, MAX_LOG_LIMIT);
        let eligible: Vec<&BrowserEvent> = self
            .entries
            .iter()
            .filter(|e| before.is_none_or(|b| e.id < b))
            .collect();
        let has_more = eligible.len() > limit;
        let start = eligible.len().saturating_sub(limit);
        let events = eligible[start..].iter().map(|e| (*e).clone()).collect();
        BrowserLogPage { events, has_more }
    }

    fn append_disk(&mut self, ev: &BrowserEvent) {
        if !is_valid_storage_id(&self.session_id) {
            return;
        }
        let dir = log_dir(&self.data_dir);
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
        let path = log_file_path(&self.data_dir, &self.session_id);
        let Ok(line) = serde_json::to_string(ev) else { return };
        let open_res = std::fs::OpenOptions::new().create(true).append(true).open(&path);
        if let Ok(mut f) = open_res {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
            }
            let _ = writeln!(f, "{line}");
            self.disk_lines += 1;
            if self.disk_lines >= COMPACT_THRESHOLD {
                self.compact_disk(&dir, &path);
            }
        }
    }

    fn compact_disk(&mut self, dir: &Path, target: &Path) {
        let tmp = dir.join(format!(".{}.jsonl.tmp.{}", self.session_id, std::process::id()));
        let Ok(mut f) = std::fs::File::create(&tmp) else { return };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
        }
        for ev in &self.entries {
            if let Ok(line) = serde_json::to_string(ev)
                && writeln!(f, "{line}").is_err()
            {
                let _ = std::fs::remove_file(&tmp);
                return;
            }
        }
        let _ = f.sync_data();
        if std::fs::rename(&tmp, target).is_ok() {
            self.disk_lines = self.entries.len();
        } else {
            let _ = std::fs::remove_file(&tmp);
        }
    }

    fn prune_thumbs(&mut self, dir: &Path) {
        if self.thumb_ids.len() <= MAX_THUMBS_PER_SESSION {
            return;
        }
        let mut sorted: Vec<u64> = self.thumb_ids.iter().copied().collect();
        sorted.sort_unstable();
        let excess = sorted.len() - MAX_THUMBS_PER_SESSION;
        for id in sorted.into_iter().take(excess) {
            self.thumb_ids.remove(&id);
            let _ = std::fs::remove_file(dir.join(format!("{id}.jpg")));
            if let Some(ev) = self.entries.iter_mut().find(|e| e.id == id) {
                ev.thumb_id = None;
            }
        }
    }
}

fn existing_thumb_ids(data_dir: &Path, session_id: &str) -> HashSet<u64> {
    let mut ids = HashSet::new();
    if !is_valid_storage_id(session_id) {
        return ids;
    }
    let dir = session_thumbs_dir(data_dir, session_id);
    let Ok(entries) = std::fs::read_dir(dir) else { return ids };
    for entry in entries.flatten() {
        if let Some(name) = entry.file_name().to_str()
            && let Some(stem) = name.strip_suffix(".jpg")
            && let Ok(id) = stem.parse::<u64>()
        {
            ids.insert(id);
        }
    }
    ids
}

/// 读取单张缩略图 JPEG 字节；严格校验 id 格式防止路径穿越。
pub fn read_thumb(data_dir: &Path, session_id: &str, thumb_id: &str) -> Option<Vec<u8>> {
    if !is_valid_storage_id(session_id) || thumb_id.is_empty() || !thumb_id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    std::fs::read(session_thumbs_dir(data_dir, session_id).join(format!("{thumb_id}.jpg"))).ok()
}

/// 删除终端会话的所有浏览器记录与缩略图（默认实例 + 所有命名实例）。
pub fn remove_session_logs(data_dir: &Path, session_id: &str) {
    if !is_valid_session_id(session_id) {
        return;
    }
    let _ = std::fs::remove_file(log_file_path(data_dir, session_id));
    let _ = std::fs::remove_dir_all(session_thumbs_dir(data_dir, session_id));
    let prefix = format!("{session_id}.");
    for (dir, is_log) in [(log_dir(data_dir), true), (thumbs_dir(data_dir), false)] {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let storage_id = if is_log { name.strip_suffix(".jsonl") } else { Some(name) };
            if !storage_id.is_some_and(|id| id.starts_with(&prefix) && is_valid_storage_id(id)) {
                continue;
            }
            if is_log {
                let _ = std::fs::remove_file(entry.path());
            } else {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_reload_pagination_and_thumbs() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SessionLog::load(dir.path(), "sess-a");
        let e1 = store.append(1000, BrowserEventKind::Url { url: "https://example.com".into(), title: Some("Ex".into()) });
        let e2 = store.append(1001, BrowserEventKind::Result {
            action: Some("click".into()),
            ok: true,
            duration_ms: Some(12),
            error: None,
            command_id: Some("c1".into()),
        });
        let e3 = store.append(1002, BrowserEventKind::Console { level: "error".into(), text: "boom".into() });
        let tid = store.save_thumb(e2.id, &[0xff, 0xd8, 0xff, 0xd9]).unwrap();
        assert_eq!(tid, e2.id.to_string());
        assert_eq!(read_thumb(dir.path(), "sess-a", &tid).unwrap(), vec![0xff, 0xd8, 0xff, 0xd9]);
        assert!(read_thumb(dir.path(), "sess-a", "../etc/passwd").is_none());

        let reloaded = SessionLog::load(dir.path(), "sess-a");
        let page1 = reloaded.query(None, 2);
        assert!(page1.has_more);
        assert_eq!(page1.events.len(), 2);
        assert_eq!(page1.events[0].id, e2.id);
        assert_eq!(page1.events[0].thumb_id.as_deref(), Some(tid.as_str()));
        assert_eq!(page1.events[1].id, e3.id);

        let page2 = reloaded.query(Some(page1.events[0].id), 2);
        assert!(!page2.has_more);
        assert_eq!(page2.events.len(), 1);
        assert_eq!(page2.events[0].id, e1.id);

        remove_session_logs(dir.path(), "sess-a");
        assert!(read_thumb(dir.path(), "sess-a", &tid).is_none());
        assert!(SessionLog::load(dir.path(), "sess-a").query(None, 10).events.is_empty());
    }
}
