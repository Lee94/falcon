//! 主区工作画布（web 的 `components/WorkCanvas.tsx`，ADR 0012）：窗口排成从左到右的列，每列从上
//! 到下若干扇，列宽与窗口高度都能拖，抓标题栏能把窗口拖到别的列或另起一列。
//!
//! 与 web 的差别只有一处：web 必须"每扇窗口绝对定位、DOM 顺序恒定"，因为 xterm 的画布换过父
//! 节点渲染尺寸就毁了；GPUI 的终端元素每帧按快照重画，挂在树的哪个位置都一样，这条约束不存在。
//! 这里仍按 `layout_frames` 算出的坐标绝对定位，只是为了与 web 共用同一套几何纯函数（列宽公式、
//! 窗口平分、拖拽落点），两边的排布行为一致。
//!
//! 不在场的窗口（别的项目的）不画，但它的终端视图仍活在工作区里：解析照跑，只省渲染。

use falcon_core::layout::{
    self, CANVAS_GAP_PX, ColumnLayout, ColumnRect, DropSpot, PaneRect, Viewport,
};
use falcon_core::pane_key::{PaneItem, parse_pane_key};
use gpui_kit::assets::IconName;
use gpui_kit::component::menu::ContextMenuExt;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::ElementExt;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Bounds, Context, CursorStyle, Div, Entity, FontWeight, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, ScrollHandle,
    SharedString, Window, div, px,
};
use rust_i18n::t;

use crate::menus::{MenuItemSpec, to_popup};
use crate::panels::diff_view::DiffView;
use crate::panels::file_view::FileView;
use crate::theme::{Ui, radius};
use crate::ui::{Mark, host_bar, icon, status_mark};
use crate::workspace::{ActiveView, Workspace, is_pending_id};
use crate::zoom::zpx;

/// 拖动多少像素才算"在拖窗口"而不是"点了下标题栏"
const DRAG_SLOP: f32 = 4.;
/// 把手压在缝上，够点得中又不挡住窗口
const HANDLE: f32 = 8.;
const HEADER_H: f32 = 28.;

enum Resize {
    Column { id: String, start_x: Pixels, start_width: f64 },
    Pane { key: String, start_y: Pixels, start_height: f64, max: f64 },
}

struct Drag {
    key: String,
    label: String,
    start: Point<Pixels>,
    pos: Point<Pixels>,
    moved: bool,
}

pub struct Canvas {
    ws: Entity<Workspace>,
    file_view: Option<Entity<FileView>>,
    diff_view: Option<Entity<DiffView>>,
    /// 画布内容盒（窗口坐标）；几何都从它算
    bounds: Bounds<Pixels>,
    scroll: ScrollHandle,
    drag: Option<Drag>,
    resize: Option<Resize>,
    /// 上一帧的活动窗口：换了就把它所在的列滚进视口
    revealed: Option<String>,
}

