//! 插件 stdout → 交给客户端的面板文档。纯函数，零 I/O。
//!
//! 两道：**逐块解析**（某一块写错只把那一块换成一条提示，别的照画——插件作者对着面板改 JSON，
//! 看得到是第几块错在哪）与**裁剪**（插件的输出不可信：块数、行数、点数、字符串长度都有上限，
//! 非有限的数字归零，`openUrl` 只放行 http / https，参数名过白名单——它会变成环境变量名）。
//! 内置插件产出的文档也过裁剪，客户端因此只需要信服务端。

use falcon_proto::{
    PluginAction, PluginBlock, PluginButton, PluginKv, PluginListItem, PluginOption, PluginSeries, PluginStat,
    PluginTone, PluginToast, PluginView,
};
use serde_json::Value;

use super::manifest::{clamp_refresh, valid_action, valid_param};

pub const MAX_BLOCKS: usize = 200;
/// stats / kv / list / actions / select 的条目数
pub const MAX_ITEMS: usize = 200;
pub const MAX_ROWS: usize = 500;
pub const MAX_COLUMNS: usize = 12;
pub const MAX_SERIES: usize = 4;
pub const MAX_POINTS: usize = 600;
/// 一块文字（等宽的命令输出）的上限，按字符
pub const MAX_TEXT: usize = 32 * 1024;
/// 标签、单元格、按钮字之类的短字符串
pub const MAX_LABEL: usize = 512;
/// `copy` 的文字、`openUrl` 的地址
pub const MAX_PAYLOAD: usize = 64 * 1024;
/// 列表行「⋯」菜单的项数
pub const MAX_MENU: usize = 32;
/// 文档改写面板参数（`params`）的个数与取值长度：与请求里能带的上限一致（mod.rs 的 check_params），
/// 改写出来的参数下一次请求要能原样带回来
pub const MAX_VIEW_PARAMS: usize = 16;
pub const MAX_PARAM_VALUE: usize = 1024;

/// 读 JSON 输出。顶层不是对象才算整份失败；`blocks` 逐块读
pub fn parse_view(stdout: &str) -> Result<PluginView, String> {
    let v: Value = serde_json::from_str(stdout.trim_start_matches('\u{feff}'))
        .map_err(|e| format!("插件输出的不是合法的 JSON：{e}"))?;
    let Value::Object(mut m) = v else { return Err("插件输出应该是一个 JSON 对象（{\"blocks\": [...]}）".into()) };
    let blocks = match m.remove("blocks") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items
            .into_iter()
            .enumerate()
            .map(|(i, b)| {
                serde_json::from_value::<PluginBlock>(b)
                    .unwrap_or_else(|e| notice(PluginTone::Danger, format!("第 {} 块解析失败：{e}", i + 1)))
            })
            .collect(),
        Some(_) => return Err("blocks 应该是数组".into()),
    };
    Ok(PluginView {
        title: m.get("title").and_then(Value::as_str).map(str::to_string),
        refresh: m.get("refresh").and_then(Value::as_f64),
        blocks,
        toast: m.remove("toast").and_then(|t| serde_json::from_value::<PluginToast>(t).ok()),
        params: match m.remove("params") {
            // 值收字符串，也收数字与布尔（同单元格的口径）；别的形状丢掉，不连累整份
            Some(Value::Object(p)) => p
                .into_iter()
                .filter_map(|(k, v)| {
                    let v = match v {
                        Value::String(s) => s,
                        Value::Number(n) => n.to_string(),
                        Value::Bool(b) => b.to_string(),
                        _ => return None,
                    };
                    Some((k, v))
                })
                .collect(),
            _ => Default::default(),
        },
    })
}

/// `output: "text"` 的插件：整段输出贴成一块等宽文字（去掉末尾的空行）
pub fn text_view(stdout: &str) -> PluginView {
    let text = stdout.trim_end();
    let blocks = if text.is_empty() {
        Vec::new()
    } else {
        vec![PluginBlock::Text { text: text.to_string(), tone: None, mono: true }]
    };
    PluginView { blocks, ..Default::default() }
}

fn notice(tone: PluginTone, text: String) -> PluginBlock {
    PluginBlock::Notice { text, tone: Some(tone) }
}

