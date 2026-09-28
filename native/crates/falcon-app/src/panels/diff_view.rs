//! 画布上的差异窗口（web 的 `components/GitDiffView.tsx`）：split / unified 两种视图，
//! 内容来自工作区的 `diff_tab`（就地替换的预览语义，全工作区只有一扇）。
//!
//! 打开 / 切换文件 / 手动刷新时拉一次，**不轮询**——正看着的 diff 在眼皮底下变动只会让人
//! 跟丢行号。
//!
//! 行一律等高交给 `uniform_list`（见 parse.rs 顶部）：web 为大 diff 手写的行窗口在这里是
//! 白送的。分栏是左右两个 list：纵向永远同步，横向各自有内容宽度、跟着滚轮一起走（web 的
//! 双向同步滚动），行号栏在横向滚动时钉在左边（web 的 `sticky left-0`）。

mod parse;

use std::rc::Rc;

use falcon_client::GitFileRef;
use falcon_proto::GitFileDiff;
use gpui_kit::assets::IconName;
use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Context, Entity, FontWeight, Hsla, IntoElement, ListHorizontalSizingBehavior, ListSizingBehavior,
    Pixels, Render, ScrollDelta, ScrollWheelEvent, SharedString, Task, UniformListScrollHandle, Window, div, point, px,
    uniform_list,
};
use rust_i18n::t;

use self::parse::{DiffRow, Line, SideLine, Sign, SplitRow};
use super::git::common::{hint, porcelain_status, reason_key, status_text, tool_button};
use crate::prefs::Prefs;
use crate::theme::Ui;
use crate::ui::icon;
use crate::workspace::{DiffTabTarget, Workspace};
use crate::zoom::zpx;

/// 正文行高（web 的 leading-5.5）
const ROW_H: f32 = 22.;
/// 行号栏宽（web 的 w-10）
const GUTTER_W: f32 = 40.;

const VIEW_KEY: &str = "falcon.diffView";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Split,
    Unified,
}

fn view_mode(cx: &App) -> Mode {
    match Prefs::global(cx).get(VIEW_KEY) {
        Some("unified") => Mode::Unified,
        _ => Mode::Split,
    }
}

/// 决定"要不要重新拉"的那几项（web 的 effect 依赖数组）
#[derive(Clone, PartialEq, Eq)]
struct DiffKey {
    project_id: String,
    repo: Option<String>,
    path: String,
    orig_path: Option<String>,
    index: String,
    work: String,
    commit: Option<String>,
}

impl DiffKey {
    fn of(t: &DiffTabTarget) -> Self {
        Self {
            project_id: t.project_id.clone(),
            repo: t.repo.clone(),
            path: t.file.path.clone(),
            orig_path: t.file.orig_path.clone(),
            index: t.file.index.clone(),
            work: t.file.work.clone(),
            commit: t.commit.as_ref().map(|c| c.sha.clone()),
        }
    }
}

/// 解析好的一份 diff：两种视图各一份行，外加各自最宽的那一行（给 uniform_list 量内容宽度）
struct Parsed {
    unified: Rc<Vec<DiffRow>>,
    split: Rc<Vec<SplitRow>>,
    widest_unified: usize,
    widest_left: usize,
    widest_right: usize,
}

