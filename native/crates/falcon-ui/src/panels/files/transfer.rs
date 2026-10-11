//! 下载 / 上传在 GPUI 客户端这一侧的流程（ADR 0008 / 0009），文件面板与文件窗口共用。
//! 与服务端的往返在 falcon-client（`download_to_file` / `upload_file`，流对流、带
//! `Content-Length` 核对）；这里只管"问用户、排队、报进度"。
//!
//! - **顺序而不是并发**：每个文件都可能要弹一次"覆盖吗"，并发的确认框会互相盖住；SSH
//!   链路上并发也不会更快（旧 React 版 `uploadItems` 同一个理由）。
//! - **进度走通知**：面板本身不摆进度条——上传是偶发动作，常驻的进度区大多数时候是空的。
//!   通知就地更新文字，不重推：同 id 重推会重播入场动画、还会挪到队尾，进度一跳一闪。
//! - **本机路径用 `std::path` 没问题**：这里的 `PathBuf` 都是**这台电脑**上用户选的文件；
//!   宿主机上的路径一律是工作目录相对的 `/` 串，拼接走 falcon-core 的 file_path。

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use falcon_client::{ApiError, FalconClient};
use falcon_core::file_path::{deepest_upload_dirs, rel_dir};
use falcon_core::file_search::basename;
use falcon_platform::Downloads;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::Sizable;
use gpui_kit::prelude::*;
use gpui_kit::{
    App, AsyncWindowContext, Context, Entity, IntoElement, PathPromptOptions, Render,
    SharedString, Window, div,
};
use rust_i18n::t;

use crate::dialogs::{self, ConfirmOpts};
use crate::toasts::ToastExt;
use crate::workspace::{ToastKind, Workspace};

/// 通知 id 的类型标签：同一个 key 再推一次就是"就地换成成功 / 失败"
struct TransferNote;

/// 进度通知里那行字：单独一个视图，更新它只重画这一行，通知本身不动
struct NoteLabel {
    text: SharedString,
}

impl Render for NoteLabel {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap_2()
            .text_sm()
            .child(Spinner::new().small())
            .child(div().flex_1().min_w_0().child(self.text.clone()))
    }
}

/// 一条"进行中"的通知：先转圈 + 百分比，结束时原地换成成功 / 失败
struct ProgressNote {
    key: SharedString,
    label: Entity<NoteLabel>,
}

impl ProgressNote {
    fn show(text: String, window: &mut Window, cx: &mut App) -> Self {
        use std::sync::atomic::AtomicUsize;
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let key: SharedString = format!("transfer-{}", SEQ.fetch_add(1, Ordering::Relaxed)).into();
        let label = cx.new(|_| NoteLabel { text: text.into() });
        let note = Self { key, label };
        note.push(window, cx);
        note
    }

    fn push(&self, window: &mut Window, cx: &mut App) {
        let label = self.label.clone();
        window.push_keyed_toast(
            self.key.clone(),
            Notification::new()
                .id1::<TransferNote>(self.key.clone())
                .autohide(false)
                .content(move |_, _, _| label.clone().into_any_element()),
            cx,
        );
    }

    fn set(&self, text: String, cx: &mut App) {
        self.label.update(cx, |l, cx| {
            if l.text.as_ref() != text {
                l.text = text.into();
                cx.notify();
            }
        });
    }

    fn dismiss(&self, window: &mut Window, cx: &mut App) {
        window.dismiss_toast(&self.key, cx);
    }

    fn success(&self, title: String, body: Option<String>, reveal: Option<PathBuf>, window: &mut Window, cx: &mut App) {
        let mut n = Notification::success(title.clone()).id1::<TransferNote>(self.key.clone());
        if let Some(body) = body {
            n = n.title(title).message(body);
        }
        if let Some(path) = reveal {
            n = n.on_click(move |_, _, cx| cx.reveal_path(&path));
        }
        window.push_keyed_toast(self.key.clone(), n, cx);
    }

    fn fail(&self, title: String, body: String, window: &mut Window, cx: &mut App) {
        window.push_keyed_toast(
            self.key.clone(),
            Notification::error(body).title(title).id1::<TransferNote>(self.key.clone()),
            cx,
        );
    }
}