/// 按字符截断（不切坏 UTF-8），截了就补一个省略号
pub fn clip(s: String, max: usize) -> String {
    match s.char_indices().nth(max) {
        None => s,
        Some((at, _)) => {
            let mut out = s[..at].to_string();
            out.push('…');
            out
        }
    }
}

fn clip_opt(s: Option<String>, max: usize) -> Option<String> {
    s.map(|s| clip(s, max))
}

fn finite(v: f64) -> f64 {
    if v.is_finite() { v } else { 0.0 }
}

fn url_ok(url: &str) -> bool {
    let lower = url.trim_start().to_ascii_lowercase();
    (lower.starts_with("https://") || lower.starts_with("http://")) && url.len() <= MAX_PAYLOAD
}

fn action(a: PluginAction) -> PluginAction {
    match a {
        PluginAction::Invoke { id, value, confirm } if valid_action(&id) => PluginAction::Invoke {
            id,
            value: clip_opt(value, 1024),
            confirm: clip_opt(confirm, MAX_LABEL * 2),
        },
        PluginAction::OpenUrl { url } if url_ok(&url) => PluginAction::OpenUrl { url: url.trim().to_string() },
        PluginAction::Copy { text } => PluginAction::Copy { text: clip(text, MAX_PAYLOAD) },
        PluginAction::Refresh => PluginAction::Refresh,
        PluginAction::SetParam { param, value } if valid_param(&param) => {
            PluginAction::SetParam { param, value: clip(value, MAX_PARAM_VALUE) }
        }
        // id 不合法的 invoke、非 http 的地址、名字不合法的参数：按钮照画，点了不做事
        _ => PluginAction::Unknown,
    }
}

fn button(b: PluginButton) -> PluginButton {
    PluginButton { label: clip(b.label, MAX_LABEL), action: action(b.action), icon: clip_opt(b.icon, MAX_LABEL), tone: b.tone }
}

fn block(b: PluginBlock) -> PluginBlock {
    let label = |s: String| clip(s, MAX_LABEL);
    let opt = |s: Option<String>| clip_opt(s, MAX_LABEL);
    match b {
        PluginBlock::Heading { text, detail } => PluginBlock::Heading { text: label(text), detail: opt(detail) },
        PluginBlock::Text { text, tone, mono } => PluginBlock::Text { text: clip(text, MAX_TEXT), tone, mono },
        PluginBlock::Notice { text, tone } => PluginBlock::Notice { text: clip(text, MAX_TEXT), tone },
        PluginBlock::Stats { items } => PluginBlock::Stats {
            items: items
                .into_iter()
                .take(MAX_ITEMS)
                .map(|s| PluginStat { label: label(s.label), value: label(s.value), detail: opt(s.detail), tone: s.tone })
                .collect(),
        },
        PluginBlock::Meter { label: l, value, max, text, tone } => {
            // max 不是正数就退回 100（缺省口径），免得客户端除以零
            let max = max.map(finite).filter(|m| *m > 0.0);
            PluginBlock::Meter { label: label(l), value: finite(value), max, text: opt(text), tone }
        }
        PluginBlock::Chart { label: l, text, series, max, slots } => PluginBlock::Chart {
            label: opt(l),
            text: opt(text),
            series: series
                .into_iter()
                .take(MAX_SERIES)
                .map(|s| {
                    // 太长就留最新的那一段（从旧到新排，尾巴是最新的）
                    let skip = s.values.len().saturating_sub(MAX_POINTS);
                    PluginSeries {
                        name: opt(s.name),
                        values: s.values.into_iter().skip(skip).map(finite).collect(),
                        tone: s.tone,
                    }
                })
                .collect(),
            max: max.map(finite).filter(|m| *m > 0.0),
            slots: slots.map(|s| s.min(MAX_POINTS as u32)),
        },
        PluginBlock::Kv { items } => PluginBlock::Kv {
            items: items
                .into_iter()
                .take(MAX_ITEMS)
                .map(|k| PluginKv { key: label(k.key), value: clip(k.value, MAX_LABEL * 4), tone: k.tone, mono: k.mono })
                .collect(),
        },
        PluginBlock::Table { columns, rows } => {
            let columns: Vec<_> = columns.into_iter().take(MAX_COLUMNS).map(|mut c| {
                c.title = label(c.title);
                c
            }).collect();
            let width = columns.len();
            PluginBlock::Table {
                rows: rows
                    .into_iter()
                    .take(MAX_ROWS)
                    // 行比表头短就补空格子、长就截掉：客户端按表头画列，不必再判
                    .map(|row| {
                        let mut row: Vec<String> = row.into_iter().take(width).map(label).collect();
                        row.resize(width, String::new());
                        row
                    })
                    .collect(),
                columns,
            }
        }
        PluginBlock::List { items } => PluginBlock::List {
            items: items
                .into_iter()
                .take(MAX_ITEMS)
                .map(|i| PluginListItem {
                    title: label(i.title),
                    detail: opt(i.detail),
                    badge: opt(i.badge),
                    tone: i.tone,
                    action: i.action.map(action),
                    menu: i.menu.into_iter().take(MAX_MENU).map(button).collect(),
                })
                .collect(),
        },
        PluginBlock::Actions { items } => PluginBlock::Actions { items: items.into_iter().take(MAX_ITEMS).map(button).collect() },
        PluginBlock::Select { param, label: l, value, options } => {
            if !valid_param(&param) {
                return notice(PluginTone::Danger, format!("select 的 param 不合法：{}（要小写字母开头的 [a-z0-9_]）", clip(param, 64)));
            }
            PluginBlock::Select {
                param,
                label: opt(l),
                value: clip_opt(value, 256),
                options: options
                    .into_iter()
                    .take(MAX_ITEMS)
                    .map(|o| PluginOption { value: clip(o.value, 256), label: label(o.label) })
                    .collect(),
            }
        }
        b @ (PluginBlock::Divider | PluginBlock::Unknown) => b,
    }
}

