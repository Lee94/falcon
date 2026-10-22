//! Docker 面板的文档。纯函数：容器列表、一个容器的详情（含日志）、镜像与磁盘占用、各种连不上的提示。
//!
//! 面板上的字是服务端写的（同系统监控，v1 只有中文），文案都在这一个文件里。
//!
//! 进出详情靠面板参数 [`CONTAINER`]：列表行点进去是 `setParam`，「返回」把它改回空串；
//! 详情里的容器没了（被删了、换了机器）由 mod.rs 改回列表并在文档的 `params` 里把它清掉。

use std::collections::{BTreeMap, HashMap};

use falcon_proto::{
    PluginAction, PluginBlock, PluginButton, PluginKv, PluginListItem, PluginOption, PluginStat, PluginTone, PluginView,
};

use super::cli::Failure;
use super::parse::{Container, Daemon, DiskUse, Image, Status, Stats, stats_for};
use crate::plugins::fmt::{fmt_bytes, fmt_pct, fmt_span};
use crate::plugins::machine::MachineChoice;

/// 面板参数：哪一页
pub const TAB: &str = "tab";
/// 面板参数：详情里的容器 id（空 = 列表）
pub const CONTAINER: &str = "container";
/// 列表与详情多久重画一次（秒）。`ps` + `inspect` 本机几十毫秒，经 SSH 两个来回
pub const REFRESH: f64 = 3.0;
/// 镜像页：`system df` 要算卷的大小，卷大了一次几秒；镜像也不常变
pub const REFRESH_IMAGES: f64 = 15.0;
/// 连不上 / 没装：没有可看的，放慢
pub const REFRESH_IDLE: f64 = 10.0;
/// 占用还在采（第一次 `docker stats` 要一两秒）：早点再画一次，好让数字出来
pub const REFRESH_SOON: f64 = 1.5;
/// 详情里的日志看最后多少行、最多多少字（面板文档里一块文字的上限是 32K 字符）
pub const LOG_LINES: usize = 200;
pub const LOG_CHARS: usize = 30_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Containers,
    Images,
}

impl Tab {
    pub fn of(params: &BTreeMap<String, String>) -> Self {
        match params.get(TAB).map(String::as_str) {
            Some("images") => Tab::Images,
            _ => Tab::Containers,
        }
    }
}

/// 面板顶上的东西：看的是哪台、它的守护进程是什么
pub struct Header<'a> {
    pub machine: &'a MachineChoice,
    pub daemon: Option<&'a Daemon>,
}

impl Header<'_> {
    fn title(&self) -> String {
        let mut parts = vec![self.machine.label.clone()];
        if let Some(d) = self.daemon {
            if !d.os.is_empty() {
                parts.push(d.os.clone());
            }
            parts.push(format!("Docker {}", d.version));
        }
        parts.join(" · ")
    }
}

fn tabs(tab: Tab) -> PluginBlock {
    PluginBlock::Select {
        param: TAB.into(),
        label: None,
        value: Some(if tab == Tab::Images { "images" } else { "containers" }.into()),
        options: vec![
            PluginOption { value: "containers".into(), label: "容器".into() },
            PluginOption { value: "images".into(), label: "镜像".into() },
        ],
    }
}

fn invoke(id: &str, value: &str, confirm: Option<String>) -> PluginAction {
    PluginAction::Invoke { id: id.into(), value: Some(value.into()), confirm }
}

fn button(label: &str, icon: &str, tone: Option<PluginTone>, action: PluginAction) -> PluginButton {
    PluginButton { label: label.into(), action, icon: Some(icon.into()), tone }
}

fn copy(label: &str, text: String) -> PluginButton {
    PluginButton { label: label.into(), action: PluginAction::Copy { text }, icon: Some("copy".into()), tone: None }
}

fn short(id: &str) -> &str {
    &id[..id.len().min(12)]
}

/// 列表里那一行的状态说法与色调
fn phrase(c: &Container, now: i64) -> (String, PluginTone) {
    let ago = |t: Option<i64>| t.map(|t| format!("{}前", fmt_span(now - t)));
    match c.status {
        Status::Running => {
            let mut s = match c.started {
                Some(t) => format!("运行 {}", fmt_span(now - t)),
                None => "运行中".into(),
            };
            let tone = match c.health.as_deref() {
                Some("healthy") => {
                    s.push_str(" · 健康");
                    PluginTone::Success
                }
                Some("unhealthy") => {
                    s.push_str(" · 不健康");
                    PluginTone::Warning
                }
                Some("starting") => {
                    s.push_str(" · 检查中");
                    PluginTone::Success
                }
                _ => PluginTone::Success,
            };
            (s, tone)
        }
        Status::Paused => ("已暂停".into(), PluginTone::Warning),
        Status::Restarting => (format!("重启中（第 {} 次）", c.restart_count), PluginTone::Warning),
        Status::Exited => {
            let why = if c.oom_killed { "内存不足被杀".to_string() } else { c.exit_code.to_string() };
            let s = match ago(c.finished) {
                Some(a) => format!("已退出（{why}）· {a}"),
                None => format!("已退出（{why}）"),
            };
            (s, if c.exit_code == 0 && !c.oom_killed { PluginTone::Muted } else { PluginTone::Danger })
        }
        Status::Created => ("已创建，没启动过".into(), PluginTone::Muted),
        Status::Removing => ("删除中".into(), PluginTone::Muted),
        Status::Dead => ("已失效（没删干净）".into(), PluginTone::Danger),
    }
}

