//! 右侧栏插件面板（ADR 0025）里与 GPUI 无关的那部分：什么时候重画、面板参数怎么跟着文档走、
//! 进度条与走势图的几何。画在 falcon-ui 的 `panels/plugin.rs`。

use std::collections::BTreeMap;
use std::time::Duration;

use falcon_proto::{PluginBlock, PluginInfo, PluginSeries, PluginView};

/// 自动重画的下限（服务端也夹过一次；这里再挡一道，免得老服务端给个 0.1 把面板变成忙等）
pub const MIN_REFRESH: Duration = Duration::from_secs(1);

/// 这一次画完之后多久再画。文档里的 `refresh` 盖过清单：> 0 就按它，≤ 0 是"别自动重画"；
/// 文档没说就按清单；都没有 = 只在打开 / 点刷新 / 点按钮时画
pub fn next_refresh(info: Option<&PluginInfo>, view: Option<&PluginView>) -> Option<Duration> {
    let secs = match view.and_then(|v| v.refresh) {
        Some(r) => r,
        None => info.and_then(|i| i.refresh)?,
    };
    (secs.is_finite() && secs > 0.0).then(|| Duration::from_secs_f64(secs).max(MIN_REFRESH))
}

/// 文档里 select 块的取值与 `params` 写回面板参数：插件说了算（它可能把一个已经不存在的选项改回了
/// 缺省，或者删掉了详情里的那一项、把参数清空回到列表）。两处都说了的以 `params` 为准
pub fn sync_params(view: &PluginView, params: &mut BTreeMap<String, String>) {
    for b in &view.blocks {
        if let PluginBlock::Select { param, value: Some(v), .. } = b {
            params.insert(param.clone(), v.clone());
        }
    }
    for (k, v) in &view.params {
        params.insert(k.clone(), v.clone());
    }
}

/// 进度条的填充比例 0–1。max 缺省 100
pub fn meter_ratio(value: f64, max: Option<f64>) -> f32 {
    let max = max.filter(|m| m.is_finite() && *m > 0.0).unwrap_or(100.0);
    if !value.is_finite() {
        return 0.0;
    }
    (value / max).clamp(0.0, 1.0) as f32
}

/// 进度条右边没给 `text` 时显示的百分比
pub fn meter_text(value: f64, max: Option<f64>) -> String {
    let p = meter_ratio(value, max) as f64 * 100.0;
    if p >= 10.0 || p == 0.0 { format!("{p:.0}%") } else { format!("{p:.1}%") }
}

/// 走势图的纵轴上限：给了 `max` 就用它；没给就取各条的最大值往上留一成（顶格的线贴着边不好看），
/// 全是 0 时给 1，免得除以零
pub fn chart_max(series: &[PluginSeries], max: Option<f64>) -> f64 {
    if let Some(m) = max.filter(|m| m.is_finite() && *m > 0.0) {
        return m;
    }
    let top = series.iter().flat_map(|s| s.values.iter().copied()).filter(|v| v.is_finite()).fold(0.0, f64::max);
    if top > 0.0 { top * 1.1 } else { 1.0 }
}