impl Canvas {
    pub fn new(ws: Entity<Workspace>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&ws, |this, _, cx| {
            this.sync_views(cx);
            cx.notify();
        })
        .detach();
        Self {
            ws,
            file_view: None,
            diff_view: None,
            bounds: Bounds::default(),
            scroll: ScrollHandle::new(),
            drag: None,
            resize: None,
            revealed: None,
        }
    }

    /// 文件 / 差异窗口的视图跟着工作区的 file_tab / diff_tab 建与换
    fn sync_views(&mut self, cx: &mut Context<Self>) {
        let (file_tab, has_diff) = {
            let ws = self.ws.read(cx);
            (ws.state.file_tab.clone(), ws.state.diff_tab.is_some())
        };
        match (&file_tab, &self.file_view) {
            (Some(tab), Some(view)) => {
                let (pid, path) = (tab.project_id.clone(), tab.path.clone());
                view.update(cx, |v, cx| {
                    if v.project_id != pid || v.path != path {
                        v.set_target(pid, path, cx);
                    }
                });
            }
            (None, _) => self.file_view = None,
            _ => {}
        }
        if !has_diff {
            self.diff_view = None;
        }
    }

    fn ensure_views(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (file_tab, has_diff) = {
            let ws = self.ws.read(cx);
            (ws.state.file_tab.clone(), ws.state.diff_tab.is_some())
        };
        if let Some(tab) = file_tab
            && self.file_view.is_none()
        {
            let ws = self.ws.clone();
            self.file_view = Some(cx.new(|cx| FileView::new(ws, tab.project_id, tab.path, window, cx)));
        }
        if has_diff && self.diff_view.is_none() {
            let ws = self.ws.clone();
            self.diff_view = Some(cx.new(|cx| DiffView::new(ws, window, cx)));
        }
    }

    /// 把输入焦点交给某扇窗口（终端才有键盘焦点）
    pub fn focus_pane(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(PaneItem::Terminal { id, .. }) = parse_pane_key(key)
            && let Some(view) = self.ws.read(cx).terminals.get(&id).cloned()
        {
            let handle = view.read(cx).focus_handle().clone();
            window.focus(&handle, cx);
        }
    }

    /// 画布此刻要画的列：当前项目的；最大化时只留活动那一扇（它不在场就当没最大化）
    fn columns(&self, cx: &App) -> Vec<ColumnLayout> {
        let ws = self.ws.read(cx);
        let visible = ws.visible_columns();
        if !ws.state.term_zoomed {
            return visible;
        }
        let Some(active) = ws.active_key() else {
            return visible;
        };
        let Some(at) = layout::find_pane(&visible, &active) else {
            return visible;
        };
        let col = &visible[at.col];
        vec![ColumnLayout {
            id: col.id.clone(),
            basis: None,
            panes: vec![col.panes[at.index].clone()],
            pinned: col.pinned,
        }]
    }

    /// 指针（窗口坐标）→ 画布内容坐标（加上横向滚动）
    fn to_content(&self, p: Point<Pixels>) -> (f64, f64) {
        let offset = self.scroll.offset();
        (
            f32::from(p.x - self.bounds.origin.x - offset.x) as f64,
            f32::from(p.y - self.bounds.origin.y - offset.y) as f64,
        )
    }

    fn rects(&self, columns: &[ColumnLayout]) -> Vec<ColumnRect> {
        let frames = layout::layout_frames(columns, self.viewport(), CANVAS_GAP_PX);
        frames
            .columns
            .iter()
            .map(|c| ColumnRect {
                left: c.x,
                right: c.x + c.width,
                panes: c.panes.iter().map(|p| PaneRect { top: p.y, bottom: p.y + p.height }).collect(),
            })
            .collect()
    }

    fn viewport(&self) -> Viewport {
        Viewport {
            width: f32::from(self.bounds.size.width) as f64,
            height: f32::from(self.bounds.size.height) as f64,
        }
    }

    fn current_spot(&self, columns: &[ColumnLayout], pos: Point<Pixels>) -> DropSpot {
        let (x, y) = self.to_content(pos);
        let spot = layout::drop_spot(layout::Point { x, y }, &self.rects(columns), layout::COLUMN_EDGE_PX);
        // 指示线也要认固定列，不然松手之后窗口会跳到指示线左边去
        layout::clamp_spot(columns, spot)
    }

    // ---------------- 鼠标：拖窗口 / 拖缝 ----------------

    fn on_mouse_move(&mut self, e: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(resize) = &self.resize {
            match resize {
                Resize::Column { id, start_x, start_width } => {
                    let w = layout::clamp_column_width(start_width + f32::from(e.position.x - *start_x) as f64, f64::INFINITY);
                    let id = id.clone();
                    self.ws.update(cx, |w_, cx| w_.set_column_width(&id, Some(w), cx));
                }
                Resize::Pane { key, start_y, start_height, max } => {
                    let h = layout::clamp_pane_height(start_height + f32::from(e.position.y - *start_y) as f64, *max);
                    let key = key.clone();
                    self.ws.update(cx, |w_, cx| w_.set_pane_height(&key, Some(h), cx));
                }
            }
            return;
        }
        if let Some(drag) = &mut self.drag {
            if !drag.moved {
                let d = (e.position.x - drag.start.x).abs() + (e.position.y - drag.start.y).abs();
                if f32::from(d) < DRAG_SLOP {
                    return;
                }
                drag.moved = true;
            }
            drag.pos = e.position;
            cx.notify();
        }
    }

    fn on_mouse_up(&mut self, _e: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.resize.take().is_some() {
            // 拖的途中不落盘，松手才写（web 同样的节制）
            self.ws.read(cx).persist();
            return;
        }
        if let Some(drag) = self.drag.take() {
            if drag.moved {
                let columns = self.columns(cx);
                let spot = self.current_spot(&columns, drag.pos);
                self.ws.update(cx, |w, cx| w.move_pane(&drag.key, spot, cx));
            }
            cx.notify();
        }
    }
}

