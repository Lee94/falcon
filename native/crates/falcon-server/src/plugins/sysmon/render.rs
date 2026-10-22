//! 一次采样 + 走势 → 系统监控的面板文档。纯函数。
//!
//! 面板上的字是服务端写的（插件文档就是插件自己的内容，客户端只管画），v1 只有中文；
//! 文案都在这一个文件里，将来按请求带的语言分开时只动这里。

use std::collections::VecDeque;

use falcon_proto::{PluginAlign, PluginBlock, PluginColumn, PluginSeries, PluginStat, PluginTone, PluginView};

use super::sample::{ProcSort, Snapshot};
use crate::plugins::fmt::{fmt_bytes, fmt_pct};
use crate::plugins::machine::MachineChoice;

/// 走势图留多少个点（两秒一采就是三分钟）
pub const HISTORY: usize = 90;
/// 面板开着时多久重画一次（秒）
pub const REFRESH: f64 = 2.0;
/// 连不上 / 不支持时放慢：没有可看的，别两秒打一次 SSH
pub const REFRESH_IDLE: f64 = 10.0;

/// 走势里的一个点（采样时刻的读数）
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point {
    pub cpu: Option<f64>,
    pub mem: Option<f64>,
    pub rx: Option<f64>,
    pub tx: Option<f64>,
}

impl Point {
    pub fn of(s: &Snapshot) -> Self {
        Point { cpu: s.cpu, mem: pct(s.mem_used, s.mem_total), rx: s.net.map(|n| n.0), tx: s.net.map(|n| n.1) }
    }
}

pub fn pct(used: u64, total: u64) -> Option<f64> {
    (total > 0).then(|| used as f64 * 100.0 / total as f64)
}

pub fn fmt_rate(b: f64) -> String {
    format!("{}/s", fmt_bytes(b))
}

pub fn fmt_uptime(secs: u64) -> String {
    let (d, h, m) = (secs / 86400, secs % 86400 / 3600, secs % 3600 / 60);
    match (d, h) {
        (0, 0) => format!("{m} 分钟"),
        (0, _) => format!("{h} 小时 {m} 分"),
        _ => format!("{d} 天 {h} 小时"),
    }
}

/// 占用率的色调：75% 起警示、90% 起危险，其余用 `normal`
fn tone_for(p: f64, normal: Option<PluginTone>) -> Option<PluginTone> {
    if p >= 90.0 {
        Some(PluginTone::Danger)
    } else if p >= 75.0 {
        Some(PluginTone::Warning)
    } else {
        normal
    }
}

/// 磁盘满得慢、满了才要命：门槛高一点（85% / 95%）
fn disk_tone(p: f64) -> Option<PluginTone> {
    if p >= 95.0 {
        Some(PluginTone::Danger)
    } else if p >= 85.0 {
        Some(PluginTone::Warning)
    } else {
        None
    }
}

fn series(history: &VecDeque<Point>, f: impl Fn(&Point) -> Option<f64>) -> Vec<f64> {
    history.iter().map(|p| f(p).unwrap_or(0.0)).collect()
}

/// 没有数可看时（连不上 / 不支持）：机器选择 + 一条提示，好让人换一台
pub fn notice_view(machine: &MachineChoice, tone: PluginTone, text: String) -> PluginView {
    PluginView {
        title: Some(machine.label.clone()),
        refresh: Some(REFRESH_IDLE),
        blocks: vec![machine.select(), PluginBlock::Notice { text, tone: Some(tone) }],
        ..Default::default()
    }
}