/// 一条线在 `w × h` 的框里的点（左上原点，y 向下）。点数固定按 `slots` 摆：数据还没攒满时线从
/// 右边长出来，不会刚打开就把两个点拉满整个宽度——走势图里"最右边是现在"比铺满更重要
pub fn chart_points(values: &[f64], max: f64, slots: usize, w: f32, h: f32) -> Vec<(f32, f32)> {
    let slots = slots.max(values.len()).max(2);
    let step = w / (slots - 1) as f32;
    let start = slots - values.len();
    let max = if max > 0.0 { max } else { 1.0 };
    values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let v = if v.is_finite() { (*v / max).clamp(0.0, 1.0) } else { 0.0 };
            ((start + i) as f32 * step, h - v as f32 * h)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use falcon_proto::{PluginKind, PluginOption, PluginScope};

    use super::*;

    fn info(refresh: Option<f64>) -> PluginInfo {
        PluginInfo {
            id: "x".into(),
            name: "x".into(),
            description: None,
            icon: "puzzle".into(),
            kind: PluginKind::Command,
            scope: PluginScope::Global,
            refresh,
            enabled: true,
            error: None,
            dir: None,
        }
    }

    fn view(refresh: Option<f64>) -> PluginView {
        PluginView { refresh, ..Default::default() }
    }

    #[test]
    fn refresh_rules() {
        assert_eq!(next_refresh(None, None), None);
        assert_eq!(next_refresh(Some(&info(Some(5.0))), None), Some(Duration::from_secs(5)));
        assert_eq!(next_refresh(Some(&info(Some(5.0))), Some(&view(None))), Some(Duration::from_secs(5)));
        assert_eq!(next_refresh(Some(&info(Some(5.0))), Some(&view(Some(2.0)))), Some(Duration::from_secs(2)));
        assert_eq!(next_refresh(Some(&info(Some(5.0))), Some(&view(Some(0.0)))), None, "文档说别重画");
        assert_eq!(next_refresh(Some(&info(None)), Some(&view(Some(0.2)))), Some(MIN_REFRESH));
        assert_eq!(next_refresh(None, Some(&view(Some(f64::NAN)))), None);
    }

    #[test]
    fn params_follow_the_document() {
        let v = PluginView {
            blocks: vec![
                PluginBlock::Select {
                    param: "machine".into(),
                    label: None,
                    value: Some("auto".into()),
                    options: vec![PluginOption { value: "auto".into(), label: "a".into() }],
                },
                PluginBlock::Select { param: "unset".into(), label: None, value: None, options: vec![] },
            ],
            ..Default::default()
        };
        let mut params = BTreeMap::from([("machine".to_string(), "host:gone".to_string()), ("other".to_string(), "1".to_string())]);
        sync_params(&v, &mut params);
        assert_eq!(params.get("machine").map(String::as_str), Some("auto"));
        assert_eq!(params.get("other").map(String::as_str), Some("1"), "文档没提的参数留着");
        assert!(!params.contains_key("unset"));

        // params 改写没有 select 露着的参数，也盖过 select
        let v = PluginView {
            params: BTreeMap::from([("container".to_string(), String::new()), ("machine".to_string(), "local".to_string())]),
            ..v
        };
        let mut params = BTreeMap::from([("container".to_string(), "abc".to_string())]);
        sync_params(&v, &mut params);
        assert_eq!(params.get("container").map(String::as_str), Some(""));
        assert_eq!(params.get("machine").map(String::as_str), Some("local"));
    }

    #[test]
    fn meters() {
        assert_eq!(meter_ratio(50.0, None), 0.5);
        assert_eq!(meter_ratio(3.0, Some(4.0)), 0.75);
        assert_eq!(meter_ratio(9.0, Some(4.0)), 1.0);
        assert_eq!(meter_ratio(-1.0, None), 0.0);
        assert_eq!(meter_ratio(1.0, Some(0.0)), 0.01, "max 不合法退回 100");
        assert_eq!(meter_text(12.6, None), "13%");
        assert_eq!(meter_text(1.0, Some(32.0)), "3.1%");
        assert_eq!(meter_text(0.0, None), "0%");
    }

    #[test]
    fn chart_geometry() {
        let s = |values: Vec<f64>| PluginSeries { name: None, values, tone: None };
        assert_eq!(chart_max(&[s(vec![1.0, 2.0])], Some(100.0)), 100.0);
        assert!((chart_max(&[s(vec![1.0, 2.0]), s(vec![10.0])], None) - 11.0).abs() < 1e-9);
        assert_eq!(chart_max(&[s(vec![0.0])], None), 1.0);
        assert_eq!(chart_max(&[], Some(-3.0)), 1.0);

        // 三个点、五个槽：线贴着右边
        let pts = chart_points(&[0.0, 50.0, 100.0], 100.0, 5, 40.0, 10.0);
        assert_eq!(pts, vec![(20.0, 10.0), (30.0, 5.0), (40.0, 0.0)]);
        // 点比槽多就按点数铺满；越界的值夹住
        let pts = chart_points(&[200.0, -5.0, f64::NAN], 100.0, 2, 20.0, 10.0);
        assert_eq!(pts, vec![(0.0, 0.0), (10.0, 10.0), (20.0, 10.0)]);
        assert!(chart_points(&[], 1.0, 10, 20.0, 10.0).is_empty());
    }
}