impl Render for Canvas {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_views(window, cx);
        let ui = Ui::global(cx).clone();
        let columns = self.columns(cx);
        let frames = layout::layout_frames(&columns, self.viewport(), CANVAS_GAP_PX);
        let (active_key, show_canvas, pane_count) = {
            let ws = self.ws.read(cx);
            let n: usize = columns.iter().map(|c| c.panes.len()).sum();
            (ws.active_key(), !matches!(ws.state.active, ActiveView::Overview), n)
        };
        // 画布上不止一扇窗口时，才需要给活动的那扇标题栏着色
        let mark_active = pane_count > 1;
        // 活动窗口换了：把它所在的列滚进视口（web termCanvas.ts 的 revealScrollLeft）。画布量出
        // 自己的宽度之前不算：开窗第一帧视口是 0，算出来的滚动量是错的，记下"已滚过"就再也不试了——
        // 重启后活动窗口停在视口外，看上去就是没有选中的那扇（web 的 effect 也依赖 viewport.width）
        if active_key != self.revealed && self.bounds.size.width > px(0.) {
            self.revealed = active_key.clone();
            if let Some(key) = &active_key
                && let Some(col) = frames.columns.iter().find(|c| c.panes.iter().any(|p| &p.key == key))
            {
                let scroll_left = -f32::from(self.scroll.offset().x) as f64;
                if let Some(next) = falcon_core::term_canvas::reveal_scroll_left(falcon_core::term_canvas::Reveal {
                    scroll_left,
                    viewport: self.viewport().width,
                    left: col.x,
                    width: col.width,
                }) {
                    self.scroll.set_offset(gpui_kit::point(px(-(next as f32)), px(0.)));
                }
            }
        }
        let shown_keys: Vec<String> = layout::pane_keys(&columns);
        let content_w = frames.width.max(self.viewport().width) as f32;

        let mut content = div().relative().h_full().w(px(content_w));

        // 列的底：岛的圆角与面板底色在这一层，窗口浮在上面
        for col in &frames.columns {
            content = content.child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(col.x as f32))
                    .w(px(col.width as f32))
                    .bg(ui.background)
                    .rounded(radius::LG),
            );
        }

        for (ci, col) in frames.columns.iter().enumerate() {
            let n = col.panes.len();
            for (pi, pane) in col.panes.iter().enumerate() {
                let round = match (pi == 0, pi + 1 == n) {
                    (true, true) => Round::All,
                    (true, false) => Round::Top,
                    (false, true) => Round::Bottom,
                    (false, false) => Round::None,
                };
                let pinned = columns[ci].pinned;
                let el = self.pane(
                    &pane.key,
                    Bounds::new(
                        Point::new(px(col.x as f32), px(pane.y as f32)),
                        gpui_kit::size(px(col.width as f32), px(pane.height as f32)),
                    ),
                    round,
                    active_key.as_deref() == Some(pane.key.as_str()),
                    mark_active,
                    pinned,
                    &shown_keys,
                    cx,
                );
                content = content.child(el);
            }
        }

        // 缝上的把手：列之间一条竖的，列内每两扇窗口之间一条横的
        let height = self.viewport().height;
        for (ci, col) in frames.columns.iter().enumerate() {
            if ci + 1 < frames.columns.len() {
                let id = col.id.clone();
                let id2 = col.id.clone();
                let width = col.width;
                let ws = self.ws.clone();
                content = content.child(
                    div()
                        .id(SharedString::from(format!("col-split-{}", col.id)))
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left(px((col.x + col.width) as f32 - HANDLE / 2.))
                        .w(px(CANVAS_GAP_PX as f32 + HANDLE))
                        .cursor(CursorStyle::ResizeLeftRight)
                        .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                            if e.click_count >= 2 {
                                // 双击还原成自适应
                                this.ws.update(cx, |w, cx| {
                                    w.set_column_width(&id2, None, cx);
                                    w.persist();
                                });
                                return;
                            }
                            this.resize = Some(Resize::Column { id: id.clone(), start_x: e.position.x, start_width: width });
                            cx.stop_propagation();
                        }))
                        .tooltip(|window, cx| Tooltip::new(t!("pane.resizeColumn").to_string()).build(window, cx))
                        .on_click(move |_, _, _| {
                            let _ = &ws;
                        }),
                );
            }
            let np = col.panes.len();
            for (i, pane) in col.panes.iter().enumerate().take(np.saturating_sub(1)) {
                let key = pane.key.clone();
                let key2 = pane.key.clone();
                let h = pane.height;
                let max = layout::pane_max_height(height, pane.y, (np - i - 1) as f64);
                content = content.child(
                    div()
                        .id(SharedString::from(format!("pane-split-{}", pane.key)))
                        .absolute()
                        .left(px(col.x as f32))
                        .w(px(col.width as f32))
                        .top(px((pane.y + pane.height) as f32 - HANDLE / 2.))
                        .h(px(HANDLE))
                        .cursor(CursorStyle::ResizeUpDown)
                        .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                            if e.click_count >= 2 {
                                this.ws.update(cx, |w, cx| {
                                    w.set_pane_height(&key2, None, cx);
                                    w.persist();
                                });
                                return;
                            }
                            this.resize = Some(Resize::Pane { key: key.clone(), start_y: e.position.y, start_height: h, max });
                            cx.stop_propagation();
                        })),
                );
            }
        }

        // 拖拽中的落点指示线与跟手的标签
        if let Some(drag) = self.drag.as_ref().filter(|d| d.moved) {
            let spot = self.current_spot(&columns, drag.pos);
            if let Some(line) = drop_indicator(spot, &self.rects(&columns), &ui) {
                content = content.child(line);
            }
        }

        let mut root = div()
            .id("work-canvas")
            .size_full()
            .relative()
            .overflow_x_scroll()
            // 只接横向手势（web 的 WheelAxisLock）。不加的话 GPUI 会把只开了横向滚动的容器上的
            // 纵向滚轮换算成横滚，而它的滚动监听从不 stop_propagation——在差异 / 文件 / 没有回滚
            // 的终端里纵向滚一下，整个画布就跟着横漂
            .restrict_scroll_to_axis()
            .track_scroll(&self.scroll)
            .on_prepaint(cx.listener_prepaint())
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up));
        if !show_canvas {
            root = root.invisible();
        }
        root = root.child(content);

        if let Some(drag) = self.drag.as_ref().filter(|d| d.moved) {
            let local = drag.pos - self.bounds.origin;
            root = root.child(
                div()
                    .absolute()
                    .left(local.x + zpx(12.))
                    .top(local.y + zpx(12.))
                    .h(zpx(28.))
                    .px(zpx(10.))
                    .flex()
                    .items_center()
                    .rounded(radius::SM)
                    .bg(ui.popover)
                    .text_xs()
                    .opacity(0.9)
                    .shadow_md()
                    .child(drag.label.clone()),
            );
        }

        if columns.is_empty() && show_canvas {
            root = root.child(self.empty_state(cx));
        }
        root
    }
}

