//! 工作区排布：主区是从左到右的**列**，每列从上到下是若干**窗口**（pane）；列再分到
//! 一块块**画布**上（[`ColumnLayout::canvas`]），一次只显示一块，一块就是一屏。
//!
//! 对应旧 React 版的 `lib/layout.ts`，权威决策在 `docs/adr/0012-column-workspace.md`。
//!
//! 终端 / 文件 / 差异混排在同一套结构里，谁在哪一列哪一格，只由这里的 key 顺序决定
//! （key 约定见 [`crate::pane_key`]）。纯函数、零 GPUI：几何（列宽、窗口高、拖拽落点）
//! 也在这里算，视图层只负责量出矩形再把结果贴回元素。
//!
//! 画布不横向滚动：一块画布上的列正好铺满视口宽，排不下（[`canvas_fits`]）的新列另起
//! 一块画布（[`assign_canvases`]）。见 ADR 0012 的「多画布」一节。
//!
//! 尺寸的两档：`basis == None` = 自适应（列平分画布上剩下的宽，窗口在列内平分剩下的
//! 高度），`Some(px)` = 用户拖过，按像素钉住。拖一次只钉一边（列钉左边那列、
//! 窗口钉上面那个），右边 / 下面的跟着让位。
//!
//! 与 React 版的差别只在"没变"怎么表达：TS 的 [`apply_drop`] / [`assign_canvases`] /
//! [`move_to_canvas`] 没变时返回**同一个数组**，调用方靠引用相等跳过一次 set；Rust 这边
//! 返回 `None`。其余函数一律返回新的 `Vec`，输入按引用借，不会被改动。
//!
//! 数字一律 `f64`：与 TS 的 number 逐位同算，四舍五入走 JS 的 `Math.round`（见
//! [`crate::js`]），视图层自己转成 GPUI 的 `Pixels`。
//!
//! ADR 0012 的"每扇窗口绝对定位、DOM 顺序恒定"在 GPUI 这边不需要（终端状态在模型里，
//! 元素每帧按快照重画，挂在树的哪个位置都一样），但几何仍然从这里出，列宽 / 窗口高
//! 沿用 React 版的同一套算法。

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use web_time::{SystemTime, UNIX_EPOCH};

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
    ///
    /// 有多块画布时固定列**每块都有**：它接在每块画布的最右（[`canvas_groups`]），所以它的
    /// `canvas` 不看。
    #[serde(default, skip_serializing_if = "is_false")]
    pub pinned: bool,
    /// 所在画布的 id。同一块画布的列在数组里保持连续（插列的函数一起维持，与 pinned 同理）。
    /// `None` = 还没分：对账补进来的、取消固定的、旧版本落盘的——由 [`assign_canvases`] 按
    /// "放得下就跟左边那一列同一块，放不下另起一块"补上。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canvas: Option<String>,
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
/// 一列"排得下"的宽：自适应的列平分下来不到这么宽，新开的列就另起一块画布，而不是
/// 把已在场的挤窄。560 ≈ 默认字号下 70 列终端——13 寸笔记本去掉侧栏还能并排两列，
/// 宽屏并排两到三列。
pub const COLUMN_FIT_PX: f64 = 560.0;
/// 窗口高度下限：标题栏 + 两三行
pub const PANE_MIN_PX: f64 = 96.0;

static SEQ: AtomicU64 = AtomicU64::new(0);

/// 列 id 只求唯一（视图的 key 与拖拽标识），不承载任何语义。
/// 形状照 React 版：`c` + 毫秒时间戳的 36 进制 + 序号的 36 进制。
pub fn new_column_id() -> String {
    seq_id('c')
}

/// 画布 id 同理，只求唯一；它要落盘（同一块画布上有哪些列得活过重启）
pub fn new_canvas_id() -> String {
    seq_id('v')
}