pub fn view(machine: &MachineChoice, s: &Snapshot, history: &VecDeque<Point>) -> PluginView {
    let mut title = vec![match &s.host {
        Some(h) if *h != machine.label => format!("{}（{h}）", machine.label),
        _ => machine.label.clone(),
    }];
    title.extend(s.os.clone());
    title.extend(s.uptime.map(|u| format!("已运行 {}", fmt_uptime(u))));

    let mem_pct = pct(s.mem_used, s.mem_total);
    let root = s.disks.first();
    let root_pct = root.and_then(|d| pct(d.used(), d.total));
    let cores = (s.cores > 0).then(|| format!("{} 核", s.cores));
    let mut stats = vec![
        PluginStat {
            label: "CPU".into(),
            value: s.cpu.map(fmt_pct).unwrap_or_else(|| "—".into()),
            detail: cores.clone(),
            tone: s.cpu.and_then(|c| tone_for(c, None)),
        },
        PluginStat {
            label: "内存".into(),
            value: mem_pct.map(fmt_pct).unwrap_or_else(|| "—".into()),
            detail: Some(format!("{} / {}", fmt_bytes(s.mem_used as f64), fmt_bytes(s.mem_total as f64))),
            tone: mem_pct.and_then(|m| tone_for(m, None)),
        },
    ];
    if let Some([l1, l5, l15]) = s.load {
        // 负载按核数看才有意义：8 核机器上 8 是满载、4 核上 8 是排队
        let per_core = (s.cores > 0).then(|| l1 * 100.0 / s.cores as f64);
        stats.push(PluginStat {
            label: "负载".into(),
            value: format!("{l1:.2}"),
            detail: Some(format!("{l5:.2} · {l15:.2}")),
            tone: per_core.and_then(|p| tone_for(p, None)),
        });
    }
    if let Some((d, p)) = root.zip(root_pct) {
        stats.push(PluginStat {
            label: "磁盘".into(),
            value: fmt_pct(p),
            detail: Some(format!("{} 可用", fmt_bytes(d.available as f64))),
            tone: disk_tone(p),
        });
    }

    let mut blocks = vec![machine.select(), PluginBlock::Stats { items: stats }];
    blocks.push(PluginBlock::Chart {
        label: Some("CPU".into()),
        text: s.cpu.map(|c| match &cores {
            Some(n) => format!("{} · {n}", fmt_pct(c)),
            None => fmt_pct(c),
        }),
        series: vec![PluginSeries { name: None, values: series(history, |p| p.cpu), tone: Some(PluginTone::Accent) }],
        max: Some(100.0),
        slots: Some(HISTORY as u32),
    });
    blocks.push(PluginBlock::Chart {
        label: Some("内存".into()),
        text: Some(format!("{} / {}", fmt_bytes(s.mem_used as f64), fmt_bytes(s.mem_total as f64))),
        series: vec![PluginSeries { name: None, values: series(history, |p| p.mem), tone: Some(PluginTone::Accent) }],
        max: Some(100.0),
        slots: Some(HISTORY as u32),
    });
    if s.swap_total > 0 {
        let p = pct(s.swap_used, s.swap_total).unwrap_or(0.0);
        blocks.push(PluginBlock::Meter {
            label: "交换".into(),
            value: s.swap_used as f64,
            max: Some(s.swap_total as f64),
            text: Some(format!("{} / {}", fmt_bytes(s.swap_used as f64), fmt_bytes(s.swap_total as f64))),
            tone: tone_for(p, None),
        });
    }
    blocks.push(PluginBlock::Chart {
        label: Some("网络".into()),
        text: s.net.map(|(rx, tx)| format!("↓ {}  ↑ {}", fmt_rate(rx), fmt_rate(tx))),
        series: vec![
            PluginSeries { name: Some("下行".into()), values: series(history, |p| p.rx), tone: Some(PluginTone::Accent) },
            PluginSeries { name: Some("上行".into()), values: series(history, |p| p.tx), tone: Some(PluginTone::Success) },
        ],
        max: None,
        slots: Some(HISTORY as u32),
    });

    if !s.disks.is_empty() {
        blocks.push(PluginBlock::Heading { text: "磁盘".into(), detail: None });
        for d in &s.disks {
            let p = pct(d.used(), d.total).unwrap_or(0.0);
            blocks.push(PluginBlock::Meter {
                label: d.mount.clone(),
                value: d.used() as f64,
                max: Some(d.total as f64),
                text: Some(format!("{} / {}", fmt_bytes(d.used() as f64), fmt_bytes(d.total as f64))),
                tone: disk_tone(p),
            });
        }
    }

    if !s.procs.is_empty() {
        let (detail, columns) = match s.procs_by {
            ProcSort::Cpu => ("按 CPU", vec!["进程", "CPU", "内存"]),
            ProcSort::Memory => ("按内存", vec!["进程", "内存"]),
        };
        blocks.push(PluginBlock::Heading { text: "进程".into(), detail: Some(detail.into()) });
        let columns = columns
            .into_iter()
            .enumerate()
            .map(|(i, t)| PluginColumn { title: t.into(), align: (i > 0).then_some(PluginAlign::Right) })
            .collect();
        let rows = s
            .procs
            .iter()
            .map(|p| {
                let mut row = vec![p.name.clone()];
                if s.procs_by == ProcSort::Cpu {
                    row.push(fmt_pct(p.cpu.unwrap_or(0.0)));
                }
                row.push(fmt_bytes(p.mem as f64));
                row
            })
            .collect();
        blocks.push(PluginBlock::Table { columns, rows });
    }

    PluginView { title: Some(title.join(" · ")), refresh: Some(REFRESH), blocks, ..Default::default() }
}

