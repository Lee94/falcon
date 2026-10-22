//! Coding Agent Token 用量面板文档。纯函数：概览页（时间周期、每日走势、Agent 与模型分布、热门会话）与单段会话详情页。
//!
//! 交互通过面板参数驱动：
//! - [`SCOPE`]（`all` | `project`）：有当前本机项目时可选「全部项目 / 当前项目」；
//! - [`RANGE`]（`today` | `7d` | `30d` | `all`）：统计周期与走势跨度；
//! - [`AGENT`]（`all` | `claude` | `codex` | `grok` | `devin` | `opencode`）：按 Agent 筛选；
//! - [`SESSION`]：非空时进入该会话详情页，点击「返回」或对应会话已不存在时清回空串。

use std::collections::BTreeMap;

use falcon_proto::{
    PluginAction, PluginAlign, PluginBlock, PluginButton, PluginColumn, PluginKv, PluginListItem, PluginOption,
    PluginSeries, PluginStat, PluginTone, PluginView,
};

use super::parse::{AgentKind, SessionRecord, Usage, civil_from_days};
use crate::plugins::fmt::{fmt_pct, fmt_span};

pub const SCOPE: &str = "scope";
pub const RANGE: &str = "range";
pub const AGENT: &str = "agent";
pub const SESSION: &str = "session";

/// 面板自动刷新间隔（秒）。会话记录只有一轮对话结束时才落盘，15 秒一刷足够跟上
pub const REFRESH: f64 = 15.0;
/// 热门会话列表最多列多少条
pub const TOP_SESSIONS: usize = 30;
/// 模型排行最多列多少条
pub const TOP_MODELS: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeRange {
    Today,
    Days7,
    Days30,
    All,
}

impl TimeRange {
    pub fn from_params(params: &BTreeMap<String, String>) -> Self {
        match params.get(RANGE).map(String::as_str) {
            Some("today") => TimeRange::Today,
            Some("7d") => TimeRange::Days7,
            Some("all") => TimeRange::All,
            _ => TimeRange::Days30,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            TimeRange::Today => "today",
            TimeRange::Days7 => "7d",
            TimeRange::Days30 => "30d",
            TimeRange::All => "all",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TimeRange::Today => "今日",
            TimeRange::Days7 => "近 7 天",
            TimeRange::Days30 => "近 30 天",
            TimeRange::All => "全部时间",
        }
    }

    /// 起始 UTC 天数（闭区间 `>= start_day`）
    pub fn start_day(self, today: i64) -> Option<i64> {
        match self {
            TimeRange::Today => Some(today),
            TimeRange::Days7 => Some(today - 6),
            TimeRange::Days30 => Some(today - 29),
            TimeRange::All => None,
        }
    }

    /// 走势图画多少天
    pub fn chart_days(self) -> usize {
        match self {
            TimeRange::Today => 7,
            TimeRange::Days7 => 7,
            TimeRange::Days30 => 30,
            TimeRange::All => 30,
        }
    }
}

/// 1000 进制的 Token 数字写法：`850`、`12.4K`、`3.85M`、`1.20B`
pub fn fmt_tokens(n: u64) -> String {
    if n < 1_000 {
        return n.to_string();
    }
    const UNITS: [&str; 4] = ["K", "M", "B", "T"];
    let mut v = n as f64 / 1_000.0;
    let mut i = 0;
    while v >= 1_000.0 && i < UNITS.len() - 1 {
        v /= 1_000.0;
        i += 1;
    }
    if v >= 100.0 {
        format!("{v:.0}{}", UNITS[i])
    } else if v >= 10.0 {
        format!("{v:.1}{}", UNITS[i])
    } else {
        format!("{v:.2}{}", UNITS[i])
    }
}

/// 判断一段会话的 `cwd` 是否属于当前项目目录（本身或其子目录）
pub fn matches_project_dir(session_cwd: Option<&str>, project_dir: &str) -> bool {
    let Some(cwd) = session_cwd else { return false };
    let cwd = cwd.trim_end_matches(['/', '\\']);
    let proj = project_dir.trim_end_matches(['/', '\\']);
    if proj.is_empty() {
        return false;
    }
    if cwd == proj {
        return true;
    }
    cwd.strip_prefix(proj)
        .is_some_and(|rest| rest.starts_with('/') || rest.starts_with('\\'))
}

