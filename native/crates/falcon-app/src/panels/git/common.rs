//! 「修改」「历史」两个面板与差异窗口共用的小件：不可用原因的文案、porcelain 状态、
//! 改动文件的列表 / 目录树（web 的 `FileChangeView.tsx`）、三态勾选框、多仓库成员切换、
//! 提交时间。
//!
//! 改动文件的列表在 web 里是一棵 React 树（目录行 + 文件行递归），这里压平成一串等高的行交给
//! `uniform_list`：几千个改动文件也只画视口里那几十行，不用 web 那种"文件多了再说"的妥协。

use std::collections::HashSet;
use std::rc::Rc;

use falcon_core::file_tree::{TreeNode, base_of, build_file_tree, dir_of};
use falcon_proto::{GitUnavailableReason, Project};
use gpui_kit::assets::IconName;
use gpui_kit::component::Sizable;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::DropdownMenu;
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, ClickEvent, Context, Div, ElementId, Entity, FontWeight, Hsla, SharedString, Window, div,
};
use rust_i18n::t;

use crate::prefs::Prefs;
use crate::theme::Ui;
use crate::ui::icon;
use crate::workspace::Workspace;
use crate::zoom::zpx;

// ---------------- 文案 ----------------

/// 面板"不可用"原因 → i18n key（web GitPanel 的 `reasonKey`；新原因一律按"没跑起来"说）
pub fn reason_key(reason: Option<GitUnavailableReason>) -> &'static str {
    match reason {
        Some(GitUnavailableReason::GitMissing) => "git.reason_git_missing",
        Some(GitUnavailableReason::NotARepo) => "git.reason_not_a_repo",
        Some(GitUnavailableReason::NoWorkingDir) => "git.reason_no_working_dir",
        _ => "git.reason_link_failed",
    }
}

/// porcelain 的 XY 两列压成一个展示用字母（web `porcelainStatus`）。
///
/// 优先看暂存列（X）：`MM` 是"暂存了一版又改了一版"，主要事实是它被改过；
/// `??` 两列都是问号，取哪个都一样。
pub fn porcelain_status(index: &str, work: &str) -> String {
    if index == "?" || work == "?" {
        return "?".into();
    }
    if index == "U" || work == "U" || (index == "A" && work == "A") || (index == "D" && work == "D") {
        return "U".into();
    }
    if index != " " && !index.is_empty() {
        return index.to_string();
    }
    let w = work.trim();
    if w.is_empty() { "M".into() } else { w.to_string() }
}

/// 单字母状态 → 一句人话（web `statusText`）
pub fn status_text(status: &str) -> String {
    let key = match status {
        "?" => "git.status_untracked",
        "A" => "git.status_added",
        "D" => "git.status_deleted",
        "R" => "git.status_renamed",
        "C" => "git.status_copied",
        "U" => "git.status_unmerged",
        "T" => "git.status_typechange",
        _ => "git.status_modified",
    };
    t!(key).to_string()
}

/// 状态字母的颜色（web `statusTone`）
pub fn status_tone(status: &str, ui: &Ui) -> Hsla {
    match status {
        "D" => ui.destructive,
        "A" | "?" => ui.success,
        _ => ui.warning,
    }
}

/// 空状态 / 说明文字（web 各面板里的 `Hint`）。detail 是 git 的原话，等宽小字另起一行
pub fn hint(text: impl Into<SharedString>, detail: Option<String>, cx: &App) -> Div {
    let ui = Ui::global(cx);
    let mut d = div()
        .px_3()
        .py_4()
        .text_xs()
        .line_height(zpx(19.5))
        .text_color(ui.muted_foreground)
        .child(div().child(text.into()));
    if let Some(detail) = detail.filter(|s| !s.is_empty()) {
        d = d.child(div().mt_1().text_size(zpx(11.)).line_height(zpx(16.)).child(detail));
    }
    d
}

