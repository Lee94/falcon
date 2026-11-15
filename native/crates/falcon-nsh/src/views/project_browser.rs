//! Host-side probes shared by the project form's embedded directory browser and shell menu.
use super::*;
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

#[derive(Clone)]
pub(super) struct FolderBrowser {
    pub open: Signal<bool>,
    pub multiple: Signal<bool>,
    pub destination: Signal<Option<Signal<String>>>,
    target: Signal<Option<ProbeTarget>>,
    listing: Signal<Option<FsListing>>,
    path: Signal<String>,
    filter: Signal<String>,
    selected: Signal<Vec<String>>,
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
            path: signal(String::new()),
            filter: signal(String::new()),
            selected: signal(Vec::new()),
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
        self.selected.set(Vec::new());
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
                if result.is_err() && path.is_some() && browser.listing.get_untracked().is_none() {
                    fallback_error = result.err();
                    result = directories(&browser.store, &target, None).await;
                    if !browser.current(request) {
                        return;
                    }
                }
                browser.busy.set(false);
                match result {
                    Ok(listing) => {
                        browser.path.set(listing.path.clone());
                        browser.filter.set(String::new());
                        browser.listing.set(Some(listing));
                        browser.error.set(fallback_error);
                    }
                    Err(error) => browser.error.set(Some(error)),
                }
            })
        });
    }

    fn current(&self, request: u64) -> bool {
        self.guard.is_current()
            && self.open.get_untracked()
            && self.request.get_untracked() == request
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
        let filter_input = text_field(
            t!("project.pickFilter"),
            self.filter,
            t!("project.pickFilter"),
            resolved,
            false,
            false,
        );
        let resolved = resolved.clone();
        let content = dynamic(move || {
            let listing = browser.listing.get();
            let busy = browser.busy.get();
            let selected = browser.selected.get();
            let multiple = browser.multiple.get();
            let mut tools = view().flex_row().flex_wrap().items_center().gap(zpx(6.0));
            for (label, path) in [
                (
                    t!("project.pickHome"),
                    listing.as_ref().map(|l| l.home.clone()),
                ),
                (
                    t!("project.pickUp"),
                    listing.as_ref().and_then(|l| l.parent.clone()),
                ),
                (
                    t!("project.pickRoots"),
                    listing.as_ref().map(|l| {
                        if l.roots.iter().any(|r| r == "/") {
                            "/".into()
                        } else {
                            String::new()
                        }
                    }),
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
            for entry in visible {
                let mut row = view().flex_row().items_center().gap(zpx(6.0));
                if multiple {
                    let checked = selected.contains(&entry.path);
                    let path = entry.path.clone();
                    let select = browser.selected;
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
                                if !busy && guard.is_current() {
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
                    false,
                    busy,
                    &resolved,
                    move || enter.load(Some(entry.path.clone())),
                ));
                entries = entries.child(row);
            }
            let can_select = !busy
                && (!selected.is_empty() || listing.as_ref().is_some_and(|l| !l.path.is_empty()));
            let accept = confirm.clone();
            let cancel = browser.open;
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
                        false,
                        &resolved,
                        move || cancel.set(false),
                    ))
                    .child(choice_button(
                        if multiple && !selected.is_empty() {
                            t!("project.pickSelected", n = selected.len().to_string())
                        } else {
                            t!("project.pickSelect")
                        },
                        true,
                        !can_select,
                        &resolved,
                        move || {
                            if !selected.is_empty() {
                                accept(selected.clone());
                            } else if let Some(listing) = &listing {
                                accept(vec![listing.path.clone()]);
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
