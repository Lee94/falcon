//! 右侧栏插件（ADR 0025，`/api/plugins/*`）。
//!
//! 错误码约定：没有这个插件 404；停用了 / 清单坏了 409（app 撞上就重新拉一次列表）；参数不合法 400；
//! 插件跑了但失败（退出码非零、超时、输出不是合法 JSON）502，`err.to_string()` 就是给插件作者看的原话。

use std::future::Future;

use falcon_proto::{PluginActionInput, PluginEnabledInput, PluginList, PluginRenderInput, PluginView};
use reqwest::Method;

use super::seg;
use crate::client::{FalconClient, json_body};
use crate::error::ApiResult;
use crate::runtime::MaybeSend;

impl FalconClient {
    /// `GET /api/plugins`：内置的与命令插件（含停用的、清单坏的），以及命令插件放在哪个目录。
    pub fn list_plugins(&self) -> impl Future<Output = ApiResult<PluginList>> + MaybeSend + 'static {
        self.get("/api/plugins".to_owned())
    }

    /// `POST /api/plugins/:id/render`：画一次面板。
    pub fn plugin_render(
        &self,
        id: &str,
        input: &PluginRenderInput,
    ) -> impl Future<Output = ApiResult<PluginView>> + MaybeSend + 'static {
        self.json(Method::POST, format!("/api/plugins/{}/render", seg(id)), json_body(input))
    }

    /// `POST /api/plugins/:id/action`：点了 invoke 按钮，回新的面板。
    pub fn plugin_action(
        &self,
        id: &str,
        input: &PluginActionInput,
    ) -> impl Future<Output = ApiResult<PluginView>> + MaybeSend + 'static {
        self.json(Method::POST, format!("/api/plugins/{}/action", seg(id)), json_body(input))
    }

    /// `PUT /api/plugins/:id/enabled`：启用 / 停用，回整份列表。
    pub fn set_plugin_enabled(
        &self,
        id: &str,
        enabled: bool,
    ) -> impl Future<Output = ApiResult<PluginList>> + MaybeSend + 'static {
        self.json(Method::PUT, format!("/api/plugins/{}/enabled", seg(id)), json_body(&PluginEnabledInput { enabled }))
    }
}
