//! 文件与 Git 差异的全局放大层。内容复用工作区视图，缓冲和异步请求仍归 WorkspaceStore。

use std::collections::HashSet;

use falcon_core::command_palette::move_selection;
use falcon_core::file_path::{is_hidden_name, parent_nav_path};
use falcon_core::file_search::{FILE_SEARCH_LIMIT, filter_files};
use falcon_core::file_tree::{TreeNode, build_file_tree};
use falcon_core::layout::find_pane;
use falcon_core::pane_key::file_key;
use nsh_core::element::input;
use nsh_core::prelude::*;

use crate::i18n::t;
use crate::shell::widgets::{Glyph, glyph, icon_button};
use crate::store::{ActiveDialog, WorkspaceStore, use_workspace};
use crate::terminal::bind_safe_key_when;
use crate::theme::{rgb_to_hsla, rgba_to_hsla, use_theme};
use crate::zoom::zpx;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PreviewTarget {
    pub project_id: String,
    /// None 为 Git 差异；所选差异路径继续由 selected_git_path 管理。
    pub path: Option<String>,
    /// 仅由本次浮层加载的缓冲；原有工作区/缓存文件不归浮层回收。
    temporary_files: HashSet<String>,
}

pub(crate) fn open_file(store: &WorkspaceStore, project_id: &str, path: &str) {
    let existing = store
        .open_files
        .get_untracked()
        .contains_key(&file_key(project_id, path));
    if project_id != store.active_project_id.get() || !store.ensure_file_buffer(project_id, path) {
        return;
    }
    store.preview_popup.set(Some(PreviewTarget {
        project_id: project_id.to_string(),
        path: Some(path.to_string()),
        temporary_files: if existing {
            HashSet::new()
        } else {
            HashSet::from([path.to_string()])
        },
    }));
    store.load_quick_open_index(project_id);
    store.active_dialog.set(ActiveDialog::PreviewPopup);
}

pub(crate) fn open_diff(store: &WorkspaceStore) {
    store.preview_popup.set(Some(PreviewTarget {
        project_id: store.active_project_id.get(),
        path: None,
        temporary_files: HashSet::new(),
    }));
    store.active_dialog.set(ActiveDialog::PreviewPopup);
}

pub(crate) fn close(store: &WorkspaceStore) {
    // 浮层不是缓冲的所有者。把脏文件留在工作区，关层和切换文件都不会丢编辑；
    // 退出、切服务端、刷新和保存冲突继续走现有 ADR 0023 护栏。
    if let Some(target) = store.preview_popup.get_untracked() {
        let dirty: Vec<_> = store
            .open_files
            .get_untracked()
            .values()
            .filter(|buffer| {
                buffer.project_id == target.project_id && (buffer.is_dirty() || buffer.saving)
            })
            .map(|buffer| buffer.path.clone())
            .collect();
        for path in dirty {
            store.open_file_pane(&target.project_id, &path);
        }
        // 未编辑的临时预览不再算「已打开文件」，否则文件操作会一直要求先关闭。
        // 正在加载的缓冲也可回收：读取响应只更新仍存在且 generation 相符的缓冲。
        let columns = store.columns.get_untracked();
        store.open_files.update(|files| {
            files.retain(|key, buffer| {
                buffer.project_id != target.project_id
                    || !target.temporary_files.contains(&buffer.path)
                    || buffer.is_dirty()
                    || buffer.saving
                    || find_pane(&columns, key).is_some()
            })
        });
    }
    store.preview_popup.set(None);
    store.active_dialog.set(ActiveDialog::None);
}

fn activate(store: &WorkspaceStore, path: &str, directory: bool) {
    let Some(mut target) = store.preview_popup.get_untracked() else {
        return;
    };
    if store.active_dialog.get_untracked() != ActiveDialog::PreviewPopup
        || target.project_id != store.active_project_id.get_untracked()
    {
        return;
    }
    if directory {
        store.browse_directory(path);
    } else if target.path.is_none() {
        store.preview_git_path(path);
    } else {
        let existing = store
            .open_files
            .get_untracked()
            .contains_key(&file_key(&target.project_id, path));
        if store.ensure_file_buffer(&target.project_id, path) {
            if !existing {
                target.temporary_files.insert(path.to_string());
            }
            target.path = Some(path.to_string());
            store.preview_popup.set(Some(target));
        }
    }
}