impl Parsed {
    fn new(text: &str) -> Self {
        let unified = parse::drop_single_file_header(parse::parse_diff(text));
        let split = parse::to_split_rows(&unified);
        let cols_u = |r: &DiffRow| match r {
            DiffRow::Line(l) => parse::display_cols(&l.text) + 12,
            DiffRow::File(s) | DiffRow::Hunk(s) | DiffRow::Note(s) => parse::display_cols(s),
        };
        let widest = |n: usize, f: &dyn Fn(usize) -> usize| (0..n).max_by_key(|&i| f(i)).unwrap_or(0);
        let side_cols = |r: &SplitRow, left: bool| match r {
            SplitRow::Pair { left: l, right: rr } => {
                let s = if left { l } else { rr };
                s.as_ref().map(|s| parse::display_cols(&s.text) + 6).unwrap_or(0)
            }
            SplitRow::File(s) | SplitRow::Hunk(s) | SplitRow::Note(s) => parse::display_cols(s),
        };
        Self {
            widest_unified: widest(unified.len(), &|i| cols_u(&unified[i])),
            widest_left: widest(split.len(), &|i| side_cols(&split[i], true)),
            widest_right: widest(split.len(), &|i| side_cols(&split[i], false)),
            unified: Rc::new(unified),
            split: Rc::new(split),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Left,
    Right,
}

pub struct DiffView {
    ws: Entity<Workspace>,
    key: Option<DiffKey>,
    result: Option<GitFileDiff>,
    parsed: Option<Parsed>,
    error: Option<String>,
    busy: bool,
    unified_scroll: UniformListScrollHandle,
    left_scroll: UniformListScrollHandle,
    right_scroll: UniformListScrollHandle,
    /// 两栏上一次对齐到的纵向位置：谁偏离了它，谁就是这次动的那一栏
    synced_y: Pixels,
    /// 最近一次滚轮落在哪一栏（两栏同时变了时以它为准）
    wheel_side: Side,
    /// 指针在差异区里（滚动条这时常显，见 [`vscrollbar`]）
    hovered: bool,
    _load: Task<()>,
}

impl DiffView {
    pub fn new(ws: Entity<Workspace>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&ws, |this, _, cx| {
            this.sync_target(false, cx);
            cx.notify();
        })
        .detach();
        let mut this = Self {
            ws,
            key: None,
            result: None,
            parsed: None,
            error: None,
            busy: false,
            unified_scroll: UniformListScrollHandle::new(),
            left_scroll: UniformListScrollHandle::new(),
            right_scroll: UniformListScrollHandle::new(),
            synced_y: px(0.),
            wheel_side: Side::Right,
            hovered: false,
            _load: Task::ready(()),
        };
        this.sync_target(false, cx);
        this
    }

    /// 目标变了（或手动刷新）就重新拉。换了文件时清掉旧内容、滚回顶
    fn sync_target(&mut self, force: bool, cx: &mut Context<Self>) {
        let target = self.ws.read(cx).state.diff_tab.clone();
        let key = target.as_ref().map(DiffKey::of);
        if !force && key == self.key {
            return;
        }
        if key != self.key {
            self.result = None;
            self.parsed = None;
            self.error = None;
            for h in [&self.unified_scroll, &self.left_scroll, &self.right_scroll] {
                h.0.borrow().base_handle.set_offset(point(px(0.), px(0.)));
            }
            self.synced_y = px(0.);
        }
        self.key = key;
        let Some(target) = target else {
            self._load = Task::ready(());
            return;
        };
        let client = self.ws.read(cx).client.clone();
        let file = GitFileRef {
            path: target.file.path.clone(),
            orig_path: target.file.orig_path.clone(),
            // 靠 index == "?" 判断走不走 --no-index 伪 diff（「修改」面板把展示字母塞在 index 列）
            untracked: target.file.index == "?",
        };
        let repo = target.repo.clone();
        let pid = target.project_id.clone();
        let commit = target.commit.as_ref().map(|c| c.sha.clone());
        self._load = cx.spawn(async move |this, cx| {
            // 带 commit = History 里点进来的，看的是那一次改动；否则是工作区现状
            let result = match commit {
                Some(sha) => client.git_commit_diff(&pid, &sha, &file, repo.as_deref()).await,
                None => client.git_file_diff(&pid, &file, repo.as_deref()).await,
            };
            this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(next) => {
                        this.parsed = (next.available && !next.diff.trim().is_empty()).then(|| Parsed::new(&next.diff));
                        this.result = Some(next);
                        this.error = None;
                    }
                    Err(err) => {
                        let msg = err.to_string();
                        this.ws.update(cx, |w, cx| w.handle_error(&err, cx));
                        this.error = Some(msg);
                    }
                }
                cx.notify();
            })
            .ok();
        });
    }

    fn set_hovered(&mut self, hovered: &bool, _: &mut Window, cx: &mut Context<Self>) {
        if self.hovered != *hovered {
            self.hovered = *hovered;
            cx.notify();
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.busy = true;
        self.sync_target(true, cx);
        cx.notify();
    }

    /// 两栏纵向对齐。在 render 里做：滚轮与拖滚动条都会通知本视图重画，而 list 是在本帧
    /// 的 prepaint 里才读位置，这里改了当帧就生效
    fn sync_split_scroll(&mut self) {
        let l = self.left_scroll.0.borrow().base_handle.offset();
        let r = self.right_scroll.0.borrow().base_handle.offset();
        let master = match (l.y != self.synced_y, r.y != self.synced_y) {
            (true, false) => Side::Left,
            (false, true) => Side::Right,
            (true, true) => self.wheel_side,
            (false, false) => return,
        };
        let (from, to, y) = match master {
            Side::Left => (l, &self.right_scroll, l.y),
            Side::Right => (r, &self.left_scroll, r.y),
        };
        let other = to.0.borrow().base_handle.offset();
        to.0.borrow().base_handle.set_offset(point(other.x, from.y));
        self.synced_y = y;
    }
}