/// 一段会话在给定起始日之后的累计用量（`start_day == None` 时取全量）
fn usage_in_range(rec: &SessionRecord, start_day: Option<i64>) -> Usage {
    let Some(start) = start_day else {
        return rec.usage;
    };
    if rec.by_day.is_empty() {
        let day = rec.updated_at / 86400;
        return if day >= start { rec.usage } else { Usage::default() };
    }
    let mut u = Usage::default();
    for (&day, du) in &rec.by_day {
        if day >= start {
            u.add(du);
        }
    }
    u
}

fn col(title: &str, align: Option<PluginAlign>) -> PluginColumn {
    PluginColumn { title: title.into(), align }
}

pub fn view(
    records: &[SessionRecord],
    params: &BTreeMap<String, String>,
    project: Option<(&str, &str)>,
    now: i64,
) -> PluginView {
    let mut cleared_session = false;
    if let Some(key) = params.get(SESSION).filter(|s| !s.is_empty()) {
        if let Some(rec) = records.iter().find(|r| r.key == *key) {
            return session_detail_view(rec, now);
        }
        cleared_session = true;
    }

    let range = TimeRange::from_params(params);
    let agent_filter = params.get(AGENT).and_then(|s| AgentKind::from_param(s));
    let only_project = project.is_some() && params.get(SCOPE).map(String::as_str) == Some("project");

    let today = (now.max(0)) / 86400;
    let start_day = range.start_day(today);

    let mut blocks = Vec::new();
    if let Some((proj_name, _)) = project {
        blocks.push(PluginBlock::Select {
            param: SCOPE.into(),
            label: Some("范围".into()),
            value: Some(if only_project { "project" } else { "all" }.into()),
            options: vec![
                PluginOption { value: "all".into(), label: "全部本机项目".into() },
                PluginOption { value: "project".into(), label: format!("当前项目（{proj_name}）") },
            ],
        });
    }
    blocks.push(PluginBlock::Select {
        param: RANGE.into(),
        label: Some("时间".into()),
        value: Some(range.as_str().into()),
        options: vec![
            PluginOption { value: "today".into(), label: "今日".into() },
            PluginOption { value: "7d".into(), label: "近 7 天".into() },
            PluginOption { value: "30d".into(), label: "近 30 天".into() },
            PluginOption { value: "all".into(), label: "全部".into() },
        ],
    });
    blocks.push(PluginBlock::Select {
        param: AGENT.into(),
        label: Some("Agent".into()),
        value: Some(agent_filter.map(AgentKind::id).unwrap_or("all").into()),
        options: {
            let mut opts = vec![PluginOption { value: "all".into(), label: "全部 Agent".into() }];
            for kind in AgentKind::ALL {
                opts.push(PluginOption { value: kind.id().into(), label: kind.label().into() });
            }
            opts
        },
    });

    // 过滤出符合项目与 Agent 条件、且在所选周期内有用量的会话
    let mut active_sessions: Vec<(&SessionRecord, Usage)> = Vec::new();
    let mut total_usage = Usage::default();
    let mut total_turns: u64 = 0;
    let mut by_agent: BTreeMap<AgentKind, (usize, Usage)> = BTreeMap::new();
    let mut by_model: BTreeMap<String, (AgentKind, Usage)> = BTreeMap::new();
    let mut daily_input: BTreeMap<i64, u64> = BTreeMap::new();
    let mut daily_output: BTreeMap<i64, u64> = BTreeMap::new();

    for rec in records {
        if let Some(kind) = agent_filter
            && rec.agent != kind
        {
            continue;
        }
        if only_project
            && let Some((_, proj_dir)) = project
            && !matches_project_dir(rec.cwd.as_deref(), proj_dir)
        {
            continue;
        }

        // 统计每日走势（不受 start_day 截断，让 Today 模式下也能看到近 7 天对比）
        for (&day, du) in &rec.by_day {
            *daily_input.entry(day).or_default() += du.total_input();
            *daily_output.entry(day).or_default() += du.output;
        }

        let u = usage_in_range(rec, start_day);
        if u.is_empty() {
            continue;
        }
        total_usage.add(&u);
        total_turns = total_turns.saturating_add(rec.turns as u64);
        let agent_entry = by_agent.entry(rec.agent).or_default();
        agent_entry.0 += 1;
        agent_entry.1.add(&u);

        if let Some(start) = start_day
            && !rec.by_day_model.is_empty()
        {
            for ((day, model), mu) in &rec.by_day_model {
                if *day >= start {
                    let e = by_model.entry(model.clone()).or_insert((rec.agent, Usage::default()));
                    e.1.add(mu);
                }
            }
        } else {
            for (model, mu) in &rec.models {
                let e = by_model.entry(model.clone()).or_insert((rec.agent, Usage::default()));
                e.1.add(mu);
            }
        }

        active_sessions.push((rec, u));
    }

    let title_scope = if only_project {
        project.map(|(n, _)| n).unwrap_or("当前项目")
    } else {
        "本机全部项目"
    };
    let title = format!("{title_scope} · {} · {} 段会话", range.label(), active_sessions.len());

    if active_sessions.is_empty() {
        blocks.push(PluginBlock::Notice {
            text: format!("{}内没有找到符合条件的 Coding Agent Token 记录。", range.label()),
            tone: Some(PluginTone::Muted),
        });
        let mut out = PluginView { title: Some(title), refresh: Some(REFRESH), blocks, ..Default::default() };
        if cleared_session {
            out.params = BTreeMap::from([(SESSION.into(), String::new())]);
        }
        return out;
    }

    let total_in = total_usage.total_input();
    let cache_hit_pct = (total_in > 0).then(|| total_usage.cache_read as f64 * 100.0 / total_in as f64);

    blocks.push(PluginBlock::Stats {
        items: vec![
            PluginStat {
                label: "总 Token".into(),
                value: fmt_tokens(total_usage.total()),
                detail: Some(format!("{} 段会话 · {} 轮", active_sessions.len(), total_turns)),
                tone: Some(PluginTone::Accent),
            },
            PluginStat {
                label: "输入（含缓存）".into(),
                value: fmt_tokens(total_in),
                detail: Some(format!("新输入 {}", fmt_tokens(total_usage.input))),
                tone: None,
            },
            PluginStat {
                label: "输出".into(),
                value: fmt_tokens(total_usage.output),
                detail: (total_usage.reasoning > 0).then(|| format!("含思考 {}", fmt_tokens(total_usage.reasoning))),
                tone: None,
            },
            PluginStat {
                label: "缓存命中".into(),
                value: cache_hit_pct.map(fmt_pct).unwrap_or_else(|| "—".into()),
                detail: Some(format!(
                    "读 {} · 写 {}",
                    fmt_tokens(total_usage.cache_read),
                    fmt_tokens(total_usage.cache_write)
                )),
                tone: cache_hit_pct.filter(|&p| p >= 50.0).map(|_| PluginTone::Success),
            },
        ],
    });

    // 每日走势图（输入 + 输出两条序列）
    let n_days = range.chart_days();
    let first_day = today - (n_days as i64 - 1);
    let mut in_points = Vec::with_capacity(n_days);
    let mut out_points = Vec::with_capacity(n_days);
    let mut peak_day: Option<(i64, u64)> = None;
    for d in first_day..=today {
        let i_val = daily_input.get(&d).copied().unwrap_or(0);
        let o_val = daily_output.get(&d).copied().unwrap_or(0);
        let sum = i_val.saturating_add(o_val);
        if sum > 0 && peak_day.is_none_or(|(_, p)| sum > p) {
            peak_day = Some((d, sum));
        }
        in_points.push(i_val as f64);
        out_points.push(o_val as f64);
    }
    let chart_text = match peak_day {
        Some((d, peak)) => format!("过去 {n_days} 天 · 峰值 {}（{}）", fmt_tokens(peak), fmt_day_short(d)),
        None => format!("过去 {n_days} 天"),
    };
    blocks.push(PluginBlock::Chart {
        label: Some("每日 Token 走势".into()),
        text: Some(chart_text),
        max: None,
        slots: Some(n_days as u32),
        series: vec![
            PluginSeries {
                name: Some(format!("输入 {}", fmt_tokens(total_in))),
                values: in_points,
                tone: Some(PluginTone::Accent),
            },
            PluginSeries {
                name: Some(format!("输出 {}", fmt_tokens(total_usage.output))),
                values: out_points,
                tone: Some(PluginTone::Success),
            },
        ],
    });

    // 按 Agent 汇总表
    let mut agent_rows: Vec<(AgentKind, usize, Usage)> = by_agent.into_iter().map(|(k, (c, u))| (k, c, u)).collect();
    agent_rows.sort_by(|a, b| b.2.total().cmp(&a.2.total()));
    let grand_total = total_usage.total().max(1);
    blocks.push(PluginBlock::Heading {
        text: "按 Agent 分布".into(),
        detail: Some(format!("{} 个活跃 Agent", agent_rows.len())),
    });
    blocks.push(PluginBlock::Table {
        columns: vec![
            col("Agent", None),
            col("会话", Some(PluginAlign::Right)),
            col("输入", Some(PluginAlign::Right)),
            col("输出", Some(PluginAlign::Right)),
            col("总计", Some(PluginAlign::Right)),
        ],
        rows: agent_rows
            .iter()
            .map(|(kind, count, u)| {
                let share = u.total() as f64 * 100.0 / grand_total as f64;
                vec![
                    kind.label().into(),
                    count.to_string(),
                    fmt_tokens(u.total_input()),
                    fmt_tokens(u.output),
                    format!("{} ({})", fmt_tokens(u.total()), fmt_pct(share)),
                ]
            })
            .collect(),
    });

    // 按模型汇总表
    if !by_model.is_empty() {
        let mut model_rows: Vec<(String, AgentKind, Usage)> =
            by_model.into_iter().map(|(m, (a, u))| (m, a, u)).collect();
        model_rows.sort_by(|a, b| b.2.total().cmp(&a.2.total()).then_with(|| a.0.cmp(&b.0)));
        let total_models = model_rows.len();
        blocks.push(PluginBlock::Heading {
            text: "按模型明细".into(),
            detail: Some(if total_models > TOP_MODELS {
                format!("前 {TOP_MODELS} / 共 {total_models} 个模型")
            } else {
                format!("共 {total_models} 个模型")
            }),
        });
        blocks.push(PluginBlock::Table {
            columns: vec![
                col("模型", None),
                col("输入", Some(PluginAlign::Right)),
                col("缓存读", Some(PluginAlign::Right)),
                col("输出", Some(PluginAlign::Right)),
                col("总计", Some(PluginAlign::Right)),
            ],
            rows: model_rows
                .into_iter()
                .take(TOP_MODELS)
                .map(|(model, _, u)| {
                    vec![
                        model,
                        fmt_tokens(u.input.saturating_add(u.cache_write)),
                        fmt_tokens(u.cache_read),
                        fmt_tokens(u.output),
                        fmt_tokens(u.total()),
                    ]
                })
                .collect(),
        });
    }

    // 热门会话列表（按所选时间范围内的总消耗降序排列，点击可看详情）
    active_sessions.sort_by(|a, b| {
        b.1.total()
            .cmp(&a.1.total())
            .then_with(|| b.0.updated_at.cmp(&a.0.updated_at))
    });
    let total_sess = active_sessions.len();
    blocks.push(PluginBlock::Heading {
        text: "消耗排行（会话）".into(),
        detail: Some(if total_sess > TOP_SESSIONS {
            format!("前 {TOP_SESSIONS} / 共 {total_sess} 段 · 点击查看明细")
        } else {
            "点击查看单段会话明细".into()
        }),
    });
    blocks.push(PluginBlock::List {
        items: active_sessions
            .into_iter()
            .take(TOP_SESSIONS)
            .map(|(rec, u)| session_list_item(rec, &u, now))
            .collect(),
    });

    let mut out = PluginView { title: Some(title), refresh: Some(REFRESH), blocks, ..Default::default() };
    if cleared_session {
        out.params = BTreeMap::from([(SESSION.into(), String::new())]);
    }
    out
}

