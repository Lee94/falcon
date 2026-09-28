//! 工作区排布：主区是从左到右的**列**，每列从上到下是若干**窗口**（pane）。
//!
//! 对应 web 的 `lib/layout.ts`，权威决策在 `docs/adr/0012-column-workspace.md`。
//!
//! 终端 / 文件 / 差异混排在同一套结构里，谁在哪一列哪一格，只由这里的 key 顺序决定
//! （key 约定见 [`crate::pane_key`]）。纯函数、零 GPUI：几何（列宽、窗口高、拖拽落点）
//! 也在这里算，视图层只负责量出矩形再把结果贴回元素。
//!
//! 尺寸的两档：`basis == None` = 自适应（列按 [`column_width`] 的公式，窗口在列内平分
//! 剩下的高度），`Some(px)` = 用户拖过，按像素钉住。拖一次只钉一边（列钉左边那列、
//! 窗口钉上面那个），右边 / 下面的继续自适应——画布本来就是横向可滚的，不需要此消彼长。
//!
//! 与 web 的差别只在"没变"怎么表达：TS 的 [`apply_drop`] 落回原地时返回**同一个数组**，
//! 调用方靠引用相等跳过一次 set；Rust 这边返回 `None`。其余函数一律返回新的 `Vec`，
//! 输入按引用借，不会被改动。
//!
//! 数字一律 `f64`：与 TS 的 number 逐位同算，四舍五入走 JS 的 `Math.round`（见
//! [`crate::js`]），视图层自己转成 GPUI 的 `Pixels`。
//!
//! ADR 0012 的"每扇窗口绝对定位、DOM 顺序恒定"在原生这边不需要（终端状态在模型里，
//! 元素每帧按快照重画，挂在树的哪个位置都一样），但几何仍然从这里出，两个客户端的
//! 列宽 / 窗口高才对得上。

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::js::{js_max, js_min, js_num, js_round};

/// 一扇窗口
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct PaneLayout {
    pub key: String,
    /// 窗口高度 px；`None` = 与同列其它自适应窗口平分
    #[serde(with = "js_num::opt")]
    pub basis: Option<f64>,
}