fn exec_command(c: &Container) -> String {
    format!("docker exec -it {} sh", c.name)
}

fn remove_confirm(c: &Container) -> String {
    format!("删除容器「{}」？它的可写层（没挂卷的那些改动）会一起删掉。", c.name)
}

/// 行尾「⋯」菜单
fn container_menu(c: &Container) -> Vec<PluginButton> {
    let id = c.id.as_str();
    let item = |label: &str, action: PluginAction, tone: Option<PluginTone>| PluginButton { label: label.into(), action, icon: None, tone };
    let mut menu = match c.status {
        Status::Running => vec![
            item("停止", invoke("container.stop", id, None), None),
            item("重启", invoke("container.restart", id, None), None),
            item("暂停", invoke("container.pause", id, None), None),
        ],
        Status::Paused => vec![
            item("恢复", invoke("container.unpause", id, None), None),
            item("停止", invoke("container.stop", id, None), None),
        ],
        Status::Restarting => vec![item("停止", invoke("container.stop", id, None), None)],
        Status::Removing => vec![],
        Status::Exited | Status::Created | Status::Dead => vec![
            item("启动", invoke("container.start", id, None), None),
            item("删除", invoke("container.remove", id, Some(remove_confirm(c))), Some(PluginTone::Danger)),
        ],
    };
    menu.push(item("详情与日志", PluginAction::SetParam { param: CONTAINER.into(), value: c.id.clone() }, None));
    menu.push(item("复制 ID", PluginAction::Copy { text: c.id.clone() }, None));
    if c.status == Status::Running {
        menu.push(item("复制进入命令", PluginAction::Copy { text: exec_command(c) }, None));
    }
    menu
}

/// 跑着的排前面，同一档按名字
fn rank(c: &Container) -> (u8, &str) {
    let r = match c.status {
        Status::Running | Status::Restarting | Status::Paused => 0,
        _ => 1,
    };
    (r, c.name.as_str())
}

pub fn containers_view(h: &Header, list: &[Container], stats: Option<&HashMap<String, Stats>>, now: i64) -> PluginView {
    let mut blocks = vec![h.machine.select(), tabs(Tab::Containers)];
    let running: Vec<&Container> = list.iter().filter(|c| c.status == Status::Running).collect();
    let stopped = list.iter().filter(|c| c.status.stopped()).count();
    let crashed = list.iter().filter(|c| c.status == Status::Exited && (c.exit_code != 0 || c.oom_killed)).count();

    let usage: Vec<&Stats> = stats.map(|s| running.iter().filter_map(|c| stats_for(s, &c.id)).collect()).unwrap_or_default();
    let cpu: Option<f64> = stats.map(|_| usage.iter().filter_map(|s| s.cpu).sum());
    let mem: Option<f64> = stats.map(|_| usage.iter().filter_map(|s| s.mem).sum());
    let waiting = stats.is_none() && !running.is_empty();
    let cpus = h.daemon.map(|d| d.cpus).filter(|n| *n > 0);
    let mem_total = h.daemon.map(|d| d.mem_total).filter(|n| *n > 0);
    let mem_pct = mem.zip(mem_total).map(|(m, t)| m * 100.0 / t as f64);
    blocks.push(PluginBlock::Stats {
        items: vec![
            PluginStat {
                label: "在跑".into(),
                value: running.len().to_string(),
                detail: Some(format!("共 {} 个容器", list.len())),
                tone: (!running.is_empty()).then_some(PluginTone::Success),
            },
            PluginStat {
                label: "已停止".into(),
                value: stopped.to_string(),
                detail: (crashed > 0).then(|| format!("{crashed} 个异常退出")),
                tone: (crashed > 0).then_some(PluginTone::Danger),
            },
            PluginStat {
                label: "CPU".into(),
                // docker 的百分比按单核计（两核跑满是 200%）；有核数就折成整机的
                value: match (cpu, cpus) {
                    (Some(c), Some(n)) => fmt_pct(c / n as f64),
                    (Some(c), None) => fmt_pct(c),
                    (None, _) if waiting => "…".into(),
                    (None, _) => "—".into(),
                },
                detail: if waiting { Some("采样中".into()) } else { cpus.map(|n| format!("整机 {n} 核")) },
                tone: None,
            },
            PluginStat {
                label: "内存".into(),
                value: match mem {
                    Some(m) => fmt_bytes(m),
                    None if waiting => "…".into(),
                    None => "—".into(),
                },
                detail: if waiting {
                    Some("采样中".into())
                } else {
                    // 窄栏里卡片只有十来个字宽：总量在系统监控里看
                    mem_pct.map(|p| format!("占整机 {}", fmt_pct(p)))
                },
                tone: mem_pct.filter(|p| *p >= 90.0).map(|_| PluginTone::Danger),
            },
        ],
    });

    if list.is_empty() {
        blocks.push(PluginBlock::Notice { text: "这台机器上还没有容器。".into(), tone: Some(PluginTone::Muted) });
    }

    // compose 起的按项目分组（项目名排序），其余的放最后；一个 compose 项目都没有就不分组
    let mut groups: BTreeMap<Option<&str>, Vec<&Container>> = BTreeMap::new();
    for c in list {
        groups.entry(c.project.as_deref()).or_default().push(c);
    }
    let grouped = groups.keys().any(Option::is_some);
    let mut order: Vec<(Option<&str>, Vec<&Container>)> = groups.into_iter().filter(|(k, _)| k.is_some()).collect();
    order.extend(list.iter().any(|c| c.project.is_none()).then(|| (None, list.iter().filter(|c| c.project.is_none()).collect())));
    for (project, mut members) in order {
        members.sort_by(|a, b| rank(a).cmp(&rank(b)));
        if grouped {
            let up = members.iter().filter(|c| c.status == Status::Running).count();
            blocks.push(PluginBlock::Heading {
                text: project.map_or_else(|| "其他容器".into(), str::to_string),
                detail: Some(format!("{up} / {} 在跑", members.len())),
            });
        }
        let items = members
            .into_iter()
            .map(|c| {
                let (state, tone) = phrase(c, now);
                let badge = (c.status == Status::Running)
                    .then(|| stats.and_then(|s| stats_for(s, &c.id)).and_then(|s| s.cpu).map(fmt_pct))
                    .flatten();
                PluginListItem {
                    title: c.name.clone(),
                    detail: Some(format!("{state} · {}", c.image)),
                    badge,
                    tone: Some(tone),
                    action: Some(PluginAction::SetParam { param: CONTAINER.into(), value: c.id.clone() }),
                    menu: container_menu(c),
                }
            })
            .collect();
        blocks.push(PluginBlock::List { items });
    }

    if stopped > 0 {
        blocks.push(PluginBlock::Actions {
            items: vec![button(
                &format!("清理停止的容器（{stopped}）"),
                "brush-cleaning",
                None,
                PluginAction::Invoke {
                    id: "containers.prune".into(),
                    value: None,
                    confirm: Some(format!("删掉这台机器上全部 {stopped} 个停止的容器？它们的可写层会一起删掉，卷不动。")),
                },
            )],
        });
    }

    PluginView {
        title: Some(h.title()),
        refresh: Some(if waiting { REFRESH_SOON } else { REFRESH }),
        blocks,
        ..Default::default()
    }
}