/// 详情头 / 提交行上的时间。当天只给时分（图里就是这样），跨天才补日期——一屏里绝大多数
/// 提交都是今天的，天天重复同一个日期是噪音（web `formatWhen`）。
pub fn format_when(ms: i64) -> String {
    if ms == 0 {
        return String::new();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let d = crate::local_time::civil(ms);
    let today = crate::local_time::civil(now);
    let time = format!("{:02}:{:02}", d.hour, d.minute);
    if (d.year, d.month, d.day) == (today.year, today.month, today.day) {
        time
    } else {
        // zh-CN 的 toLocaleDateString：2026/9/24
        format!("{}/{}/{} {time}", d.year, d.month, d.day)
    }
}

// ---------------- 列表 / 目录树偏好 ----------------

/// 列表 / 目录树。「修改」面板与 History 的提交详情共用一个键（web 的 `falcon.fileView`）：
/// 同一个人对"列表还是树"的偏好不会在两个面板之间反复横跳。每次渲染现读，两处天然同步
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileViewMode {
    List,
    Tree,
}

const FILE_VIEW_KEY: &str = "falcon.fileView";

pub fn file_view_mode(cx: &App) -> FileViewMode {
    match Prefs::global(cx).get(FILE_VIEW_KEY) {
        Some("tree") => FileViewMode::Tree,
        _ => FileViewMode::List,
    }
}

pub fn set_file_view_mode(mode: FileViewMode, cx: &mut App) {
    let v = match mode {
        FileViewMode::List => "list",
        FileViewMode::Tree => "tree",
    };
    cx.global_mut::<Prefs>().set(FILE_VIEW_KEY, v.to_string());
    cx.refresh_windows();
}

/// 头部的小号图标按钮（web 的 `variant="ghost" size="icon-xs"`）：按下态用 accent 底
pub fn tool_button(
    id: impl Into<ElementId>,
    name: IconName,
    tooltip: impl Into<SharedString>,
    pressed: bool,
    cx: &App,
) -> Button {
    let ui = Ui::global(cx);
    let b = Button::new(id).ghost().xsmall().icon(name).tooltip(tooltip);
    if pressed {
        b.bg(ui.accent).text_color(ui.foreground)
    } else {
        b.text_color(ui.muted_foreground)
    }
}

/// 列表 / 目录树两个切换按钮（web `FileViewToggle`）
pub fn file_view_toggle(prefix: &str, cx: &App) -> Div {
    let mode = file_view_mode(cx);
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(zpx(2.))
        .child(
            tool_button(
                format!("{prefix}-view-list"),
                IconName::List,
                t!("git.viewList").to_string(),
                mode == FileViewMode::List,
                cx,
            )
            .on_click(|_, _, cx| set_file_view_mode(FileViewMode::List, cx)),
        )
        .child(
            tool_button(
                format!("{prefix}-view-tree"),
                IconName::FolderTree,
                t!("git.viewTree").to_string(),
                mode == FileViewMode::Tree,
                cx,
            )
            .on_click(|_, _, cx| set_file_view_mode(FileViewMode::Tree, cx)),
        )
}

// ---------------- 三态勾选框 ----------------

/// gpui-component 的 Checkbox 没有半选态，而目录行 / 全选框要它（web 的 "indeterminate"）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Check {
    Off,
    On,
    Mixed,
}

pub fn checkbox(
    id: impl Into<ElementId>,
    state: Check,
    disabled: bool,
    tooltip: Option<SharedString>,
    cx: &App,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let ui = Ui::global(cx);
    let on = state != Check::Off;
    let mut b = div()
        .id(id.into())
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(zpx(14.))
        .rounded(zpx(4.))
        .border_1()
        .border_color(if on { ui.primary } else { ui.input })
        .when(on, |d| d.bg(ui.primary))
        .when(!on, |d| d.bg(ui.background))
        .when(disabled, |d| d.opacity(0.5));
    match state {
        Check::On => b = b.child(icon(IconName::Check).size(zpx(10.)).text_color(ui.primary_foreground)),
        Check::Mixed => b = b.child(icon(IconName::Minus).size(zpx(10.)).text_color(ui.primary_foreground)),
        Check::Off => {}
    }
    if let Some(tip) = tooltip {
        b = b.tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx));
    }
    if !disabled {
        b = b.cursor_pointer().on_click(move |e, window, cx| {
            cx.stop_propagation();
            on_click(e, window, cx)
        });
    }
    b.into_any_element()
}

// ---------------- 多仓库成员切换 ----------------

