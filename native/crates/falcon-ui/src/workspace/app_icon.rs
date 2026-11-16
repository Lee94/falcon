//! 应用图标（ADR 0018）：这台服务端上的选择、自定义图（已套好 Dock 版式）、换 Dock。
//!
//! 选择是服务端级的，每个窗口（= 一台服务端）各记各的；Dock 图标整个 App 只有一个，
//! 谁最后取到 / 改过谁说了算。

use std::path::PathBuf;
use std::sync::Arc;

use falcon_core::app_icon::{CUSTOM, DEFAULT_APP_ICON};
use falcon_proto::AppIconState;
use gpui_kit::{Context, Image, ImageFormat, PathPromptOptions};
use rust_i18n::t;

use super::{ToastKind, Workspace};
use crate::app_icon;

/// 自定义图标：Dock 用的 PNG（套好版式的）与设置里的预览（同一份字节）
pub struct CustomIcon {
    pub version: String,
    pub dock: Vec<u8>,
    pub image: Arc<Image>,
}

impl Workspace {
    /// 连上 / 登录后取一次（load_all 里）
    pub fn load_app_icon(&mut self, cx: &mut Context<Self>) {
        let fut = self.client.app_icon();
        cx.spawn(async move |this, cx| match fut.await {
            Ok(state) => {
                this.update(cx, |this, cx| this.apply_app_icon(state, cx)).ok();
            }
            Err(err) => log::warn!("读取应用图标设置失败：{err}"),
        })
        .detach();
    }

    /// 收下服务端返回的状态：记下；自定义图换了版本就重新取；换 Dock
    fn apply_app_icon(&mut self, state: AppIconState, cx: &mut Context<Self>) {
        let have = self.custom_icon.as_ref().map(|c| c.version.as_str());
        if state.custom.as_deref() != have {
            self.custom_icon = None;
            if let Some(version) = state.custom.clone() {
                self.fetch_custom_icon(version, cx);
            }
        }
        self.app_icon = Some(state);
        self.update_dock();
        cx.notify();
    }

    fn fetch_custom_icon(&mut self, version: String, cx: &mut Context<Self>) {
        let fut = self.client.custom_app_icon(&version);
        cx.spawn(async move |this, cx| {
            let bytes = match fut.await {
                Ok(b) => b,
                Err(err) => return log::warn!("取自定义图标失败：{err}"),
            };
            let dock = cx.background_executor().spawn(async move { app_icon::dock_png(&bytes) }).await;
            let dock = match dock {
                Ok(png) => png,
                Err(err) => return log::warn!("自定义图标套 Dock 版式失败：{err:#}"),
            };
            this.update(cx, |this, cx| {
                // 取图的工夫里又换过一张：这张作废
                if this.app_icon.as_ref().and_then(|s| s.custom.as_deref()) != Some(version.as_str()) {
                    return;
                }
                let image = Arc::new(Image::from_bytes(ImageFormat::Png, dock.clone()));
                this.custom_icon = Some(CustomIcon { version, dock, image });
                this.update_dock();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 换 Dock 图标（整个 App 一个；几台服务端各开一个窗口时，最后换的那台说了算）
    fn update_dock(&self) {
        let Some(state) = &self.app_icon else { return };
        if state.selected == CUSTOM {
            // 图还没取回来：先不动，取回来会再调一次
            if let Some(c) = &self.custom_icon {
                self.platform.set_app_icon(&c.dock);
            }
            return;
        }
        if let Some(png) = app_icon::builtin_png(&state.selected).or_else(|| app_icon::builtin_png(DEFAULT_APP_ICON)) {
            self.platform.set_app_icon(png);
        }
    }

    /// 设置里点了一个图标（内置 id 或 `"custom"`）
    pub fn set_app_icon(&mut self, choice: &str, cx: &mut Context<Self>) {
        if self.app_icon_busy || self.app_icon.as_ref().is_some_and(|s| s.selected == choice) {
            return;
        }
        let fut = self.client.set_app_icon(choice);
        self.run_app_icon(async move { fut.await.map_err(|e| e.to_string()) }, "appIcon.saveFailed", cx);
    }

    pub fn remove_custom_app_icon(&mut self, cx: &mut Context<Self>) {
        if self.app_icon_busy {
            return;
        }
        let fut = self.client.remove_custom_app_icon();
        self.run_app_icon(async move { fut.await.map_err(|e| e.to_string()) }, "appIcon.removeFailed", cx);
    }

    /// 选一张本机图片 → 规整成 512 PNG → 上传（上传即选中）
    pub fn upload_app_icon(&mut self, cx: &mut Context<Self>) {
        if self.app_icon_busy {
            return;
        }
        let rx = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: None });
        let svg = cx.svg_renderer();
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let png = cx.background_executor().spawn(async move { read_and_normalize(path, svg) }).await;
            this.update(cx, |this, cx| match png {
                Ok(png) => {
                    let fut = client.upload_app_icon(png);
                    this.run_app_icon(async move { fut.await.map_err(|e| e.to_string()) }, "appIcon.uploadFailed", cx);
                }
                Err(err) => {
                    log::warn!("自定义图标读不出来（{name}）：{err:#}");
                    this.toast(ToastKind::Danger, t!("appIcon.decodeFailed").to_string(), Some(name), cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn run_app_icon(
        &mut self,
        fut: impl Future<Output = Result<AppIconState, String>> + 'static,
        fail_key: &'static str,
        cx: &mut Context<Self>,
    ) {
        self.app_icon_busy = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = fut.await;
            this.update(cx, |this, cx| {
                this.app_icon_busy = false;
                match res {
                    Ok(state) => this.apply_app_icon(state, cx),
                    Err(err) => {
                        this.toast(ToastKind::Danger, t!(fail_key).to_string(), Some(err), cx);
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }
}

fn read_and_normalize(path: PathBuf, svg: gpui_kit::SvgRenderer) -> anyhow::Result<Vec<u8>> {
    let format = crate::panels::file_view::format_by_ext(&path.to_string_lossy())
        .ok_or_else(|| anyhow::anyhow!("不认识的图片格式"))?;
    let bytes = std::fs::read(&path)?;
    app_icon::normalize_upload(bytes, format, svg)
}