fn port_text(p: &super::parse::Port) -> String {
    match &p.host {
        Some((ip, port)) if ip.is_empty() => format!(":{port} → {}", p.container),
        Some((ip, port)) => format!("{ip}:{port} → {}", p.container),
        None => format!("{}（没发布）", p.container),
    }
}

fn restart_text(c: &Container) -> String {
    let policy = match c.restart_policy.as_str() {
        "" | "no" => "不自动重启".to_string(),
        "always" => "总是重启".into(),
        "unless-stopped" => "除非手动停止".into(),
        "on-failure" if c.restart_max > 0 => format!("失败时重启（最多 {} 次）", c.restart_max),
        "on-failure" => "失败时重启".into(),
        other => other.to_string(),
    };
    if c.restart_count > 0 { format!("{policy}，已重启 {} 次", c.restart_count) } else { policy }
}

/// 多值的键（端口、挂载、网络）：第一行写键，其余行空着，看着是一组
fn kv_rows(items: &mut Vec<PluginKv>, key: &str, values: Vec<String>, mono: bool) {
    for (i, value) in values.into_iter().enumerate() {
        items.push(PluginKv { key: if i == 0 { key.into() } else { String::new() }, value, tone: None, mono });
    }
}

pub fn detail_view(h: &Header, c: &Container, stats: Option<&Stats>, logs: &Result<String, String>, now: i64) -> PluginView {
    let (state, tone) = phrase(c, now);
    let mut blocks = vec![
        PluginBlock::Actions {
            items: vec![button("返回", "arrow-left", None, PluginAction::SetParam { param: CONTAINER.into(), value: String::new() })],
        },
        PluginBlock::Heading { text: c.name.clone(), detail: Some(state.clone()) },
    ];
    if !c.error.is_empty() {
        blocks.push(PluginBlock::Notice { text: format!("守护进程记下的错误：{}", c.error), tone: Some(PluginTone::Danger) });
    }
    if c.oom_killed {
        blocks.push(PluginBlock::Notice { text: "上一次是因为内存不足被杀掉的（OOMKilled）。".into(), tone: Some(PluginTone::Warning) });
    }

    let id = c.id.as_str();
    let ops = match c.status {
        Status::Running => vec![
            button("停止", "square", None, invoke("container.stop", id, None)),
            button("重启", "rotate-cw", None, invoke("container.restart", id, None)),
            button("暂停", "pause", None, invoke("container.pause", id, None)),
        ],
        Status::Paused => vec![
            button("恢复", "play", Some(PluginTone::Accent), invoke("container.unpause", id, None)),
            button("停止", "square", None, invoke("container.stop", id, None)),
        ],
        Status::Restarting => vec![button("停止", "square", None, invoke("container.stop", id, None))],
        Status::Removing => vec![],
        Status::Exited | Status::Created | Status::Dead => vec![
            button("启动", "play", Some(PluginTone::Accent), invoke("container.start", id, None)),
            button("删除", "trash", Some(PluginTone::Danger), invoke("container.remove", id, Some(remove_confirm(c)))),
        ],
    };
    if !ops.is_empty() {
        blocks.push(PluginBlock::Actions { items: ops });
    }

    if c.status == Status::Running {
        let pending = || "…".to_string();
        let limit = stats.and_then(|s| s.mem_text.split('/').nth(1)).map(|l| l.trim().to_string());
        // `830B / 126B` 放不进一张卡片的大字：收的放大字，发的放小字（docker 给的是启动以来的累计）
        let net = stats.map(|s| s.net.split('/').map(|x| x.trim().to_string()).collect::<Vec<_>>()).filter(|v| v.len() == 2);
        blocks.push(PluginBlock::Stats {
            items: vec![
                PluginStat {
                    label: "CPU".into(),
                    value: stats.and_then(|s| s.cpu).map_or_else(pending, fmt_pct),
                    detail: Some(if stats.is_some() { "按单核计".into() } else { "采样中".into() }),
                    tone: None,
                },
                PluginStat {
                    label: "内存".into(),
                    value: stats.and_then(|s| s.mem).map_or_else(pending, fmt_bytes),
                    // 占上限的比例只看色调：没设限制时上限就是整机内存，百分比总是零点几
                    detail: limit.map(|l| format!("上限 {l}")),
                    tone: stats.and_then(|s| s.mem_pct).filter(|p| *p >= 90.0).map(|_| PluginTone::Danger),
                },
                PluginStat {
                    label: "网络（累计）".into(),
                    value: net.as_ref().map_or_else(pending, |v| format!("↓ {}", v[0])),
                    detail: net.as_ref().map(|v| format!("↑ {}", v[1])),
                    tone: None,
                },
                PluginStat {
                    label: "进程".into(),
                    value: stats.map(|s| s.pids.clone()).filter(|s| !s.is_empty()).unwrap_or_else(pending),
                    detail: None,
                    tone: None,
                },
            ],
        });
    }

    let mut kv = vec![
        PluginKv { key: "状态".into(), value: state, tone: Some(tone), mono: false },
        PluginKv { key: "镜像".into(), value: c.image.clone(), tone: None, mono: true },
    ];
    if let Some(health) = &c.health {
        let (text, tone) = match health.as_str() {
            "healthy" => ("健康", PluginTone::Success),
            "unhealthy" => ("不健康", PluginTone::Danger),
            "starting" => ("检查中", PluginTone::Muted),
            other => (other, PluginTone::Muted),
        };
        kv.push(PluginKv { key: "健康检查".into(), value: text.into(), tone: Some(tone), mono: false });
    }
    if let Some(p) = &c.project {
        let value = match &c.service {
            Some(s) => format!("{p} · {s}"),
            None => p.clone(),
        };
        kv.push(PluginKv { key: "Compose".into(), value, tone: None, mono: false });
    }
    kv_rows(&mut kv, "端口", c.ports.iter().map(port_text).collect(), true);
    kv_rows(&mut kv, "网络", c.networks.iter().map(|(n, ip)| if ip.is_empty() { n.clone() } else { format!("{n} · {ip}") }).collect(), false);
    kv_rows(
        &mut kv,
        "挂载",
        c.mounts
            .iter()
            .map(|m| format!("{} → {}{}", m.source, m.destination, if m.rw { "" } else { "（只读）" }))
            .collect(),
        true,
    );
    if !c.command.is_empty() {
        kv.push(PluginKv { key: "命令".into(), value: c.command.join(" "), tone: None, mono: true });
    }
    kv.push(PluginKv { key: "重启策略".into(), value: restart_text(c), tone: None, mono: false });
    if let Some(t) = c.created {
        kv.push(PluginKv { key: "创建于".into(), value: format!("{}前", fmt_span(now - t)), tone: None, mono: false });
    }
    kv.push(PluginKv { key: "ID".into(), value: short(id).into(), tone: None, mono: true });
    blocks.push(PluginBlock::Kv { items: kv });

    blocks.push(PluginBlock::Heading { text: "日志".into(), detail: Some(format!("最近 {LOG_LINES} 行")) });
    let mut copies = vec![copy("复制 ID", c.id.clone())];
    if c.status == Status::Running {
        copies.push(copy("复制进入命令", exec_command(c)));
    }
    match logs {
        Ok(text) if text.trim().is_empty() => {
            blocks.push(PluginBlock::Actions { items: copies });
            blocks.push(PluginBlock::Text { text: "还没有日志。".into(), tone: Some(PluginTone::Muted), mono: false });
        }
        Ok(text) => {
            copies.insert(0, copy("复制日志", text.clone()));
            blocks.push(PluginBlock::Actions { items: copies });
            blocks.push(PluginBlock::Text { text: text.clone(), tone: None, mono: true });
        }
        Err(e) => {
            blocks.push(PluginBlock::Actions { items: copies });
            blocks.push(PluginBlock::Notice { text: format!("读不了日志：{e}"), tone: Some(PluginTone::Warning) });
        }
    }

    PluginView {
        title: Some(h.title()),
        refresh: Some(if c.status == Status::Running && stats.is_none() { REFRESH_SOON } else { REFRESH }),
        blocks,
        ..Default::default()
    }
}