impl Render for DiffView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        let Some(target) = self.ws.read(cx).state.diff_tab.clone() else {
            return div().size_full().into_any_element();
        };
        let mode = view_mode(cx);
        let file = &target.file;
        let path = match &file.orig_path {
            Some(o) => format!("{o} → {}", file.path),
            None => file.path.clone(),
        };

        // ---- 工具条 ----
        let path_tip: SharedString = path.clone().into();
        let mut bar = div()
            .id("diff-bar")
            .h(zpx(34.))
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .pl_3()
            .pr(zpx(6.))
            .border_b_1()
            .border_color(ui.border)
            .child(icon(IconName::FileDiff).size(zpx(14.)).text_color(ui.muted_foreground))
            .child(
                div()
                    .id("diff-path")
                    .min_w_0()
                    .flex_shrink(1.)
                    .truncate()
                    .text_xs()
                    .tooltip(move |window, cx| Tooltip::new(path_tip.clone()).build(window, cx))
                    .child(path),
            )
            .child(
                div()
                    .flex_none()
                    .rounded(zpx(4.))
                    .bg(ui.muted)
                    .px(zpx(6.))
                    .py(zpx(1.))
                    .text_size(zpx(11.))
                    .text_color(ui.muted_foreground)
                    .child(status_text(&porcelain_status(&file.index, &file.work))),
            );
        if let Some(commit) = &target.commit {
            // 看的是哪一次提交必须写在标题上：同一个文件在不同提交里的 diff 长得可以很像
            let tip: SharedString = commit.subject.clone().into();
            bar = bar.child(
                div()
                    .id("diff-commit")
                    .min_w_0()
                    .flex_shrink(1.)
                    .truncate()
                    .text_size(zpx(11.))
                    .text_color(ui.muted_foreground)
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .child(format!("{} {}", commit.short, commit.subject)),
            );
        }
        if self.result.as_ref().is_some_and(|r| r.truncated) {
            bar = bar.child(
                div()
                    .flex_none()
                    .text_size(zpx(11.))
                    .text_color(ui.warning)
                    .child(t!("git.diffTruncated").to_string()),
            );
        }
        let busy = self.busy;
        bar = bar
            .child(div().flex_1())
            .child(
                tool_button(
                    "diff-split",
                    IconName::Columns2,
                    t!("git.viewSplit").to_string(),
                    mode == Mode::Split,
                    cx,
                )
                .on_click(|_, _, cx| set_mode(Mode::Split, cx)),
            )
            .child(
                tool_button(
                    "diff-unified",
                    IconName::Rows3,
                    t!("git.viewUnified").to_string(),
                    mode == Mode::Unified,
                    cx,
                )
                .on_click(|_, _, cx| set_mode(Mode::Unified, cx)),
            )
            .child(
                tool_button(
                    "diff-refresh",
                    IconName::RefreshCw,
                    t!("git.refresh").to_string(),
                    false,
                    cx,
                )
                .loading(busy)
                .loading_icon(IconName::RefreshCw)
                .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            );

        // ---- 内容 ----
        let body: AnyElement = if let Some(err) = &self.error {
            hint(t!("git.diffFailed").to_string(), Some(err.clone()), cx).into_any_element()
        } else if let Some(result) = &self.result {
            if !result.available {
                hint(t!(reason_key(result.reason)).to_string(), result.detail.clone(), cx).into_any_element()
            } else if self.parsed.is_none() {
                hint(t!("git.diffEmpty").to_string(), None, cx).into_any_element()
            } else if mode == Mode::Split {
                self.sync_split_scroll();
                self.split_view(cx)
            } else {
                self.unified_view(cx)
            }
        } else {
            hint(t!("git.diffLoading").to_string(), None, cx).into_any_element()
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(ui.background)
            .text_color(ui.foreground)
            .child(bar)
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }
}