/// 多仓库项目在 Git / 修改面板头部的成员切换器（web `MultiRepoSelect`）。非多仓库项目给
/// None，两个面板可以无条件挂上它。选择存在工作区（`multi_repo`）——两个面板与差异窗口
/// 必须看同一个成员，面板自己记必然漂移。
pub fn multi_repo_select(id: &str, ws: &Entity<Workspace>, project: Option<&Project>, cx: &App) -> Option<AnyElement> {
    let project = project?;
    let multi = project.multi.as_ref()?;
    let current =
        falcon_core::workspace::select_multi_repo_dir(&ws.read(cx).state.multi_repo, Some(project)).unwrap_or_default();
    let dirs: Vec<String> = multi.repos.iter().map(|m| m.dir.clone()).collect();
    let label = falcon_core::multi_derive::member_basename(&current).to_string();
    let pid = project.id.clone();
    let ws = ws.clone();
    let tip: SharedString = if current.is_empty() {
        t!("multi.pickRepo").to_string().into()
    } else {
        current.clone().into()
    };
    Some(
        Button::new(SharedString::from(format!("{id}-multi-repo")))
            .outline()
            .xsmall()
            .label(label)
            .dropdown_caret(true)
            .max_w(zpx(144.))
            .text_size(zpx(11.))
            .tooltip(tip)
            .dropdown_menu(move |mut menu, _, _| {
                for dir in &dirs {
                    let (ws, pid, d) = (ws.clone(), pid.clone(), dir.clone());
                    menu = menu.item(
                        PopupMenuItem::new(falcon_core::multi_derive::member_basename(dir).to_string())
                            .checked(*dir == current)
                            .on_click(move |_, _, cx| ws.update(cx, |w, cx| w.set_multi_repo(&pid, &d, cx))),
                    );
                }
                menu
            })
            .into_any_element(),
    )
}

// ---------------- 改动文件：列表 / 目录树 ----------------

/// 一组改动文件里的一个。「修改」面板与提交详情各自把自己的数据翻译成这个中立形状
/// （web `FileChangeItem`）
#[derive(Clone, Debug, PartialEq)]
pub struct FileItem {
    /// 仓库根相对路径
    pub path: String,
    /// 重命名 / 复制前的路径
    pub orig_path: Option<String>,
    /// 单字母状态：A M D R C T U ?
    pub status: String,
    /// 二进制文件或算不出来时为 None——那与"改了 0 行"是两回事
    pub added: Option<u64>,
    pub deleted: Option<u64>,
}

/// 压平后的一行。所有行等高（ROW_H），才能交给 uniform_list
#[derive(Clone, Debug)]
pub enum FileRow {
    Dir {
        name: String,
        /// 折叠状态的键；仓库根那一行是空串
        path: String,
        count: usize,
        depth: usize,
        open: bool,
        /// 这个目录下（含更深层）的全部文件路径，目录行的勾选框一次切换它们
        paths: Rc<Vec<String>>,
    },
    File {
        /// 在 files 里的下标
        idx: usize,
        depth: usize,
        /// 目录树里不重复显示目录（它就在上一层），列表视图才显示
        name_only: bool,
    },
    /// 列表尾巴上的一行说明（"另有 N 个文件未列出"）
    Note(String),
}

pub const FILE_ROW_H: f32 = 24.;
/// 每层缩进多少像素（web 的 INDENT）
const INDENT: f32 = 12.;

/// 把文件摊成行。目录树默认全展开：改动文件通常就几十个，进来还要一层层点开才看得见反而
/// 更慢；所以记的是**折叠**的那些，新出现的目录天然是展开的。
pub fn build_rows(
    files: &[FileItem],
    mode: FileViewMode,
    root_name: Option<&str>,
    collapsed: &HashSet<String>,
) -> Vec<FileRow> {
    let mut out = Vec::with_capacity(files.len() + 8);
    if mode == FileViewMode::List {
        out.extend((0..files.len()).map(|idx| FileRow::File {
            idx,
            depth: 0,
            name_only: false,
        }));
        return out;
    }
    let nodes = build_file_tree((0..files.len()).collect::<Vec<usize>>(), |&i| files[i].path.clone());
    let depth = if root_name.is_some() { 1 } else { 0 };
    if let Some(root) = root_name {
        let open = !collapsed.contains("");
        out.push(FileRow::Dir {
            name: root.to_string(),
            path: String::new(),
            count: files.len(),
            depth: 0,
            open,
            paths: Rc::new(files.iter().map(|f| f.path.clone()).collect()),
        });
        if !open {
            return out;
        }
    }
    push_level(&nodes, depth, files, collapsed, &mut out);
    out
}

fn subtree_paths(nodes: &[TreeNode<usize>], files: &[FileItem], out: &mut Vec<String>) {
    for node in nodes {
        match node {
            TreeNode::File(f) => out.push(files[f.item].path.clone()),
            TreeNode::Dir(d) => subtree_paths(&d.children, files, out),
        }
    }
}