fn df_label(kind: &str) -> &str {
    match kind {
        "Images" => "镜像",
        "Build Cache" => "构建缓存",
        "Local Volumes" => "卷",
        "Containers" => "容器可写层",
        other => other,
    }
}

pub fn images_view(h: &Header, images: &[Image], df: &[DiskUse], now: i64) -> PluginView {
    let mut blocks = vec![h.machine.select(), tabs(Tab::Images)];
    // 镜像、构建缓存、卷、容器：最常要清的在前
    let rank = |k: &str| ["Images", "Build Cache", "Local Volumes", "Containers"].iter().position(|x| *x == k).unwrap_or(9);
    let mut df: Vec<&DiskUse> = df.iter().collect();
    df.sort_by_key(|d| rank(&d.kind));
    if !df.is_empty() {
        blocks.push(PluginBlock::Stats {
            items: df
                .iter()
                .map(|d| {
                    let count = match d.kind.as_str() {
                        // 构建缓存的"个数"是缓存记录数，对人没有意义；镜像的个数含中间层，与下面列表
                        // （`image ls` 不列中间层）对不上，个数看列表的标题
                        "Build Cache" | "Containers" | "Images" => None,
                        _ => Some(format!("{} 个", d.count)),
                    };
                    // `3.48GB (69%)`：卡片只有十来个字宽，比例不要
                    let amount = d.reclaimable.split(" (").next().unwrap_or("").trim();
                    let reclaim = (!amount.is_empty() && amount != "0B").then(|| format!("可回收 {amount}"));
                    let detail = [count, reclaim].into_iter().flatten().collect::<Vec<_>>().join(" · ");
                    PluginStat {
                        label: df_label(&d.kind).into(),
                        value: d.size.clone(),
                        detail: (!detail.is_empty()).then_some(detail),
                        tone: None,
                    }
                })
                .collect(),
        });
    }
    let dangling = images.iter().filter(|i| i.dangling()).count();
    let mut cleanup = Vec::new();
    if dangling > 0 {
        cleanup.push(button(
            &format!("清理悬空镜像（{dangling}）"),
            "brush-cleaning",
            None,
            PluginAction::Invoke {
                id: "images.prune".into(),
                value: None,
                confirm: Some("删掉所有悬空镜像（重新 build 之后被顶掉标签、也没有容器在用的老版本）？".into()),
            },
        ));
    }
    let cache = df.iter().find(|d| d.kind == "Build Cache").is_some_and(|d| !d.size.is_empty() && !d.size.starts_with("0B"));
    if cache {
        cleanup.push(button(
            "清理构建缓存",
            "eraser",
            None,
            PluginAction::Invoke {
                id: "builder.prune".into(),
                value: None,
                confirm: Some("清掉没在用的构建缓存？下次 docker build 会从头构建，慢一些。".into()),
            },
        ));
    }
    if !cleanup.is_empty() {
        blocks.push(PluginBlock::Actions { items: cleanup });
    }

    blocks.push(PluginBlock::Heading { text: "镜像".into(), detail: Some(format!("{} 个", images.len())) });
    if images.is_empty() {
        blocks.push(PluginBlock::Notice { text: "这台机器上还没有镜像。".into(), tone: Some(PluginTone::Muted) });
    } else {
        let items = images
            .iter()
            .map(|i| {
                let reference = i.reference();
                // 窄栏里长名字截掉的正好是最要紧的末尾：标题只放最后一段（名字:标签），仓库路径挪进小字的
                // 最后（私有仓库的地址很长，放前面会把大小挤掉）
                let (repo, name) = match reference.rsplit_once('/') {
                    Some((repo, name)) if !i.dangling() => (Some(repo.to_string()), name.to_string()),
                    _ => (None, reference.clone()),
                };
                let mut detail = vec![i.size.clone()];
                detail.extend(i.created.map(|t| format!("{}前", fmt_span(now - t))));
                detail.extend(i.containers.filter(|n| *n > 0).map(|n| format!("{n} 个容器在用")));
                detail.extend(repo);
                let in_use = i.containers.is_some_and(|n| n > 0);
                PluginListItem {
                    title: if i.dangling() { format!("<无标签> {}", short(&i.id)) } else { name },
                    detail: Some(detail.join(" · ")),
                    badge: None,
                    tone: Some(if in_use { PluginTone::Success } else { PluginTone::Muted }),
                    action: None,
                    menu: vec![
                        PluginButton { label: "复制名称".into(), action: PluginAction::Copy { text: reference.clone() }, icon: None, tone: None },
                        PluginButton {
                            label: "删除".into(),
                            action: invoke("image.remove", &reference, Some(format!("删除镜像「{reference}」？"))),
                            icon: None,
                            tone: Some(PluginTone::Danger),
                        },
                    ],
                }
            })
            .collect();
        blocks.push(PluginBlock::List { items });
    }

    PluginView { title: Some(h.title()), refresh: Some(REFRESH_IMAGES), blocks, ..Default::default() }
}