/// 传输线程报上来的进度（客户端在自己的运行时里回调，这边定时取数更新通知）
#[derive(Default)]
struct Progress {
    done: AtomicU64,
    /// `u64::MAX` = 不知道总数
    total: AtomicU64,
}

impl Progress {
    fn new() -> Arc<Self> {
        Arc::new(Self { done: AtomicU64::new(0), total: AtomicU64::new(u64::MAX) })
    }

    fn callback(self: &Arc<Self>) -> impl Fn(u64, Option<u64>) + Send + Sync + 'static {
        let p = self.clone();
        move |done, total| {
            p.done.store(done, Ordering::Relaxed);
            p.total.store(total.unwrap_or(u64::MAX), Ordering::Relaxed);
        }
    }

    /// React 版的算法：没有总数算 0；到 100 之前封顶 99——请求体发完 ≠ 服务端落盘，满格要等响应
    fn pct(&self) -> u64 {
        let total = self.total.load(Ordering::Relaxed);
        if total == 0 || total == u64::MAX {
            return 0;
        }
        let done = self.done.load(Ordering::Relaxed);
        ((done as f64 / total as f64 * 100.0).floor() as u64).min(99)
    }
}

/// 传输进行时每 150ms 刷一次通知文字；返回的 Task 丢掉即停
fn tick_progress(
    note: &ProgressNote,
    progress: &Arc<Progress>,
    text: impl Fn(u64) -> String + 'static,
    cx: &mut AsyncWindowContext,
) -> gpui_kit::Task<()> {
    let label = note.label.clone();
    let progress = progress.clone();
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(Duration::from_millis(150)).await;
            let s = text(progress.pct());
            let ok = cx
                .update(|_, cx| {
                    label.update(cx, |l, cx| {
                        if l.text.as_ref() != s {
                            l.text = s.into();
                            cx.notify();
                        }
                    })
                })
                .is_ok();
            if !ok {
                break;
            }
        }
    })
}

/// React 版的 `confirmAsync`：确认 → true；取消 / Esc / 点外面关掉 → false。
///
/// 借 `dialogs::confirm` 的样子（与别处的确认框一模一样），只是把"确认"接到一个 oneshot 上：
/// 对话框无论怎么关，Root 都会丢掉它的构造闭包，连带丢掉发送端，接收端于是收到 Canceled。
pub(crate) async fn confirm_async(opts: ConfirmOpts, cx: &mut AsyncWindowContext) -> bool {
    let (tx, rx) = futures::channel::oneshot::channel::<()>();
    let tx = Rc::new(RefCell::new(Some(tx)));
    let opened = cx
        .update(|window, cx| {
            dialogs::confirm(
                opts,
                move |_, _| {
                    if let Some(tx) = tx.borrow_mut().take() {
                        let _ = tx.send(());
                    }
                },
                window,
                cx,
            )
        })
        .is_ok();
    opened && rx.await.is_ok()
}

async fn ask_overwrite(name: &str, cx: &mut AsyncWindowContext) -> bool {
    confirm_async(
        ConfirmOpts {
            title: t!("files.overwriteTitle", name = name).to_string(),
            body: t!("files.overwriteBody").to_string(),
            confirm_label: t!("files.overwrite").to_string(),
            danger: true,
            ..Default::default()
        },
        cx,
    )
    .await
}

fn report(ws: &Entity<Workspace>, err: &ApiError, title: String, cx: &mut AsyncWindowContext) {
    let body = err.to_string();
    let _ = cx.update(|_, cx| {
        ws.update(cx, |w, cx| {
            w.handle_error(err, cx);
            w.toast(ToastKind::Danger, title, Some(body), cx);
        })
    });
}

// ---------------- 上传 ----------------

/// 一个待上传的文件：落到工作目录里的 `dir/name`
pub(crate) struct UploadItem {
    pub dir: String,
    pub name: String,
    pub src: PathBuf,
}

