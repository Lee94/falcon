//! Host-side probes shared by the project form's embedded directory browser and shell menu.
use std::cell::RefCell;

use super::*;
use crate::terminal::bind_safe_key_when;
use falcon_proto::{FsListing, ShellsInfo};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ProbeTarget {
    Backend,
    SshHost(String),
    Project(String),
}

impl ProbeTarget {
    #[cfg(not(target_arch = "wasm32"))]
    fn native(&self) -> falcon_client::ProbeTarget {
        match self {
            Self::Backend => falcon_client::ProbeTarget::Backend,
            Self::SshHost(id) => falcon_client::ProbeTarget::SshHost(id.clone()),
            Self::Project(id) => falcon_client::ProbeTarget::Project(id.clone()),
        }
    }
}

pub(super) async fn shells(
    store: &crate::store::WorkspaceStore,
    target: &ProbeTarget,
) -> Result<ShellsInfo, String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let client = store
            .client()
            .ok_or_else(|| t!("native.projectForm.probeUnavailable"))?;
        client
            .list_shells(&target.native())
            .await
            .map_err(|error| error.to_string())
    }
    #[cfg(target_arch = "wasm32")]
    fetch_probe(store, "/api/shells", target, None).await
}

async fn directories(
    store: &crate::store::WorkspaceStore,
    target: &ProbeTarget,
    path: Option<&str>,
) -> Result<FsListing, String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let client = store
            .client()
            .ok_or_else(|| t!("native.projectForm.probeUnavailable"))?;
        client
            .list_dir(path, &target.native())
            .await
            .map_err(|error| error.to_string())
    }
    #[cfg(target_arch = "wasm32")]
    fetch_probe(store, "/api/fs/list", target, path).await
}

#[cfg(target_arch = "wasm32")]
async fn fetch_probe<T: serde::de::DeserializeOwned>(
    store: &crate::store::WorkspaceStore,
    route: &str,
    target: &ProbeTarget,
    path: Option<&str>,
) -> Result<T, String> {
    let mut url = url::Url::parse(&format!(
        "{}{route}",
        store.server_url.get_untracked().trim_end_matches('/')
    ))
    .map_err(|error| error.to_string())?;
    {
        let mut query = url.query_pairs_mut();
        if let Some(path) = path {
            query.append_pair("path", path);
        }
        match target {
            ProbeTarget::Backend => {}
            ProbeTarget::SshHost(id) => {
                query.append_pair("hostId", id);
            }
            ProbeTarget::Project(id) => {
                query.append_pair("projectId", id);
            }
        }
    }
    let response = fetch(HttpRequest::get(url.to_string()))
        .await
        .map_err(|error| format!("{error:?}"))?;
    if !response.ok() {
        return Err(
            serde_json::from_slice::<falcon_proto::ApiError>(&response.body)
                .map(|error| error.error)
                .unwrap_or_else(|_| t!("native.projectForm.probeUnavailable")),
        );
    }
    serde_json::from_slice(&response.body).map_err(|error| error.to_string())
}

// Navigation stays valid when a path fails, unlike that path's directory entries.
#[derive(Clone, PartialEq)]
struct FolderNavigation {
    home: String,
    parent: Option<String>,
    root: String,
}

#[derive(Clone)]
pub(super) struct FolderBrowser {
    pub open: Signal<bool>,
    pub multiple: Signal<bool>,
    pub destination: Signal<Option<Signal<String>>>,
    target: Signal<Option<ProbeTarget>>,
    listing: Signal<Option<FsListing>>,
    navigation: Signal<Option<FolderNavigation>>,
    path: Signal<String>,
    filter: Signal<String>,
    selected: Signal<Vec<String>>,
    /// 高亮行（GPUI FolderPicker 的 highlight）：↑↓ 移动、悬停同步、回车进入
    highlight: Signal<usize>,
    /// 已确认、外层正在创建：两个按钮禁用防重复提交（GPUI 的 confirming）
    confirming: Signal<bool>,
    busy: Signal<bool>,
    error: Signal<Option<String>>,
    request: Signal<u64>,
    store: crate::store::WorkspaceStore,
    guard: FormUiGuard,
}