/// `on_prepaint` 的回调：记下画布的矩形，尺寸变了就再画一帧（首帧 / 窗口缩放）
trait PrepaintListener {
    fn listener_prepaint(&self) -> Box<dyn Fn(Bounds<Pixels>, &mut Window, &mut App)>;
}

impl PrepaintListener for Context<'_, Canvas> {
    fn listener_prepaint(&self) -> Box<dyn Fn(Bounds<Pixels>, &mut Window, &mut App)> {
        let this = self.entity().downgrade();
        Box::new(move |bounds, _, cx| {
            if let Some(this) = this.upgrade() {
                this.update(cx, |c, cx| {
                    if c.bounds != bounds {
                        let resized = c.bounds.size != bounds.size;
                        c.bounds = bounds;
                        if resized {
                            cx.notify();
                        }
                    }
                });
            }
        })
    }
}

#[derive(Clone, Copy)]
enum Round {
    All,
    Top,
    Bottom,
    None,
}

impl Round {
    fn apply(self, d: Div, r: Pixels) -> Div {
        match self {
            Round::All => d.rounded(r),
            Round::Top => d.rounded_t(r),
            Round::Bottom => d.rounded_b(r),
            Round::None => d,
        }
    }
}

/// 窗口的圆角。GPUI 的裁剪只有矩形（ContentMask 不带圆角），给窗口 `.rounded()` 裁不住子元素：
/// 终端 / 文件 / 差异的实心底和活动标题栏的 tint 会把角铺成直角。所以在窗口最上层叠一圈窗口底色
/// （`app`，岛外露出来的那层）的粗描边：外框向四周各扩 8px、外圆角 R+8，内缘正好是半径 R 的
/// 圆角矩形（着色器按 CSS 的规矩算，内圆角 = 外圆角 − 边宽）；外扩的部分被窗口的 overflow_hidden
/// 裁掉，剩下的只有四个角上圆弧以外那一小块。角点离圆心 √2·R，边宽至少要 (√2−1)·R ≈ 5px 才盖
/// 得住。不挂监听就没有 hitbox，点击照样落到下面。
fn corner_caps(round: Round, ui: &Ui) -> Div {
    let cap = px(8.);
    round
        .apply(div(), radius::LG + cap)
        .absolute()
        .top(-cap)
        .left(-cap)
        .right(-cap)
        .bottom(-cap)
        .border_8()
        .border_color(ui.app)
}