fn set_mode(mode: Mode, cx: &mut App) {
    let v = if mode == Mode::Split { "split" } else { "unified" };
    cx.global_mut::<Prefs>().set(VIEW_KEY, v.to_string());
    cx.refresh_windows();
}

/// 纵向滚动条。GPUI 在 macOS（系统设成"自动"时）默认滚动时才出现、停 2 秒淡出，它的 Hover 模式
/// 也只认指针落在滚动条那一窄条上——停在内容上根本看不见它，也就无从去抓。web 的约定是指针进了
/// 窗口就浮出来（styles.css 的悬停胶囊），所以指针在差异区里时强制 Always，离开后退回系统偏好
/// （由组件库主题同步），照常淡出。
fn vscrollbar(handle: &UniformListScrollHandle, hovered: bool) -> impl IntoElement {
    let bar = Scrollbar::vertical(handle).viewport_from_layout();
    div().absolute().inset_0().child(if hovered { bar.mode(ScrollbarMode::Always) } else { bar })
}

/// 这一下横向滚轮 list 接住了没有。list 的监听先跑（子元素后注册、冒泡时先执行），偏移已经加上
/// 了这一下的 delta、还没夹回范围：落点仍在 [-max, 0] 里就是接住了，调用方据此 stop_propagation，
/// 否则画布会跟着一起横滚（web 的 innerTakesWheel：窗口内部还能沿手势方向滚时让给内部）。
/// 一下滚过头的那次算没接住，最多与画布重叠一次，换来不用去猜 Lines 滚轮在 list 里的行高
fn list_took_x(handle: &UniformListScrollHandle, e: &ScrollWheelEvent) -> bool {
    let (dx, dy) = match e.delta {
        ScrollDelta::Pixels(p) => (f32::from(p.x), f32::from(p.y)),
        ScrollDelta::Lines(l) => (l.x, l.y),
    };
    if dx == 0. || dx.abs() <= dy.abs() {
        return false;
    }
    let base = &handle.0.borrow().base_handle;
    let (x, max) = (base.offset().x, base.max_offset().x);
    max > px(0.) && x <= px(0.) && x >= -max
}

// ---------------- 行 ----------------

/// 行底色 / 行号栏的字色（web 的 bg-success/10 等）
fn tones(sign: Sign, ui: &Ui) -> (Option<Hsla>, Hsla) {
    match sign {
        Sign::Add => (Some(ui.success.opacity(0.1)), ui.success),
        Sign::Del => (Some(ui.destructive.opacity(0.1)), ui.destructive),
        Sign::Ctx => (None, ui.muted_foreground.opacity(0.6)),
    }
}