impl FolderBrowser {
    pub fn new(
        open: Signal<bool>,
        store: crate::store::WorkspaceStore,
        guard: FormUiGuard,
    ) -> Self {
        open.set(false);
        Self {
            open,
            multiple: signal(false),
            destination: signal(None),
            target: signal(None),
            listing: signal(None),
            navigation: signal(None),
            path: signal(String::new()),
            filter: signal(String::new()),
            selected: signal(Vec::new()),
            highlight: signal(0),
            confirming: signal(false),
            busy: signal(false),
            error: signal(None),
            request: signal(0),
            store,
            guard,
        }
    }

    pub fn start(&self, target: ProbeTarget, initial: String, destination: Option<Signal<String>>) {
        if !self.guard.is_current() {
            return;
        }
        self.target.set(Some(target));
        self.multiple.set(destination.is_none());
        self.destination.set(destination);
        self.listing.set(None);
        self.navigation.set(None);
        self.selected.set(Vec::new());
        self.highlight.set(0);
        self.confirming.set(false);
        self.path.set(initial.clone());
        self.open.set(true);
        self.load((!initial.trim().is_empty()).then(|| initial.trim().to_string()));
    }

    fn load(&self, path: Option<String>) {
        if !self.guard.is_current() || !self.open.get_untracked() {
            return;
        }
        let Some(target) = self.target.get_untracked() else {
            return;
        };
        let request = self.request.get_untracked() + 1;
        self.request.set(request);
        self.busy.set(true);
        self.error.set(None);
        let browser = self.clone();
        self.guard.context.owner.run(move || {
            spawn(async move {
                let mut result = directories(&browser.store, &target, path.as_deref()).await;
                if !browser.current(request) {
                    return;
                }
                // A stale saved directory should not strand the picker: open home, keeping the reason.
                let mut fallback_error = None;
                if result.is_err() && path.is_some() && browser.navigation.get_untracked().is_none()
                {
                    fallback_error = result.err();
                    result = directories(&browser.store, &target, None).await;
                    if !browser.current(request) {
                        return;
                    }
                }
                browser.busy.set(false);
                match result {
                    Ok(listing) => {
                        browser.navigation.set(Some(FolderNavigation {
                            home: listing.home.clone(),
                            parent: listing.parent.clone(),
                            root: if listing.roots.iter().any(|r| r == "/") {
                                "/".into()
                            } else {
                                String::new()
                            },
                        }));
                        browser.path.set(listing.path.clone());
                        browser.filter.set(String::new());
                        browser.highlight.set(0);
                        browser.listing.set(Some(listing));
                        browser.error.set(fallback_error);
                    }
                    Err(error) => {
                        // A typed destination can differ from the last successful
                        // listing. Do not let confirmation submit that previous path.
                        browser.listing.set(None);
                        browser.error.set(Some(error));
                    }
                }
            })
        });
    }

    fn current(&self, request: u64) -> bool {
        self.guard.is_current()
            && self.open.get_untracked()
            && self.request.get_untracked() == request
    }