/// 裁剪整份文档。`refresh` 夹到合法区间；≤ 0 的意思是"这次之后别自动重画"，保留成 0
pub fn sanitize(view: PluginView) -> PluginView {
    PluginView {
        title: clip_opt(view.title, MAX_LABEL),
        refresh: view.refresh.map(|r| clamp_refresh(r).unwrap_or(0.0)),
        blocks: view
            .blocks
            .into_iter()
            .filter(|b| *b != PluginBlock::Unknown)
            .take(MAX_BLOCKS)
            .map(block)
            .collect(),
        toast: view.toast.map(|t| PluginToast { tone: t.tone, title: clip(t.title, MAX_LABEL), body: clip_opt(t.body, MAX_TEXT) }),
        // 参数名会变成插件的环境变量名（FALCON_PARAM_*）：过白名单
        params: view
            .params
            .into_iter()
            .filter(|(k, _)| valid_param(k))
            .take(MAX_VIEW_PARAMS)
            .map(|(k, v)| (k, clip(v, MAX_PARAM_VALUE)))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_bad_block_does_not_sink_the_panel() {
        let v = parse_view(
            r#"{"title":"t","refresh":3,"blocks":[
                {"type":"text","text":"ok"},
                {"type":"meter","label":"x","value":"lots"},
                {"type":"stats"}
            ]}"#,
        )
        .unwrap();
        assert_eq!(v.title.as_deref(), Some("t"));
        assert_eq!(v.refresh, Some(3.0));
        assert_eq!(v.blocks[0], PluginBlock::Text { text: "ok".into(), tone: None, mono: false });
        let PluginBlock::Notice { text, tone } = &v.blocks[1] else { panic!("{:?}", v.blocks[1]) };
        assert!(text.starts_with("第 2 块解析失败"), "{text}");
        assert_eq!(*tone, Some(PluginTone::Danger));
        assert!(matches!(&v.blocks[2], PluginBlock::Notice { text, .. } if text.starts_with("第 3 块")));
    }

    #[test]
    fn whole_document_failures() {
        assert!(parse_view("not json").unwrap_err().contains("不是合法的 JSON"));
        assert!(parse_view("[1]").unwrap_err().contains("JSON 对象"));
        assert!(parse_view(r#"{"blocks":{}}"#).unwrap_err().contains("数组"));
        // 空对象就是空面板；BOM 不碍事；坏 toast 丢掉不报错
        assert_eq!(parse_view("{}").unwrap(), PluginView::default());
        assert_eq!(parse_view("\u{feff}{\"toast\":3}").unwrap(), PluginView::default());
    }

    #[test]
    fn text_output() {
        let v = text_view(" 10:00 up 3 days\n\n");
        assert_eq!(v.blocks, vec![PluginBlock::Text { text: " 10:00 up 3 days".into(), tone: None, mono: true }]);
        assert!(text_view("\n  \n").blocks.is_empty());
    }

    #[test]
    fn clip_respects_char_boundaries() {
        assert_eq!(clip("你好世界".into(), 2), "你好…");
        assert_eq!(clip("abc".into(), 3), "abc");
    }

    #[test]
    fn sanitize_limits_and_rewrites() {
        let raw = format!(
            r#"{{"refresh":-1,"blocks":[
                {{"type":"chart","series":[{{"values":[{}]}}],"max":0}},
                {{"type":"table","columns":[{{"title":"a"}},{{"title":"b"}}],"rows":[["1"],["1","2","3"]]}},
                {{"type":"meter","label":"m","value":5,"max":-2}},
                {{"type":"actions","items":[
                    {{"label":"a","action":{{"kind":"openUrl","url":"javascript:alert(1)"}}}},
                    {{"label":"b","action":{{"kind":"openUrl","url":" https://x.y/z"}}}},
                    {{"label":"c","action":{{"kind":"invoke","id":"has space"}}}}
                ]}},
                {{"type":"select","param":"Bad-Name","options":[]}},
                {{"type":"whatever"}}
            ]}}"#,
            (0..700).map(|i| i.to_string()).collect::<Vec<_>>().join(",")
        );
        let v = sanitize(parse_view(&raw).unwrap());
        assert_eq!(v.refresh, Some(0.0));
        assert_eq!(v.blocks.len(), 5, "认不出的块在服务端就丢掉");
        let PluginBlock::Chart { series, max, .. } = &v.blocks[0] else { panic!() };
        assert_eq!(series[0].values.len(), MAX_POINTS);
        assert_eq!(series[0].values[0], 100.0, "截掉的是最旧的那头");
        assert_eq!(*max, None);
        let PluginBlock::Table { rows, .. } = &v.blocks[1] else { panic!() };
        assert_eq!(rows, &vec![vec!["1".to_string(), String::new()], vec!["1".into(), "2".into()]]);
        assert!(matches!(&v.blocks[2], PluginBlock::Meter { max: None, .. }));
        let PluginBlock::Actions { items } = &v.blocks[3] else { panic!() };
        assert_eq!(items[0].action, PluginAction::Unknown);
        assert_eq!(items[1].action, PluginAction::OpenUrl { url: "https://x.y/z".into() });
        assert_eq!(items[2].action, PluginAction::Unknown);
        assert!(matches!(&v.blocks[4], PluginBlock::Notice { text, .. } if text.contains("param 不合法")));
    }

    #[test]
    fn menus_set_param_and_view_params() {
        let v = sanitize(
            parse_view(
                r#"{"params":{"container":"","Bad":"x","n":3,"obj":{}},"blocks":[
                    {"type":"list","items":[{"title":"web","action":{"kind":"setParam","param":"container","value":42},
                        "menu":[{"label":"删","tone":"danger","action":{"kind":"invoke","id":"rm","value":"abc"}},
                                {"label":"坏","action":{"kind":"setParam","param":"Nope","value":"x"}}]}]}
                ]}"#,
            )
            .unwrap(),
        );
        assert_eq!(
            v.params,
            std::collections::BTreeMap::from([("container".to_string(), String::new()), ("n".to_string(), "3".to_string())])
        );
        let PluginBlock::List { items } = &v.blocks[0] else { panic!() };
        assert_eq!(items[0].action, Some(PluginAction::SetParam { param: "container".into(), value: "42".into() }));
        assert_eq!(items[0].menu.len(), 2);
        assert_eq!(items[0].menu[0].tone, Some(PluginTone::Danger));
        assert_eq!(items[0].menu[1].action, PluginAction::Unknown, "参数名不合法的 setParam 点了不做事");
    }

    #[test]
    fn non_finite_numbers_never_reach_the_client() {
        let v = sanitize(PluginView {
            blocks: vec![PluginBlock::Meter { label: "m".into(), value: f64::NAN, max: Some(f64::INFINITY), text: None, tone: None }],
            ..Default::default()
        });
        assert_eq!(v.blocks[0], PluginBlock::Meter { label: "m".into(), value: 0.0, max: None, text: None, tone: None });
    }
}
