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
use crate::shell::widgets::{Glyph, icon_button};
use crate::shell::popups::themed_tooltip;
use crate::store::{ActiveDialog, FileBufferState, FilePreviewKind, use_workspace};
use crate::theme::{rgb_to_hsla, rgba_to_hsla, use_theme};
use crate::views::file_operations::file_operations_toolbar;
use crate::views::file_operations::{selection_icon, selection_indicator};
use crate::zoom::zpx;
use super::file_preview::{image_preview, markdown_preview};

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
    let show_hidden = signal(false);
    let table_width = signal(0.0_f32);

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
                    view().flex_row().items_center().justify_between().child(
                        text({
                            let st = store.clone();
                            move || {
                                st.projects
                                    .get()
                                    .into_iter()
                                    .find(|p| p.id == st.active_project_id.get())
                                    .map(|p| p.path)
                                    .unwrap_or_else(|| t!("files.panel"))
                            }
                        })
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(move || rgb_to_hsla(theme_ctrl.resolved().ui.foreground)),
                    ),
                )
                .child(dynamic({
                    let st = store.clone();
                    move || {
                        let resolved = theme_ctrl.resolved();
                        let dir = st.file_directory.get();
                        let refresh = st.clone();
                        let up = st.clone();
                        let parent = falcon_core::file_path::parent_nav_path(&dir);
                        let parent_disabled = parent.is_none() || st.files_loading.get();
                        let refresh_disabled = st.files_loading.get();
                        view()
                            .flex_row()
                            .items_center()
                            .gap(zpx(4.0))
                            .child(
                                view()
                                    .when(parent_disabled, |button| button.opacity(0.45))
                                    .child(icon_button(
                                        "files-parent",
                                        t!("files.parent"),
                                        Glyph::ArrowUp,
                                        false,
                                        &resolved,
                                        move || {
                                            if !up.files_loading.get() {
                                                let current_dir = up.file_directory.get();
                                                if let Some(parent) =
                                                    falcon_core::file_path::parent_nav_path(
                                                        &current_dir,
                                                    )
                                                {
                                                    up.browse_directory(&parent);
                                                }
                                            }
                                        },
                                    )),
                            )
                            .child(
                                text(if dir.is_empty() { "/".to_string() } else { dir })
                                    .flex_1()
                                    .min_w_0()
                                    .text_xs()
                                    .number_of_lines(1)
                                    .text_color(rgb_to_hsla(resolved.ui.muted_foreground)),
                            )
                            .child(icon_button(
                                "files-hidden",
                                if show_hidden.get() {
                                    t!("files.hideHidden")
                                } else {
                                    t!("files.showHidden")
                                },
                                Glyph::Search,
                                show_hidden.get(),
                                &resolved,
                                move || show_hidden.update(|v| *v = !*v),
                            ))
                            .child(
                                view()
                                    .when(refresh_disabled, |button| button.opacity(0.45))
                                    .child(icon_button(
                                        "files-refresh",
                                        t!("files.refresh"),
                                        Glyph::Refresh,
                                        false,
                                        &resolved,
                                        move || {
                                            if !refresh.files_loading.get() {
                                                let current_dir = refresh.file_directory.get();
                                                refresh.browse_directory(&current_dir);
                                            }
                                        },
                                    )),
                            )
                    }
                }))
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
        .child(file_operations_toolbar())
        .child(scroll().flex_1().on_layout(move |bounds: &Bounds<Points>| {
            table_width.set(bounds.size.width.0);
        }).child(dynamic(move || {
            let resolved = theme_ctrl.resolved();
            let ui = &resolved.ui;
            // 与 GPUI 一样，窄面板优先给名称留空间；时间列在面板展开后出现。
            let show_size = table_width.get() >= zpx(300.0).0;
            let show_mtime = table_width.get() >= zpx(360.0).0;
            let q = filter_query.get().trim().to_ascii_lowercase();
            let proj_id = store.active_project_id.get();
            if store.files_loading.get() {
                return text(t!("files.loading"))
                    .test_id("files-loading")
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.muted_foreground))
                    .into_element();
            }
            if let Some(error) = store.file_error.get() {
                return view()
                    .p(zpx(12.0))
                    .child(
                        text(format!("{}: {error}", t!("files.loadFailed")))
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.destructive)),
                    )
                    .into_element();
            }
            let entries = store.file_entries.get();
            let filtered: Vec<_> = entries
                .into_iter()
                .filter(|e| {
                    q.is_empty()
                        || e.name.to_ascii_lowercase().contains(&q)
                        || e.path.to_ascii_lowercase().contains(&q)
                })
                .filter(|e| show_hidden.get() || !e.name.starts_with('.'))
                .collect();

            if filtered.is_empty() {
                return view()
                    .p(zpx(12.0))
                    .child(
                        text(if q.is_empty() {
                            t!("files.empty")
                        } else {
                            t!("files.noMatchingFiles")
                        })
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.muted_foreground)),
                    )
                    .into_element();
            }

            let visible_paths: Vec<String> =
                filtered.iter().map(|entry| entry.path.clone()).collect();
            let selected_paths = store.file_operations.selected.get();
            let selected_visible = visible_paths
                .iter()
                .filter(|path| selected_paths.contains(*path))
                .count();
            let all_visible_selected = selected_visible == visible_paths.len();
            let bulk_store = store.clone();
            let bulk_paths = visible_paths.clone();
            let bulk_label = if all_visible_selected {
                t!("files.operations.deselectVisible")
            } else {
                t!("files.operations.selectVisible")
            };

            view()
                .flex_col()
                .p(zpx(6.0))
                .gap(zpx(2.0))
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .gap(zpx(6.0))
                        .px(zpx(5.0))
                        .py(zpx(4.0))
                        .child(
                            view()
                                .w(zpx(24.0))
                                .flex_none()
                                .flex_row()
                                .items_center()
                                .gap(zpx(6.0))
                                .cursor_pointer()
                                .on_press(move || {
                                    bulk_store.file_operations.select_visible(&bulk_paths)
                                })
                                .child(selection_indicator(all_visible_selected, &resolved))
                                .tooltip(themed_tooltip(text(bulk_label), None, &resolved)),
                        )
                        .child(
                            text(t!("native.files.name"))
                            .flex_1().min_w_0()
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.muted_foreground)),
                        )
                        .when(show_size, |row| row.child(text(t!("native.files.size")).w(zpx(56.0)).flex_none().text_xs()
                            .text_color(rgb_to_hsla(ui.muted_foreground))))
                        .when(show_mtime, |row| row.child(text(t!("native.files.mtime")).w(zpx(152.0)).flex_none().text_xs()
                            .text_color(rgb_to_hsla(ui.muted_foreground)))),
                )
                .children(filtered.into_iter().map(|entry| {
                    let st = store.clone();
                    let pid = proj_id.clone();
                    let path = entry.path.clone();
                    let is_dir = entry.is_dir;
                    let size_label = falcon_core::file_path::format_size(Some(entry.size_bytes as f64));
                    let mtime = format_mtime(entry.mtime);
                    let selected = selected_paths.contains(&path);
                    let checkbox_store = store.file_operations.clone();
                    let checkbox_path = path.clone();
                    let checkbox_visible = visible_paths.clone();
                    let row_store = store.file_operations.clone();
                    let row_path = path.clone();
                    let row_visible = visible_paths.clone();
                    let file_open_blocked = !is_dir && row_store.blocks_file_open(&store);
                    view()
                        .flex_row()
                        .items_center()
                        .px(zpx(8.0))
                        .py(zpx(6.0))
                        .rounded(zpx(6.0))
                        .gap(zpx(6.0))
                        .text_color(rgb_to_hsla(ui.muted_foreground))
                        .hover(|s| s.bg(rgb_to_hsla(ui.muted)))
                        .child(
                            view()
                                .test_id(&format!("file-select-{}", entry.name))
                                .w(zpx(24.0))
                                .h(zpx(24.0))
                                .flex_none()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .on_press(move |event: &PressEvent| {
                                    if event.modifiers.shift {
                                        checkbox_store
                                            .select_range(&checkbox_path, &checkbox_visible);
                                    } else {
                                        checkbox_store.toggle_selected(&checkbox_path);
                                    }
                                })
                                .child(selection_indicator(selected, &resolved)),
                        )
                        .child(
                            view()
                                .flex_row()
                                .flex_1()
                                .min_w_0()
                                .items_center()
                                .gap(zpx(6.0))
                                .cursor_pointer()
                                .on_press(move |event: &PressEvent| {
                                    if event.modifiers.shift {
                                        row_store.select_range(&row_path, &row_visible);
                                    } else if event.modifiers.cmd || event.modifiers.ctrl {
                                        row_store.toggle_selected(&row_path);
                                    } else if is_dir {
                                        st.browse_directory(&path);
                                    } else if row_store.blocks_file_open(&st) {
                                        st.show_toast(row_store.file_open_lock_message(&st).unwrap_or_else(
                                            || t!("files.operations.busy").to_string(),
                                        ));
                                    } else {
                                        st.open_file_pane(&pid, &path);
                                    }
                                })
                                .child(selection_icon(is_dir))
                                .child(
                                    text(entry.name)
                                        .flex_1()
                                        .min_w_0()
                                        .number_of_lines(1)
                                        .text_xs()
                                        .font_family(FONT_FAMILY_MONO)
                                        .text_color(rgb_to_hsla(ui.foreground)),
                                )
                                .when(file_open_blocked, |row| row.opacity(0.45)),
                        )
                        .when(show_size, |row| row.child(
                            text(if is_dir {
                                String::new()
                            } else {
                                size_label
                            })
                            .w(zpx(56.0)).flex_none()
                            .text_xs()
                            .font_family(FONT_FAMILY_MONO)
                            .text_color(rgb_to_hsla(ui.muted_foreground)),
                        ))
                        .when(show_mtime, |row| row.child(
                            text(mtime).w(zpx(152.0)).flex_none().text_xs()
                                .font_family(FONT_FAMILY_MONO)
                                .text_color(rgb_to_hsla(ui.muted_foreground)),
                        ))
                }))
                .into_element()
        })))
}