    /// 筛选后可见的条目，与渲染同一套过滤（GPUI FolderPicker 的 `visible()`）。
    fn visible_entries(&self) -> Vec<falcon_proto::FsDirEntry> {
        let query = self.filter.get_untracked().trim().to_lowercase();
        self.listing
            .get_untracked()
            .map(|listing| {
                listing
                    .entries
                    .iter()
                    .filter(|entry| entry.name.to_lowercase().contains(&query))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// ↑↓ 移动高亮条并把该行滚进视野（GPUI `move_highlight`：到边即停，不环绕）。
    /// `rows` 是渲染时按可见顺序记下的行元素引用。
    fn move_highlight(
        &self,
        delta: isize,
        rows: &Rc<RefCell<Vec<nsh_core::element::ElementRef>>>,
    ) {
        if !self.guard.is_current() || !self.open.get_untracked() {
            return;
        }
        let count = self.visible_entries().len();
        if count == 0 {
            return;
        }
        let next = (self.highlight.get_untracked() as isize + delta)
            .clamp(0, count as isize - 1) as usize;
        self.highlight.set(next);
        if let Some(row) = rows.borrow().get(next) {
            row.reveal(Default::default());
        }
    }

    /// 筛选框里的回车：进入高亮的目录（GPUI 在 filter 上 capture Enter）。
    fn enter_highlighted(&self) {
        if !self.guard.is_current() || !self.open.get_untracked() {
            return;
        }
        let entries = self.visible_entries();
        let at = self
            .highlight
            .get_untracked()
            .min(entries.len().saturating_sub(1));
        if let Some(hit) = entries.get(at) {
            self.load(Some(hit.path.clone()));
        }
    }

    /// 筛选为空时的退格：回上一级；有字时交给输入框照常删字（GPUI 的 Backspace-on-empty）。
    /// `on_key_press` 在文本变化之前报键，此刻筛选框还是空就是要回上级。
    fn back_up_on_empty_filter(&self, key: &str) {
        if key != "Backspace"
            || !self.filter.get_untracked().is_empty()
            || !self.guard.is_current()
            || !self.open.get_untracked()
        {
            return;
        }
        if let Some(parent) = self.navigation.get_untracked().and_then(|nav| nav.parent) {
            self.load(Some(parent));
        }
    }

    pub fn render(
        &self,
        resolved: &falcon_theme::ResolvedTheme,
        confirm: Rc<dyn Fn(Vec<String>)>,
    ) -> AnyElement {
        use crate::shell::widgets::{Glyph, glyph};
        let browser = self.clone();
        let jump = self.clone();
        let path = self.path;
        let path_guard = self.guard.clone();
        // Inputs stay mounted while filtering or receiving a directory response.
        let path_input = view()
            .flex_col()
            .gap(zpx(4.0))
            .child(text(t!("project.workingDir")).text_xs())
            .child(
                input()
                    .label("folder-picker-path")
                    .w_full()
                    .px(zpx(8.0))
                    .py(zpx(6.0))
                    .rounded(zpx(6.0))
                    .bg(rgba_to_hsla(resolved.ui.input))
                    .font_family(FONT_FAMILY_MONO)
                    .text_xs()
                    .text_color(rgb_to_hsla(resolved.ui.foreground))
                    .placeholder(t!("project.pickRoots"))
                    .value(move || path.get())
                    .on_change(move |value: &str| {
                        if path_guard.is_current() {
                            path.set(value.to_string());
                        }
                    })
                    .on_submit(move |_: &str| {
                        jump.load(Some(path.get_untracked().trim().to_string()))
                    }),
            );
        // 筛选框自带按键路径：打开即聚焦（GPUI `focus()` 落在 filter 上），回车进高亮
        // 目录、空串退格回上级走 `on_submit` / `on_key_press`——键在文本变化之前到达。
        let filter = self.filter;
        let filter_owner = Owner::current();
        let filter_guard = self.guard.clone();
        let filter_submit = self.clone();
        let filter_key = self.clone();
        let filter_input = view()
            .flex_col()
            .gap(zpx(4.0))
            .child(
                text(t!("project.pickFilter"))
                    .text_xs()
                    .text_color(rgb_to_hsla(resolved.ui.muted_foreground)),
            )
            .child(
                input()
                    .label(t!("project.pickFilter"))
                    .auto_focus(true)
                    .w_full()
                    .px(zpx(8.0))
                    .py(zpx(6.0))
                    .rounded(zpx(6.0))
                    .bg(rgba_to_hsla(resolved.ui.input))
                    .text_xs()
                    .text_color(rgb_to_hsla(resolved.ui.foreground))
                    .placeholder(t!("project.pickFilter"))
                    .placeholder_color(rgb_to_hsla(resolved.ui.muted_foreground))
                    // 回车进目录后焦点留在筛选框（GPUI 截走 Enter，输入框保持焦点）
                    .submit_behavior(nsh_core::SubmitBehavior::Submit)
                    .value(move || filter.get())
                    .on_change(move |next: &str| {
                        if filter_owner.is_live() && filter_guard.is_current() {
                            filter.set(next.to_string());
                        }
                    })
                    .on_submit(move |_: &str| filter_submit.enter_highlighted())
                    .on_key_press(move |key: &str| filter_key.back_up_on_empty_filter(key)),
            );
        // ↑↓ 移动高亮条（GPUI 在 filter 上 capture MoveUp/MoveDown）：与主题选择器、
        // 快开同用窗口级绑定，选择器收起时随这块区域解绑。
        let row_refs: Rc<RefCell<Vec<nsh_core::element::ElementRef>>> =
            Rc::new(RefCell::new(Vec::new()));
        for (spec, delta) in [("down", 1isize), ("up", -1)] {
            let keys = self.clone();
            let rows = row_refs.clone();
            bind_safe_key_when(spec, false, nsh_core::keymap::When::Always, move || {
                keys.move_highlight(delta, &rows);
            });
        }
        let resolved = resolved.clone();
        let content_rows = row_refs.clone();
        let content = dynamic(move || {
            let listing = browser.listing.get();
            let navigation = browser.navigation.get();
            let busy = browser.busy.get();
            let selected = browser.selected.get();
            let multiple = browser.multiple.get();
            let mut tools = view().flex_row().flex_wrap().items_center().gap(zpx(6.0));
            for (label, path) in [
                (
                    t!("project.pickHome"),
                    navigation.as_ref().map(|nav| nav.home.clone()),
                ),
                (
                    t!("project.pickUp"),
                    navigation.as_ref().and_then(|nav| nav.parent.clone()),
                ),
                (
                    t!("project.pickRoots"),
                    navigation.as_ref().map(|nav| nav.root.clone()),
                ),
            ] {
                let nav = browser.clone();
                tools = tools.child(choice_button(
                    label,
                    false,
                    busy || path.is_none(),
                    &resolved,
                    move || nav.load(path.clone()),
                ));
            }
            let jump = browser.clone();
            tools = tools.child(choice_button(
                t!("project.pickJump"),
                false,
                busy,
                &resolved,
                move || jump.load(Some(jump.path.get_untracked().trim().to_string())),
            ));
            let query = browser.filter.get().trim().to_lowercase();
            let mut entries = view().flex_col().gap(zpx(3.0));
            let visible: Vec<_> = listing
                .as_ref()
                .map(|l| {
                    l.entries
                        .iter()
                        .filter(|entry| entry.name.to_lowercase().contains(&query))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            if busy {
                entries = entries.child(
                    text(
                        if matches!(browser.target.get(), Some(ProbeTarget::Backend)) {
                            t!("project.pickLoading")
                        } else {
                            t!("project.pickConnecting")
                        },
                    )
                    .text_xs(),
                );
            } else if visible.is_empty() {
                entries = entries.child(
                    text(if query.is_empty() {
                        t!("project.pickEmpty")
                    } else {
                        t!("project.pickEmptyFilter")
                    })
                    .text_xs(),
                );
            }
            // 高亮钳在可见范围里：筛选变短时回收（GPUI 在筛选 Change 时 clamp）
            let highlighted = browser
                .highlight
                .get()
                .min(visible.len().saturating_sub(1));
            browser.highlight.set(highlighted);
            let mut rows = Vec::with_capacity(visible.len());
            for (index, entry) in visible.into_iter().enumerate() {
                let on = index == highlighted;
                let row_ref = element_ref();
                let hover = browser.highlight;
                let mut row = view()
                    .flex_row()
                    .items_center()
                    .gap(zpx(6.0))
                    .rounded(zpx(6.0))
                    .cursor_pointer()
                    .element_ref(&row_ref)
                    // 悬停同步高亮条（GPUI 行的 on_mouse_move 联动）
                    .on_mouse_move(move |_: &nsh_core::element::MouseEvent| {
                        if hover.get_untracked() != index {
                            hover.set(index);
                        }
                    })
                    .when(on, |v| v.bg(rgb_to_hsla(resolved.ui.tint)));
                if multiple {
                    let checked = selected.contains(&entry.path);
                    let path = entry.path.clone();
                    let select = browser.selected;
                    let submitting = browser.confirming;
                    let guard = browser.guard.clone();
                    row = row.child(
                        view()
                            .label(format!("folder-select-{path}"))
                            .focusable(true)
                            .cursor_pointer()
                            .p(zpx(4.0))
                            .tooltip(crate::shell::popups::themed_tooltip(
                                text(if checked {
                                    t!("native.projectForm.unselect")
                                } else {
                                    t!("native.projectForm.select")
                                }),
                                None,
                                &resolved,
                            ))
                            .on_press(move || {
                                if !busy && !submitting.get_untracked() && guard.is_current() {
                                    select.update(|paths| {
                                        if let Some(index) = paths.iter().position(|p| p == &path) {
                                            paths.remove(index);
                                        } else {
                                            paths.push(path.clone());
                                        }
                                    });
                                }
                            })
                            .child(crate::views::file_operations::selection_indicator(
                                checked, &resolved,
                            )),
                    );
                }
                let enter = browser.clone();
                row = row.child(glyph(Glyph::Folder)).child(choice_button(
                    entry.name,
                    on,
                    busy,
                    &resolved,
                    move || enter.load(Some(entry.path.clone())),
                ));
                entries = entries.child(row);
                rows.push(row_ref);
            }
            *content_rows.borrow_mut() = rows;
            let confirming = browser.confirming.get();
            let can_select = !busy
                && !confirming
                && (!selected.is_empty()
                    || listing.as_ref().is_some_and(|l| {
                        !l.path.is_empty() && l.path == browser.path.get().trim()
                    }));
            let accept = confirm.clone();
            let cancel = browser.open;
            let select_guard = browser.clone();
            let mut body = view()
                .flex_col()
                .gap(zpx(10.0))
                .child(tools)
                .when(multiple, |v| {
                    v.child(text(t!("project.pickMultipleHint")).text_xs())
                })
                .child(scroll().h(zpx(240.0)).child(entries));
            if let Some(error) = browser.error.get() {
                body = body.child(
                    text(error)
                        .text_xs()
                        .text_color(rgb_to_hsla(resolved.ui.destructive)),
                );
            }
            body.child(
                view()
                    .flex_row()
                    .justify_end()
                    .gap(zpx(8.0))
                    .child(choice_button(
                        t!("common.cancel"),
                        false,
                        confirming,
                        &resolved,
                        move || cancel.set(false),
                    ))
                    .child(choice_button(
                        if confirming {
                            t!("project.creating")
                        } else if multiple && !selected.is_empty() {
                            t!("project.pickSelected", n = selected.len().to_string())
                        } else {
                            t!("project.pickSelect")
                        },
                        true,
                        !can_select,
                        &resolved,
                        move || {
                            // 确认防重（GPUI `select()` 的 busy || confirming 前置返回）
                            if select_guard.busy.get_untracked()
                                || select_guard.confirming.get_untracked()
                            {
                                return;
                            }
                            let paths = if !selected.is_empty() {
                                selected.clone()
                            } else if let Some(listing) = &listing {
                                vec![listing.path.clone()]
                            } else {
                                return;
                            };
                            select_guard.confirming.set(true);
                            accept(paths);
                            // 回调没有关掉选择器（表单守卫失效等）：解锁，别锁死按钮
                            if select_guard.open.get_untracked() {
                                select_guard.confirming.set(false);
                            }
                        },
                    )),
            )
            .into_element()
        });
        view()
            .flex_col()
            .p(zpx(14.0))
            .gap(zpx(10.0))
            .child(
                text(t!("project.pickTitle"))
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD),
            )
            .child(path_input)
            .child(filter_input)
            .child(content)
            .into_element()
    }
}