/// 一列
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ColumnLayout {
    pub id: String,
    /// 列宽 px；`None` = 按列数自动分
    #[serde(with = "js_num::opt")]
    pub basis: Option<f64>,
    pub panes: Vec<PaneLayout>,
    /// 固定在最右：新开的窗口、对账补的新列都排在它左边，拖拽也越不过去，直到从菜单
    /// 取消固定。至多一列，且**必须是数组里的最后一列**——这条不变式由下面每个会插列的
    /// 函数一起维持（插列一律夹到 [`pin_edge`] 之前），别绕过它们直接 `insert`。
    #[serde(default, skip_serializing_if = "is_false")]
    pub pinned: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// 窗口在排布里的坐标
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneAt {
    pub col: usize,
    pub index: usize,
}

/// 列宽下限：再窄终端就只剩三十来列，装不下一条命令
pub const COLUMN_MIN_PX: f64 = 260.0;
/// 窗口高度下限：标题栏 + 两三行
pub const PANE_MIN_PX: f64 = 96.0;

static SEQ: AtomicU64 = AtomicU64::new(0);

/// 列 id 只求唯一（视图的 key 与拖拽标识），不承载任何语义。
/// 形状照 web：`c` + 毫秒时间戳的 36 进制 + 序号的 36 进制。
pub fn new_column_id() -> String {
    let seq = SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    let ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    format!("c{}{}", base36(ms), base36(seq))
}

fn base36(mut n: u64) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if n == 0 {
        return "0".into();
    }
    let mut out = Vec::new();
    while n > 0 {
        out.push(DIGITS[(n % 36) as usize]);
        n /= 36;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// 一列新列，窗口都是自适应高度
pub fn column<I, S>(keys: I, basis: Option<f64>) -> ColumnLayout
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    ColumnLayout {
        id: new_column_id(),
        basis,
        panes: keys.into_iter().map(|key| PaneLayout { key: key.into(), basis: None }).collect(),
        pinned: false,
    }
}

/// 从左到右、列内从上到下的全部 key —— 切换窗口的快捷键按这个顺序走
pub fn pane_keys(columns: &[ColumnLayout]) -> Vec<String> {
    columns.iter().flat_map(|c| c.panes.iter().map(|p| p.key.clone())).collect()
}

pub fn find_pane(columns: &[ColumnLayout], key: &str) -> Option<PaneAt> {
    columns.iter().enumerate().find_map(|(col, c)| {
        c.panes.iter().position(|p| p.key == key).map(|index| PaneAt { col, index })
    })
}

/// 去掉空列。列是窗口的容器，空了就没有存在的理由（宽度也一并忘掉）
fn compact(columns: Vec<ColumnLayout>) -> Vec<ColumnLayout> {
    columns.into_iter().filter(|c| !c.panes.is_empty()).collect()
}

pub fn remove_pane(columns: &[ColumnLayout], key: &str) -> Vec<ColumnLayout> {
    compact(
        columns
            .iter()
            .map(|c| {
                let mut c = c.clone();
                c.panes.retain(|p| p.key != key);
                c
            })
            .collect(),
    )
}

/// 插进某列的某一格。key 已经在别处就是"移动"——先摘掉再插，
/// 同一个 key 绝不允许在排布里出现两次（两份终端挂同一个会话会互相抢 WS）。
pub fn insert_pane(columns: &[ColumnLayout], key: &str, at: PaneAt) -> Vec<ColumnLayout> {
    let basis = if find_pane(columns, key).is_some() { pane_basis(columns, key) } else { None };
    let mut base = remove_pane(columns, key);
    if base.is_empty() {
        return vec![column([key], None)];
    }
    let col = at.col.min(base.len() - 1);
    let c = &mut base[col];
    let index = at.index.min(c.panes.len());
    c.panes.insert(index, PaneLayout { key: key.to_string(), basis });
    base
}

/// 独占一列插在第 `at` 列的位置（`at = columns.len()` 就是接到最右）。
/// 有固定列时"最右"到它为止——真要钉到它右边只有 [`pin_pane`] 一条路。
pub fn insert_column(columns: &[ColumnLayout], key: &str, at: usize) -> Vec<ColumnLayout> {
    let mut next = remove_pane(columns, key);
    let at = at.min(pin_edge(&next));
    next.insert(at, column([key], None));
    next
}

/// 新列能插到的最右位置：固定列的下标，没有固定列就是末尾。
/// 固定列永远是最后一列，所以这个下标也就是"固定区"的起点。
pub fn pin_edge(columns: &[ColumnLayout]) -> usize {
    columns.iter().position(|c| c.pinned).unwrap_or(columns.len())
}

/// 这扇窗口是不是待在固定列里
pub fn is_pinned(columns: &[ColumnLayout], key: &str) -> bool {
    columns.iter().any(|c| c.pinned && c.panes.iter().any(|p| p.key == key))
}

/// 固定到最右：把这扇窗口摘出来独占一列钉在末尾。固定至多一列——两列都叫"最右"
/// 就没意义了，所以先把旧的那个标记清掉（它待的位置不动）。
pub fn pin_pane(columns: &[ColumnLayout], key: &str) -> Vec<ColumnLayout> {
    let basis = pane_basis(columns, key);
    let mut base = unpin_all(&remove_pane(columns, key));
    let mut pinned = column([key], None);
    pinned.panes[0].basis = basis;
    pinned.pinned = true;
    base.push(pinned);
    base
}

/// 取消固定：只清标记，列不动——它本来就在最右，取消之后只是不再挡着别人
pub fn unpin_all(columns: &[ColumnLayout]) -> Vec<ColumnLayout> {
    columns.iter().map(|c| ColumnLayout { pinned: false, ..c.clone() }).collect()
}

fn pane_basis(columns: &[ColumnLayout], key: &str) -> Option<f64> {
    columns.iter().find_map(|c| c.panes.iter().find(|p| p.key == key)).and_then(|p| p.basis)
}

/// 就地换 key，位置与高度都留着。文件 / 差异的"预览"语义靠它：点另一个文件
/// 是同一扇窗口换了内容，不是新开一扇。
pub fn replace_pane(columns: &[ColumnLayout], from: &str, to: &str) -> Vec<ColumnLayout> {
    if from == to {
        return columns.to_vec();
    }
    let mut base = remove_pane(columns, to);
    if find_pane(&base, from).is_none() {
        return base;
    }
    for c in &mut base {
        for p in &mut c.panes {
            if p.key == from {
                p.key = to.to_string();
            }
        }
    }
    base
}

/// 尺寸一律按 id / key 定位，不按下标：视图手上只有**可见**的那几列（别的项目的窗口
/// 被过滤掉了），下标与全量排布对不上。
pub fn set_column_basis(columns: &[ColumnLayout], id: &str, basis: Option<f64>) -> Vec<ColumnLayout> {
    columns
        .iter()
        .map(|c| if c.id == id { ColumnLayout { basis, ..c.clone() } } else { c.clone() })
        .collect()
}

pub fn set_pane_basis(columns: &[ColumnLayout], key: &str, basis: Option<f64>) -> Vec<ColumnLayout> {
    columns
        .iter()
        .map(|c| {
            let mut c = c.clone();
            for p in &mut c.panes {
                if p.key == key {
                    p.basis = basis;
                }
            }
            c
        })
        .collect()
}

/// 与数据源对账：`live` 之外的 key 一律摘掉（会话结束、文件关了），`live` 里还没排布的
/// 按给定顺序各自追加成新列。
///
/// 每次改动数据源之后都跑：漏掉一次，画布上就会有一扇连着死会话的窗口，或者开出来的
/// 会话找不到落脚的地方。
pub fn sync_columns<S: AsRef<str>>(columns: &[ColumnLayout], live: &[S]) -> Vec<ColumnLayout> {
    let want: HashSet<&str> = live.iter().map(AsRef::as_ref).collect();
    let mut kept = compact(
        columns
            .iter()
            .map(|c| {
                let mut c = c.clone();
                c.panes.retain(|p| want.contains(p.key.as_str()));
                c
            })
            .collect(),
    );
    let have: HashSet<String> = pane_keys(&kept).into_iter().collect();
    let missing: Vec<&str> = live.iter().map(AsRef::as_ref).filter(|k| !have.contains(*k)).collect();
    if missing.is_empty() {
        return kept;
    }
    // 补的新列也得让开固定列
    let at = pin_edge(&kept);
    let tail = kept.split_off(at);
    kept.extend(missing.into_iter().map(|k| column([k], None)));
    kept.extend(tail);
    kept
}

/// 当前该画出来的列：只留 visible 的窗口，空列不占位。
///
/// 不写回状态——切项目只是"看不见"，别的项目的排布（列宽、上下关系）要原样等在那，
/// 切回去还是老样子。
pub fn visible_columns(columns: &[ColumnLayout], visible: impl Fn(&str) -> bool) -> Vec<ColumnLayout> {
    compact(
        columns
            .iter()
            .map(|c| {
                let mut c = c.clone();
                c.panes.retain(|p| visible(&p.key));
                c
            })
            .collect(),
    )
}

// ---------------- 几何 ----------------

/// 列间距，与画布上的 gap 同一个数
pub const CANVAS_GAP_PX: f64 = 8.0;

/// 自适应列的宽度：一列时占满；两列及以上每列 max(半屏, min(全屏, 640))——
/// 宽屏并排两列，再多的往右排，已在场的列不会因为新开第三列而被挤窄。
/// 640 = 40rem，与设计里其它"一栏内容"的上限同一个数。
///
/// `gap` 在 TS 里缺省是 [`CANVAS_GAP_PX`]。
pub fn column_width(column: &ColumnLayout, count: usize, viewport_width: f64, gap: f64) -> f64 {
    if let Some(basis) = column.basis {
        return basis;
    }
    if count <= 1 {
        return viewport_width;
    }
    js_max((viewport_width - gap) / 2.0, js_min(viewport_width, 640.0))
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    pub width: f64,
    pub height: f64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct PaneFrame {
    pub key: String,
    pub y: f64,
    pub height: f64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ColumnFrame {
    pub id: String,
    pub x: f64,
    pub width: f64,
    pub panes: Vec<PaneFrame>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct CanvasFrames {
    pub columns: Vec<ColumnFrame>,
    /// 内容总宽（含列间距），画布靠它撑出横向滚动区
    pub width: f64,
}

/// 排布 → 像素坐标。
///
/// web 那边不用嵌套 flex 而是自己算坐标，是因为 xterm 的画布一旦换过父节点渲染尺寸
/// 就错了（ADR 0012）；原生没有这个坑，但坐标仍从这里出，两个客户端才画得一样。
///
/// `gap` 在 TS 里缺省是 [`CANVAS_GAP_PX`]。
pub fn layout_frames(columns: &[ColumnLayout], viewport: Viewport, gap: f64) -> CanvasFrames {
    let mut x = 0.0;
    let mut out = Vec::with_capacity(columns.len());
    for column in columns {
        let width = column_width(column, columns.len(), viewport.width, gap);
        out.push(ColumnFrame {
            id: column.id.clone(),
            x,
            width,
            panes: pane_frames(&column.panes, viewport.height),
        });
        x += width + gap;
    }
    CanvasFrames { columns: out, width: js_max(0.0, x - gap) }
}

/// 列内各窗口的高度：钉死的按钉死的算，其余平分剩下的；除不尽的零头给最后一扇
/// 自适应窗口，免得列底留一条一像素的缝。
fn pane_frames(panes: &[PaneLayout], height: f64) -> Vec<PaneFrame> {
    let fixed: f64 = panes.iter().map(|p| p.basis.unwrap_or(0.0)).sum();
    let autos = panes.iter().filter(|p| p.basis.is_none()).count();
    let each = if autos > 0 { js_max(PANE_MIN_PX, (height - fixed) / autos as f64) } else { 0.0 };
    let last_auto = panes.iter().rposition(|p| p.basis.is_none());
    let mut out = Vec::with_capacity(panes.len());
    let mut y = 0.0;
    for (i, p) in panes.iter().enumerate() {
        let h = match p.basis {
            Some(b) => b,
            None if Some(i) == last_auto => js_max(PANE_MIN_PX, height - y),
            None => each,
        };
        out.push(PaneFrame { key: p.key.clone(), y: js_round(y), height: js_round(h) });
        y += h;
    }
    out
}

// ---------------- 拖拽落点 ----------------

/// 窗口拖到哪：落进某列的第 index 格，或在第 at 列的位置另起一列
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum DropSpot {
    Into { col: usize, index: usize },
    Column { at: usize },
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub struct PaneRect {
    pub top: f64,
    pub bottom: f64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ColumnRect {
    pub left: f64,
    pub right: f64,
    pub panes: Vec<PaneRect>,
}

/// 列两侧多宽算"要另起一列"。够窄才不会挡住列内插入，够宽才点得中
pub const COLUMN_EDGE_PX: f64 = 28.0;

/// 指针落点 → 落位。列之间的缝、最左 / 最右之外都算另起一列；落在列身上则按各窗口
/// 的中线决定插在第几格（与 Chrome 拖 tab 一样，越过邻居中点才换位）。
///
/// `edge` 在 TS 里缺省是 [`COLUMN_EDGE_PX`]。
pub fn drop_spot(point: Point, rects: &[ColumnRect], edge: f64) -> DropSpot {
    let Some(first) = rects.first() else { return DropSpot::Column { at: 0 } };
    if point.x < first.left {
        return DropSpot::Column { at: 0 };
    }
    for (i, rect) in rects.iter().enumerate() {
        if point.x > rect.right {
            // 落在两列之间的缝里：插一列进去
            let Some(next) = rects.get(i + 1) else { return DropSpot::Column { at: rects.len() } };
            if point.x < next.left {
                return DropSpot::Column { at: i + 1 };
            }
            continue;
        }
        let width = rect.right - rect.left;
        // 窄列上不留边缘区：整列都进不去反而更难受
        let band = js_min(edge, width / 4.0);
        if point.x - rect.left < band {
            return DropSpot::Column { at: i };
        }
        if rect.right - point.x < band {
            return DropSpot::Column { at: i + 1 };
        }
        let mut index = 0;
        for (j, pane) in rect.panes.iter().enumerate() {
            if point.y > (pane.top + pane.bottom) / 2.0 {
                index = j + 1;
            }
        }
        return DropSpot::Into { col: i, index };
    }
    DropSpot::Column { at: rects.len() }
}

/// 把落点夹到固定列左边。固定列右边不接新列，"固定在最右"才立得住；
/// 拖拽的指示线与真正落位共用它，免得松手之后窗口跳到别的地方去。
pub fn clamp_spot(columns: &[ColumnLayout], spot: DropSpot) -> DropSpot {
    match spot {
        DropSpot::Column { at } => DropSpot::Column { at: at.min(pin_edge(columns)) },
        into => into,
    }
}

/// 把拖拽落点应用到排布上。
///
/// 落回原地（同列同格、或从独占列拖到自己那一列的缝里）、或排布里根本没有这个 key
/// 时返回 `None`（TS 返回原数组），调用方据此跳过一次状态更新——拖着没动也重排一遍
/// 会让所有终端白白量一次尺寸。
pub fn apply_drop(columns: &[ColumnLayout], key: &str, raw: DropSpot) -> Option<Vec<ColumnLayout>> {
    let from = find_pane(columns, key)?;
    let spot = clamp_spot(columns, raw);
    let alone = columns[from.col].panes.len() == 1;
    match spot {
        DropSpot::Column { at } => {
            // 本来就独占一列，拖到自己左右两条缝里等于没动
            if alone && (at == from.col || at == from.col + 1) {
                return None;
            }
            let at = if alone && at > from.col { at - 1 } else { at };
            Some(insert_column(columns, key, at))
        }
        DropSpot::Into { col, index } => {
            if col == from.col && (index == from.index || index == from.index + 1) {
                return None;
            }
            // 摘掉自己之后：同列里排在后面的格子上移一格；独占的那一列整个消失，
            // 它右边的列跟着左移一格
            let target_col = if alone && col > from.col { col - 1 } else { col };
            let target_index = if col == from.col && index > from.index { index - 1 } else { index };
            Some(insert_pane(columns, key, PaneAt { col: target_col, index: target_index }))
        }
    }
}

/// 把可见列上量出来的落点翻成全量排布的坐标。
///
/// 画布画的是 [`visible_columns`] 的结果，而状态里存着所有项目的列；两边的下标只在
/// "当前项目就是全部"时才碰巧一致。转换一律认 id 与 key，不认下标。
pub fn resolve_spot(columns: &[ColumnLayout], visible: &[ColumnLayout], spot: DropSpot) -> DropSpot {
    let tail = DropSpot::Column { at: columns.len() };
    match spot {
        DropSpot::Column { at } => {
            let Some(anchor) = visible.get(at) else { return tail };
            let at = columns.iter().position(|c| c.id == anchor.id).unwrap_or(columns.len());
            DropSpot::Column { at }
        }
        DropSpot::Into { col, index } => {
            let Some(vcol) = visible.get(col) else { return tail };
            let Some(col) = columns.iter().position(|c| c.id == vcol.id) else { return tail };
            let panes = &columns[col].panes;
            // 落在可见的第 index 扇窗口**之前**；越过最后一扇就是列尾
            let index = vcol
                .panes
                .get(index)
                .and_then(|anchor| panes.iter().position(|p| p.key == anchor.key))
                .unwrap_or(panes.len());
            DropSpot::Into { col, index }
        }
    }
}

// ---------------- 尺寸 ----------------

/// 列宽夹到 [`COLUMN_MIN_PX`, max]；`max` 在 TS 里缺省是 `+∞`
pub fn clamp_column_width(px: f64, max: f64) -> f64 {
    if !px.is_finite() {
        return COLUMN_MIN_PX;
    }
    js_round(js_min(js_max(px, COLUMN_MIN_PX), js_max(COLUMN_MIN_PX, max)))
}

pub fn clamp_pane_height(px: f64, max: f64) -> f64 {
    if !px.is_finite() {
        return PANE_MIN_PX;
    }
    js_round(js_min(js_max(px, PANE_MIN_PX), js_max(PANE_MIN_PX, max)))
}

/// 拖列内分隔线时，上面那个窗口最多能长到多高：列高扣掉它上面已钉死的、
/// 以及它下面每个窗口至少要留的那点高度。
pub fn pane_max_height(column_height: f64, above: f64, below: f64) -> f64 {
    js_max(PANE_MIN_PX, column_height - above - below * PANE_MIN_PX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_ids_are_unique_and_web_shaped() {
        let a = new_column_id();
        let b = new_column_id();
        assert_ne!(a, b);
        assert!(a.starts_with('c') && a[1..].bytes().all(|c| c.is_ascii_alphanumeric()));
        assert_eq!(base36(0), "0");
        assert_eq!(base36(35), "z");
        assert_eq!(base36(36), "10");
    }

    #[test]
    fn column_layout_serializes_like_web() {
        let mut c = column(["t:s1"], Some(320.0));
        c.id = "c1".into();
        assert_eq!(
            serde_json::to_string(&c).unwrap(),
            r#"{"id":"c1","basis":320,"panes":[{"key":"t:s1","basis":null}]}"#
        );
        c.pinned = true;
        assert!(serde_json::to_string(&c).unwrap().ends_with(r#""pinned":true}"#));
        let spot: DropSpot = serde_json::from_str(r#"{"kind":"into","col":1,"index":2}"#).unwrap();
        assert_eq!(spot, DropSpot::Into { col: 1, index: 2 });
        assert_eq!(serde_json::to_string(&DropSpot::Column { at: 3 }).unwrap(), r#"{"kind":"column","at":3}"#);
    }

    #[test]
    fn unpin_keeps_order_and_basis() {
        let cols = pin_pane(&[column(["a"], Some(300.0)), column(["b"], None)], "a");
        let un = unpin_all(&cols);
        assert_eq!(pane_keys(&un), ["b", "a"]);
        assert!(un.iter().all(|c| !c.pinned));
        assert_eq!(un[0].basis, None);
    }
}