/// 文件头 / hunk 头 / 说明行。`shift` 是横向滚动量：分栏里这几种行的文字钉在左边（web 的
/// `sticky left-3`），只让底色跟着滚
fn banner(kind: &DiffRow, shift: Pixels, ui: &Ui) -> AnyElement {
    let base = div()
        .h(zpx(ROW_H))
        .w_full()
        .flex()
        .items_center()
        .whitespace_nowrap()
        .pl(zpx(12.) + shift)
        .pr_3();
    match kind {
        DiffRow::File(label) => base
            .bg(ui.app)
            .font_weight(FontWeight::MEDIUM)
            .text_color(ui.foreground)
            .child(label.clone())
            .into_any_element(),
        DiffRow::Hunk(text) => base
            .bg(ui.accent.opacity(0.4))
            .text_color(ui.muted_foreground)
            .child(text.clone())
            .into_any_element(),
        DiffRow::Note(text) => base
            .italic()
            .text_color(ui.muted_foreground)
            .child(text.clone())
            .into_any_element(),
        DiffRow::Line(_) => div().into_any_element(),
    }
}

fn unified_row(row: &DiffRow, ui: &Ui) -> AnyElement {
    let DiffRow::Line(Line { sign, old, new, text }) = row else {
        return banner(row, px(0.), ui);
    };
    let (bg, gutter_fg) = tones(*sign, ui);
    let gutter = |n: Option<u32>| {
        div()
            .w(zpx(GUTTER_W))
            .flex_none()
            .pr_2()
            .flex()
            .justify_end()
            .text_color(gutter_fg)
            .child(n.map(|n| n.to_string()).unwrap_or_default())
    };
    let sign_text = match sign {
        Sign::Add => "+",
        Sign::Del => "-",
        Sign::Ctx => "",
    };
    div()
        .h(zpx(ROW_H))
        .w_full()
        .flex()
        .items_center()
        .whitespace_nowrap()
        .when_some(bg, |d, bg| d.bg(bg))
        .child(gutter(*old))
        .child(gutter(*new))
        .child(
            div()
                .flex_none()
                .flex()
                .pl_1()
                .pr_4()
                .child(div().w(zpx(12.)).flex_none().text_color(gutter_fg).child(sign_text))
                .child(text.clone()),
        )
        .into_any_element()
}

/// 分栏的一侧。行号栏钉在左边：绝对定位到横向滚动量处，底色不透明（先铺面板底再叠色调），
/// 盖住滑到它底下的代码
fn side_row(row: &SplitRow, side: Side, shift: Pixels, ui: &Ui) -> AnyElement {
    let line: &Option<SideLine> = match row {
        SplitRow::Pair { left, right } => {
            if side == Side::Left {
                left
            } else {
                right
            }
        }
        SplitRow::File(s) => return banner(&DiffRow::File(s.clone()), shift, ui),
        SplitRow::Hunk(s) => return banner(&DiffRow::Hunk(s.clone()), shift, ui),
        SplitRow::Note(s) => return banner(&DiffRow::Note(s.clone()), shift, ui),
    };
    let (bg, gutter_fg) = match line {
        Some(l) => tones(l.sign, ui),
        None => (Some(ui.muted.opacity(0.4)), ui.muted_foreground),
    };
    let text = line.as_ref().map(|l| l.text.clone()).unwrap_or_default();
    div()
        .relative()
        .h(zpx(ROW_H))
        .w_full()
        .flex()
        .items_center()
        .whitespace_nowrap()
        .when_some(bg, |d, bg| d.bg(bg))
        .child(div().flex_none().pl(zpx(GUTTER_W + 6.)).pr_3().child(text))
        .child(
            div()
                .absolute()
                .top_0()
                .left(shift)
                .w(zpx(GUTTER_W))
                .h_full()
                .bg(ui.background)
                .child(
                    div()
                        .size_full()
                        .pr_2()
                        .flex()
                        .items_center()
                        .justify_end()
                        .when(line.as_ref().is_some_and(|l| l.sign != Sign::Ctx), |d| {
                            d.when_some(bg, |d, bg| d.bg(bg))
                        })
                        .text_color(gutter_fg)
                        .child(line.as_ref().map(|l| l.no.to_string()).unwrap_or_default()),
                ),
        )
        .into_any_element()
}