fn session_list_item(rec: &SessionRecord, u: &Usage, now: i64) -> PluginListItem {
    let mut sub_parts = vec![rec.agent.label().to_string()];
    if let Some(cwd) = &rec.cwd
        && let Some(short) = short_path(cwd)
    {
        sub_parts.push(short);
    }
    if rec.updated_at > 0 && now >= rec.updated_at {
        sub_parts.push(format!("{}前", fmt_span(now - rec.updated_at)));
    }
    let badge_tone = if u.total() >= 10_000_000 {
        Some(PluginTone::Warning)
    } else if u.total() >= 1_000_000 {
        Some(PluginTone::Accent)
    } else {
        Some(PluginTone::Muted)
    };
    let mut menu = vec![PluginButton {
        label: "复制会话 ID".into(),
        action: PluginAction::Copy { text: rec.id.clone() },
        icon: Some("copy".into()),
        tone: None,
    }];
    if let Some(cwd) = &rec.cwd {
        menu.push(PluginButton {
            label: "复制工作目录".into(),
            action: PluginAction::Copy { text: cwd.clone() },
            icon: Some("folder".into()),
            tone: None,
        });
    }
    PluginListItem {
        title: rec.title.clone(),
        detail: Some(sub_parts.join(" · ")),
        badge: Some(fmt_tokens(u.total())),
        tone: badge_tone,
        action: Some(PluginAction::SetParam {
            param: SESSION.into(),
            value: rec.key.clone(),
        }),
        menu,
    }
}