fn push_level(
    nodes: &[TreeNode<usize>],
    depth: usize,
    files: &[FileItem],
    collapsed: &HashSet<String>,
    out: &mut Vec<FileRow>,
) {
    for node in nodes {
        match node {
            TreeNode::File(f) => out.push(FileRow::File {
                idx: f.item,
                depth,
                name_only: true,
            }),
            TreeNode::Dir(d) => {
                let open = !collapsed.contains(&d.path);
                let mut paths = Vec::new();
                subtree_paths(&d.children, files, &mut paths);
                out.push(FileRow::Dir {
                    name: d.name.clone(),
                    path: d.path.clone(),
                    count: d.file_count,
                    depth,
                    open,
                    paths: Rc::new(paths),
                });
                if open {
                    push_level(&d.children, depth + 1, files, collapsed, out);
                }
            }
        }
    }
}

/// 一组路径的勾选合成态
pub fn group_check(paths: &[String], excluded: &HashSet<String>) -> Check {
    let on = paths.iter().filter(|p| !excluded.contains(*p)).count();
    if on == 0 {
        Check::Off
    } else if on == paths.len() {
        Check::On
    } else {
        Check::Mixed
    }
}

/// 列表的宿主（「修改」面板 / 历史面板）：点文件、折叠目录、勾选
pub trait FileListHost: Sized + 'static {
    fn open_file(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>);
    fn toggle_dir(&mut self, path: &str, cx: &mut Context<Self>);
    /// 勾选（只有「修改」面板有）。next = 勾上
    fn toggle_select(&mut self, _paths: &[String], _next: bool, _cx: &mut Context<Self>) {}
}

