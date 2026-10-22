//! 内置插件面板上共用的数字写法。纯函数。

/// 1024 进制，三位有效数字上下：`512 B`、`8.1 GB`、`120 GB`
pub fn fmt_bytes(b: f64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut v = b.max(0.0);
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 || v >= 100.0 { format!("{v:.0} {}", UNITS[i]) } else if v >= 10.0 { format!("{v:.1} {}", UNITS[i]) } else { format!("{v:.2} {}", UNITS[i]) }
}

/// 10% 以上不带小数，以下带一位（`3.2%` 与 `0.4%` 的区别有意义，`43.2%` 与 `43%` 没有）
pub fn fmt_pct(p: f64) -> String {
    if p >= 10.0 { format!("{p:.0}%") } else { format!("{p:.1}%") }
}

/// 一段时长的粗略说法（"3 小时"、"2 天"），列表里一眼看个大概用；不足一分钟是"不到 1 分钟"
pub fn fmt_span(secs: i64) -> String {
    let s = secs.max(0);
    match s {
        0..60 => "不到 1 分钟".into(),
        60..3600 => format!("{} 分钟", s / 60),
        3600..86400 => format!("{} 小时", s / 3600),
        86400..2_592_000 => format!("{} 天", s / 86400),
        2_592_000..31_536_000 => format!("{} 个月", s / 2_592_000),
        _ => format!("{} 年", s / 31_536_000),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    #[test]
    fn formatting() {
        assert_eq!(fmt_bytes(0.0), "0 B");
        assert_eq!(fmt_bytes(512.0), "512 B");
        assert_eq!(fmt_bytes(1536.0), "1.50 KB");
        assert_eq!(fmt_bytes(8.1 * GIB as f64), "8.10 GB");
        assert_eq!(fmt_bytes(16.0 * GIB as f64), "16.0 GB");
        assert_eq!(fmt_bytes(500.0 * GIB as f64), "500 GB");
        assert_eq!(fmt_bytes(-5.0), "0 B");
        assert_eq!(fmt_pct(43.24), "43%");
        assert_eq!(fmt_pct(3.24), "3.2%");
        assert_eq!(fmt_span(-3), "不到 1 分钟");
        assert_eq!(fmt_span(59), "不到 1 分钟");
        assert_eq!(fmt_span(61), "1 分钟");
        assert_eq!(fmt_span(3 * 3600 + 59), "3 小时");
        assert_eq!(fmt_span(5 * 86400), "5 天");
        assert_eq!(fmt_span(70 * 86400), "2 个月");
        assert_eq!(fmt_span(800 * 86400), "2 年");
    }
}