#[cfg(test)]
mod tests {
    use super::super::sample::{DiskUse, Proc};
    use super::*;

    const GIB: u64 = 1 << 30;

    #[test]
    fn formatting() {
        assert_eq!(fmt_rate(2048.0), "2.00 KB/s");
        assert_eq!(fmt_uptime(59), "0 分钟");
        assert_eq!(fmt_uptime(3 * 3600 + 7 * 60), "3 小时 7 分");
        assert_eq!(fmt_uptime(3 * 86400 + 4 * 3600 + 30), "3 天 4 小时");
    }

    fn machine() -> MachineChoice {
        use falcon_proto::PluginOption;
        MachineChoice {
            value: "auto".into(),
            options: vec![PluginOption { value: "auto".into(), label: "跟随当前项目".into() }],
            label: "本机".into(),
        }
    }

    fn snap() -> Snapshot {
        Snapshot {
            host: Some("mac-mini".into()),
            os: Some("macOS 15.1".into()),
            uptime: Some(90000),
            cores: 8,
            cpu: Some(12.34),
            load: Some([9.0, 2.0, 1.0]),
            mem_total: 16 * GIB,
            mem_used: 15 * GIB,
            swap_total: 0,
            swap_used: 0,
            net: Some((1024.0, 0.0)),
            disks: vec![DiskUse { mount: "/".into(), total: 100 * GIB, available: 4 * GIB }],
            procs: vec![Proc { name: "cargo".into(), cpu: Some(250.0), mem: GIB }],
            procs_by: ProcSort::Cpu,
        }
    }

    #[test]
    fn full_view() {
        let history: VecDeque<Point> = [Point { cpu: Some(10.0), ..Default::default() }, Point::of(&snap())].into();
        let v = view(&machine(), &snap(), &history);
        assert_eq!(v.title.as_deref(), Some("本机（mac-mini） · macOS 15.1 · 已运行 1 天 1 小时"));
        assert_eq!(v.refresh, Some(REFRESH));
        let PluginBlock::Select { value, .. } = &v.blocks[0] else { panic!() };
        assert_eq!(value.as_deref(), Some("auto"));
        let PluginBlock::Stats { items } = &v.blocks[1] else { panic!() };
        let values: Vec<_> = items.iter().map(|s| (s.label.as_str(), s.value.as_str(), s.tone)).collect();
        assert_eq!(
            values,
            vec![
                ("CPU", "12%", None),
                ("内存", "94%", Some(PluginTone::Danger)),
                ("负载", "9.00", Some(PluginTone::Danger)),
                ("磁盘", "96%", Some(PluginTone::Danger)),
            ]
        );
        let PluginBlock::Chart { series, text, .. } = &v.blocks[2] else { panic!() };
        assert_eq!(series[0].values, vec![10.0, 12.34]);
        assert_eq!(text.as_deref(), Some("12% · 8 核"));
        // 没有交换就不画那一条；网络、磁盘标题、一块盘、进程标题、进程表
        assert!(matches!(&v.blocks[4], PluginBlock::Chart { label: Some(l), .. } if l == "网络"));
        assert!(matches!(&v.blocks[5], PluginBlock::Heading { text, .. } if text == "磁盘"));
        let PluginBlock::Table { columns, rows } = v.blocks.last().unwrap() else { panic!() };
        assert_eq!(columns.len(), 3);
        assert_eq!(rows[0], vec!["cargo", "250%", "1.00 GB"]);
    }

    #[test]
    fn first_sample_and_remote_shape() {
        let mut s = snap();
        s.cpu = None;
        s.net = None;
        s.load = None;
        s.host = Some("本机".into());
        s.procs_by = ProcSort::Memory;
        let v = view(&machine(), &s, &VecDeque::new());
        assert!(v.title.unwrap().starts_with("本机 · "), "主机名与标签相同就不重复");
        let PluginBlock::Stats { items } = &v.blocks[1] else { panic!() };
        assert_eq!(items[0].value, "—");
        assert_eq!(items.len(), 3, "没有负载就不放那一格");
        let PluginBlock::Table { columns, rows } = v.blocks.last().unwrap() else { panic!() };
        assert_eq!(columns.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(), vec!["进程", "内存"]);
        assert_eq!(rows[0].len(), 2);
    }

    #[test]
    fn notice_keeps_the_machine_picker() {
        let v = notice_view(&machine(), PluginTone::Warning, "不支持".into());
        assert_eq!(v.refresh, Some(REFRESH_IDLE));
        assert!(matches!(v.blocks[0], PluginBlock::Select { .. }));
        assert!(matches!(&v.blocks[1], PluginBlock::Notice { text, .. } if text == "不支持"));
    }
}
