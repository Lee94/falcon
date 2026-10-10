//! 表单小件：项目 / 附属项目 / 主机表单与 Zellij 安装框共用（web 的 `common/Field.tsx` 与
//! shadcn Select 的原生版）。只有这几个表单用，所以放在这里而不是 `ui.rs`。
//!
//! - [`field`]：标签 + 控件 + 提示，提示带 ok / err 语气（web 的 `Field`）；
//! - [`segmented`]：二选一 / 三选一切换，槽用 sunken、选中项用 island（web 的 `Segmented`）；
//! - [`Opt`] / [`Options`]：Select 的选项与分组数据源。值一律是字符串——web 那边 Radix Select
//!   也只认字符串 value，哨兵值（"__auto__" 之类）的写法可以原样照搬。

use falcon_core::reason::Translate;
use gpui_kit::component::IndexPath;
use gpui_kit::component::select::{SelectDelegate, SelectItem, SelectState};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, App, ClickEvent, Context, Div, FontWeight, IntoElement, SharedString, Window, div};

use crate::theme::{Ui, radius};
use crate::zoom::zpx;

/// 提示行的语气
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Muted,
    Ok,
    Err,
    Warn,
}

/// 表单一行：标签 + 控件 + 提示（web 的 `Field`）
pub fn field(label: Option<String>, control: impl IntoElement, hint: Option<(String, Tone)>, cx: &App) -> Div {
    let ui = Ui::global(cx);
    let mut d = div().flex().flex_col().gap(zpx(6.)).min_w_0();
    if let Some(label) = label {
        d = d.child(div().text_xs().text_color(ui.muted_foreground).child(label));
    }
    d = d.child(control);
    if let Some((text, tone)) = hint {
        let color = match tone {
            Tone::Muted => ui.muted_foreground,
            Tone::Ok => ui.success,
            Tone::Err => ui.destructive,
            Tone::Warn => ui.warning,
        };
        d = d.child(div().text_xs().line_height(zpx(18.)).text_color(color).child(text));
    }
    d
}

/// 服务端错误 / 校验错误：就地一行红字（web 的 `text-[13px] text-destructive`）
pub fn error_line(text: impl Into<SharedString>, cx: &App) -> Div {
    div().text_size(zpx(13.)).text_color(Ui::global(cx).destructive).child(text.into())
}

/// 一行说明文字（web 的 `text-xs text-muted-foreground`）
pub fn note(text: impl Into<SharedString>, cx: &App) -> Div {
    div().text_xs().line_height(zpx(18.)).text_color(Ui::global(cx).muted_foreground).child(text.into())
}

pub struct SegItem {
    pub label: String,
    pub on: bool,
    pub on_click: Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>,
}

/// 同一份表单的几种填法（语义是 radiogroup，不是 tab）：凹槽里浮起一块
pub fn segmented(id: &str, items: Vec<SegItem>, cx: &App) -> Div {
    let ui = Ui::global(cx).clone();
    let mut d = crate::ui::sunken(cx).flex().gap(zpx(2.)).p(zpx(2.));
    for (i, item) in items.into_iter().enumerate() {
        let on_click = item.on_click;
        let mut b = div()
            .id(SharedString::from(format!("{id}-{i}")))
            .flex_1()
            .h(zpx(28.))
            .px_3()
            .flex()
            .items_center()
            .justify_center()
            .rounded(radius::SM)
            .text_size(zpx(13.))
            .cursor_pointer()
            .on_click(move |e, window, cx| on_click(e, window, cx))
            .child(item.label);
        b = if item.on {
            b.bg(ui.background).font_weight(FontWeight::MEDIUM).text_color(ui.foreground)
        } else {
            b.text_color(ui.muted_foreground).hover(|s| s.text_color(ui.foreground))
        };
        d = d.child(b);
    }
    d
}

/// 可展开的一段（web 的 `Disclosure`）：箭头 + 标签，展开后显示内容
pub fn disclosure(
    id: &str,
    open: bool,
    label: String,
    on_toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    content: impl IntoElement,
    cx: &App,
) -> Div {
    let ui = Ui::global(cx).clone();
    let head = div()
        .id(SharedString::from(id.to_string()))
        .flex()
        .items_center()
        .gap(zpx(6.))
        .py(zpx(6.))
        .text_size(zpx(12.5))
        .text_color(ui.muted_foreground)
        .cursor_pointer()
        .hover(|s| s.text_color(ui.foreground))
        .on_click(on_toggle)
        .child(
            crate::ui::icon(if open { gpui_kit::assets::IconName::ChevronDown } else { gpui_kit::assets::IconName::ChevronRight })
                .size(zpx(12.)),
        )
        .child(label);
    let mut d = div().flex().flex_col().child(head);
    if open {
        d = d.child(content);
    }
    d
}

// ---------------- Select 的数据源 ----------------

/// Select 的一项。`detail` 画在标签右边（主机下拉里的 user@host:port）
#[derive(Clone, Debug)]
pub struct Opt {
    pub value: String,
    pub label: SharedString,
    pub detail: Option<SharedString>,
    pub disabled: bool,
    /// 组头行（见 [`Options::grouped`]）：不可选，画成小号灰字
    header: bool,
}