/// 画一行。`excluded` 给了才画勾选框列——History 的提交详情不需要选
pub fn render_file_row<V: FileListHost>(
    prefix: &str,
    row: &FileRow,
    files: &[FileItem],
    excluded: Option<&HashSet<String>>,
    cx: &mut Context<V>,
) -> AnyElement {
    let ui = Ui::global(cx).clone();
    match row {
        FileRow::Note(text) => div()
            .h(zpx(FILE_ROW_H))
            .w_full()
            .px_3()
            .flex()
            .items_center()
            .text_size(zpx(11.))
            .text_color(ui.muted_foreground)
            .child(text.clone())
            .into_any_element(),
        FileRow::Dir {
            name,
            path,
            count,
            depth,
            open,
            paths,
        } => {
            let key = path.clone();
            let mut line = div()
                .id(SharedString::from(format!("{prefix}-dir-{path}")))
                .h(zpx(FILE_ROW_H))
                .w_full()
                .flex()
                .items_center()
                .pr_2()
                .cursor_pointer()
                .hover(|s| s.bg(ui.muted))
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_dir(&key, cx)))
                .child(
                    div()
                        .flex()
                        .flex_1()
                        .min_w_0()
                        .items_center()
                        .gap_1()
                        .pl(zpx(12. + *depth as f32 * INDENT))
                        .child(
                            icon(if *open {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .size(zpx(12.))
                            .text_color(ui.muted_foreground),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(zpx(11.5))
                                .font_weight(FontWeight::MEDIUM)
                                .child(name.clone()),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_size(zpx(11.))
                                .text_color(ui.muted_foreground)
                                .child(t!("git.nFiles", n = count).to_string()),
                        ),
                );
            if let Some(excluded) = excluded {
                let state = group_check(paths, excluded);
                let paths = paths.clone();
                let entity = cx.entity();
                line = line.child(div().ml_2().flex_none().child(checkbox(
                    SharedString::from(format!("{prefix}-dirchk-{path}")),
                    state,
                    false,
                    Some(t!("changes.selectDir", name = name).to_string().into()),
                    cx,
                    // 半选点一下应该变全选（"我要这一整个目录"），而不是全不选
                    move |_, _, cx| entity.update(cx, |this, cx| this.toggle_select(&paths, state != Check::On, cx)),
                )));
            }
            line.into_any_element()
        }
        FileRow::File { idx, depth, name_only } => {
            let Some(file) = files.get(*idx) else {
                return div().h(zpx(FILE_ROW_H)).into_any_element();
            };
            let idx = *idx;
            let dir = if *name_only { "" } else { dir_of(&file.path) };
            let label = status_text(&file.status);
            let full = match &file.orig_path {
                Some(o) => format!("{o} → {}", file.path),
                None => file.path.clone(),
            };
            let tip: SharedString = format!("{label} · {full} · {}", t!("git.viewDiff")).into();
            let stats: AnyElement = match (file.added, file.deleted) {
                (Some(a), Some(d)) => div()
                    .flex()
                    .gap_1()
                    .child(div().text_color(ui.success).child(format!("+{a}")))
                    .child(div().text_color(ui.destructive).child(format!("−{d}")))
                    .into_any_element(),
                _ => div()
                    .text_color(ui.muted_foreground)
                    .child(t!("git.binary").to_string())
                    .into_any_element(),
            };
            let mut body = div()
                .flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .gap_2()
                // 缩进要跳过目录行的那个折叠箭头（12 + gap 4），文件名才和上一层的目录名对齐
                .pl(zpx(12. + *depth as f32 * INDENT + if *name_only { 16. } else { 0. }))
                .child(
                    div()
                        .w(zpx(10.))
                        .flex_none()
                        .text_size(zpx(11.))
                        .text_color(status_tone(&file.status, &ui))
                        .child(if file.status == "?" {
                            "+".to_string()
                        } else {
                            file.status.clone()
                        }),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(zpx(11.5))
                        .child(base_of(&file.path).to_string()),
                );
            if !dir.is_empty() {
                body = body.child(
                    div()
                        .min_w_0()
                        .flex_shrink(1.)
                        .truncate()
                        .text_size(zpx(11.))
                        .text_color(ui.muted_foreground)
                        .child(dir.to_string()),
                );
            }
            body = body.child(div().ml_auto().pl_1().flex_none().text_size(zpx(11.)).child(stats));
            let mut line = div()
                .id(SharedString::from(format!("{prefix}-file-{idx}")))
                .h(zpx(FILE_ROW_H))
                .w_full()
                .flex()
                .items_center()
                .pr_2()
                .cursor_pointer()
                .hover(|s| s.bg(ui.muted))
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                .on_click(cx.listener(move |this, _, window, cx| this.open_file(idx, window, cx)))
                .child(body);
            if let Some(excluded) = excluded {
                let on = !excluded.contains(&file.path);
                let path = file.path.clone();
                let entity = cx.entity();
                line = line.child(div().ml_2().flex_none().child(checkbox(
                    SharedString::from(format!("{prefix}-chk-{idx}")),
                    if on { Check::On } else { Check::Off },
                    false,
                    Some(t!("changes.selectFile", name = base_of(&file.path)).to_string().into()),
                    cx,
                    move |_, _, cx| {
                        entity.update(cx, |this, cx| this.toggle_select(std::slice::from_ref(&path), !on, cx))
                    },
                )));
            }
            line.into_any_element()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(path: &str) -> FileItem {
        FileItem {
            path: path.into(),
            orig_path: None,
            status: "M".into(),
            added: Some(1),
            deleted: Some(0),
        }
    }

    #[test]
    fn porcelain_status_matches_web() {
        assert_eq!(porcelain_status("?", "?"), "?");
        assert_eq!(porcelain_status("U", "U"), "U");
        assert_eq!(porcelain_status("A", "A"), "U");
        assert_eq!(porcelain_status("D", "D"), "U");
        assert_eq!(porcelain_status("M", "M"), "M");
        assert_eq!(porcelain_status(" ", "D"), "D");
        assert_eq!(porcelain_status(" ", " "), "M");
        assert_eq!(porcelain_status("R", " "), "R");
    }

    #[test]
    fn tree_rows_flatten_and_collapse() {
        let files = vec![item("a/x.ts"), item("a/y.ts"), item("b.ts")];
        let rows = build_rows(&files, FileViewMode::Tree, Some("repo"), &HashSet::new());
        // repo / a / x / y / b
        assert_eq!(rows.len(), 5);
        assert!(matches!(&rows[0], FileRow::Dir { path, count: 3, .. } if path.is_empty()));
        assert!(matches!(&rows[1], FileRow::Dir { name, depth: 1, paths, .. } if name == "a" && paths.len() == 2));
        assert!(matches!(
            &rows[4],
            FileRow::File {
                idx: 2,
                depth: 1,
                name_only: true
            }
        ));
        let collapsed: HashSet<String> = ["a".to_string()].into();
        assert_eq!(
            build_rows(&files, FileViewMode::Tree, Some("repo"), &collapsed).len(),
            3
        );
        let root: HashSet<String> = [String::new()].into();
        assert_eq!(build_rows(&files, FileViewMode::Tree, Some("repo"), &root).len(), 1);
        assert_eq!(
            build_rows(&files, FileViewMode::List, Some("repo"), &HashSet::new()).len(),
            3
        );
    }

    #[test]
    fn group_check_states() {
        let paths = vec!["a".to_string(), "b".to_string()];
        assert_eq!(group_check(&paths, &HashSet::new()), Check::On);
        assert_eq!(group_check(&paths, &["a".to_string()].into()), Check::Mixed);
        assert_eq!(
            group_check(&paths, &["a".to_string(), "b".to_string()].into()),
            Check::Off
        );
    }
}