fn session_detail_view(rec: &SessionRecord, now: i64) -> PluginView {
    let mut blocks = Vec::new();
    let mut items = vec![
        PluginButton {
            label: "返回列表".into(),
            action: PluginAction::SetParam { param: SESSION.into(), value: String::new() },
            icon: Some("arrow-left".into()),
            tone: None,
        },
        PluginButton {
            label: "复制会话 ID".into(),
            action: PluginAction::Copy { text: rec.id.clone() },
            icon: Some("copy".into()),
            tone: None,
        },
    ];
    if let Some(cwd) = &rec.cwd {
        items.push(PluginButton {
            label: "复制目录".into(),
            action: PluginAction::Copy { text: cwd.clone() },
            icon: Some("folder".into()),
            tone: None,
        });
    }
    blocks.push(PluginBlock::Actions { items });

    blocks.push(PluginBlock::Heading {
        text: rec.title.clone(),
        detail: Some(format!("{} · {}", rec.agent.label(), rec.id)),
    });

    let u = &rec.usage;
    let total_in = u.total_input();
    let cache_pct = (total_in > 0).then(|| u.cache_read as f64 * 100.0 / total_in as f64);

    blocks.push(PluginBlock::Stats {
        items: vec![
            PluginStat {
                label: "总 Token".into(),
                value: fmt_tokens(u.total()),
                detail: Some(format!("{} 轮调用", rec.turns)),
                tone: Some(PluginTone::Accent),
            },
            PluginStat {
                label: "输入（含缓存）".into(),
                value: fmt_tokens(total_in),
                detail: Some(format!("新输入 {}", fmt_tokens(u.input))),
                tone: None,
            },
            PluginStat {
                label: "输出".into(),
                value: fmt_tokens(u.output),
                detail: (u.reasoning > 0).then(|| format!("含思考 {}", fmt_tokens(u.reasoning))),
                tone: None,
            },
            PluginStat {
                label: "缓存命中".into(),
                value: cache_pct.map(fmt_pct).unwrap_or_else(|| "—".into()),
                detail: Some(format!("读 {} · 写 {}", fmt_tokens(u.cache_read), fmt_tokens(u.cache_write))),
                tone: cache_pct.filter(|&p| p >= 50.0).map(|_| PluginTone::Success),
            },
        ],
    });

    if let Some(pct) = cache_pct {
        blocks.push(PluginBlock::Meter {
            label: "输入缓存命中率".into(),
            value: pct,
            max: Some(100.0),
            text: Some(format!("{} / {} ({})", fmt_tokens(u.cache_read), fmt_tokens(total_in), fmt_pct(pct))),
            tone: Some(if pct >= 50.0 { PluginTone::Success } else { PluginTone::Accent }),
        });
    }

    let mut kv_items = vec![
        PluginKv { key: "Agent".into(), value: rec.agent.label().into(), mono: false, tone: None },
        PluginKv { key: "会话 ID".into(), value: rec.id.clone(), mono: true, tone: None },
    ];
    if let Some(cwd) = &rec.cwd {
        kv_items.push(PluginKv { key: "工作目录".into(), value: cwd.clone(), mono: true, tone: None });
    }
    kv_items.push(PluginKv {
        key: "轮次".into(),
        value: format!("{} 轮", rec.turns),
        mono: false,
        tone: None,
    });
    if rec.created_at > 0 {
        kv_items.push(PluginKv {
            key: "创建时间".into(),
            value: fmt_timestamp(rec.created_at),
            mono: true,
            tone: None,
        });
    }
    if rec.updated_at > 0 {
        let ago = if now >= rec.updated_at {
            format!("（{}前）", fmt_span(now - rec.updated_at))
        } else {
            String::new()
        };
        kv_items.push(PluginKv {
            key: "最近活跃".into(),
            value: format!("{}{ago}", fmt_timestamp(rec.updated_at)),
            mono: true,
            tone: None,
        });
    }
    if let Some(cost) = rec.cost_usd {
        kv_items.push(PluginKv {
            key: "记录费用".into(),
            value: format!("${cost:.4}"),
            mono: true,
            tone: None,
        });
    }
    blocks.push(PluginBlock::Kv { items: kv_items });

    if !rec.models.is_empty() {
        let mut model_rows: Vec<(&String, &Usage)> = rec.models.iter().collect();
        model_rows.sort_by(|a, b| b.1.total().cmp(&a.1.total()));
        blocks.push(PluginBlock::Heading { text: "模型用量明细".into(), detail: None });
        blocks.push(PluginBlock::Table {
            columns: vec![
                col("模型", None),
                col("输入", Some(PluginAlign::Right)),
                col("缓存读", Some(PluginAlign::Right)),
                col("输出", Some(PluginAlign::Right)),
                col("总计", Some(PluginAlign::Right)),
            ],
            rows: model_rows
                .into_iter()
                .map(|(m, mu)| {
                    vec![
                        m.clone(),
                        fmt_tokens(mu.input.saturating_add(mu.cache_write)),
                        fmt_tokens(mu.cache_read),
                        fmt_tokens(mu.output),
                        fmt_tokens(mu.total()),
                    ]
                })
                .collect(),
        });
    }

    if rec.by_day.len() > 1 {
        let mut day_rows: Vec<(i64, &Usage)> = rec.by_day.iter().map(|(&d, u)| (d, u)).collect();
        day_rows.sort_by(|a, b| b.0.cmp(&a.0));
        blocks.push(PluginBlock::Heading { text: "按日消耗".into(), detail: None });
        blocks.push(PluginBlock::Table {
            columns: vec![
                col("日期 (UTC)", None),
                col("输入", Some(PluginAlign::Right)),
                col("输出", Some(PluginAlign::Right)),
                col("总计", Some(PluginAlign::Right)),
            ],
            rows: day_rows
                .into_iter()
                .map(|(d, du)| {
                    vec![
                        fmt_day_iso(d),
                        fmt_tokens(du.total_input()),
                        fmt_tokens(du.output),
                        fmt_tokens(du.total()),
                    ]
                })
                .collect(),
        });
    }

    PluginView {
        title: Some(format!("{} · {}", rec.agent.label(), fmt_tokens(u.total()))),
        refresh: Some(REFRESH),
        blocks,
        ..Default::default()
    }
}