/// 本机选中的文件 / 文件夹 → (相对路径, 本机路径)。文件夹里的每个文件带着顶层文件夹名
/// （同 `webkitRelativePath`），上传时按这段路径在工作目录里重建树。
///
/// 指向目录的符号链接不跟进去：成环时会无限递归，而把链接那头整棵树搬过去多半也不是用户
/// 想要的；指向文件的链接按文件内容传。空文件夹给不出文件，照 React 版不建（ADR 0009）。
fn collect_local(paths: &[PathBuf]) -> Vec<(String, PathBuf)> {
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<(String, PathBuf)>) {
        let Ok(read) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<_> = read.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel = format!("{prefix}/{name}");
            let path = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                walk(&path, &rel, out);
            } else if ft.is_file() || (ft.is_symlink() && std::fs::metadata(&path).is_ok_and(|m| m.is_file())) {
                out.push((rel, path));
            }
        }
    }
    let mut out = Vec::new();
    for p in paths {
        let Some(name) = p.file_name().map(|n| n.to_string_lossy().into_owned()) else { continue };
        match std::fs::metadata(p) {
            Ok(m) if m.is_dir() => walk(p, &name, &mut out),
            Ok(m) if m.is_file() => out.push((name, p.clone())),
            _ => {}
        }
    }
    out
}

fn join_rel(dir: &str, name: &str) -> String {
    match (dir.is_empty(), name.is_empty()) {
        (true, _) => name.to_string(),
        (_, true) => dir.to_string(),
        _ => format!("{dir}/{name}"),
    }
}