#[component]
pub(crate) fn PreviewPopup() -> impl IntoElement {
    let store = use_workspace();
    let theme = use_theme();
    let target = store.preview_popup.get_untracked();
    let Some(target) = target else {
        return view().into_element();
    };
    let diff = target.path.is_none();
    // 文件栏开合归 store：差异头部那枚开关（GPUI popup chrome）与浮层头部这枚共用。
    let sidebar = store.preview_sidebar_open;
    let query = signal(String::new());
    let tree = signal(diff);
    let hidden = signal(false);
    let collapsed = signal(HashSet::<String>::new());
    let cursor = signal(0usize);
    let search_focused = signal(false);
    let search_ref = element_ref();
    let watch_store = store.clone();
    effect(move || {
        // 项目上下文变更后不得把旧索引/差异应用到新项目。
        if watch_store.active_project_id.get() != target.project_id
            && watch_store.active_dialog.get_untracked() == ActiveDialog::PreviewPopup
        {
            close(&watch_store);
        }
    });
    let close_store = store.clone();
    let content_store = store.clone();
    let resolved = theme.resolved();
    let header = view()
        .flex_none()
        .flex_row()
        .items_center()
        .justify_between()
        .px(zpx(10.0))
        .py(zpx(6.0))
        .border_b_1()
        .border_color(rgba_to_hsla(resolved.ui.border))
        .child(icon_button(
            "preview-sidebar",
            t!("native.preview.toggleSidebar"),
            Glyph::Sidebar,
            false,
            &resolved,
            move || sidebar.update(|open| *open = !*open),
        ))
        .child(
            text(if diff {
                t!("native.preview.diffTitle")
            } else {
                t!("native.preview.fileTitle")
            })
            .text_sm()
            .text_color(rgb_to_hsla(resolved.ui.foreground)),
        )
        .child(icon_button(
            "preview-close",
            t!("native.preview.close"),
            Glyph::Close,
            false,
            &resolved,
            move || close(&close_store),
        ));
    view()
        .test_id("preview-popup")
        .size_full()
        .flex_col()
        .overflow_hidden()
        .child(header)
        .child(
            view()
                .flex_1()
                .min_h_0()
                .flex_row()
                .child(dynamic(move || {
                    if !sidebar.get() {
                        return view().into_element();
                    }
                    let resolved = theme.resolved();
                    let ui = &resolved.ui;
                    let input_store = store.clone();
                    let list_store = store.clone();
                    let mut tools =
                        view()
                            .flex_row()
                            .items_center()
                            .gap(zpx(4.0))
                            .child(icon_button(
                                "preview-tree",
                                t!("native.preview.toggleTree"),
                                Glyph::Folder,
                                tree.get(),
                                &resolved,
                                move || tree.update(|value| *value = !*value),
                            ));
                    if !diff {
                        let up = store.clone();
                        let refresh = store.clone();
                        tools = tools
                            .child(icon_button(
                                "preview-parent",
                                t!("native.preview.parent"),
                                Glyph::ArrowUp,
                                false,
                                &resolved,
                                move || {
                                    if let Some(parent) = parent_nav_path(&up.file_directory.get())
                                    {
                                        up.browse_directory(&parent);
                                    }
                                },
                            ))
                            .child(icon_button(
                                "preview-hidden",
                                t!("native.preview.toggleHidden"),
                                Glyph::Search,
                                hidden.get(),
                                &resolved,
                                move || hidden.update(|value| *value = !*value),
                            ))
                            .child(icon_button(
                                "preview-refresh",
                                t!("native.preview.refresh"),
                                Glyph::Refresh,
                                false,
                                &resolved,
                                move || {
                                    refresh.browse_directory(&refresh.file_directory.get());
                                    refresh.load_quick_open_index(&refresh.active_project_id.get());
                                },
                            ));
                    } else {
                        let refresh = store.clone();
                        tools = tools.child(icon_button(
                            "preview-refresh",
                            t!("native.preview.refresh"),
                            Glyph::Refresh,
                            false,
                            &resolved,
                            move || refresh.refresh_git_data(),
                        ));
                    }
                    let mut panel = view()
                        .test_id("preview-file-sidebar")
                        .w(zpx(264.0))
                        .max_w(relative(0.45))
                        .flex_none()
                        .flex_col()
                        .min_h_0()
                        .bg(rgb_to_hsla(ui.card))
                        .p(zpx(8.0))
                        .gap(zpx(6.0))
                        .child(tools);
                    if diff {
                        // 成员仓库切换复用 NSH 弹出菜单的所有权与服务端上下文护栏。
                        let repos = store.git_repos_for_active_project();
                        if !repos.is_empty() {
                            let guard = crate::shell::popups::ServerUrlGuard::capture(&store);
                            let actions = repos
                                .into_iter()
                                .map(|repo| {
                                    let st = store.clone();
                                    crate::shell::popups::PopupAction::new(
                                        guard.clone(),
                                        repo.clone(),
                                        false,
                                        false,
                                        move || st.select_git_repo(&repo),
                                    )
                                })
                                .collect();
                            panel =
                                panel.child(crate::shell::popups::overflow_menu(
                                    view()
                                        .test_id("preview-repo")
                                        .focusable(true)
                                        .cursor_pointer()
                                        .p(zpx(6.0))
                                        .child(
                                            text(store.selected_git_repo.get().unwrap_or_else(
                                                || t!("native.preview.repository"),
                                            ))
                                            .text_xs()
                                            .text_color(rgb_to_hsla(ui.foreground)),
                                        ),
                                    actions,
                                    &resolved,
                                ));
                        }
                    }
                    panel = panel.child(
                        input()
                            .test_id("preview-search")
                            .element_ref(&search_ref)
                            .auto_focus(true)
                            .w_full()
                            .px(zpx(8.0))
                            .py(zpx(6.0))
                            .rounded(zpx(6.0))
                            .bg(rgba_to_hsla(ui.input))
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.foreground))
                            .placeholder(t!("native.preview.filter"))
                            .value(move || query.get())
                            .on_focus(move || search_focused.set(true))
                            .on_blur(move || search_focused.set(false))
                            .on_change(move |value: &str| {
                                query.set(value.to_string());
                                cursor.set(0);
                                if !diff
                                    && input_store.quick_open_project.get()
                                        != input_store.active_project_id.get()
                                {
                                    input_store.load_quick_open_index(
                                        &input_store.active_project_id.get(),
                                    );
                                }
                            }),
                    );
                    panel
                        .child(dynamic(move || {
                            render_list(
                                &list_store,
                                diff,
                                query,
                                tree,
                                hidden,
                                collapsed,
                                cursor,
                                search_focused,
                                search_ref,
                            )
                        }))
                        .into_element()
                }))
                .child(view().flex_1().min_w_0().min_h_0().child(dynamic(move || {
                    let Some(target) = content_store.preview_popup.get() else {
                        return view().into_element();
                    };
                    if let Some(path) = target.path {
                        super::files::file_pane_view()
                            .project_id(target.project_id)
                            .path(path)
                            .expanded(true)
                            .into_element()
                    } else {
                        super::git::diff_pane_view().into_element()
                    }
                }))),
        )
        .into_element()
}

