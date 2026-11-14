//! 文件浏览面板与文件编辑/沙盒预览窗口（ADR 0007 + ADR 0023）。
//!
//! - 右侧栏 [`FilesPanel`]：按当前活动项目展示目录项与搜索框，点击后在列式工作区打开 [`FilePaneView`]；
//! - 工作区 [`FilePaneView`]：
//!   1. 使用 `OpenFileBuffer` 保持原文件换行风格（LF / CRLF）与末尾换行形状，并在 `remote_changed_conflict`
//!      触发时弹出磁盘冲突保护横幅，拒绝静默覆盖；
//!   2. HTML 预览严格遵循 ADR 0007：仅允许加载 `/api/projects/<id>/raw/` 只读原始字节路由。

use falcon_core::pane_key::file_key;
use nsh_core::element::input;
use nsh_core::prelude::*;

use crate::fonts::FONT_FAMILY_MONO;
use crate::i18n::t;
use crate::store::use_workspace;
use crate::theme::{rgb_to_hsla, rgba_to_hsla, use_theme};
use crate::zoom::zpx;

/// 校验 HTML 预览地址是否严格位于 `/api/projects/<id>/raw/` 只读作用域路由下（ADR 0007）。
pub fn is_scoped_raw_preview_url(url: &str) -> bool {
    (url.starts_with("http://") || url.starts_with("https://"))
        && url.contains("/api/projects/")
        && url.contains("/raw/")
}

/// 右侧栏文件目录浏览面板。
#[component]
pub fn FilesPanel() -> impl IntoElement {
    let store = use_workspace();
    let theme_ctrl = use_theme();
    let filter_query = signal(String::new());

    view()
        .size_full()
        .flex_col()
        .overflow_hidden()
        .child(
            view()
                .flex_col()
                .gap(zpx(6.0))
                .p(zpx(10.0))
                .border_b_1()
                .border_color(move || rgba_to_hsla(theme_ctrl.resolved().ui.border))
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .child(
                            text(move || t!("files.panel"))
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(move || rgb_to_hsla(theme_ctrl.resolved().ui.foreground)),
                        ),
                )
                .child(
                    input()
                        .w_full()
                        .px(zpx(8.0))
                        .py(zpx(5.0))
                        .rounded(zpx(6.0))
                        .bg(move || rgba_to_hsla(theme_ctrl.resolved().ui.input))
                        .text_xs()
                        .text_color(move || rgb_to_hsla(theme_ctrl.resolved().ui.foreground))
                        .placeholder(move || t!("files.filterFiles"))
                        .placeholder_color(move || {
                            rgb_to_hsla(theme_ctrl.resolved().ui.muted_foreground)
                        })
                        .value(move || filter_query.get())
                        .on_change(move |v: &str| filter_query.set(v.to_string())),
                ),
        )
        .child(
            scroll()
                .flex_1()
                .child(dynamic(move || {
                    let resolved = theme_ctrl.resolved();
                    let ui = &resolved.ui;
                    let q = filter_query.get().trim().to_ascii_lowercase();
                    let proj_id = store.active_project_id.get();
                    let entries = store.file_entries.get();
                    let filtered: Vec<_> = entries
                        .into_iter()
                        .filter(|e| {
                            q.is_empty()
                                || e.name.to_ascii_lowercase().contains(&q)
                                || e.path.to_ascii_lowercase().contains(&q)
                        })
                        .collect();

                    if filtered.is_empty() {
                        return view()
                            .p(zpx(12.0))
                            .child(
                                text(t!("files.noMatchingFiles"))
                                    .text_xs()
                                    .text_color(rgb_to_hsla(ui.muted_foreground)),
                            )
                            .into_element();
                    }

                    view()
                        .flex_col()
                        .p(zpx(6.0))
                        .gap(zpx(2.0))
                        .children(filtered.into_iter().map(|entry| {
                            let st = store.clone();
                            let pid = proj_id.clone();
                            let path = entry.path.clone();
                            let size_kb = (entry.size_bytes as f64) / 1024.0;
                            view()
                                .flex_row()
                                .items_center()
                                .justify_between()
                                .px(zpx(8.0))
                                .py(zpx(6.0))
                                .rounded(zpx(6.0))
                                .cursor_pointer()
                                .on_press(move || {
                                    st.open_file_pane(&pid, &path);
                                })
                                .child(
                                    text(entry.name)
                                        .text_xs()
                                        .font_family(FONT_FAMILY_MONO)
                                        .text_color(rgb_to_hsla(ui.foreground)),
                                )
                                .child(
                                    text(format!("{size_kb:.1} KB"))
                                        .text_xs()
                                        .font_family(FONT_FAMILY_MONO)
                                        .text_color(rgb_to_hsla(ui.muted_foreground)),
                                )
                        }))
                        .into_element()
                })),
        )
}

/// 工作区文件编辑 / HTML 沙箱预览窗口属性。
#[derive(Clone, PartialEq, Props)]
pub struct FilePaneViewProps {
    pub project_id: String,
    pub path: String,
}

