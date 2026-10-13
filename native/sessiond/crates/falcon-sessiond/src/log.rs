//! holder 的日志：写 `<root>/sessiond/log/<sid>.log`，满 1 MiB 轮转一次（留一份 `.1`）。
//! holder 的 stdio 都指到了空设备，出了事只能看这里。

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Mutex;

const ROTATE_AT: u64 = 1 << 20;

static LOG: Mutex<Option<PathBuf>> = Mutex::new(None);

pub fn init(path: PathBuf) {
    *LOG.lock().unwrap_or_else(|e| e.into_inner()) = Some(path);
}

pub fn write(msg: &str) {
    let guard = LOG.lock().unwrap_or_else(|e| e.into_inner());
    let Some(path) = guard.as_ref() else {
        eprintln!("{msg}");
        return;
    };
    if std::fs::metadata(path).is_ok_and(|m| m.len() > ROTATE_AT) {
        let _ = std::fs::rename(path, path.with_extension("log.1"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{} [{}] {msg}", crate::layout::now_ms(), std::process::id());
    }
}

macro_rules! logf {
    ($($t:tt)*) => { $crate::log::write(&format!($($t)*)) };
}
pub(crate) use logf;
