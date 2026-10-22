//! 右侧栏插件（ADR 0025）：这台服务端装了哪些、右侧栏放哪几个、启用 / 停用。
//!
//! 列表是服务端级的（插件装在后端那台机器上），每个窗口（= 一台服务端）各拉各的。连上 / 登录后
//! 拉一次；设置页的「重新扫描」、插件请求撞上 409（停用了 / 清单坏了）时再拉。

use falcon_proto::{PluginInfo, PluginList};
use gpui_kit::Context;
use rust_i18n::t;

use super::{ToastKind, Workspace};

impl Workspace {
    /// 连上 / 登录后取一次（load_all 里），设置页的「重新扫描」也走这里
    pub fn refresh_plugins(&mut self, cx: &mut Context<Self>) {
        let fut = self.client.list_plugins();
        cx.spawn(async move |this, cx| {
            let result = fut.await;
            this.update(cx, |this, cx| match result {
                Ok(list) => this.apply_plugins(list, cx),
                Err(err) => {
                    log::warn!("读取插件列表失败：{err}");
                    this.handle_error(&err, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn apply_plugins(&mut self, list: PluginList, cx: &mut Context<Self>) {
        if self.plugins.as_ref() != Some(&list) {
            self.plugins = Some(list);
            cx.notify();
        }
    }

    pub fn plugin(&self, id: &str) -> Option<&PluginInfo> {
        self.plugins.as_ref()?.plugins.iter().find(|p| p.id == id)
    }

    /// 右侧栏上放的插件：启用了、清单没坏的，顺序同服务端（内置的在前，其余按 id）
    pub fn panel_plugins(&self) -> Vec<PluginInfo> {
        self.plugins
            .as_ref()
            .map(|l| l.plugins.iter().filter(|p| p.enabled && p.error.is_none()).cloned().collect())
            .unwrap_or_default()
    }

    /// 设置页的开关。服务端回整份列表，直接换上
    pub fn set_plugin_enabled(&mut self, id: &str, enabled: bool, cx: &mut Context<Self>) {
        let fut = self.client.set_plugin_enabled(id, enabled);
        cx.spawn(async move |this, cx| {
            let result = fut.await;
            this.update(cx, |this, cx| match result {
                Ok(list) => this.apply_plugins(list, cx),
                Err(err) => {
                    this.handle_error(&err, cx);
                    this.toast(ToastKind::Danger, t!("plugin.toggleFailed").to_string(), Some(err.to_string()), cx);
                }
            })
            .ok();
        })
        .detach();
    }
}