impl Opt {
    pub fn new(value: impl Into<String>, label: impl Into<SharedString>) -> Self {
        Self { value: value.into(), label: label.into(), detail: None, disabled: false, header: false }
    }
    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    fn row(&self, muted: gpui_kit::Hsla) -> Div {
        let mut d = div().flex().items_center().gap_2().min_w_0().child(div().truncate().child(self.label.clone()));
        if let Some(detail) = &self.detail {
            d = d.child(div().flex_none().text_xs().text_color(muted).child(detail.clone()));
        }
        d
    }
}

impl SelectItem for Opt {
    type Value = String;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    // 触发器里只显示标题：detail（user@host:port）由 Field 的提示行给出，与 web 一致

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let muted = Ui::global(cx).muted_foreground;
        if self.header {
            return div().text_size(zpx(11.)).text_color(muted).child(self.label.clone());
        }
        self.row(muted)
    }

    fn value(&self) -> &String {
        &self.value
    }

    fn disabled(&self) -> bool {
        self.disabled
    }
}

/// 一组选项；`title` 为 None 的组不画组头（"当前 HEAD" 那种顶在最前面的单项）
#[derive(Clone, Debug, Default)]
pub struct Section {
    pub title: Option<SharedString>,
    pub items: Vec<Opt>,
}

/// 分组的选项表（web 的 SelectGroup + SelectLabel）
#[derive(Clone, Debug, Default)]
pub struct Options(pub Vec<Section>);

impl Options {
    /// 不分组
    pub fn flat(items: Vec<Opt>) -> Self {
        Options(vec![Section { title: None, items }])
    }

    /// 顶上几项不归组、下面分组（web 的「当前 HEAD」+ 本地分支 / 远程分支）。
    ///
    /// gpui-component 的 List 只量第 0 组的组头高度、套给所有组：第 0 组没有组头而后面的有，
    /// 组头就会压在选项上。所以有不归组的顶项时整张表摊平成一组，组头改成不可选的标签行。
    pub fn grouped(top: Vec<Opt>, groups: Vec<(String, Vec<Opt>)>) -> Self {
        let groups: Vec<(String, Vec<Opt>)> = groups.into_iter().filter(|(_, items)| !items.is_empty()).collect();
        if top.is_empty() {
            return Options(
                groups
                    .into_iter()
                    .map(|(title, items)| Section { title: Some(title.into()), items })
                    .collect(),
            );
        }
        let mut items = top;
        for (title, group) in groups {
            let mut header = Opt::new(format!("\u{0}group:{title}"), title).disabled(true);
            header.header = true;
            items.push(header);
            items.extend(group);
        }
        Options::flat(items)
    }
}

impl SelectDelegate for Options {
    type Item = Opt;

    fn sections_count(&self, _: &App) -> usize {
        self.0.len()
    }

    fn items_count(&self, section: usize) -> usize {
        self.0.get(section).map_or(0, |s| s.items.len())
    }

    fn item(&self, ix: IndexPath) -> Option<&Opt> {
        self.0.get(ix.section)?.items.get(ix.row)
    }

    fn position<V>(&self, value: &V) -> Option<IndexPath>
    where
        Self::Item: SelectItem<Value = V>,
        V: PartialEq,
    {
        for (s, section) in self.0.iter().enumerate() {
            for (r, item) in section.items.iter().enumerate() {
                if item.value() == value {
                    return Some(IndexPath::default().section(s).row(r));
                }
            }
        }
        None
    }

    fn render_section_header(&self, section: usize, _: &mut Window, cx: &mut App) -> Option<AnyElement> {
        let title = self.0.get(section)?.title.clone()?;
        Some(
            div()
                .px_2()
                .pt_2()
                .pb_1()
                .text_size(zpx(11.))
                .text_color(Ui::global(cx).muted_foreground)
                .child(title)
                .into_any_element(),
        )
    }
}

pub type OptState = SelectState<Options>;

/// 新建一个 Select 状态并选中 `value`（找不到就不选，显示占位）
pub fn new_select(options: Options, value: &str, window: &mut Window, cx: &mut Context<OptState>) -> OptState {
    let ix = options.position(&value.to_string());
    SelectState::new(options, ix, window, cx)
}

/// 换一批选项并重新选中 `value`
pub fn reset_select(state: &gpui_kit::Entity<OptState>, options: Options, value: &str, window: &mut Window, cx: &mut App) {
    state.update(cx, |s, cx| {
        s.set_items(options, window, cx);
        s.set_selected_value(&value.to_string(), window, cx);
    });
}

/// 当前选中项的值
pub fn selected(state: &gpui_kit::Entity<OptState>, cx: &App) -> Option<String> {
    state.read(cx).selected_value().cloned()
}

/// `falcon_core::reason` 要的翻译函数：key 是运行时拼出来的（`worktree.reason_*` 之类）
pub struct Tr;

impl Translate for Tr {
    fn t(&self, key: &str, params: &[(&str, &str)]) -> String {
        let mut s = rust_i18n::t!(key).to_string();
        for (k, v) in params {
            s = s.replace(&format!("%{{{k}}}"), v);
        }
        s
    }
}