#[component]
pub fn FilePaneView(props: FilePaneViewProps) -> impl IntoElement {
    let store = use_workspace();
    let theme_ctrl = use_theme();
    let pane_key = file_key(&props.project_id, &props.path);
    let show_html_preview = signal(props.path.ends_with(".html") || props.path.ends_with(".htm"));

    view()
        .size_full()
        .flex_col()
        .overflow_hidden()
        .child(dynamic(move || {
            let resolved = theme_ctrl.resolved();
            let ui = &resolved.ui;
            let files = store.open_files.get();
            let Some(buf) = files.get(&pane_key).cloned() else {
                return view()
                    .p(zpx(12.0))
                    .child(
                        text(t!("files.viewFailed"))
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.muted_foreground)),
                    )
                    .into_element();
            };

            let dirty = buf.is_dirty();
            let has_preview = buf.raw_preview_url.is_some();
            let preview_on = has_preview && show_html_preview.get();

            let mut toolbar = view()
                .flex_row()
                .items_center()
                .justify_between()
                .px(zpx(10.0))
                .py(zpx(6.0))
                .border_b_1()
                .border_color(rgba_to_hsla(ui.border))
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .gap(zpx(8.0))
                        .child(
                            text(buf.path.clone())
                                .text_xs()
                                .font_family(FONT_FAMILY_MONO)
                                .text_color(rgb_to_hsla(ui.foreground)),
                        )
                        .child(
                            text(if dirty {
                                t!("native.nsh.fileModified")
                            } else {
                                t!("native.nsh.fileClean")
                            })
                            .text_xs()
                            .text_color(if dirty {
                                rgb_to_hsla(ui.warning)
                            } else {
                                rgb_to_hsla(ui.muted_foreground)
                            }),
                        ),
                );

            let mut actions = view().flex_row().items_center().gap(zpx(6.0));
            if has_preview {
                actions = actions.child(
                    view()
                        .px(zpx(8.0))
                        .py(zpx(3.0))
                        .rounded(zpx(5.0))
                        .cursor_pointer()
                        .bg(rgb_to_hsla(ui.secondary))
                        .on_press(move || show_html_preview.set(!preview_on))
                        .child(
                            text(if preview_on {
                                t!("files.source")
                            } else {
                                t!("files.preview")
                            })
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.secondary_foreground)),
                        ),
                );
            }

            {
                let st_save = store.clone();
                let pk_save = pane_key.clone();
                actions = actions.child(
                    view()
                        .px(zpx(8.0))
                        .py(zpx(3.0))
                        .rounded(zpx(5.0))
                        .cursor_pointer()
                        .bg(rgb_to_hsla(ui.tint))
                        .on_press(move || {
                            let _ = st_save.save_file_buffer(&pk_save);
                        })
                        .child(
                            text(t!("native.nsh.saveFile"))
                                .text_xs()
                                .text_color(rgb_to_hsla(ui.tint_foreground)),
                        ),
                );
            }

            toolbar = toolbar.child(actions);

            let mut page = view().size_full().flex_col().child(toolbar);

            if buf.remote_changed_conflict {
                page = page.child(
                    view()
                        .px(zpx(10.0))
                        .py(zpx(6.0))
                        .bg(rgb_to_hsla(ui.muted))
                        .child(
                            text(t!("native.nsh.fileConflictWarning"))
                                .text_xs()
                                .text_color(rgb_to_hsla(ui.warning)),
                        ),
                );
            }

            if preview_on {
                if let Some(raw_url) = buf.raw_preview_url.filter(|u| is_scoped_raw_preview_url(u))
                {
                    let notice_url = raw_url.clone();
                    page = page
                        .child(
                            view()
                                .px(zpx(10.0))
                                .py(zpx(4.0))
                                .bg(rgb_to_hsla(ui.muted))
                                .child(
                                    text(t!(
                                        "native.nsh.htmlSandboxNotice",
                                        url = notice_url.as_str()
                                    ))
                                    .text_xs()
                                    .text_color(rgb_to_hsla(ui.muted_foreground)),
                                ),
                        )
                        .child(web_view(raw_url).flex_1().w_full());
                } else {
                    page = page.child(
                        view().p(zpx(12.0)).child(
                            text(t!("native.files.htmlNoWebview"))
                                .text_xs()
                                .text_color(rgb_to_hsla(ui.muted_foreground)),
                        ),
                    );
                }
            } else {
                let st_edit = store.clone();
                let pk_edit = pane_key.clone();
                let current_text = buf.current_body.clone();
                page = page.child(
                    input()
                        .flex_1()
                        .w_full()
                        .p(zpx(10.0))
                        .multiline(true)
                        .auto_correct(false)
                        .auto_capitalize(AutoCapitalize::None)
                        .font_family(FONT_FAMILY_MONO)
                        .text_sm()
                        .bg(rgb_to_hsla(ui.background))
                        .text_color(rgb_to_hsla(ui.foreground))
                        .caret_color(rgb_to_hsla(ui.tint_strong))
                        .value(current_text)
                        .on_change(move |next: &str| {
                            st_edit.open_files.update(|map| {
                                if let Some(entry) = map.get_mut(&pk_edit) {
                                    entry.current_body = next.to_string();
                                }
                            });
                        }),
                );
            }

            page.into_element()
        }))
}