/// 跑不成的样子：机器选项留着（好换一台），下面一条说清楚怎么回事
pub fn failure_view(h: &Header, failure: &Failure, local: bool) -> PluginView {
    let who = &h.machine.label;
    let (tone, text, raw) = match failure {
        Failure::NotInstalled => (
            PluginTone::Warning,
            if local {
                format!("{who}上找不到 docker 命令（找过登录 shell 的 PATH 与 Docker Desktop、OrbStack、Homebrew 的安装位置）。装好之后这里会自己出来。")
            } else {
                format!("{who} 上找不到 docker 命令。装好之后这里会自己出来。")
            },
            None,
        ),
        Failure::DaemonDown(raw) => (
            PluginTone::Warning,
            if local {
                format!("{who}的 Docker 没在运行：打开 Docker Desktop / OrbStack，或启动守护进程。")
            } else {
                format!("{who} 上的 Docker 没在运行：在那台机器上 `sudo systemctl start docker`。")
            },
            Some(raw),
        ),
        Failure::Permission(raw) => (
            PluginTone::Warning,
            format!(
                "{who} 上的这个用户没有权限连 Docker：把它加进 docker 组（`sudo usermod -aG docker $USER`），重新登录之后生效——falcon 的 SSH 连接要断开重连一次。"
            ),
            Some(raw),
        ),
        Failure::Other(raw) => (PluginTone::Danger, "docker 命令失败了。".to_string(), Some(raw)),
    };
    problem(h, tone, text, raw.cloned())
}