/// 把本机的文件 / 文件夹传到工作目录里的 `dest`（工作目录相对，`""` 是根）。
///
/// 文件夹 = 先对最深的那批中间层 `mkdir -p`，再逐个 PUT（ADR 0009 决定四）。`existing` 是
/// 调用方此刻看到的 `dest` 里已有的名字，用来在发请求前先问"覆盖吗"——目录列表可能已经过期，
/// 服务端的 409 是第二道，两道问的是同一个确认框。`on_done(有文件落盘了)` 用来刷新列表。
pub(crate) fn upload_local(
    ws: Entity<Workspace>,
    project_id: String,
    dest: String,
    paths: Vec<PathBuf>,
    existing: HashSet<String>,
    on_done: impl FnOnce(bool, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    if paths.is_empty() {
        return;
    }
    let client = ws.read(cx).client.clone();
    window
        .spawn(cx, async move |cx| {
            let items = cx.background_executor().spawn(async move { collect_local(&paths) }).await;
            if items.is_empty() {
                return;
            }
            let rels: Vec<&str> = items.iter().map(|(r, _)| r.as_str()).collect();
            for nested in deepest_upload_dirs(&dest, &rels) {
                if let Err(err) = client.mkdir(&project_id, &nested, true).await {
                    report(&ws, &err, t!("files.mkdirFailed").to_string(), cx);
                    return;
                }
            }
            let uploads: Vec<UploadItem> = items
                .into_iter()
                .map(|(rel, src)| UploadItem {
                    dir: join_rel(&dest, &rel_dir(&rel)),
                    name: basename(&rel).to_string(),
                    src,
                })
                .collect();
            let dest2 = dest.clone();
            let changed = upload_items(&ws, &client, &project_id, uploads, move |dir, name| dir == dest2 && existing.contains(name), cx).await;
            let _ = cx.update(|window, cx| on_done(changed, window, cx));
        })
        .detach();
}

async fn upload_items(
    ws: &Entity<Workspace>,
    client: &FalconClient,
    project_id: &str,
    items: Vec<UploadItem>,
    has_entry: impl Fn(&str, &str) -> bool,
    cx: &mut AsyncWindowContext,
) -> bool {
    let mut any = false;
    for item in items {
        let mut overwrite = false;
        if has_entry(&item.dir, &item.name) {
            overwrite = ask_overwrite(&item.name, cx).await;
            if !overwrite {
                continue;
            }
        }
        let name = item.name.clone();
        let text = move |pct: u64| t!("files.uploading", name = name.clone(), pct = pct).to_string();
        let Ok(note) = cx.update(|window, cx| ProgressNote::show(text(0), window, cx)) else { return any };
        let mut result = upload_one(client, project_id, &item, overwrite, &note, &text, cx).await;
        if let Err(err) = &result
            && err.is_conflict()
        {
            let _ = cx.update(|window, cx| note.dismiss(window, cx));
            if !ask_overwrite(&item.name, cx).await {
                continue;
            }
            let _ = cx.update(|window, cx| {
                note.set(text(0), cx);
                note.push(window, cx);
            });
            result = upload_one(client, project_id, &item, true, &note, &text, cx).await;
        }
        match result {
            Ok(_) => {
                any = true;
                let title = t!("files.uploaded", name = item.name.clone()).to_string();
                let _ = cx.update(|window, cx| note.success(title, None, None, window, cx));
            }
            Err(err) => {
                let title = t!("files.uploadFailed", name = item.name.clone()).to_string();
                let body = err.to_string();
                let _ = cx.update(|window, cx| {
                    ws.update(cx, |w, cx| w.handle_error(&err, cx));
                    note.fail(title, body, window, cx);
                });
            }
        }
    }
    any
}

async fn upload_one(
    client: &FalconClient,
    project_id: &str,
    item: &UploadItem,
    overwrite: bool,
    note: &ProgressNote,
    text: &(impl Fn(u64) -> String + Clone + 'static),
    cx: &mut AsyncWindowContext,
) -> Result<falcon_proto::UploadResult, ApiError> {
    // 浏览器版上传（<input type=file> + XHR）在 C3 做；在那之前选不出文件，也走不到这里
    // （wasm 上的 upload_file 直接报错）
    let progress = Progress::new();
    let fut = client.upload_file(project_id, &item.dir, &item.name, item.src.clone(), overwrite, progress.callback());
    let _ticker = tick_progress(note, &progress, text.clone(), cx);
    fut.await
}

/// 自动化验证的替身（只在 `automation` feature 下编进来）：锁屏时系统的打开 / 存储面板没法
/// 操作，用环境变量直接给出"用户选了什么"。值是 `:` 分隔的本机路径
#[cfg(feature = "automation")]
fn test_paths(var: &str) -> Option<Vec<PathBuf>> {
    std::env::var(var).ok().map(|v| v.split(':').filter(|s| !s.is_empty()).map(PathBuf::from).collect())
}

#[cfg(not(feature = "automation"))]
fn test_paths(_var: &str) -> Option<Vec<PathBuf>> {
    None
}

/// "上传文件…" / "上传文件夹…"：弹系统选择框，选完交给 [`upload_local`]
pub(crate) fn pick_and_upload(
    ws: Entity<Workspace>,
    project_id: String,
    dest: String,
    folder: bool,
    existing: HashSet<String>,
    on_done: impl FnOnce(bool, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    if let Some(paths) = test_paths(if folder { "FALCON_TEST_PICK_DIR" } else { "FALCON_TEST_PICK" }) {
        upload_local(ws, project_id, dest, paths, existing, on_done, window, cx);
        return;
    }
    let rx = cx.prompt_for_paths(PathPromptOptions {
        files: !folder,
        directories: folder,
        multiple: !folder,
        prompt: None,
    });
    window
        .spawn(cx, async move |cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let _ = cx.update(|window, cx| upload_local(ws, project_id, dest, paths, existing, on_done, window, cx));
        })
        .detach();
}

// ---------------- 下载 ----------------

/// 目标文件夹里已有同名文件时，照浏览器的习惯另起 `名字 (2).扩展名`，不悄悄覆盖
fn unique_dest(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .unwrap_or(first)
}

/// 把工作目录里的文件下载到本机。调用方只传**文件**：文件夹要打包，那是终端里的事（ADR 0009）。
///
/// - 原生：一个文件弹系统"存储为"对话框（它自己会问覆盖）；多个文件选一次文件夹，逐个存
///   进去——每个文件弹一次存储框太烦，而浏览器那种做法（交给浏览器的下载目录）在原生上没有对应物；
/// - 浏览器：逐个交给 `<a download>`，进度与存到哪儿归浏览器的下载管理器（沿用 React 版）。
///   在点击的同步调用栈里触发，不会被当成弹窗拦掉。
pub(crate) fn download(ws: Entity<Workspace>, project_id: String, paths: Vec<String>, window: &mut Window, cx: &mut App) {
    if paths.is_empty() {
        return;
    }
    let client = ws.read(cx).client.clone();
    let platform = falcon_platform::get(cx);
    let downloads_dir = match platform.downloads() {
        Downloads::LocalDir(dir) => dir,
        Downloads::Browser => {
            for path in paths {
                let url = format!("{}{}", client.base_url(), client.download_path(&project_id, &path));
                if let Err(err) = platform.hand_off_download(&url) {
                    log::warn!("触发下载失败（{path}）：{err}");
                }
            }
            return;
        }
    };
    let single = paths.len() == 1;
    if let Some(dest) = test_paths(if single { "FALCON_TEST_SAVE_AS" } else { "FALCON_TEST_SAVE_DIR" }).and_then(|p| p.into_iter().next()) {
        let targets = if single {
            vec![(paths[0].clone(), dest)]
        } else {
            paths.iter().map(|p| (p.clone(), unique_dest(&dest, basename(p)))).collect()
        };
        run_downloads(ws, client, project_id, targets, window, cx);
        return;
    }
    let rx_file = single.then(|| cx.prompt_for_new_path(&downloads_dir, Some(basename(&paths[0]))));
    let rx_dir = (!single).then(|| {
        cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(t!("native.files.downloadHere").to_string().into()),
        })
    });
    window
        .spawn(cx, async move |cx| {
            let targets: Vec<(String, PathBuf)> = if let Some(rx) = rx_file {
                let Ok(Ok(Some(dest))) = rx.await else { return };
                vec![(paths[0].clone(), dest)]
            } else if let Some(rx) = rx_dir {
                let Ok(Ok(Some(dirs))) = rx.await else { return };
                let Some(dir) = dirs.into_iter().next() else { return };
                paths.iter().map(|p| (p.clone(), unique_dest(&dir, basename(p)))).collect()
            } else {
                return;
            };
            let _ = cx.update(|window, cx| run_downloads(ws, client, project_id, targets, window, cx));
        })
        .detach();
}

