//! 文件仍从会话的宿主机读取；悬停满两秒才发请求，移开就取消，迟到响应不能替换下一张图。

use super::*;
use falcon_client::{ProbeTarget, raw_url};
use falcon_core::terminal_link::{file_path, resolve_file_path};
use falcon_term::links::LinkTarget;
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Image, ImageSource, Point, RenderImage, anchored, deferred, img};
use rust_i18n::t;

#[derive(Clone, PartialEq, Eq)]
pub(super) struct FileLinkContext {
    project_id: String,
    cwd: Option<String>,
    /// 不属于多文件夹容器、也没配置目录时，shell 从宿主机家目录启动。
    use_home: bool,
    target: ProbeTarget,
}

pub(super) struct ImageHover {
    link: LinkSpan,
    position: Point<Pixels>,
    /// None 在两秒内不显示任何浮层；请求开始后才显示 Loading。
    status: Option<Preview>,
    _task: Task<()>,
}

enum Preview {
    Loading,
    Ready(Arc<RenderImage>),
    Failed,
}

impl FileLinkContext {
    async fn resolve(
        &self,
        client: &FalconClient,
        raw: &str,
        cwd: Option<&str>,
        home: Option<&str>,
    ) -> anyhow::Result<String> {
        let path = file_path(raw).ok_or_else(|| anyhow::anyhow!("invalid terminal file path"))?;
        let cwd = cwd.or(self.cwd.as_deref());
        let relative = !path.starts_with(['/', '\\']) && path.as_bytes().get(1) != Some(&b':');
        let home_cwd = self.use_home && cwd.is_none() && relative;
        let needs_home = home_cwd || path.starts_with("~/") || path.starts_with("~\\");
        let home = if needs_home && home.is_none() {
            Some(client.list_dir(None, &self.target).await?.home)
        } else {
            home.map(str::to_string)
        };
        resolve_file_path(
            &path,
            cwd.or_else(|| home_cwd.then_some(home.as_deref()).flatten()),
            home.as_deref(),
        )
        .ok_or_else(|| anyhow::anyhow!("invalid terminal file path"))
    }
}

impl TerminalView {
    pub fn set_file_context(&mut self, project: Option<falcon_proto::Project>) {
        let next = project.map(|project| {
            let target = if project.project_type == falcon_proto::ProjectType::Ssh {
                // 普通 SSH 项目使用自己保存的连接副本；无项目会话才按主机 id 查。
                falcon_proto::standalone_host_id(&project.id)
                    .map(|id| ProbeTarget::SshHost(id.to_string()))
                    .unwrap_or_else(|| ProbeTarget::Project(project.id.clone()))
            } else {
                ProbeTarget::Backend
            };
            FileLinkContext {
                use_home: project.multi.is_none()
                    && project.working_dir.as_ref().is_none_or(|s| s.is_empty()),
                project_id: project.id,
                cwd: project.working_dir,
                target,
            }
        });
        if self.file_context != next {
            self.file_context = next;
            self.image_hover = None;
        }
    }

    pub(super) fn open_link(&mut self, link: LinkSpan, cx: &mut Context<Self>) {
        self.image_hover = None;
        match link.target {
            LinkTarget::Url(url) => cx.open_url(&url),
            LinkTarget::File(raw) => {
                let Some(context) = self.file_context.clone() else {
                    return;
                };
                let client = self.client.clone();
                let cwd = self.output_cwd.clone();
                cx.spawn(async move |this, cx| {
                    let result = context
                        .resolve(&client, &raw, cwd.as_deref(), None)
                        .await;
                    this.update(cx, |_, cx| match result {
                        Ok(path) => cx.emit(TerminalViewEvent::OpenFile {
                            project_id: context.project_id,
                            path,
                        }),
                        Err(err) => cx.emit(TerminalViewEvent::Toast {
                            kind: crate::workspace::ToastKind::Danger,
                            title: t!("files.viewFailed").to_string(),
                            body: Some(err.to_string()),
                        }),
                    })
                    .ok();
                })
                .detach();
            }
        }
        cx.notify();
    }

