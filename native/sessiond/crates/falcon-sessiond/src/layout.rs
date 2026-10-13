//! 宿主机上的文件布局（设计文档第 3 节）：
//!
//! ```text
//! <root>/sessiond/sessions/<sid>.json   holder 起来后写入（SessionMeta）
//! <root>/sessiond/sessions/<sid>.exit   shell 退出后写入（ExitRecord）
//! <root>/sessiond/sessions/<sid>.spec   spawn 交给 holder 的启动参数，holder 读完即删
//! <root>/sessiond/run/<sid>.sock        Unix socket（超长时落到 /tmp，见 socket_path）
//! <root>/sessiond/log/<sid>.log
//! ```
//!
//! 目录 0700：socket 的权限之外再挡一道，别的用户连目录都进不去。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use falcon_proto::sessiond::{ExitRecord, ListEntry, SESSIOND_DIR, SessionMeta, SpawnSpec};

use crate::proc;

/// `.exit` 留多久：服务端断线期间 shell 退了，重连时要靠它分辨"退出了"与"没了"
const EXIT_KEEP_MS: i64 = 7 * 24 * 3600 * 1000;

#[derive(Debug, Clone)]
pub struct Layout {
    pub root: PathBuf,
}

impl Layout {
    pub fn new(root: PathBuf) -> Self {
        // 相对路径会随 holder 的 cwd 变：一律转绝对
        let root = std::path::absolute(&root).unwrap_or(root);
        Layout { root }
    }

    pub fn base(&self) -> PathBuf {
        self.root.join(SESSIOND_DIR)
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.base().join("sessions")
    }

    pub fn run_dir(&self) -> PathBuf {
        self.base().join("run")
    }

    pub fn log_dir(&self) -> PathBuf {
        self.base().join("log")
    }

    pub fn meta_path(&self, sid: &str) -> PathBuf {
        self.sessions_dir().join(format!("{sid}.json"))
    }

    pub fn exit_path(&self, sid: &str) -> PathBuf {
        self.sessions_dir().join(format!("{sid}.exit"))
    }

    pub fn spec_path(&self, sid: &str) -> PathBuf {
        self.sessions_dir().join(format!("{sid}.spec"))
    }

    pub fn log_path(&self, sid: &str) -> PathBuf {
        self.log_dir().join(format!("{sid}.log"))
    }

    /// 会话的连接端点：Unix 是 socket 路径，Windows 是命名管道名
    pub fn endpoint(&self, sid: &str) -> String {
        let root = self.root.to_string_lossy();
        #[cfg(unix)]
        {
            falcon_proto::sessiond::socket_path(&root, sid, unsafe { libc::getuid() })
        }
        #[cfg(windows)]
        {
            falcon_proto::sessiond::pipe_name(&root, sid)
        }
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        for d in [self.base(), self.sessions_dir(), self.run_dir(), self.log_dir()] {
            std::fs::create_dir_all(&d).with_context(|| format!("建不了目录 {}", d.display()))?;
            private_dir(&d)?;
        }
        Ok(())
    }

    pub fn read_meta(&self, sid: &str) -> Option<SessionMeta> {
        serde_json::from_slice(&std::fs::read(self.meta_path(sid)).ok()?).ok()
    }

    pub fn write_meta(&self, meta: &SessionMeta) -> Result<()> {
        write_atomic(&self.meta_path(&meta.sid), &serde_json::to_vec_pretty(meta)?)
    }

    pub fn read_exit(&self, sid: &str) -> Option<ExitRecord> {
        serde_json::from_slice(&std::fs::read(self.exit_path(sid)).ok()?).ok()
    }

    pub fn write_exit(&self, sid: &str, rec: &ExitRecord) -> Result<()> {
        write_atomic(&self.exit_path(sid), &serde_json::to_vec(rec)?)
    }

    pub fn write_spec(&self, sid: &str, spec: &SpawnSpec) -> Result<()> {
        write_atomic(&self.spec_path(sid), &serde_json::to_vec(spec)?)
    }

    /// 读出并删掉：里面有整份环境变量，不该在磁盘上久留
    pub fn take_spec(&self, sid: &str) -> Result<SpawnSpec> {
        let path = self.spec_path(sid);
        let bytes = std::fs::read(&path).with_context(|| format!("读不到 {}", path.display()))?;
        let _ = std::fs::remove_file(&path);
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// 会话的 holder 还活着（pid 在、而且确实是 falcon-sessiond，不是被复用的 pid）
    pub fn holder_alive(&self, sid: &str) -> Option<SessionMeta> {
        let meta = self.read_meta(sid)?;
        proc::is_sessiond(meta.pid).then_some(meta)
    }
}

/// 目录只给自己（Windows 上靠用户目录本身的 ACL）
pub fn private_dir(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("改不了 {} 的权限", dir.display()))?;
    }
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