/// 逐个下载，每个一条进度通知
fn run_downloads(
    ws: Entity<Workspace>,
    client: FalconClient,
    project_id: String,
    targets: Vec<(String, PathBuf)>,
    window: &mut Window,
    cx: &mut App,
) {
    window
        .spawn(cx, async move |cx| {
            for (path, dest) in targets {
                let name = basename(&path).to_string();
                let text = {
                    let name = name.clone();
                    move |pct: u64| t!("native.files.downloading", name = name.clone(), pct = pct).to_string()
                };
                let Ok(note) = cx.update(|window, cx| ProgressNote::show(text(0), window, cx)) else { return };
                let progress = Progress::new();
                let fut = client.download_to_file(&project_id, &path, dest.clone(), progress.callback());
                let ticker = tick_progress(&note, &progress, text, cx);
                let result = fut.await;
                drop(ticker);
                match result {
                    Ok(_) => {
                        let title = t!("native.files.downloaded", name = name.clone()).to_string();
                        let body = format!("{}\n{}", dest.display(), t!("native.files.revealHint"));
                        let _ = cx.update(|window, cx| note.success(title, Some(body), Some(dest.clone()), window, cx));
                    }
                    Err(err) => {
                        let title = t!("native.files.downloadFailed", name = name.clone()).to_string();
                        let body = err.to_string();
                        let _ = cx.update(|window, cx| {
                            ws.update(cx, |w, cx| w.handle_error(&err, cx));
                            note.fail(title, body, window, cx);
                        });
                    }
                }
            }
        })
        .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_rel_handles_root() {
        assert_eq!(join_rel("", "a"), "a");
        assert_eq!(join_rel("src", ""), "src");
        assert_eq!(join_rel("src", "lib"), "src/lib");
    }

    #[test]
    fn collect_local_keeps_top_folder_name() {
        let root = std::env::temp_dir().join(format!("falcon-collect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("pkg/src")).unwrap();
        std::fs::write(root.join("pkg/a.txt"), "a").unwrap();
        std::fs::write(root.join("pkg/src/b.rs"), "b").unwrap();
        std::fs::write(root.join("c.md"), "c").unwrap();
        let got: Vec<String> = collect_local(&[root.join("pkg"), root.join("c.md")]).into_iter().map(|(r, _)| r).collect();
        assert_eq!(got, vec!["pkg/a.txt", "pkg/src/b.rs", "c.md"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unique_dest_numbers_like_browsers() {
        let root = std::env::temp_dir().join(format!("falcon-unique-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(unique_dest(&root, "a.txt"), root.join("a.txt"));
        std::fs::write(root.join("a.txt"), "").unwrap();
        assert_eq!(unique_dest(&root, "a.txt"), root.join("a (2).txt"));
        std::fs::write(root.join("a (2).txt"), "").unwrap();
        assert_eq!(unique_dest(&root, "a.txt"), root.join("a (3).txt"));
        std::fs::write(root.join(".env"), "").unwrap();
        assert_eq!(unique_dest(&root, ".env"), root.join(".env (2)"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
