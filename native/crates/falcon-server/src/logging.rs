//! 服务端的日志出口。
//!
//! 默认写 stderr（`FALCON_LOG=debug` 打开调试日志，与原生客户端同一个开关）。Windows 的
//! release 是 GUI 子系统：计划任务 / 双击起它时没有控制台，std 句柄无效，写出去的东西被
//! std 静默丢弃（`StdoutRaw` / `StderrRaw` 把 ERROR_INVALID_HANDLE 特判成写成功，所以
//! `println!` 不会 panic，但也没人看得到），所以那种处境下改写
//! `<dataDir>/logs/falcon.log`——与 macOS launchd 的 StandardOutPath 同一个文件。
//! 写失败一律忽略：日志不该把服务弄崩。

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

/// 轮转口径与 sessiond 的 log.rs 一致：满 1 MiB 轮转一次，留一份 `.log.1`
const ROTATE_AT: u64 = 1 << 20;

static FILE: Mutex<Option<PathBuf>> = Mutex::new(None);

/// 装 logger。先于一切子命令：service / node / update-helper 也可能要记日志
pub fn init() {
    struct Logger(log::LevelFilter);
    impl log::Log for Logger {
        fn enabled(&self, m: &log::Metadata) -> bool {
            m.level() <= self.0
        }
        fn log(&self, r: &log::Record) {
            if self.enabled(r.metadata()) {
                emit(&format!("[{} {}] {}", r.level(), r.target(), r.args()));
            }
        }
        fn flush(&self) {}
    }
    let level = if std::env::var("FALCON_LOG").is_ok_and(|v| v == "debug") {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    };
    let _ = log::set_boxed_logger(Box::new(Logger(level))).map(|()| log::set_max_level(level));
}

/// 日志改写到 `<dataDir>/logs/falcon.log`（launchd 不会替我们建日志目录，这里自己建）。
/// 目录建不出来就维持原出口——反正写失败是被忽略的
pub fn redirect_to_file(data_dir: &Path) {
    let dir = data_dir.join("logs");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    *guard() = Some(dir.join("falcon.log"));
}

fn guard() -> MutexGuard<'static, Option<PathBuf>> {
    FILE.lock().unwrap_or_else(|e| e.into_inner())
}

fn emit(line: &str) {
    if let Some(path) = guard().as_ref() {
        append_line(path, line);
        return;
    }
    // 没有控制台时这句写不出去（std 静默丢弃），所以服务实例要先 redirect_to_file
    let _ = writeln!(std::io::stderr(), "{line}");
}

/// stderr 还写不写得出去：GUI 子系统 + 没有控制台 + 没有重定向时 `GetStdHandle` 返回 NULL。
/// 管道与文件重定向都是有效句柄，照旧写 stderr（调用方正在收）
#[cfg(windows)]
pub fn stderr_usable() -> bool {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE};
    let handle = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
    !handle.is_null() && handle != INVALID_HANDLE_VALUE
}

/// 每次写都重新 open-append：Windows 上改名一个打开着的文件会失败，常驻句柄就轮转不动了
fn append_line(path: &Path, line: &str) {
    if std::fs::metadata(path).is_ok_and(|m| m.len() > ROTATE_AT) {
        let _ = std::fs::rename(path, path.with_extension("log.1"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_then_rotates_once() {
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join("falcon.log");
        append_line(&path, "第一行");
        append_line(&path, "第二行");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "第一行\n第二行\n");
        // 撑过阈值：旧内容整份挪到 .log.1，新文件只留这一行
        std::fs::write(&path, "x".repeat(ROTATE_AT as usize + 1)).unwrap();
        append_line(&path, "轮转后");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "轮转后\n");
        assert_eq!(std::fs::metadata(path.with_extension("log.1")).unwrap().len(), ROTATE_AT + 1);
    }

    #[test]
    fn redirect_creates_logs_dir() {
        let dir = tempfile::tempdir().expect("临时目录");
        redirect_to_file(dir.path());
        assert_eq!(guard().as_deref(), Some(dir.path().join("logs").join("falcon.log").as_path()));
        *guard() = None; // 别把出口留给同进程的其它测试
    }
}