fn seq_id(prefix: char) -> String {
    let seq = SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    let ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    format!("{prefix}{}{}", base36(ms), base36(seq))
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
        canvas: None,
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
///
/// `canvas` 给了就落在那块画布上（拖拽落点、挪到某块画布：用户指了地方，挤也认）；
/// 不给就是还没分，等 [`assign_canvases`] 按排不排得下来定。
pub fn insert_column(columns: &[ColumnLayout], key: &str, at: usize, canvas: Option<&str>) -> Vec<ColumnLayout> {
    let mut next = remove_pane(columns, key);
    let at = at.min(pin_edge(&next));
    let mut col = column([key], None);
    col.canvas = canvas.filter(|c| !c.is_empty()).map(str::to_string);
    next.insert(at, col);
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

/// 取消固定：只清标记，列不动——它本来就在最右，取消之后只是不再挡着别人。
/// 它原先在每块画布上都有，取消之后归哪块重新分（[`assign_canvases`]：最后一块放得下就
/// 进去，放不下另起一块）。
pub fn unpin_all(columns: &[ColumnLayout]) -> Vec<ColumnLayout> {
    columns
        .iter()
        .map(|c| if c.pinned { ColumnLayout { pinned: false, canvas: None, ..c.clone() } } else { c.clone() })
        .collect()
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
/// 按给定顺序各自追加成新列。新列还没分画布，由 [`assign_canvases`] 按排不排得下去分。
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

// ---------------- 画布 ----------------

/// 一块画布：它的列按排布顺序，固定列接在最右
#[derive(Debug, Clone, PartialEq)]
pub struct CanvasGroup {
    pub id: String,
    pub columns: Vec<ColumnLayout>,
}

/// 把（已按项目过滤过的）可见列分成一块块画布。画布按它第一列在排布里的先后排，块内
/// 按排布顺序；固定列接在**每一块**的最右——它就是为了"切到哪块都在"才固定的。只有
/// 固定列时它自己算一块。
///
/// 认 id 分组而不是认连续段：插列的函数都维持着"同一块的列连续"，但这里不靠它，
/// 万一断开了也只是块内顺序有点怪，不会冒出两块同 id 的画布。还没分画布的列跟着左边
/// 那一列走（工作区量到画布宽之后会用 [`assign_canvases`] 正式分好，这里只是不让它们
/// 无处可去）。
pub fn canvas_groups(columns: &[ColumnLayout]) -> Vec<CanvasGroup> {
    let mut groups: Vec<CanvasGroup> = Vec::new();
    let mut prev: Option<usize> = None;
    let mut pinned: Option<&ColumnLayout> = None;
    for c in columns {
        if c.pinned {
            pinned = Some(c);
            continue;
        }
        let id = c.canvas.clone().or_else(|| prev.map(|i| groups[i].id.clone())).unwrap_or_else(|| c.id.clone());
        let at = match groups.iter().position(|g| g.id == id) {
            Some(i) => i,
            None => {
                groups.push(CanvasGroup { id, columns: Vec::new() });
                groups.len() - 1
            }
        };
        groups[at].columns.push(c.clone());
        prev = Some(at);
    }
    let Some(pinned) = pinned else { return groups };
    if groups.is_empty() {
        return vec![CanvasGroup {
            id: pinned.canvas.clone().unwrap_or_else(|| pinned.id.clone()),
            columns: vec![pinned.clone()],
        }];
    }
    for g in &mut groups {
        g.columns.push(pinned.clone());
    }
    groups
}

/// 按画布排好的可见列（画布一块接一块，固定列在最后）：切窗口的快捷键按它走
pub fn order_by_canvas(columns: &[ColumnLayout]) -> Vec<ColumnLayout> {
    let mut out: Vec<ColumnLayout> = canvas_groups(columns)
        .into_iter()
        .flat_map(|g| g.columns.into_iter().filter(|c| !c.pinned))
        .collect();
    if let Some(pinned) = columns.iter().find(|c| c.pinned) {
        out.push(pinned.clone());
    }
    out
}

/// 正在显示第几块：活动窗口所在的那块；活动窗口在固定列里（每块都有它）或者不在场，
/// 就沿用 `remembered`（上一次显示的那块），它也不在了就第一块。
pub fn canvas_index(groups: &[CanvasGroup], active_key: Option<&str>, remembered: Option<&str>) -> usize {
    if let Some(key) = active_key
        && let Some(i) = groups
            .iter()
            .position(|g| g.columns.iter().any(|c| !c.pinned && c.panes.iter().any(|p| p.key == key)))
    {
        return i;
    }
    remembered.and_then(|id| groups.iter().position(|g| g.id == id)).unwrap_or(0)
}

/// 这几列（含固定列）摆在 `width` 宽的画布上排不排得下：钉了宽的按钉的算，自适应的
/// 按 [`COLUMN_FIT_PX`] 算。一列永远排得下。`gap` 在 TS 里缺省是 [`CANVAS_GAP_PX`]。
pub fn canvas_fits(columns: &[ColumnLayout], width: f64, gap: f64) -> bool {
    if columns.len() <= 1 {
        return true;
    }
    let need = columns.iter().fold(0.0, |n, c| n + c.basis.unwrap_or(COLUMN_FIT_PX));
    need + gap * (columns.len() - 1) as f64 <= width
}

/// 给还没分画布的可见列分画布——"排不下就自动新开一块画布"就在这里。
///
/// 按排布顺序逐列：跟它左边最近的那一列（看得见、已分好）同一块，前提是加上它之后那块
/// 还排得下（[`canvas_fits`]，含固定列）；排不下就另起一块新画布，并挪到左边那块的最后
/// 一列后面，保持同一块的列连续——这样新画布就紧跟在原来那块后面。左边没有列的另起一块。
///
/// 只分 `visible` 的：别的项目补进来的列，等切过去看得见了再按那时的画布分。画布宽还没
/// 量出来（`width <= 0`）什么也不做。没东西可分时返回 `None`（TS 返回原数组）。
///
/// 分过的不再动：窗口缩窄了、邻居关掉了，已在场的列都待在原来那块（窄了就挤一挤，
/// 见 [`column_widths`]），不会在画布之间跳来跳去。
pub fn assign_canvases(
    columns: &[ColumnLayout],
    visible: impl Fn(&str) -> bool,
    width: f64,
    gap: f64,
) -> Option<Vec<ColumnLayout>> {
    // NaN 也算没量出来
    if !(width > 0.0) {
        return None;
    }
    let shown = |c: &ColumnLayout| c.panes.iter().any(|p| visible(&p.key));
    if !columns.iter().any(|c| !c.pinned && c.canvas.is_none() && shown(c)) {
        return None;
    }
    let view = |c: &ColumnLayout| {
        let mut c = c.clone();
        c.panes.retain(|p| visible(&p.key));
        c
    };
    let pinned: Vec<ColumnLayout> = columns.iter().filter(|c| c.pinned && shown(c)).map(view).collect();
    let mut out = columns.to_vec();
    let mut i = 0;
    while i < out.len() {
        let c = &out[i];
        if c.pinned || c.canvas.is_some() || !shown(c) {
            i += 1;
            continue;
        }
        let prev: Option<String> =
            out[..i].iter().rev().find(|p| !p.pinned && p.canvas.is_some() && shown(p)).and_then(|p| p.canvas.clone());
        if let Some(prev) = &prev {
            let mut group: Vec<ColumnLayout> = out
                .iter()
                .filter(|x| !x.pinned && x.canvas.as_ref() == Some(prev) && shown(x))
                .map(view)
                .collect();
            group.push(view(&out[i]));
            group.extend(pinned.iter().cloned());
            if canvas_fits(&group, width, gap) {
                out[i].canvas = Some(prev.clone());
                i += 1;
                continue;
            }
        }
        let mut fresh = out[i].clone();
        fresh.canvas = Some(new_canvas_id());
        let end = prev.as_ref().and_then(|prev| out.iter().rposition(|x| !x.pinned && x.canvas.as_ref() == Some(prev)));
        match end {
            Some(end) if end > i => {
                // 摘掉自己之后 end 前移了一格，插在 end 上正好是原来那一列的后面；
                // 下标 i 上现在是下一列，不前进
                out.remove(i);
                out.insert(end, fresh);
            }
            _ => {
                out[i] = fresh;
                i += 1;
            }
        }
    }
    Some(out)
}

/// 某块画布最后一列之后的下标（新列插在这里就是接在这块画布的最右）；没有这块返回 `None`
pub fn canvas_end(columns: &[ColumnLayout], canvas: &str) -> Option<usize> {
    columns.iter().rposition(|c| !c.pinned && c.canvas.as_deref() == Some(canvas)).map(|i| i + 1)
}

/// 把一扇窗口挪到另一块画布：独占一列，接在那块画布的最右（挤也认，用户指了地方）。
/// `target` 为 `None` = 新开一块，紧跟在 `after` 那块后面（`after` 找不到就放到最后）。
/// 本来就独占一列待在 `target` 上的、排布里没有这扇窗口的，返回 `None`（TS 返回原数组）。
pub fn move_to_canvas(
    columns: &[ColumnLayout],
    key: &str,
    target: Option<&str>,
    after: Option<&str>,
) -> Option<Vec<ColumnLayout>> {
    let from = find_pane(columns, key)?;
    let home = &columns[from.col];
    if let Some(target) = target
        && !home.pinned
        && home.canvas.as_deref() == Some(target)
        && home.panes.len() == 1
    {
        return None;
    }
    let base = remove_pane(columns, key);
    if let Some(target) = target {
        let at = canvas_end(&base, target).unwrap_or_else(|| pin_edge(&base));
        return Some(insert_column(&base, key, at, Some(target)));
    }
    let at = after.and_then(|after| canvas_end(&base, after)).unwrap_or_else(|| pin_edge(&base));
    Some(insert_column(&base, key, at, Some(&new_canvas_id())))
}

// ---------------- 几何 ----------------

/// 列间距，与画布上的 gap 同一个数
pub const CANVAS_GAP_PX: f64 = 8.0;

/// 一块画布上各列的宽：正好铺满 `width`，不溢出——主区不再横向滚动，放不下的列在别的画布上。
///
/// - 只剩一列时占满，连钉死的宽度也不认：把手只长在列与列之间，最后一列没有；而只有
///   一扇窗口时最大化按钮也藏了。并排时拖窄的列在邻居关掉 / 切到只有它的项目之后，就会
///   窄窄地停在左边、右边空一大片，没有任何办法拉回来。basis 不清掉，再有列进来时照旧按它排。
/// - 钉了宽的按钉的给，自适应的平分剩下的；一列自适应的都没有时最后一列收尾（与窗口高度
///   同一个道理：最后一列的右边没有把手）。
/// - 剩下的不够每列自适应的分到 [`COLUMN_MIN_PX`]（窗口缩窄了、或者硬拖进来的）：钉了宽的
///   按各自超出下限的部分等比让出来；让到下限还不够，就全部平分——宁可比下限窄也不溢出画布。
///
/// `gap` 在 TS 里缺省是 [`CANVAS_GAP_PX`]。
pub fn column_widths(columns: &[ColumnLayout], width: f64, gap: f64) -> Vec<f64> {
    let n = columns.len();
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![js_max(0.0, width)];
    }
    let avail = js_max(0.0, width - gap * (n - 1) as f64);
    let last_auto = columns.iter().rposition(|c| c.basis.is_none());
    let is_auto: Vec<bool> =
        columns.iter().enumerate().map(|(i, c)| c.basis.is_none() || (last_auto.is_none() && i == n - 1)).collect();
    let autos = is_auto.iter().filter(|a| **a).count() as f64;
    let fixed: Vec<f64> =
        columns.iter().zip(&is_auto).map(|(c, auto)| if *auto { 0.0 } else { c.basis.unwrap_or(0.0) }).collect();
    let fixed_sum = fixed.iter().fold(0.0, |a, b| a + b);
    let room = avail - fixed_sum;
    if room >= autos * COLUMN_MIN_PX {
        return is_auto.iter().zip(&fixed).map(|(auto, w)| if *auto { room / autos } else { *w }).collect();
    }
    // 钉了宽的让位：各自超出下限的部分按比例让
    let need = autos * COLUMN_MIN_PX - room;
    let slack = fixed.iter().zip(&is_auto).fold(0.0, |n, (w, auto)| n + if *auto { 0.0 } else { js_max(0.0, w - COLUMN_MIN_PX) });
    if slack >= need && slack > 0.0 {
        let k = need / slack;
        return is_auto
            .iter()
            .zip(&fixed)
            .map(|(auto, w)| if *auto { COLUMN_MIN_PX } else { w - js_max(0.0, w - COLUMN_MIN_PX) * k })
            .collect();
    }
    vec![avail / n as f64; n]
}

/// 拖列缝时，左边那列最多能拖到多宽：右边每列至少留下限宽。
/// `left` = 这一列的左边，`right` = 它右边还有几列。
pub fn column_max_width(width: f64, left: f64, right: f64) -> f64 {
    js_max(COLUMN_MIN_PX, width - left - right * (COLUMN_MIN_PX + CANVAS_GAP_PX))
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
}

/// 一块画布的排布 → 像素坐标（`columns` 就是这一块上的列，含接在最右的固定列）。
/// 列的左右边取整，最后一列的右边正好落在 `viewport.width` 上。
///
/// 旧 React 版不用嵌套 flex 而是自己算坐标，是因为 xterm 的画布一旦换过父节点渲染尺寸
/// 就错了（ADR 0012）；GPUI 没有这个坑，坐标仍从这里出，沿用同一套几何。
///
/// `gap` 在 TS 里缺省是 [`CANVAS_GAP_PX`]。
pub fn layout_frames(columns: &[ColumnLayout], viewport: Viewport, gap: f64) -> CanvasFrames {
    let widths = column_widths(columns, viewport.width, gap);
    let mut x = 0.0;
    let mut out = Vec::with_capacity(columns.len());
    for (i, column) in columns.iter().enumerate() {
        let left = js_round(x);
        let right = if i + 1 == columns.len() { js_max(left, viewport.width) } else { js_round(x + widths[i]) };
        out.push(ColumnFrame {
            id: column.id.clone(),
            x: left,
            width: right - left,
            panes: pane_frames(&column.panes, viewport.height),
        });
        x += widths[i] + gap;
    }
    CanvasFrames { columns: out }
}

/// 列内各窗口的高度：钉死的按钉死的算，其余平分剩下的；除不尽的零头给最后一扇
/// 自适应窗口，免得列底留一条一像素的缝。
///
/// 一扇自适应的都没有（钉了上面那扇之后把下面的关掉 / 拖走，只剩一扇时最常见）就让最后
/// 一扇收尾：把手只长在两扇之间，最后一扇的底边拖不动，按钉的高度排就会在列底留一截
/// 永远填不上的空。
fn pane_frames(panes: &[PaneLayout], height: f64) -> Vec<PaneFrame> {
    let fixed: f64 = panes.iter().map(|p| p.basis.unwrap_or(0.0)).sum();
    let autos = panes.iter().filter(|p| p.basis.is_none()).count();
    let each = if autos > 0 { js_max(PANE_MIN_PX, (height - fixed) / autos as f64) } else { 0.0 };
    let filler = panes.iter().rposition(|p| p.basis.is_none()).or(panes.len().checked_sub(1));
    let mut out = Vec::with_capacity(panes.len());
    let mut y = 0.0;
    for (i, p) in panes.iter().enumerate() {
        let h = match p.basis {
            _ if Some(i) == filler => js_max(PANE_MIN_PX, height - y),
            Some(b) => b,
            None => each,
        };
        out.push(PaneFrame { key: p.key.clone(), y: js_round(y), height: js_round(h) });
        y += h;
    }
    out
}

// ---------------- 画布缩略图 ----------------

/// 画布条上缩略图的高（px）；宽按画布的宽高比算，见 [`canvas_thumb_size`]
pub const CANVAS_THUMB_H: f64 = 16.0;
/// 缩略图宽的上下限：竖屏 / 超宽屏也不至于缩成一条线或撑满画布条
pub const CANVAS_THUMB_MIN_W: f64 = 20.0;
pub const CANVAS_THUMB_MAX_W: f64 = 40.0;

/// 缩略图的尺寸：高固定，宽按画布宽高比；画布还没量出来时给个常见比例（3:2）
pub fn canvas_thumb_size(viewport: Viewport) -> Viewport {
    let ratio = if viewport.width > 0.0 && viewport.height > 0.0 { viewport.width / viewport.height } else { 1.5 };
    let width = js_round(CANVAS_THUMB_H * ratio);
    Viewport { width: js_min(CANVAS_THUMB_MAX_W, js_max(CANVAS_THUMB_MIN_W, width)), height: CANVAS_THUMB_H }
}

/// 缩略图里一扇窗口的矩形（相对缩略图左上角，整数像素）
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ThumbRect {
    pub key: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// 画布缩略图里每扇窗口的矩形：按这块画布真实的几何（[`layout_frames`]）等比缩进 `size`，
/// 窗口之间留 `gap` 宽的缝。不能直接缩真实坐标：8px 的列缝、窗口之间没有缝，缩到十几像素
/// 高全糊成一块，认不出几列几扇。所以按"格子"缩——列占到下一列的左边为止、窗口占到
/// 下一扇的顶为止，取整之后各自让出 `gap`；整张图正好铺满 `size`，四周不留边（边距归外框）。
///
/// 取整走 [`js_round`]（与 JS 的 `Math.round` 一致，8.5 → 9），沿用 React 版的取整口径。
/// `gap` 在 TS 里缺省是 1。
pub fn canvas_thumb(columns: &[ColumnLayout], viewport: Viewport, size: Viewport, gap: f64) -> Vec<ThumbRect> {
    if !(viewport.width > 0.0 && viewport.height > 0.0) {
        return Vec::new();
    }
    let frames = layout_frames(columns, viewport, CANVAS_GAP_PX).columns;
    let sx = (size.width + gap) / viewport.width;
    let sy = (size.height + gap) / viewport.height;
    let mut out = Vec::new();
    for (ci, col) in frames.iter().enumerate() {
        let left = js_round(col.x * sx);
        let right = js_round(frames.get(ci + 1).map_or(viewport.width, |c| c.x) * sx);
        for (pi, pane) in col.panes.iter().enumerate() {
            let top = js_round(pane.y * sy);
            let bottom = js_round(col.panes.get(pi + 1).map_or(viewport.height, |p| p.y) * sy);
            out.push(ThumbRect {
                key: pane.key.clone(),
                x: left,
                y: top,
                width: js_max(1.0, right - left - gap),
                height: js_max(1.0, bottom - top - gap),
            });
        }
    }
    out
}

// ---------------- 拖拽落点 ----------------

/// 窗口拖到哪：落进某列的第 index 格，或在第 at 列的位置另起一列。
/// `canvas` 只在翻成全量坐标之后才有（[`resolve_spot`]）：另起的那一列落在哪块画布上。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum DropSpot {
    Into {
        col: usize,
        index: usize,
    },
    Column {
        at: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        canvas: Option<String>,
    },
}

impl DropSpot {
    /// 不指定画布的另起一列
    pub fn column(at: usize) -> Self {
        DropSpot::Column { at, canvas: None }
    }
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
    let Some(first) = rects.first() else { return DropSpot::column(0) };
    if point.x < first.left {
        return DropSpot::column(0);
    }
    for (i, rect) in rects.iter().enumerate() {
        if point.x > rect.right {
            // 落在两列之间的缝里：插一列进去
            let Some(next) = rects.get(i + 1) else { return DropSpot::column(rects.len()) };
            if point.x < next.left {
                return DropSpot::column(i + 1);
            }
            continue;
        }
        let width = rect.right - rect.left;
        // 窄列上不留边缘区：整列都进不去反而更难受
        let band = js_min(edge, width / 4.0);
        if point.x - rect.left < band {
            return DropSpot::column(i);
        }
        if rect.right - point.x < band {
            return DropSpot::column(i + 1);
        }
        let mut index = 0;
        for (j, pane) in rect.panes.iter().enumerate() {
            if point.y > (pane.top + pane.bottom) / 2.0 {
                index = j + 1;
            }
        }
        return DropSpot::Into { col: i, index };
    }
    DropSpot::column(rects.len())
}

/// 把落点夹到固定列左边。固定列右边不接新列，"固定在最右"才立得住；
/// 拖拽的指示线与真正落位共用它，免得松手之后窗口跳到别的地方去。
pub fn clamp_spot(columns: &[ColumnLayout], spot: DropSpot) -> DropSpot {
    match spot {
        DropSpot::Column { at, canvas } => DropSpot::Column { at: at.min(pin_edge(columns)), canvas },
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
    let home = &columns[from.col];
    let alone = home.panes.len() == 1;
    match spot {
        DropSpot::Column { at, canvas } => {
            // 本来就独占一列，拖到自己左右两条缝里等于没动
            let same_canvas = canvas.is_none() || canvas == home.canvas;
            if alone && same_canvas && (at == from.col || at == from.col + 1) {
                return None;
            }
            let at = if alone && at > from.col { at - 1 } else { at };
            Some(insert_column(columns, key, at, canvas.as_deref()))
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
/// 画布画的是当前那块画布上的可见列（含接在最右的固定列），而状态里存着所有项目、
/// 所有画布的列；两边的下标只在"只有一块画布、当前项目就是全部"时才碰巧一致。
/// 转换一律认 id 与 key，不认下标。
///
/// 另起一列时认它左边那一列：插在它后面、落在它那块画布上；左边没有（落在最左）才认
/// 右边那一列、插在它前面。固定列不当锚——它在每块画布上都有，在排布里却永远是最后一列，
/// 认它就会插到最后一块画布的末尾去。两边都没有（画布上只有固定列）就接到固定列前面，
/// 不指定画布，交给 [`assign_canvases`] 去分。
pub fn resolve_spot(columns: &[ColumnLayout], visible: &[ColumnLayout], spot: DropSpot) -> DropSpot {
    let tail = DropSpot::column(columns.len());
    match spot {
        DropSpot::Column { at, .. } => {
            let index_of = |c: Option<&ColumnLayout>| {
                c.filter(|c| !c.pinned).and_then(|c| columns.iter().position(|x| x.id == c.id))
            };
            if let Some(left) = at.checked_sub(1).and_then(|i| index_of(visible.get(i))) {
                return DropSpot::Column { at: left + 1, canvas: columns[left].canvas.clone() };
            }
            if let Some(right) = index_of(visible.get(at)) {
                return DropSpot::Column { at: right, canvas: columns[right].canvas.clone() };
            }
            DropSpot::column(pin_edge(columns))
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
        assert_eq!(serde_json::to_string(&DropSpot::column(3)).unwrap(), r#"{"kind":"column","at":3}"#);
        c.pinned = false;
        c.canvas = Some("v1".into());
        assert!(serde_json::to_string(&c).unwrap().ends_with(r#""canvas":"v1"}"#));
        let back: ColumnLayout = serde_json::from_str(r#"{"id":"c","basis":null,"panes":[]}"#).unwrap();
        assert_eq!(back.canvas, None);
    }

    #[test]
    fn canvas_ids_are_unique_and_web_shaped() {
        let a = new_canvas_id();
        assert_ne!(a, new_canvas_id());
        assert!(a.starts_with('v'));
    }

    #[test]
    fn unpin_forgets_canvas() {
        let mut cols = pin_pane(&[column(["a"], None), column(["b"], None)], "a");
        cols[1].canvas = Some("v1".into());
        let un = unpin_all(&cols);
        assert_eq!(un[1].canvas, None);
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