/// 活动窗口的描边（画布上不止一扇时才有，与标题栏的 tint 同一条件）：语义蓝 `tint_strong`，
/// 圆角与 [`corner_caps`] 的内缘重合。叠在内容上而不是给窗口加 border——border 占布局，
/// 终端会因此少一列 / 一行、换活动窗口就触发一次 resize
fn active_ring(round: Round, ui: &Ui) -> Div {
    round.apply(div(), radius::LG).absolute().inset_0().border_1().border_color(ui.tint_strong)
}

fn drop_indicator(spot: DropSpot, rects: &[ColumnRect], ui: &Ui) -> Option<AnyElement> {
    match spot {
        DropSpot::Column { at } => {
            let before = rects.get(at);
            let after = if at > 0 { rects.get(at - 1) } else { None };
            let x = before.map(|b| b.left - 4.).or(after.map(|a| a.right + 4.))?;
            let top = before.or(after).and_then(|r| r.panes.first()).map(|p| p.top).unwrap_or(0.);
            let bottom = before.or(after).and_then(|r| r.panes.last()).map(|p| p.bottom).unwrap_or(0.);
            Some(
                div()
                    .absolute()
                    .left(px(x as f32 - 1.))
                    .top(px(top as f32))
                    .w(px(2.))
                    .h(px((bottom - top).max(0.) as f32))
                    .rounded_full()
                    .bg(ui.primary)
                    .into_any_element(),
            )
        }
        DropSpot::Into { col, index } => {
            let rect = rects.get(col)?;
            let y = rect
                .panes
                .get(index)
                .map(|p| p.top)
                .or_else(|| index.checked_sub(1).and_then(|i| rect.panes.get(i)).map(|p| p.bottom))
                .unwrap_or(0.);
            Some(
                div()
                    .absolute()
                    .left(px(rect.left as f32))
                    .top(px(y as f32 - 1.))
                    .w(px((rect.right - rect.left).max(0.) as f32))
                    .h(px(2.))
                    .rounded_full()
                    .bg(ui.primary)
                    .into_any_element(),
            )
        }
    }
}

fn basename(path: &str) -> String {
    path.rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or(path).to_string()
}