/// 连不上远端（链路故障、超时）
pub fn link_view(h: &Header, error: &str) -> PluginView {
    problem(h, PluginTone::Danger, format!("连不上 {}：{error}", h.machine.label), None)
}

fn problem(h: &Header, tone: PluginTone, text: String, raw: Option<String>) -> PluginView {
    let mut blocks = vec![h.machine.select(), PluginBlock::Notice { text, tone: Some(tone) }];
    if let Some(raw) = raw.filter(|r| !r.trim().is_empty()) {
        blocks.push(PluginBlock::Text { text: raw.trim().to_string(), tone: Some(PluginTone::Muted), mono: true });
    }
    PluginView { title: Some(h.machine.label.clone()), refresh: Some(REFRESH_IDLE), blocks, ..Default::default() }
}

#[cfg(test)]
mod tests {
    use super::super::parse::{Mount, Port};
    use super::*;

    const NOW: i64 = 1_800_000_000;

    fn machine() -> MachineChoice {
        MachineChoice { value: "auto".into(), options: vec![PluginOption { value: "auto".into(), label: "跟随当前项目".into() }], label: "本机".into() }
    }

    fn container(name: &str, status: Status) -> Container {
        Container {
            id: format!("{:0<64}", name.bytes().map(|b| format!("{:x}", b % 16)).collect::<String>()),
            name: name.into(),
            image: "nginx:1.27".into(),
            status,
            started: Some(NOW - 7200),
            finished: Some(NOW - 3 * 86400),
            ..Default::default()
        }
    }

    fn daemon() -> Daemon {
        Daemon { version: "29.8.2".into(), os: "OrbStack".into(), cpus: 10, mem_total: 8 << 30 }
    }

    fn items(v: &PluginView) -> Vec<&PluginListItem> {
        v.blocks.iter().flat_map(|b| match b {
            PluginBlock::List { items } => items.iter().collect(),
            _ => vec![],
        })
        .collect()
    }

    #[test]
    fn list_groups_by_compose_project_and_sorts_running_first() {
        let mut web = container("web", Status::Running);
        web.project = Some("shop".into());
        let mut db = container("db", Status::Exited);
        db.project = Some("shop".into());
        db.exit_code = 137;
        let lone = container("lone", Status::Exited);
        let d = daemon();
        let h = Header { machine: &machine(), daemon: Some(&d) };
        let stats = HashMap::from([(web.id.clone(), Stats { cpu: Some(50.0), mem: Some(1e9), ..Default::default() })]);
        let v = containers_view(&h, &[db.clone(), lone.clone(), web.clone()], Some(&stats), NOW);
        assert_eq!(v.title.as_deref(), Some("本机 · OrbStack · Docker 29.8.2"));
        assert_eq!(v.refresh, Some(REFRESH));
        assert!(matches!(&v.blocks[1], PluginBlock::Select { param, .. } if param == TAB));
        let PluginBlock::Stats { items: cards } = &v.blocks[2] else { panic!() };
        let cards: Vec<_> = cards.iter().map(|s| (s.label.as_str(), s.value.as_str())).collect();
        assert_eq!(cards, vec![("在跑", "1"), ("已停止", "2"), ("CPU", "5.0%"), ("内存", "954 MB")]);
        let headings: Vec<_> = v
            .blocks
            .iter()
            .filter_map(|b| match b {
                PluginBlock::Heading { text, detail } => Some((text.as_str(), detail.as_deref())),
                _ => None,
            })
            .collect();
        assert_eq!(headings, vec![("shop", Some("1 / 2 在跑")), ("其他容器", Some("0 / 1 在跑"))]);
        let rows = items(&v);
        assert_eq!(rows.iter().map(|i| i.title.as_str()).collect::<Vec<_>>(), vec!["web", "db", "lone"]);
        assert_eq!(rows[0].detail.as_deref(), Some("运行 2 小时 · nginx:1.27"));
        assert_eq!((rows[0].badge.as_deref(), rows[0].tone), (Some("50%"), Some(PluginTone::Success)));
        assert_eq!(rows[1].detail.as_deref(), Some("已退出（137）· 3 天前 · nginx:1.27"));
        assert_eq!(rows[1].tone, Some(PluginTone::Danger));
        assert_eq!(rows[2].tone, Some(PluginTone::Muted), "正常退出是灰的");
        assert_eq!(rows[0].action, Some(PluginAction::SetParam { param: CONTAINER.into(), value: web.id.clone() }));
        let labels = |i: &PluginListItem| i.menu.iter().map(|b| b.label.clone()).collect::<Vec<_>>();
        assert_eq!(labels(rows[0]), vec!["停止", "重启", "暂停", "详情与日志", "复制 ID", "复制进入命令"]);
        assert_eq!(labels(rows[1]), vec!["启动", "删除", "详情与日志", "复制 ID"]);
        assert_eq!(rows[1].menu[1].tone, Some(PluginTone::Danger));
        assert!(matches!(&rows[1].menu[1].action, PluginAction::Invoke { confirm: Some(_), .. }), "删除要确认");
        // 有停止的容器才给清理
        let PluginBlock::Actions { items: last } = v.blocks.last().unwrap() else { panic!() };
        assert_eq!(last[0].label, "清理停止的容器（2）");
    }