/// 工作区文件编辑 / HTML 沙箱预览窗口属性。
#[derive(Clone, PartialEq, Props)]
pub struct FilePaneViewProps {
    pub project_id: String,
    pub path: String,
    #[prop(default)]
    pub expanded: bool,
}

#[component]
pub fn FilePaneView(props: FilePaneViewProps) -> impl IntoElement {
    let store = use_workspace();
    let theme_ctrl = use_theme();
    let pane_key = file_key(&props.project_id, &props.path);
    let markdown_file = props.path.ends_with(".md") || props.path.ends_with(".markdown");
    let show_markdown_preview = signal(markdown_file);
    let show_html_preview = signal(props.path.ends_with(".html") || props.path.ends_with(".htm"));
    let editor_ready = signal(false);
    let editor_generation: Signal<Option<u64>> = signal(None);
    let editor_initial_body = signal(String::new());
    let editor_store = store.clone();
    let editor_key = pane_key.clone();
    let editor_theme = theme_ctrl.clone();

    view()
        .size_full()
        .flex_col()
        .overflow_hidden()
        .child(dynamic(move || {
            let resolved = theme_ctrl.resolved();
            let ui = &resolved.ui;
            let files = store.open_files.get();
            let Some(buf) = files.get(&pane_key).cloned() else {
                editor_ready.set(false);
                return view()
                    .p(zpx(12.0))
                    .child(
                        text(t!("files.viewFailed"))
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.muted_foreground)),
                    )
                    .into_element();
            };
            let preview_active = (buf.raw_preview_url.is_some() && show_html_preview.get())
                || (markdown_file && show_markdown_preview.get());
            // 挂起背景编辑器，浮层关闭时以共享缓冲的最新内容重新建立原生输入。
            let covered = !props.expanded && store.active_dialog.get() == ActiveDialog::PreviewPopup;
            if buf.editable() && !preview_active && !covered {
                if !editor_ready.get() || editor_generation.get() != Some(buf.generation) {
                    editor_initial_body.set(buf.current_body.clone());
                    editor_generation.set(Some(buf.generation));
                    editor_ready.set(true);
                }
            } else {
                editor_ready.set(false);
            }

            let st_save_shortcut = store.clone();
            let shortcut_key = pane_key.clone();
            if (store.focused_pane_key.get() == pane_key
                && store.active_dialog.get() == ActiveDialog::None)
                || (props.expanded && store.active_dialog.get() == ActiveDialog::PreviewPopup)
            {
                crate::terminal::bind_safe_key_when(
                    "mod+s",
                    false,
                    nsh_core::keymap::When::Always,
                    move || {
                        let _ = st_save_shortcut.save_file_buffer(&shortcut_key);
                    },
                );
            }

            let dirty = buf.is_dirty();
            let has_preview = buf.raw_preview_url.is_some()
                && (buf.path.ends_with(".html") || buf.path.ends_with(".htm"));
            let preview_on = has_preview && show_html_preview.get();
            let can_save = buf.editable() && dirty && !buf.saving && !buf.remote_changed_conflict;
            let readonly_notice = if buf.state == FileBufferState::ReadOnly {
                Some(match buf.preview_kind.as_ref() {
                    Some(FilePreviewKind::Text {
                        truncated: true, ..
                    }) => t!("native.nsh.fileTruncatedReadonly"),
                    Some(FilePreviewKind::Text {
                        has_version: false, ..
                    }) => t!("native.nsh.fileVersionMissingReadonly"),
                    Some(FilePreviewKind::Image { mime, size }) => t!(
                        "native.nsh.fileImageReadonly",
                        mime = mime.as_str(),
                        size = *size
                    ),
                    Some(FilePreviewKind::Binary { size }) => {
                        t!("native.nsh.fileBinaryReadonly", size = *size)
                    }
                    Some(FilePreviewKind::TooLarge { size }) => {
                        t!("native.nsh.fileTooLargeReadonly", size = *size)
                    }
                    Some(FilePreviewKind::Unknown) | None => t!("native.nsh.fileUnknownReadonly"),
                    _ => t!("native.nsh.fileUnknownReadonly"),
                })
            } else {
                None
            };

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
            if !props.expanded {
                let expand = store.clone();
                let project = buf.project_id.clone();
                let path = buf.path.clone();
                actions = actions.child(icon_button(
                    format!("file-expand-{pane_key}"), t!("native.preview.expandFile"), Glyph::Maximize, false,
                    &resolved, move || crate::views::preview_popup::open_file(&expand, &project, &path),
                ));
            }
            let reload_store = store.clone();
            let reload_key = pane_key.clone();
            actions = actions.child(icon_button(
                "file-reload", t!("native.files.refresh"), Glyph::Refresh, false, &resolved,
                move || reload_store.request_reload_file(&reload_key),
            ));
            if let Some(raw) = buf.raw_preview_url.clone().filter(|u| is_scoped_raw_preview_url(u)) {
                if has_preview || matches!(buf.preview_kind, Some(FilePreviewKind::Image { .. })) {
                    actions = actions.child(icon_button(
                        "file-open-raw", t!("native.files.openInBrowser"), Glyph::Link,
                        false, &resolved, move || { drop(nsh_core::element::open_url(raw.clone())); },
                    ));
                }
            }
            if markdown_file {
                actions = actions.child(
                    view()
                        .test_id(&format!("file-markdown-preview-{pane_key}"))
                        .px(zpx(8.0))
                        .py(zpx(3.0))
                        .rounded(zpx(8.0))
                        .cursor_pointer()
                        .bg(rgb_to_hsla(ui.secondary))
                        .on_press(move || show_markdown_preview.update(|v| *v = !*v))
                        .child(
                            text(if show_markdown_preview.get() {
                                t!("files.source")
                            } else {
                                t!("files.preview")
                            })
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.foreground)),
                        ),
                );
            }
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

            if buf.editable() {
                let st_save = store.clone();
                let pk_save = pane_key.clone();
                let mut save_button = view()
                    .test_id(&format!("file-save-{pane_key}"))
                    .px(zpx(8.0))
                    .py(zpx(3.0))
                    .rounded(zpx(5.0))
                    .bg(rgb_to_hsla(ui.tint));
                if can_save {
                    save_button = save_button.cursor_pointer().on_press(move || {
                        let _ = st_save.save_file_buffer(&pk_save);
                    });
                }
                actions = actions.child(
                    save_button.child(
                        text(if buf.saving {
                            t!("native.nsh.fileSaving")
                        } else {
                            t!("native.nsh.saveFile")
                        })
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.tint_foreground)),
                    ),
                );
            }

            toolbar = toolbar.child(actions);

            let mut page = view().flex_col().min_h_0()
                .when(!buf.editable() || preview_active, |page| page.flex_1())
                .child(toolbar);

            if let Some(notice) = readonly_notice {
                page = page.child(
                    view()
                        .px(zpx(10.0))
                        .py(zpx(6.0))
                        .bg(rgb_to_hsla(ui.muted))
                        .child(text(notice).text_xs().text_color(rgb_to_hsla(ui.warning))),
                );
            }

            if buf.remote_changed_conflict {
                page = page.child(
                    view()
                        .px(zpx(10.0))
                        .py(zpx(6.0))
                        .bg(rgb_to_hsla(ui.muted))
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .child(
                            text(t!("native.nsh.fileConflictWarning"))
                                .text_xs()
                                .text_color(rgb_to_hsla(ui.warning)),
                        )
                        .child(
                            view()
                                .flex_row()
                                .gap(zpx(6.0))
                                .child(
                                    view()
                                        .px(zpx(8.0))
                                        .py(zpx(3.0))
                                        .rounded(zpx(5.0))
                                        .cursor_pointer()
                                        .bg(rgb_to_hsla(ui.secondary))
                                        .on_press({
                                            let st = store.clone();
                                            let key = pane_key.clone();
                                            move || st.request_reload_file(&key)
                                        })
                                        .child(
                                            text(t!("native.nsh.reloadFile"))
                                                .text_xs()
                                                .text_color(rgb_to_hsla(ui.secondary_foreground)),
                                        ),
                                )
                                .child({
                                    let st = store.clone();
                                    let key = pane_key.clone();
                                    let mut button = view()
                                        .px(zpx(8.0))
                                        .py(zpx(3.0))
                                        .rounded(zpx(5.0))
                                        .bg(rgb_to_hsla(ui.destructive));
                                    if !buf.saving {
                                        button = button.cursor_pointer().on_press(move || {
                                            let _ = st.overwrite_file_buffer(&key);
                                        });
                                    }
                                    button.child(
                                        text(t!("native.nsh.overwriteFile"))
                                            .text_xs()
                                            .text_color(rgb_to_hsla(ui.destructive_foreground)),
                                    )
                                }),
                        ),
                );
            }

            if let Some(error) = buf.save_error.clone().or(buf.load_error.clone()) {
                page = page.child(
                    view()
                        .px(zpx(10.0))
                        .py(zpx(5.0))
                        .bg(rgb_to_hsla(ui.muted))
                        .child(text(error).text_xs().text_color(rgb_to_hsla(ui.warning))),
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
                        .child(web_view(raw_url).incognito(true).flex_1().w_full());
                } else {
                    page = page.child(
                        view().p(zpx(12.0)).child(
                            text(t!("native.files.htmlNoWebview"))
                                .text_xs()
                                .text_color(rgb_to_hsla(ui.muted_foreground)),
                        ),
                    );
                }
            } else if buf.state == FileBufferState::Loading {
                page = page.child(
                    view().p(zpx(12.0)).child(
                        text(t!("native.nsh.fileLoading"))
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.muted_foreground)),
                    ),
                );
            } else if buf.state == FileBufferState::Failed {
                page = page.child(
                    view().p(zpx(12.0)).child(
                        text(t!("native.nsh.fileLoadFailed"))
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.warning)),
                    ),
                );
                let st_reload = store.clone();
                let key_reload = pane_key.clone();
                page = page.child(
                    view().px(zpx(12.0)).pb(zpx(8.0)).child(
                        view()
                            .px(zpx(8.0))
                            .py(zpx(4.0))
                            .rounded(zpx(5.0))
                            .cursor_pointer()
                            .bg(rgb_to_hsla(ui.secondary))
                            .on_press(move || st_reload.request_reload_file(&key_reload))
                            .child(
                                text(t!("native.nsh.reloadFile"))
                                    .text_xs()
                                    .text_color(rgb_to_hsla(ui.secondary_foreground)),
                            ),
                    ),
                );
            } else if matches!(buf.preview_kind, Some(FilePreviewKind::Image { .. })) {
                if let Some(url) = buf.raw_preview_url.clone().filter(|u| is_scoped_raw_preview_url(u)) {
                    page = page.child(image_preview().url(url));
                } else {
                    page = page.child(view().p(zpx(12.0)).child(text(t!("native.files.imageFailed"))
                        .text_xs().text_color(rgb_to_hsla(ui.muted_foreground))));
                }
            } else if markdown_file && show_markdown_preview.get() {
                page = page.child(markdown_preview()
                    .source(buf.current_body.clone())
                    .project_id(buf.project_id.clone())
                    .path(buf.path.clone())
                    .raw_url(buf.raw_preview_url.clone()));
            } else if !buf.editable() && !buf.current_body.is_empty() {
                page = page.child(
                    scroll().flex_1().min_h_0().child(
                        text(buf.current_body.clone())
                            .p(zpx(12.0))
                            .font_family(FONT_FAMILY_MONO)
                            .text_sm()
                            .text_color(rgb_to_hsla(ui.foreground))
                            .selectable(true),
                    ),
                );
            }

            page.into_element()
        }))
        .child(dynamic(move || {
            if !editor_ready.get() {
                return view().into_element();
            }
            let current_text = editor_initial_body.get();
            let ui = &editor_theme.resolved().ui;
            let st_edit = editor_store.clone();
            let pk_edit = editor_key.clone();
            input()
                .test_id(&format!("file-editor-{pk_edit}"))
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
                            if entry.current_body != next {
                                entry.current_body = next.to_string();
                                entry.edit_revision = entry.edit_revision.wrapping_add(1);
                            }
                        }
                    });
                })
                .into_element()
        }))
}

