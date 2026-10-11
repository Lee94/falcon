//! 本地时区。旧 React 版用 `new Date()` 自带本地时区，Rust 标准库没有时区数据库，交给 chrono
//! （macOS / Linux 读 TZ 数据库，Windows 走系统 API）。
//!
//! 按**每个时间戳**取一次偏移，而不是启动时取一个固定值：有夏令时的地区，冬天与夏天的
//! 时间戳偏移差一小时，固定偏移会让一半的时间错一小时（React 版的 Date 没有这个错）。

use chrono::{Datelike, Local, TimeZone, Timelike};

/// 那一刻本地时间相对 UTC 的偏移（秒）；时间戳落在夏令时切换的空档 / 越界时按 UTC
pub fn utc_offset_at(sec: i64) -> i64 {
    Local
        .timestamp_opt(sec, 0)
        .earliest()
        .map(|dt| dt.offset().local_minus_utc() as i64)
        .unwrap_or(0)
}

/// 修改时间（Unix 秒）→ `YYYY-MM-DD HH:MM:SS`（本地时区），缺失画成 —
pub fn format_mtime(sec: Option<i64>) -> String {
    let offset = sec.map(utc_offset_at).unwrap_or(0);
    falcon_core::file_path::format_mtime(sec.map(|s| s as f64), offset)
}

/// 本地时区的年月日时分
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Civil {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
}

/// Unix 毫秒 → 本地年月日时分；越界时按 UTC 的 1970-01-01 00:00
pub fn civil(ms: i64) -> Civil {
    let dt = Local.timestamp_millis_opt(ms).earliest();
    match dt {
        Some(dt) => Civil {
            year: dt.year() as i64,
            month: dt.month(),
            day: dt.day(),
            hour: dt.hour(),
            minute: dt.minute(),
        },
        None => Civil { year: 1970, month: 1, day: 1, hour: 0, minute: 0 },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offset_is_sane() {
        // 真实时区在 UTC-12 … UTC+14 之间，且以 15 分钟为单位
        let off = utc_offset_at(1_758_700_000);
        assert!((-12 * 3600..=14 * 3600).contains(&off), "{off}");
        assert_eq!(off % 900, 0);
        assert_eq!(format_mtime(None), "—");
    }

    #[test]
    fn civil_matches_offset() {
        let ms = 1_758_700_000_000;
        let c = civil(ms);
        let local = ms / 1000 + utc_offset_at(ms / 1000);
        assert_eq!((c.hour as i64, c.minute as i64), ((local % 86_400) / 3600, (local % 3600) / 60));
    }
}