    #[test]
    fn list_without_compose_and_without_stats() {
        let h = Header { machine: &machine(), daemon: None };
        let v = containers_view(&h, &[container("a", Status::Running)], None, NOW);
        assert_eq!(v.title.as_deref(), Some("本机"));
        assert_eq!(v.refresh, Some(REFRESH_SOON), "占用还在采，早点再画");
        assert!(!v.blocks.iter().any(|b| matches!(b, PluginBlock::Heading { .. })), "没有 compose 项目就不分组");
        let PluginBlock::Stats { items: cards } = &v.blocks[2] else { panic!() };
        assert_eq!((cards[2].value.as_str(), cards[2].detail.as_deref()), ("…", Some("采样中")));
        assert!(!matches!(v.blocks.last(), Some(PluginBlock::Actions { .. })));

        let empty = containers_view(&h, &[], None, NOW);
        assert_eq!(empty.refresh, Some(REFRESH));
        assert!(empty.blocks.iter().any(|b| matches!(b, PluginBlock::Notice { text, .. } if text.contains("还没有容器"))));
    }

    #[test]
    fn detail_of_a_running_container() {
        let mut c = container("web", Status::Running);
        c.health = Some("unhealthy".into());
        c.project = Some("shop".into());
        c.service = Some("web".into());
        c.ports = vec![
            Port { container: "80/tcp".into(), host: Some((String::new(), "8080".into())) },
            Port { container: "443/tcp".into(), host: None },
        ];
        c.mounts = vec![Mount { kind: "bind".into(), source: "/srv".into(), destination: "/data".into(), rw: false }];
        c.command = vec!["nginx".into(), "-g".into(), "daemon off;".into()];
        c.restart_policy = "unless-stopped".into();
        let h = Header { machine: &machine(), daemon: None };
        let stats = Stats { cpu: Some(1.5), mem: Some(2e6), mem_text: "1.9MiB / 7.8GiB".into(), mem_pct: Some(0.02), net: "1kB / 2kB".into(), pids: "3".into(), ..Default::default() };
        let v = detail_view(&h, &c, Some(&stats), &Ok("line 1\nline 2".into()), NOW);
        assert!(matches!(&v.blocks[0], PluginBlock::Actions { items } if items[0].action == PluginAction::SetParam { param: CONTAINER.into(), value: String::new() }));
        assert!(matches!(&v.blocks[1], PluginBlock::Heading { text, detail: Some(d) } if text == "web" && d == "运行 2 小时 · 不健康"));
        let PluginBlock::Actions { items: ops } = &v.blocks[2] else { panic!() };
        assert_eq!(ops.iter().map(|b| b.label.as_str()).collect::<Vec<_>>(), vec!["停止", "重启", "暂停"]);
        let PluginBlock::Stats { items: cards } = &v.blocks[3] else { panic!() };
        assert_eq!(cards[1].detail.as_deref(), Some("上限 7.8GiB"));
        assert_eq!((cards[2].value.as_str(), cards[2].detail.as_deref()), ("↓ 1kB", Some("↑ 2kB")));
        let PluginBlock::Kv { items: kv } = &v.blocks[4] else { panic!() };
        let pairs: Vec<_> = kv.iter().map(|k| (k.key.as_str(), k.value.as_str())).collect();
        assert!(pairs.contains(&("健康检查", "不健康")));
        assert!(pairs.contains(&("Compose", "shop · web")));
        assert!(pairs.contains(&("端口", ":8080 → 80/tcp")));
        assert!(pairs.contains(&("", "443/tcp（没发布）")), "同一个键的第二行不再写键");
        assert!(pairs.contains(&("挂载", "/srv → /data（只读）")));
        assert!(pairs.contains(&("命令", "nginx -g daemon off;")));
        assert!(pairs.contains(&("重启策略", "除非手动停止")));
        let PluginBlock::Actions { items: copies } = &v.blocks[6] else { panic!() };
        assert_eq!(copies.iter().map(|b| b.label.as_str()).collect::<Vec<_>>(), vec!["复制日志", "复制 ID", "复制进入命令"]);
        assert!(matches!(v.blocks.last(), Some(PluginBlock::Text { text, mono: true, .. }) if text == "line 1\nline 2"));
    }