fn short_path(p: &str) -> Option<String> {
    let trimmed = p.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return None;
    }
    let parts: Vec<&str> = trimmed.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
    match parts.len() {
        0 => None,
        1 => Some(parts[0].to_string()),
        n => Some(format!("{}/{}", parts[n - 2], parts[n - 1])),
    }
}

fn fmt_day_short(day: i64) -> String {
    let (_, m, d) = civil_from_days(day);
    format!("{m:02}-{d:02}")
}

fn fmt_day_iso(day: i64) -> String {
    let (y, m, d) = civil_from_days(day);
    format!("{y:04}-{m:02}-{d:02}")
}

fn fmt_timestamp(ts: i64) -> String {
    let day = ts.div_euclid(86400);
    let rem = ts.rem_euclid(86400);
    let (y, m, d) = civil_from_days(day);
    let h = rem / 3600;
    let min = (rem % 3600) / 60;
    format!("{y:04}-{m:02}-{d:02} {h:02}:{min:02} UTC")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_record(agent: AgentKind, id: &str, cwd: &str, day: i64, model: &str, u: Usage) -> SessionRecord {
        let ts = day * 86400 + 3600;
        let mut r = SessionRecord {
            key: format!("{}:{id}", agent.id()),
            id: id.into(),
            agent,
            title: format!("会话 {id}"),
            cwd: Some(cwd.into()),
            created_at: ts,
            updated_at: ts,
            turns: 3,
            usage: u,
            models: BTreeMap::from([(model.to_string(), u)]),
            by_day: BTreeMap::from([(day, u)]),
            by_day_model: BTreeMap::from([((day, model.to_string()), u)]),
            cost_usd: Some(0.12),
        };
        if r.title.is_empty() {
            r.title = id.into();
        }
        r
    }

    #[test]
    fn token_formatting() {
        assert_eq!(fmt_tokens(0), "0");
        assert_eq!(fmt_tokens(999), "999");
        assert_eq!(fmt_tokens(1_250), "1.25K");
        assert_eq!(fmt_tokens(15_400), "15.4K");
        assert_eq!(fmt_tokens(250_000), "250K");
        assert_eq!(fmt_tokens(3_450_000), "3.45M");
        assert_eq!(fmt_tokens(1_200_000_000), "1.20B");
    }

    #[test]
    fn project_dir_matching() {
        assert!(matches_project_dir(Some("/Volumes/faye/Code/mojito"), "/Volumes/faye/Code/mojito"));
        assert!(matches_project_dir(Some("/Volumes/faye/Code/mojito/native"), "/Volumes/faye/Code/mojito/"));
        assert!(!matches_project_dir(Some("/Volumes/faye/Code/mojito-other"), "/Volumes/faye/Code/mojito"));
        assert!(!matches_project_dir(None, "/Volumes/faye/Code/mojito"));
    }

    #[test]
    fn overview_and_detail_views() {
        let day = 20_740; // ~2026-10-14
        let now = day * 86400 + 7200;
        let r1 = sample_record(
            AgentKind::Claude,
            "s1",
            "/work/mojito/native",
            day,
            "claude-opus-5-5",
            Usage { input: 2_000, cache_read: 8_000, cache_write: 1_000, output: 1_500, reasoning: 500 },
        );
        let r2 = sample_record(
            AgentKind::Codex,
            "s2",
            "/work/other",
            day - 1,
            "gpt-5.4",
            Usage { input: 3_000, cache_read: 0, cache_write: 0, output: 1_000, reasoning: 0 },
        );
        let records = vec![r1, r2];

        // 全部项目概览
        let v = view(&records, &BTreeMap::new(), Some(("mojito", "/work/mojito")), now);
        assert!(v.title.as_deref().unwrap().contains("2 段会话"));
        assert!(v.blocks.iter().any(|b| matches!(b, PluginBlock::Stats { .. })));
        assert!(v.blocks.iter().any(|b| matches!(b, PluginBlock::Chart { .. })));

        // 仅当前项目筛选
        let params = BTreeMap::from([(SCOPE.to_string(), "project".to_string())]);
        let v_proj = view(&records, &params, Some(("mojito", "/work/mojito")), now);
        assert!(v_proj.title.as_deref().unwrap().contains("1 段会话"));

        // 进入会话详情
        let detail_params = BTreeMap::from([(SESSION.to_string(), "claude:s1".to_string())]);
        let v_detail = view(&records, &detail_params, None, now);
        assert!(v_detail.title.as_deref().unwrap().contains("Claude Code"));
        assert!(v_detail.blocks.iter().any(|b| matches!(b, PluginBlock::Kv { .. })));

        // 不存在的会话自动清掉 session 参数回退列表
        let stale_params = BTreeMap::from([(SESSION.to_string(), "claude:missing".to_string())]);
        let v_stale = view(&records, &stale_params, None, now);
        assert_eq!(v_stale.params.get(SESSION).map(String::as_str), Some(""));
    }
}
