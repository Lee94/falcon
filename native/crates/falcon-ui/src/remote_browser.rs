//! 侧栏只放入口。窗口打开时固定项目，之后切换工作区焦点不会把正在操作的网站换到另一台机器。

use std::rc::Rc;

use falcon_platform::HtmlView;
use futures::StreamExt;
use gpui_kit::assets::IconName;
use gpui_kit::component::Root;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Bounds, Context, Entity, IntoElement, Render, Task, TitlebarOptions, Window,
    WindowBounds, WindowOptions, div, px, size,
};
use rust_i18n::t;

use crate::panels::file_view::html::HtmlSurface;
use crate::theme::Ui;
use crate::workspace::{ToastKind, Workspace};
use crate::zoom::zpx;

fn entry_url(base: &str, project: Option<&str>) -> String {
    let mut url = format!("{}/remote-browser/open", base.trim_end_matches('/'));
    if let Some(project) = project {
        url.push('?');
        url.push_str(&url::form_urlencoded::Serializer::new(String::new()).append_pair("project", project).finish());
    }
    url
}

pub fn open(ws: &Entity<Workspace>, cx: &mut App) {
    let w = ws.read(cx);
    let project = w.focus_project_id();
    let label = project
        .as_deref()
        .and_then(|id| w.project(id))
        .map(|p| p.name.clone())
        .unwrap_or_else(|| t!("remoteBrowser.local").to_string());
    let url = entry_url(w.client.base_url(), project.as_deref());
    let prefix = format!("{}/remote-browser/", w.client.base_url().trim_end_matches('/'));
    let title = format!("{} · {label}", t!("remoteBrowser.title"));
    let platform = falcon_platform::get(cx);
    let result = if platform.info().browser {
        platform.open_browser_popup(&url)
    } else {
        open_native(url, prefix, title, cx)
    };
    if let Err(err) = result {
        ws.update(cx, |w, cx| w.toast(ToastKind::Danger, t!("remoteBrowser.failed").to_string(), Some(err), cx));
    }
}

fn open_native(url: String, prefix: String, title: String, cx: &mut App) -> Result<(), String> {
    for handle in cx.windows() {
        let Some(root) = handle.downcast::<Root>() else { continue };
        let matches = root
            .read(cx)
            .ok()
            .and_then(|r| r.view().clone().downcast::<BrowserWindow>().ok())
            .is_some_and(|v| v.read(cx).url == url);
        if matches {
            let _ = root.update(cx, |_, window, _| window.activate_window());
            return Ok(());
        }
    }
    let options = WindowOptions {
        titlebar: Some(TitlebarOptions { title: Some(title.clone().into()), ..Default::default() }),
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(1280.), px(840.)), cx))),
        window_min_size: Some(size(px(640.), px(420.))),
        ..Default::default()
    };
    cx.open_window(options, |window, cx| {
        let view = cx.new(|cx| BrowserWindow::new(url, prefix, title, window, cx));
        cx.new(|cx| Root::new(view, window, cx))
    })
    .map(|_| ())
    .map_err(|err| err.to_string())
}

struct BrowserWindow {
    url: String,
    title: String,
    surface: Option<Result<Rc<dyn HtmlView>, String>>,
    _opener: Task<()>,
}

impl BrowserWindow {
    fn new(url: String, prefix: String, title: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<String>();
        let open_external = Box::new(move |u: String| {
            let _ = tx.unbounded_send(u);
        });
        let surface = falcon_platform::get(cx).remote_browser_view(&url, &prefix, open_external, window);
        let opener = cx.spawn(async move |_, cx| {
            while let Some(u) = rx.next().await {
                cx.update(|cx| cx.open_url(&u));
            }
        });
        Self { url, title, surface, _opener: opener }
    }
}

impl Drop for BrowserWindow {
    fn drop(&mut self) {
        if let Some(Ok(view)) = &self.surface {
            view.set_visible(false);
        }
    }
}

impl Render for BrowserWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx);
        let url = self.url.clone();
        let body = match &self.surface {
            Some(Ok(view)) => HtmlSurface { view: view.clone(), hidden: false }.into_any_element(),
            error => div()
                .size_full()
                .p_6()
                .text_color(ui.muted_foreground)
                .child(match error {
                    Some(Err(err)) => format!("{}：{err}", t!("remoteBrowser.failed")),
                    _ => t!("remoteBrowser.unavailable").to_string(),
                })
                .into_any_element(),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(ui.background)
            .text_color(ui.foreground)
            .child(
                div()
                    .flex_none()
                    .h(zpx(42.))
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().flex_1().text_sm().child(self.title.clone()))
                    .child(div().text_xs().text_color(ui.muted_foreground).child(t!("remoteBrowser.transport").to_string()))
                    .child(
                        Button::new("remote-browser-external")
                            .ghost()
                            .icon(IconName::ExternalLink)
                            .label(t!("remoteBrowser.external").to_string())
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    ),
            )
            .child(div().flex_1().min_h_0().child(body))
    }
}

#[cfg(test)]
mod tests {
    use super::entry_url;

    #[test]
    fn entry_freezes_project_and_encodes_query() {
        assert_eq!(entry_url("https://falcon.example/", None), "https://falcon.example/remote-browser/open");
        assert_eq!(
            entry_url("https://falcon.example", Some("p&next=/other#中")),
            "https://falcon.example/remote-browser/open?project=p%26next%3D%2Fother%23%E4%B8%AD"
        );
    }
}