fn format_mtime(sec: Option<i64>) -> String {
    use chrono::{Local, TimeZone};
    let offset = sec.and_then(|s| Local.timestamp_opt(s, 0).earliest())
        .map_or(0, |dt| i64::from(dt.offset().local_minus_utc()));
    falcon_core::file_path::format_mtime(sec.map(|s| s as f64), offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nsh_core::element::Root;
    use nsh_engine::EngineConfig;
    use nsh_testing::Headless;

    #[component]
    fn ImagePaneFixture() -> impl IntoElement {
        crate::theme::install_theme(None);
        crate::zoom::install_zoom();
        crate::shell::action_owner::ActionOwner::install();
        let store = crate::store::WorkspaceStore::install(&crate::FalconLaunchOptions {
            offline_demo: true, ..Default::default()
        });
        store.open_files.update(|files| {
            files.insert(file_key("p", "image.png"), crate::store::OpenFileBuffer::from_preview(
                "p", "image.png", 1,
                falcon_proto::FilePreview::Image { mime: "image/png".into(), size: 100 },
                Some("https://example.test/api/projects/p/raw/token/image.png".into()),
            ));
        });
        view().size_full().child(file_pane_view().project_id("p").path("image.png"))
    }

    #[test]
    fn loaded_image_keeps_the_full_file_pane_viewport_headless() {
        let mut config = EngineConfig::new(Root::of::<ImagePaneFixture>().translations(crate::i18n::translations()));
        config.viewport = Size::new(Points(848.0), Points(680.0));
        config.ui_sets = vec![nsh_base::registry(), crate::registry()];
        let mut app = Headless::start(config).unwrap();
        app.first_render().unwrap();
        let image = app.screen().layout_nodes().into_iter()
            .find(|n| n.test_id.as_deref() == Some("file-image")).unwrap();
        app.engine().control().dispatch_event(image.id, nsh_core::EventKind::Load,
            nsh_core::EventPayload::Map(vec![
                ("width", nsh_core::PayloadValue::Number(1600.0)),
                ("height", nsh_core::PayloadValue::Number(1200.0)),
            ]));
        app.wait_until("loaded image fills the file pane", std::time::Duration::from_secs(2), |screen| {
            screen.layout_nodes().iter().any(|node| node.id == image.id && node.bounds.size.height.0 > 300.0)
        }).unwrap();
        let viewport = app.screen().layout_nodes().into_iter()
            .find(|n| n.test_id.as_deref() == Some("file-image-viewport")).unwrap();
        assert!(viewport.bounds.size.height.0 > 400.0);
        assert!(viewport.bounds.origin.y.0 + viewport.bounds.size.height.0 <= 680.0);
    }

    #[component]
    fn FilesFixture() -> impl IntoElement {
        crate::theme::install_theme(None);
        crate::zoom::install_zoom();
        crate::shell::action_owner::ActionOwner::install();
        let store = crate::store::WorkspaceStore::install(&crate::FalconLaunchOptions {
            offline_demo: true, ..Default::default()
        });
        store.file_entries.set(vec![crate::store::FileEntryItem {
            name: "readme.md".into(), path: "docs/readme.md".into(), is_dir: false,
            size_bytes: 2048, mtime: Some(1_700_000_000), is_html: false,
        }]);
        view().size_full().child(files_panel())
    }

    #[test]
    fn file_columns_reveal_size_then_mtime_as_the_panel_widens_headless() {
        let mut config = EngineConfig::new(Root::of::<FilesFixture>().translations(crate::i18n::translations()));
        config.viewport = Size::new(Points(280.0), Points(700.0));
        config.ui_sets = vec![nsh_base::registry(), crate::registry()];
        let mut app = Headless::start(config).unwrap();
        app.first_render().unwrap();
        app.wait_for("text:readme.md").unwrap();
        assert!(!app.screen().matches("text:Size").unwrap());
        assert!(!app.screen().matches("text:Modified").unwrap());
        app.set_viewport(Size::new(Points(320.0), Points(700.0))).unwrap();
        app.wait_for("text:Size").unwrap();
        assert!(!app.screen().matches("text:Modified").unwrap());
        app.set_viewport(Size::new(Points(460.0), Points(700.0))).unwrap();
        app.wait_for("text:Modified").unwrap();
        app.wait_for(&format!("text:{}", format_mtime(Some(1_700_000_000)))).unwrap();
        assert!(app.screen().matches("id:file-select-readme.md").unwrap());
    }
}