impl Canvas {
    fn empty_state(&self, cx: &mut Context<Self>) -> AnyElement {
        let ui = Ui::global(cx).clone();
        let ws = self.ws.read(cx);
        let pid = ws.state.selected_project_id.clone();
        let mut col = div()
            .absolute()
            .inset_0()
            .flex()
            .flex_col()
            .gap_3()
            .items_center()
            .justify_center()
            .text_xs()
            .text_color(ui.muted_foreground)
            .child(t!("canvas.empty").to_string());
        if let Some(pid) = pid {
            let items = crate::menus::new_session_items(&self.ws, &pid, None);
            use gpui_kit::component::button::Button;
            use gpui_kit::component::menu::DropdownMenu;
            col = col.child(
                Button::new("empty-new-terminal")
                    .outline()
                    .icon(IconName::Plus)
                    .label(t!("sidebar.newTerminal").to_string())
                    .dropdown_menu(move |menu, _, _| to_popup(menu, items.clone())),
            );
        }
        col.into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn pane(
        &self,
        key: &str,
        frame: Bounds<Pixels>,
        round: Round,
        is_active: bool,
        mark_active: bool,
        pinned: bool,
        shown_keys: &[String],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ui = Ui::global(cx).clone();
        let Some(item) = parse_pane_key(key) else {
            return div().into_any_element();
        };
        let ws = self.ws.read(cx);
        let dimmed = self.drag.as_ref().is_some_and(|d| d.moved && d.key == key);

        let project_id = match &item {
            PaneItem::Terminal { id, .. } => ws.tab_project_id(id),
            PaneItem::File { project_id, .. } => Some(project_id.clone()),
            PaneItem::Diff { .. } => ws.state.diff_tab.as_ref().map(|d| d.project_id.clone()),
        };
        let project = project_id.as_deref().and_then(|p| ws.project(p)).cloned();

        // 菜单标题不能空着（它要说清"你在操作谁"），空闲会话兜底回项目名
        let label = match &item {
            PaneItem::Terminal { id, .. } => ws
                .session(id)
                .map(|s| crate::labels::session_title(s).unwrap_or_else(|| s.project_name.clone()))
                .unwrap_or_else(|| t!("tab.creating").to_string()),
            PaneItem::File { path, .. } => basename(path),
            PaneItem::Diff { .. } => ws
                .state
                .diff_tab
                .as_ref()
                .map(|d| basename(&d.file.path))
                .unwrap_or_else(|| t!("tab.diff").to_string()),
        };

        // ---- 标题栏内容 ----
        let mut info = div().flex().flex_1().min_w_0().items_center().gap(zpx(6.));
        let where_color = if is_active { ui.muted_foreground } else { ui.muted_foreground.opacity(0.7) };
        match &item {
            PaneItem::Terminal { id, .. } => {
                let pending = ws.state.pending.iter().find(|p| &p.id == id);
                let session = ws.session(id);
                let name = session.and_then(crate::labels::session_title);
                let conn = falcon_core::host_color::conn_label(project.as_ref(), ws.system.as_ref(), &crate::labels::local_word());
                let where_ = project.as_ref().and_then(|p| p.working_dir.clone()).unwrap_or(conn);
                let state = match (pending, session) {
                    (Some(p), _) => Some(if p.error.is_some() { Mark::Dead } else { Mark::Creating }),
                    (None, Some(s)) => (s.state != falcon_proto::SessionState::Active).then(|| Mark::from(s.state)),
                    (None, None) => Some(Mark::Creating),
                };
                if let Some(bar) = falcon_core::host_color::ssh_bar(project.as_ref()) {
                    info = info.child(host_bar(Some(bar), zpx(14.), cx));
                }
                if let Some(state) = state {
                    info = info.child(status_mark(state, zpx(12.), cx));
                }
                // 标题这一格空着是正常的：右边紧跟着完整工作目录，编一个占位名只会挤掉路径
                if let Some(name) = name {
                    info = info.child(div().flex_none().max_w(zpx(260.)).truncate().font_weight(FontWeight::MEDIUM).child(name));
                }
                info = info.child(div().flex_1().min_w_0().truncate().text_size(zpx(11.)).text_color(where_color).child(where_));
            }
            PaneItem::File { path, .. } => {
                info = info
                    .child(icon(IconName::FileText).size(zpx(14.)).text_color(ui.muted_foreground))
                    .child(div().flex_none().truncate().font_weight(FontWeight::MEDIUM).child(basename(path)))
                    .child(div().flex_1().min_w_0().truncate().text_size(zpx(11.)).text_color(where_color).child(path.clone()));
            }
            PaneItem::Diff { .. } => {
                let path = ws.state.diff_tab.as_ref().map(|d| d.file.path.clone()).unwrap_or_default();
                info = info
                    .child(icon(IconName::FileDiff).size(zpx(14.)).text_color(ui.muted_foreground))
                    .child(div().flex_none().truncate().font_weight(FontWeight::MEDIUM).child(label.clone()))
                    .child(div().flex_1().min_w_0().truncate().text_size(zpx(11.)).text_color(where_color).child(path));
            }
        }

        // ---- 标题栏菜单（右键） ----
        let menu = self.pane_menu(&item, key, project_id.as_deref(), pinned, shown_keys, cx);
        let zoomable = shown_keys.len() > 1 || ws.state.term_zoomed;
        let zoomed = ws.state.term_zoomed;

        let key_s = key.to_string();
        let key_drag = key.to_string();
        let label_drag = label.clone();
        let item_close = item.clone();
        let ws_close = self.ws.clone();
        let ws_zoom = self.ws.clone();
        let key_zoom = key.to_string();

        let mut header = div()
            .id(SharedString::from(format!("pane-head-{key}")))
            .h(zpx(HEADER_H))
            .flex_none()
            .flex()
            .items_center()
            .gap(zpx(6.))
            .px_2()
            .text_xs()
            .border_b_1()
            .border_color(ui.border)
            .cursor(CursorStyle::OpenHand)
            .text_color(if is_active { ui.foreground } else { ui.muted_foreground })
            .when(is_active && mark_active, |d| d.bg(ui.tint))
            .tooltip(|window, cx| Tooltip::new(t!("pane.dragHint").to_string()).build(window, cx))
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                if e.click_count >= 2 {
                    if zoomable {
                        let k = key_zoom.clone();
                        this.ws.update(cx, |w, cx| {
                            w.focus_pane(&k, cx);
                            w.toggle_term_zoom(cx);
                        });
                    }
                    return;
                }
                this.drag = Some(Drag {
                    key: key_drag.clone(),
                    label: label_drag.clone(),
                    start: e.position,
                    pos: e.position,
                    moved: false,
                });
                this.ws.update(cx, |w, cx| w.focus_pane(&key_drag, cx));
                this.focus_pane(&key_drag, window, cx);
            }))
            .child(info);
        if pinned {
            header = header.child(icon(IconName::Pin).size(zpx(12.)).text_color(ui.muted_foreground));
        }
        if zoomable {
            header = header.child(crate::ui::icon_button(
                SharedString::from(format!("pane-zoom-{key}")),
                if zoomed { IconName::Minimize2 } else { IconName::Maximize2 },
                if zoomed { t!("pane.restore").to_string() } else { t!("pane.maximize").to_string() },
                zpx(20.),
                cx,
                move |_, _, cx| {
                    cx.stop_propagation();
                    let k = key_s.clone();
                    ws_zoom.update(cx, |w, cx| {
                        w.focus_pane(&k, cx);
                        w.toggle_term_zoom(cx);
                    });
                },
            ));
        }
        header = header.child(crate::ui::icon_button(
            SharedString::from(format!("pane-close-{key}")),
            IconName::X,
            match &item {
                PaneItem::Terminal { .. } => t!("tab.closeHint").to_string(),
                _ => t!("tab.closeView").to_string(),
            },
            zpx(20.),
            cx,
            move |e, _, cx| {
                cx.stop_propagation();
                // Shift+关闭 = Detach，普通关闭 = Terminate（CONTEXT.md）
                let shift = e.modifiers().shift;
                ws_close.update(cx, |w, cx| match &item_close {
                    PaneItem::Terminal { id, .. } if shift => w.detach_tab(id, cx),
                    PaneItem::Terminal { id, .. } => w.close_tab(id, cx),
                    PaneItem::File { .. } => w.close_file(cx),
                    PaneItem::Diff { .. } => w.close_diff(cx),
                });
            },
        ));
        let header = header.context_menu(move |m, _, _| to_popup(m, menu.clone()));

        // ---- 内容 ----
        let body: AnyElement = match &item {
            PaneItem::Terminal { id, .. } => match ws.terminals.get(id) {
                Some(view) => view.clone().into_any_element(),
                None => self.pending_body(id, cx),
            },
            PaneItem::File { .. } => self
                .file_view
                .clone()
                .map(|v| v.into_any_element())
                .unwrap_or_else(|| div().into_any_element()),
            PaneItem::Diff { .. } => self
                .diff_view
                .clone()
                .map(|v| v.into_any_element())
                .unwrap_or_else(|| div().into_any_element()),
        };

        let key_focus = key.to_string();
        div()
            .id(SharedString::from(format!("pane-{key}")))
            .absolute()
            .left(frame.origin.x)
            .top(frame.origin.y)
            .w(frame.size.width)
            .h(frame.size.height)
            .flex()
            .flex_col()
            .overflow_hidden()
            .when(dimmed, |d| d.opacity(0.5))
            .child(header)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    // 点进内容就成为活动窗口，必须在捕获阶段接：zellij 常开鼠标上报，终端的
                    // mouse_down 上报完就 stop_propagation，冒泡阶段等不到——键盘焦点跟过去了，
                    // 标题栏的着色却还留在原来那扇上。标题栏自己的 mouse_down 另有处理
                    .capture_any_mouse_down(cx.listener(move |this, _, _, cx| {
                        if !is_active {
                            let k = key_focus.clone();
                            this.ws.update(cx, |w, cx| w.focus_pane(&k, cx));
                        }
                    }))
                    .child(body),
            )
            .child(corner_caps(round, &ui))
            .when(is_active && mark_active, |d| d.child(active_ring(round, &ui)))
            .into_any_element()
    }

    /// 会话还没建起来时占住窗口：立刻有反馈，而不是等 REST 返回才出现（web 的 PendingPane）
    fn pending_body(&self, id: &str, cx: &mut Context<Self>) -> AnyElement {
        use gpui_kit::component::Sizable;
        use gpui_kit::component::button::{Button, ButtonVariants as _};
        let ui = Ui::global(cx).clone();
        let ws = self.ws.read(cx);
        let entry = ws.state.pending.iter().find(|p| p.id == id).cloned();
        let project = entry.as_ref().and_then(|e| ws.project(&e.project_id)).cloned();
        let conn = falcon_core::host_color::conn_label(project.as_ref(), ws.system.as_ref(), &crate::labels::local_word());
        let error = entry.as_ref().and_then(|e| e.error.clone());
        let (w_cancel, w_retry) = (self.ws.clone(), self.ws.clone());
        let (id_cancel, id_retry) = (id.to_string(), id.to_string());
        let cancel = Button::new(SharedString::from(format!("pending-cancel-{id}")))
            .ghost()
            .small()
            .label(t!("common.cancel").to_string())
            .on_click(move |_, _, cx| w_cancel.update(cx, |w, cx| w.drop_tab(&id_cancel, cx)));
        let (bg, border, mark, title, body, actions): (_, _, _, String, String, Vec<AnyElement>) = match error {
            Some(err) => (
                ui.destructive.opacity(0.1),
                ui.destructive.opacity(0.4),
                Mark::Dead,
                t!("session.createFailedTitle").to_string(),
                err,
                vec![
                    Button::new(SharedString::from(format!("pending-retry-{id}")))
                        .outline()
                        .small()
                        .label(t!("session.retry").to_string())
                        .on_click(move |_, _, cx| w_retry.update(cx, |w, cx| w.retry_pending(&id_retry, cx)))
                        .into_any_element(),
                    cancel.into_any_element(),
                ],
            ),
            None => (
                ui.primary.opacity(0.1),
                ui.primary.opacity(0.25),
                Mark::Creating,
                t!("session.creatingTitle").to_string(),
                t!("session.creatingBody", conn = conn.clone()).to_string(),
                vec![cancel.into_any_element()],
            ),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px(zpx(14.))
                    .py(zpx(8.))
                    .bg(bg)
                    .border_b_1()
                    .border_color(border)
                    .child(status_mark(mark, zpx(14.), cx))
                    .child(div().flex_none().text_size(zpx(13.)).child(title))
                    .child(div().flex_1().min_w_0().truncate().text_xs().text_color(ui.muted_foreground).child(body))
                    .child(div().flex_none().flex().gap_2().children(actions)),
            )
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(zpx(13.))
                    .text_color(ui.muted_foreground)
                    .child(conn),
            )
            .into_any_element()
    }

    fn pane_menu(
        &self,
        item: &PaneItem,
        key: &str,
        project_id: Option<&str>,
        pinned: bool,
        shown_keys: &[String],
        _cx: &App,
    ) -> Vec<MenuItemSpec> {
        let mut items = Vec::new();
        if let Some(pid) = project_id {
            let ws = self.ws.clone();
            let pid = pid.to_string();
            let after = key.to_string();
            items.push(
                MenuItemSpec::new(t!("tab.newToRight").to_string(), move |_, cx| {
                    ws.update(cx, |w, cx| w.new_terminal(&pid, None, Some(after.clone()), cx))
                })
                .kbd(crate::ui::chord("newTerminal")),
            );
        }
        {
            let ws = self.ws.clone();
            let k = key.to_string();
            items.push(
                MenuItemSpec::new(t!("pane.pinRight").to_string(), move |_, cx| ws.update(cx, |w, cx| w.toggle_pin_pane(&k, cx)))
                    .checked(pinned),
            );
        }
        if let PaneItem::Terminal { id, .. } = item
            && !is_pending_id(id)
        {
            let ws = self.ws.clone();
            let id = id.clone();
            items.push(MenuItemSpec::new(t!("session.rename").to_string(), move |window, cx| {
                crate::dialogs::rename::open(&ws, &id, window, cx)
            }));
        }
        {
            let ws = self.ws.clone();
            let it = item.clone();
            let label = if matches!(item, PaneItem::Terminal { .. }) { t!("tab.close") } else { t!("tab.closeView") };
            items.push(
                MenuItemSpec::new(label.to_string(), move |_, cx| {
                    ws.update(cx, |w, cx| match &it {
                        PaneItem::Terminal { id, .. } => w.close_tab(id, cx),
                        PaneItem::File { .. } => w.close_file(cx),
                        PaneItem::Diff { .. } => w.close_diff(cx),
                    })
                })
                .sep(),
            );
        }
        if let PaneItem::Terminal { id, .. } = item {
            let ws = self.ws.clone();
            let id = id.clone();
            items.push(MenuItemSpec::new(t!("tab.detach").to_string(), move |_, cx| ws.update(cx, |w, cx| w.detach_tab(&id, cx))));
        }
        let index = shown_keys.iter().position(|k| k == key);
        if let (true, Some(index)) = (shown_keys.len() > 1, index) {
            let others: Vec<String> = shown_keys.iter().filter(|k| *k != key).cloned().collect();
            let ws = self.ws.clone();
            items.push(
                MenuItemSpec::new(t!("tab.closeOthers").to_string(), move |_, cx| {
                    ws.update(cx, |w, cx| w.close_pane_keys(others.clone(), cx))
                })
                .sep(),
            );
            if index > 0 {
                let left: Vec<String> = shown_keys[..index].to_vec();
                let ws = self.ws.clone();
                items.push(MenuItemSpec::new(t!("tab.closeToLeft").to_string(), move |_, cx| {
                    ws.update(cx, |w, cx| w.close_pane_keys(left.clone(), cx))
                }));
            }
            if index + 1 < shown_keys.len() {
                let right: Vec<String> = shown_keys[index + 1..].to_vec();
                let ws = self.ws.clone();
                items.push(MenuItemSpec::new(t!("tab.closeToRight").to_string(), move |_, cx| {
                    ws.update(cx, |w, cx| w.close_pane_keys(right.clone(), cx))
                }));
            }
        }
        items
    }
}