    pub(super) fn refresh_image_hover(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let link = if self.visible && self.dragging.is_none() && self.scrollbar.drag.is_none() {
            self.link_at(position).filter(|link| match &link.target {
                LinkTarget::File(raw) => file_path(raw)
                    .is_some_and(|p| crate::panels::file_view::format_by_ext(&p).is_some()),
                _ => false,
            })
        } else {
            None
        };
        if self.image_hover.as_ref().map(|h| &h.link) == link.as_ref() {
            return;
        }
        self.image_hover = None;
        cx.notify();
        let (Some(link), Some(context)) = (link, self.file_context.clone()) else {
            return;
        };
        let client = self.client.clone();
        let cwd = self.output_cwd.clone();
        let expected = link.clone();
        let svg = cx.svg_renderer();
        let task = cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            let valid = this
                .update_in(cx, |this, window, cx| {
                    if !this.visible
                        || this.link_at(window.mouse_position()).as_ref() != Some(&expected)
                    {
                        this.image_hover = None;
                        cx.notify();
                        return false;
                    }
                    if let Some(hover) = this.image_hover.as_mut() {
                        hover.status = Some(Preview::Loading);
                    }
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            if !valid {
                return;
            }
            let LinkTarget::File(raw) = &expected.target else {
                return;
            };
            let result = async {
                let path = context
                    .resolve(&client, raw, cwd.as_deref(), None)
                    .await?;
                let file = client.read_file(&context.project_id, &path).await?;
                if !matches!(file.preview, falcon_proto::FilePreview::Image { .. }) {
                    anyhow::bail!("file is not a previewable image");
                }
                let bytes = client
                    .raw_bytes(&raw_url(
                        &file.raw_base,
                        file.raw_path.as_deref().unwrap_or(&path),
                    ))
                    .await?;
                let format = crate::panels::file_view::format_by_ext(&path)
                    .ok_or_else(|| anyhow::anyhow!("unsupported image format"))?;
                cx.background_executor()
                    .spawn(async move {
                        Image::from_bytes(format, bytes.bytes.to_vec()).to_image_data(svg)
                    })
                    .await
                    .map_err(|e| anyhow::anyhow!("{e}"))
            }
            .await;
            this.update(cx, |this, cx| {
                if let Some(hover) = this.image_hover.as_mut().filter(|h| h.link == expected) {
                    hover.status = Some(match result {
                        Ok(image) => Preview::Ready(image),
                        Err(_) => Preview::Failed,
                    });
                    cx.notify();
                }
            })
            .ok();
        });
        self.image_hover = Some(ImageHover {
            link,
            position,
            status: None,
            _task: task,
        });
    }

    pub(super) fn file_preview(&self, cx: &App) -> Option<AnyElement> {
        let hover = self.image_hover.as_ref()?;
        let status = hover.status.as_ref()?;
        let ui = crate::theme::Ui::global(cx);
        let body = match status {
            Preview::Loading => div()
                .p_3()
                .child(t!("files.viewLoading").to_string())
                .into_any_element(),
            Preview::Failed => div()
                .p_3()
                .child(t!("files.viewImageFailed").to_string())
                .into_any_element(),
            Preview::Ready(image) => div()
                .flex()
                .items_center()
                .justify_center()
                .w(zpx(320.))
                .h(zpx(220.))
                .child(
                    img(ImageSource::Render(image.clone()))
                        .max_w(zpx(320.))
                        .max_h(zpx(220.))
                        .object_fit(gpui_kit::ObjectFit::Contain),
                )
                .into_any_element(),
        };
        Some(
            deferred(
                anchored()
                    .position(hover.position + gpui_kit::point(zpx(12.), zpx(20.)))
                    .snap_to_window_with_margin(zpx(8.))
                    .child(
                        div()
                            .w(zpx(336.))
                            .p_2()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .rounded(crate::theme::radius::MD)
                            .bg(ui.popover)
                            .text_color(ui.foreground)
                            .text_size(zpx(12.))
                            .shadow_lg()
                            .child(body)
                            .child(
                                div()
                                    .max_w_full()
                                    .truncate()
                                    .text_color(ui.muted_foreground)
                                    .child(hover.link.target.text().to_string()),
                            ),
                    ),
            )
            .with_priority(5)
            .into_any_element(),
        )
    }
}