/// 先写临时文件再改名：读的一方永远看不到半截
pub fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    {
        #[cfg(unix)]
        let mut f = {
            use std::os::unix::fs::OpenOptionsExt as _;
            std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)
        }
        .with_context(|| format!("写不了 {}", tmp.display()))?;
        #[cfg(not(unix))]
        let mut f = std::fs::File::create(&tmp).with_context(|| format!("写不了 {}", tmp.display()))?;
        std::io::Write::write_all(&mut f, data)?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("改不了名 {}", path.display()))?;
    Ok(())
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// 全部会话：活着的（有 meta）与已退出的（有 .exit）。顺手清掉过期的 .exit 与死掉的 meta
pub fn entries(layout: &Layout) -> Vec<ListEntry> {
    let Ok(dir) = std::fs::read_dir(layout.sessions_dir()) else { return Vec::new() };
    let now = now_ms();
    let mut out: Vec<ListEntry> = Vec::new();
    for e in dir.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some((sid, ext)) = name.split_once('.') else { continue };
        if !falcon_proto::sessiond::valid_sid(sid) {
            continue;
        }
        match ext {
            "json" => {
                let Some(meta) = layout.read_meta(sid) else { continue };
                let alive = proc::is_sessiond(meta.pid);
                out.push(ListEntry {
                    sid: sid.into(),
                    session_id: meta.session_id,
                    owner: meta.owner,
                    pid: meta.pid,
                    alive,
                    created_at: meta.created_at,
                    version: meta.version,
                    exited: layout.read_exit(sid),
                });
            }
            "exit" => {
                let Some(rec) = layout.read_exit(sid) else { continue };
                if now - rec.at > EXIT_KEEP_MS {
                    let _ = std::fs::remove_file(e.path());
                    continue;
                }
                if layout.meta_path(sid).is_file() {
                    continue;
                }
                out.push(ListEntry {
                    sid: sid.into(),
                    session_id: String::new(),
                    owner: String::new(),
                    pid: 0,
                    alive: false,
                    created_at: 0,
                    version: String::new(),
                    exited: Some(rec),
                });
            }
            _ => {}
        }
    }
    out.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.sid.cmp(&b.sid)));
    out
}

pub fn list(layout: &Layout, json: bool) -> Result<()> {
    let list = entries(layout);
    if json {
        println!("{}", serde_json::to_string(&list)?);
        return Ok(());
    }
    if list.is_empty() {
        println!("（没有会话）");
        return Ok(());
    }
    println!("{:<16}  {:<8}  {:<10}  创建于", "SID", "PID", "状态");
    for e in list {
        let state = match (&e.exited, e.alive) {
            (Some(x), _) => match (x.status.code, x.status.signal) {
                (Some(c), _) => format!("退出 {c}"),
                (None, Some(s)) => format!("信号 {s}"),
                _ => "已退出".into(),
            },
            (None, true) => "运行中".into(),
            (None, false) => "失联".into(),
        };
        let created = if e.created_at > 0 { format_time(e.created_at) } else { "-".into() };
        println!("{:<16}  {:<8}  {:<10}  {created}", e.sid, e.pid, state);
    }
    Ok(())
}

/// UTC 的 `YYYY-MM-DD HH:MM`：不为一个时间格式拉日期库
fn format_time(ms: i64) -> String {
    let secs = ms / 1000;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Howard Hinnant 的 civil_from_days
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02} UTC", rem / 3600, rem % 3600 / 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_time_format() {
        assert_eq!(format_time(0), "1970-01-01 00:00 UTC");
        assert_eq!(format_time(1_760_356_800_000), "2025-10-13 12:00 UTC");
    }

    #[test]
    fn spec_is_read_once() {
        let dir = std::env::temp_dir().join(format!("sd-layout-{}", std::process::id()));
        let l = Layout::new(dir.clone());
        l.ensure_dirs().unwrap();
        let spec = SpawnSpec { session_id: "x".into(), argv: vec!["/bin/sh".into()], cols: 80, rows: 24, ..Default::default() };
        l.write_spec("0123456789abcdef", &spec).unwrap();
        assert_eq!(l.take_spec("0123456789abcdef").unwrap(), spec);
        assert!(l.take_spec("0123456789abcdef").is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