impl DiffView {
    fn unified_view(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(parsed) = &self.parsed else {
            return div().into_any_element();
        };
        let rows = parsed.unified.clone();
        let widest = parsed.widest_unified;
        let list = uniform_list("diff-unified-list", rows.len(), move |range, _, cx| {
            let ui = Ui::global(cx).clone();
            range.map(|i| unified_row(&rows[i], &ui)).collect::<Vec<_>>()
        })
        .with_sizing_behavior(ListSizingBehavior::Auto)
        .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
        .with_width_from_item(Some(widest))
        .track_scroll(&self.unified_scroll)
        .size_full()
        .text_xs();
        let h = self.unified_scroll.clone();
        div()
            .id("diff-unified")
            .relative()
            .size_full()
            .on_hover(cx.listener(Self::set_hovered))
            .on_scroll_wheel(move |e: &ScrollWheelEvent, _, cx| {
                if list_took_x(&h, e) {
                    cx.stop_propagation();
                }
            })
            .child(list)
            .child(vscrollbar(&self.unified_scroll, self.hovered))
            .into_any_element()
    }

    fn split_view(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let ui = Ui::global(cx).clone();
        let Some(parsed) = &self.parsed else {
            return div().into_any_element();
        };
        let pane = |side: Side,
                    handle: &UniformListScrollHandle,
                    other: &UniformListScrollHandle,
                    widest: usize,
                    cx: &mut Context<Self>| {
            let rows = parsed.split.clone();
            let h = handle.clone();
            let list = uniform_list(
                if side == Side::Left {
                    "diff-split-left"
                } else {
                    "diff-split-right"
                },
                rows.len(),
                move |range, _, cx| {
                    let ui = Ui::global(cx).clone();
                    // 行号栏与横幅文字钉住的位置 = 当前横向滚动量（list 在本帧 prepaint 里已经夹好了）
                    let shift = -h.0.borrow().base_handle.offset().x;
                    range.map(|i| side_row(&rows[i], side, shift, &ui)).collect::<Vec<_>>()
                },
            )
            .with_sizing_behavior(ListSizingBehavior::Auto)
            .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
            .with_width_from_item(Some(widest))
            .track_scroll(handle)
            .size_full()
            .text_xs();
            let (me, other) = (handle.clone(), other.clone());
            div()
                .id(if side == Side::Left { "diff-left" } else { "diff-right" })
                .relative()
                .flex_1()
                .min_w_0()
                .h_full()
                // 横向跟着滚：list 自己的滚轮处理先跑（子元素后注册、冒泡时先执行），这里把
                // 它刚得到的位置抄给另一栏；纵向在 render 里再对齐一次（拖滚动条也要跟）
                .on_scroll_wheel(cx.listener(move |this, e: &ScrollWheelEvent, _, cx| {
                    this.wheel_side = side;
                    let p = me.0.borrow().base_handle.offset();
                    other.0.borrow().base_handle.set_offset(p);
                    if list_took_x(&me, e) {
                        cx.stop_propagation();
                    }
                    cx.notify();
                }))
                .child(list)
        };
        let left = pane(
            Side::Left,
            &self.left_scroll,
            &self.right_scroll,
            parsed.widest_left,
            cx,
        );
        let right = pane(
            Side::Right,
            &self.right_scroll,
            &self.left_scroll,
            parsed.widest_right,
            cx,
        )
        .border_l_1()
        .border_color(ui.border)
        .child(vscrollbar(&self.right_scroll, self.hovered));
        div()
            .id("diff-split")
            .size_full()
            .flex()
            .on_hover(cx.listener(Self::set_hovered))
            .child(left)
            .child(right)
            .into_any_element()
    }
}