    #[test]
    fn detail_of_a_stopped_container() {
        let mut c = container("job", Status::Exited);
        c.oom_killed = true;
        c.exit_code = 137;
        let h = Header { machine: &machine(), daemon: None };
        let v = detail_view(&h, &c, None, &Err("configured logging driver does not support reading".into()), NOW);
        assert_eq!(v.refresh, Some(REFRESH));
        assert!(v.blocks.iter().any(|b| matches!(b, PluginBlock::Notice { text, .. } if text.contains("OOMKilled"))));
        let ops = v.blocks.iter().find_map(|b| match b {
            PluginBlock::Actions { items } if items.iter().any(|i| i.label == "启动") => Some(items),
            _ => None,
        });
        let ops = ops.unwrap();
        assert_eq!(ops[1].tone, Some(PluginTone::Danger));
        assert!(!v.blocks.iter().any(|b| matches!(b, PluginBlock::Stats { .. })), "停着的容器没有占用");
        assert!(matches!(v.blocks.last(), Some(PluginBlock::Notice { text, .. }) if text.starts_with("读不了日志")));
    }

    #[test]
    fn images_tab() {
        let imgs = vec![
            Image { id: "5aabe105d210".into(), repository: "nginx".into(), tag: "1.27".into(), size: "190MB".into(), created: Some(NOW - 86400), containers: Some(2) },
            Image { id: "1111aaaa2222".into(), repository: "registry.example.com/team/api".into(), tag: "v2".into(), size: "50MB".into(), created: None, containers: None },
            Image { id: "86a8aefa495f".into(), repository: "<none>".into(), tag: "<none>".into(), size: "1.2GB".into(), created: None, containers: Some(0) },
        ];
        let df = vec![
            DiskUse { kind: "Build Cache".into(), count: "112".into(), active: "0".into(), size: "11.14GB".into(), reclaimable: "9.536GB".into() },
            DiskUse { kind: "Images".into(), count: "2".into(), active: "1".into(), size: "1.4GB".into(), reclaimable: "1.2GB (85%)".into() },
            DiskUse { kind: "Local Volumes".into(), count: "0".into(), active: "0".into(), size: "0B".into(), reclaimable: "0B".into() },
        ];
        let h = Header { machine: &machine(), daemon: None };
        let v = images_view(&h, &imgs, &df, NOW);
        assert_eq!(v.refresh, Some(REFRESH_IMAGES));
        let PluginBlock::Select { value, .. } = &v.blocks[1] else { panic!() };
        assert_eq!(value.as_deref(), Some("images"));
        let PluginBlock::Stats { items: cards } = &v.blocks[2] else { panic!() };
        let cards: Vec<_> = cards.iter().map(|s| (s.label.as_str(), s.detail.as_deref())).collect();
        assert_eq!(cards, vec![("镜像", Some("可回收 1.2GB")), ("构建缓存", Some("可回收 9.536GB")), ("卷", Some("0 个"))]);
        let PluginBlock::Actions { items: cleanup } = &v.blocks[3] else { panic!() };
        assert_eq!(cleanup.iter().map(|b| b.label.as_str()).collect::<Vec<_>>(), vec!["清理悬空镜像（1）", "清理构建缓存"]);
        let rows = items(&v);
        assert_eq!(rows[0].title, "nginx:1.27");
        assert_eq!(rows[0].detail.as_deref(), Some("190MB · 1 天前 · 2 个容器在用"));
        assert_eq!(rows[0].tone, Some(PluginTone::Success));
        assert_eq!((rows[1].title.as_str(), rows[1].detail.as_deref()), ("api:v2", Some("50MB · registry.example.com/team")));
        assert_eq!(rows[1].menu[0].action, PluginAction::Copy { text: "registry.example.com/team/api:v2".into() }, "复制与删除用完整的引用");
        assert_eq!(rows[2].title, "<无标签> 86a8aefa495f");
        assert_eq!(rows[2].menu[1].action, invoke("image.remove", "86a8aefa495f", Some("删除镜像「86a8aefa495f」？".into())));
    }

    #[test]
    fn failures_keep_the_machine_picker() {
        let h = Header { machine: &machine(), daemon: None };
        let v = failure_view(&h, &Failure::DaemonDown("Cannot connect to the Docker daemon".into()), true);
        assert_eq!(v.refresh, Some(REFRESH_IDLE));
        assert!(matches!(v.blocks[0], PluginBlock::Select { .. }));
        assert!(matches!(&v.blocks[1], PluginBlock::Notice { text, tone: Some(PluginTone::Warning) } if text.contains("没在运行")));
        assert!(matches!(&v.blocks[2], PluginBlock::Text { text, .. } if text.starts_with("Cannot connect")));
        let v = failure_view(&h, &Failure::NotInstalled, false);
        assert_eq!(v.blocks.len(), 2);
        let v = link_view(&h, "超时");
        assert!(matches!(&v.blocks[1], PluginBlock::Notice { text, .. } if text == "连不上 本机：超时"));
    }
}