// 树的展开状态只影响渲染；路径排序与压缩目录链由 falcon-core 的共用树模型决定。
fn tree_rows(
    nodes: Vec<TreeNode<String>>,
    collapsed: &HashSet<String>,
    depth: usize,
    rows: &mut Vec<(String, String, bool, usize)>,
) {
    for node in nodes {
        match node {
            TreeNode::File(file) => rows.push((file.path, file.name, false, depth)),
            TreeNode::Dir(dir) => {
                let open = !collapsed.contains(&dir.path);
                rows.push((dir.path, dir.name, true, depth));
                if open {
                    tree_rows(dir.children, collapsed, depth + 1, rows);
                }
            }
        }
    }
}

fn render_list(
    store: &WorkspaceStore,
    diff: bool,
    query: Signal<String>,
    tree: Signal<bool>,
    hidden: Signal<bool>,
    collapsed: Signal<HashSet<String>>,
    cursor: Signal<usize>,
    search_focused: Signal<bool>,
    search_ref: nsh_core::element::ElementRef,
) -> AnyElement {
    let resolved = use_theme().resolved();
    let ui = &resolved.ui;
    let searching = !query.get().trim().is_empty();
    let paths: Vec<String> = if diff {
        store
            .git_changes
            .get()
            .into_iter()
            .map(|item| item.path)
            .collect()
    } else {
        store
            .quick_open_paths
            .get()
            .into_iter()
            .filter(|path| hidden.get() || !path.split('/').any(is_hidden_name))
            .collect()
    };
    let mut rows = Vec::new();
    let tree_mode = tree.get() && !searching;
    if searching {
        rows.extend(
            filter_files(&paths, &query.get(), FILE_SEARCH_LIMIT)
                .into_iter()
                .map(|path| (path.clone(), path, false, 0)),
        );
    } else if tree_mode {
        tree_rows(
            build_file_tree(paths, Clone::clone),
            &collapsed.get(),
            0,
            &mut rows,
        );
    } else if diff {
        rows.extend(paths.into_iter().map(|path| (path.clone(), path, false, 0)));
    } else {
        rows.extend(
            store
                .file_entries
                .get()
                .into_iter()
                .filter(|entry| hidden.get() || !is_hidden_name(&entry.name))
                .map(|entry| (entry.path.clone(), entry.name, entry.is_dir, 0)),
        );
    }
    let active = store
        .preview_popup
        .get()
        .and_then(|target| target.path)
        .or_else(|| store.selected_git_path.get());
    let count = rows.len();
    let selected = cursor.get().min(count.saturating_sub(1));
    let row_refs: Vec<_> = rows.iter().map(|_| element_ref()).collect();
    // 只在文件栏搜索输入获得焦点时接管方向键/回车，编辑器光标移动保持原生行为。
    if search_focused.get() {
        for (key, step) in [("down", 1), ("up", -1), ("enter", 0)] {
            let st = store.clone();
            let items = rows.clone();
            let refs = row_refs.clone();
            bind_safe_key_when(key, true, nsh_core::keymap::When::Always, move || {
                if count == 0 {
                    return;
                }
                let mut next = if step == 0 {
                    selected
                } else {
                    move_selection(selected, count, step)
                };
                // 方向键直接切换预览，目录行只通过点击/回车展开，不成为切换的停靠点。
                if step != 0 {
                    for _ in 0..count {
                        if !items[next].2 {
                            break;
                        }
                        next = move_selection(next, count, step);
                    }
                }
                cursor.set(next);
                let (path, _, directory, _) = &items[next];
                if tree_mode && *directory {
                    if step == 0 {
                        collapsed.update(|set| {
                            if !set.remove(path) {
                                set.insert(path.clone());
                            }
                        });
                    }
                } else if !directory || step == 0 {
                    activate(&st, path, *directory);
                }
                if let Some(row) = refs.get(next) {
                    row.reveal(Default::default());
                }
                search_ref.focus();
            });
        }
    }
    let mut list = view().flex_col().gap(zpx(2.0));
    let error = if diff {
        store.git_error.get()
    } else if searching || tree_mode {
        store.quick_open_error.get()
    } else {
        store.file_error.get()
    };
    let loading = if diff {
        store.git_loading.get()
    } else if searching || tree_mode {
        store.quick_open_loading.get()
    } else {
        store.files_loading.get()
    };
    if loading {
        list = list.child(
            text(t!("native.preview.loading"))
                .text_xs()
                .text_color(rgb_to_hsla(ui.muted_foreground)),
        );
    } else if let Some(error) = error {
        list = list.child(
            text(error)
                .text_xs()
                .text_color(rgb_to_hsla(ui.destructive)),
        );
    } else if rows.is_empty() {
        list = list.child(
            text(t!("native.preview.empty"))
                .text_xs()
                .text_color(rgb_to_hsla(ui.muted_foreground)),
        );
    }
    if !diff && (searching || tree_mode) && store.quick_open_truncated.get() {
        list = list.child(
            text(t!("native.preview.truncated"))
                .text_xs()
                .text_color(rgb_to_hsla(ui.warning)),
        );
    }
    for (index, (path, label, directory, depth)) in rows.into_iter().enumerate() {
        let st = store.clone();
        let active = active.as_deref() == Some(path.as_str());
        let open = !collapsed.get().contains(&path);
        list = list.child(
            view()
                .test_id(&format!("preview-file-{index}"))
                .element_ref(&row_refs[index])
                .flex_row()
                .items_center()
                .gap(zpx(5.0))
                .pl(zpx(6.0 + depth as f32 * 12.0))
                .pr(zpx(6.0))
                .py(zpx(5.0))
                .rounded(zpx(5.0))
                .cursor_pointer()
                .when(active || (search_focused.get() && selected == index), |v| {
                    v.bg(rgb_to_hsla(ui.muted))
                })
                .text_color(rgb_to_hsla(if active {
                    ui.tint_strong
                } else {
                    ui.foreground
                }))
                .on_press(move || {
                    cursor.set(index);
                    if tree_mode && directory {
                        collapsed.update(|set| {
                            if !set.remove(&path) {
                                set.insert(path.clone());
                            }
                        });
                    } else {
                        activate(&st, &path, directory);
                    }
                    search_ref.focus();
                })
                .child(glyph(if directory {
                    if open {
                        Glyph::ChevronDown
                    } else {
                        Glyph::ChevronRight
                    }
                } else {
                    Glyph::File
                }))
                .child(text(label).text_xs().number_of_lines(1)),
        );
    }
    scroll()
        .test_id("preview-file-list")
        .flex_1()
        .min_h_0()
        .child(list)
        .into_element()
}
