//! 右侧「飞书项目」面板（ADR 0010，`/api/meegle/*`）。
//!
//! 数据源是 falcon 后端本机上的 meegle CLI。错误码约定：CLI 没装 / 没登录是 409
//! （`err.meegle_reason()` 给出原因，app 据此重新拉 `meegle_status` 切到安装 / 登录
//! 提示）；CLI 跑了但飞书那边报错是 502；参数不合法 / 链接不支持是 400。
//!
//! 查询默认走服务端的 TTL 缓存（5 分钟）；`fresh = true` 打穿（面板的刷新按钮）。

use std::future::Future;

use falcon_proto::{
    MeegleLogin, MeeglePage, MeeglePin, MeeglePinInput, MeegleSearchResult, MeegleSpace,
    MeegleStatus, MeegleTodoAction, MeegleTodoItem, MeegleUrlTarget, MeegleWorkItem,
    MeegleWorkItemDetail, MeegleWorkItemType, OkResponse,
};
use reqwest::Method;
use serde::Serialize;

use super::{Query, seg};
use crate::client::{FalconClient, json_body};
use crate::error::ApiResult;

impl FalconClient {
    /// `GET /api/meegle/status`：装没装、登没登、站点、用户、进行中的 device-code 登录。
    pub fn meegle_status(&self, fresh: bool) -> impl Future<Output = ApiResult<MeegleStatus>> + Send + 'static {
        let q = Query::new().flag("fresh", fresh).finish();
        self.get(format!("/api/meegle/status{q}"))
    }

    /// `POST /api/meegle/login`：起一个 device-code 登录进程，返回授权链接与授权码
    /// （app 用系统浏览器打开链接，再轮询 `meegle_status`）。
    pub fn meegle_login(&self, host: &str) -> impl Future<Output = ApiResult<MeegleLogin>> + Send + 'static {
        #[derive(Serialize)]
        struct Body<'a> {
            host: &'a str,
        }
        self.json(Method::POST, "/api/meegle/login".to_owned(), json_body(&Body { host }))
    }

    pub fn meegle_cancel_login(&self) -> impl Future<Output = ApiResult<OkResponse>> + Send + 'static {
        self.bare(Method::POST, "/api/meegle/login/cancel".to_owned())
    }

    /// 清掉服务端的 TTL 缓存（面板顶栏的刷新）。
    pub fn meegle_clear_cache(&self) -> impl Future<Output = ApiResult<OkResponse>> + Send + 'static {
        self.bare(Method::POST, "/api/meegle/cache/clear".to_owned())
    }

    /// `GET /api/meegle/spaces`。`q` 是按名字筛（服务端支持，web 的 api.ts 没用上）。
    pub fn meegle_spaces(
        &self,
        q: Option<&str>,
        fresh: bool,
    ) -> impl Future<Output = ApiResult<Vec<MeegleSpace>>> + Send + 'static {
        let q = Query::new().nonempty("q", q).flag("fresh", fresh).finish();
        self.get(format!("/api/meegle/spaces{q}"))
    }

    pub fn meegle_types(
        &self,
        space_key: &str,
        fresh: bool,
    ) -> impl Future<Output = ApiResult<Vec<MeegleWorkItemType>>> + Send + 'static {
        let q = Query::new().flag("fresh", fresh).finish();
        self.get(format!("/api/meegle/spaces/{}/types{q}", seg(space_key)))
    }

    /// 关键字搜索：视图与工作项两路并行，各自的失败记在 `errors` 里。缺关键字是 400。
    pub fn meegle_search(
        &self,
        space_key: &str,
        q: &str,
        type_key: Option<&str>,
        fresh: bool,
    ) -> impl Future<Output = ApiResult<MeegleSearchResult>> + Send + 'static {
        let q = Query::new().push("q", q).nonempty("type", type_key).flag("fresh", fresh).finish();
        self.get(format!("/api/meegle/spaces/{}/search{q}", seg(space_key)))
    }

    pub fn meegle_recent(
        &self,
        space_key: &str,
        type_key: &str,
        fresh: bool,
    ) -> impl Future<Output = ApiResult<Vec<MeegleWorkItem>>> + Send + 'static {
        let q = Query::new().push("type", type_key).flag("fresh", fresh).finish();
        self.get(format!("/api/meegle/spaces/{}/recent{q}", seg(space_key)))
    }

    /// 视图条目的一页（`page` 从 1 起）。
    pub fn meegle_view_items(
        &self,
        space_key: &str,
        view_id: &str,
        page: u32,
        fresh: bool,
    ) -> impl Future<Output = ApiResult<MeeglePage<MeegleWorkItem>>> + Send + 'static {
        let q = Query::new().push("page", &page.to_string()).flag("fresh", fresh).finish();
        self.get(format!("/api/meegle/spaces/{}/views/{}/items{q}", seg(space_key), seg(view_id)))
    }

    pub fn meegle_work_item(
        &self,
        space_key: &str,
        id: &str,
        fresh: bool,
    ) -> impl Future<Output = ApiResult<MeegleWorkItemDetail>> + Send + 'static {
        let q = Query::new().flag("fresh", fresh).finish();
        self.get(format!("/api/meegle/spaces/{}/items/{}{q}", seg(space_key), seg(id)))
    }

    /// 待办页的一个分组（`page` 从 1 起）。
    pub fn meegle_todo(
        &self,
        action: MeegleTodoAction,
        page: u32,
        fresh: bool,
    ) -> impl Future<Output = ApiResult<MeeglePage<MeegleTodoItem>>> + Send + 'static {
        let q = Query::new()
            .push("action", action.as_str())
            .push("page", &page.to_string())
            .flag("fresh", fresh)
            .finish();
        self.get(format!("/api/meegle/todo{q}"))
    }

    /// 全景视图条目的一页。
    pub fn meegle_multi_view_items(
        &self,
        space_key: &str,
        view_id: &str,
        page: u32,
        fresh: bool,
    ) -> impl Future<Output = ApiResult<MeeglePage<MeegleWorkItem>>> + Send + 'static {
        let q = Query::new().push("page", &page.to_string()).flag("fresh", fresh).finish();
        self.get(format!(
            "/api/meegle/spaces/{}/multi-views/{}/items{q}",
            seg(space_key),
            seg(view_id)
        ))
    }

    /// 粘贴的飞书项目链接 → 视图 / 全景视图 / 工作项。不支持的链接是 400。
    pub fn meegle_resolve_url(&self, url: &str) -> impl Future<Output = ApiResult<MeegleUrlTarget>> + Send + 'static {
        #[derive(Serialize)]
        struct Body<'a> {
            url: &'a str,
        }
        self.json(Method::POST, "/api/meegle/resolve-url".to_owned(), json_body(&Body { url }))
    }

    pub fn meegle_pins(&self) -> impl Future<Output = ApiResult<Vec<MeeglePin>>> + Send + 'static {
        self.get("/api/meegle/pins".to_owned())
    }

    /// 固定一项。同一个东西再固定一次，服务端回已有的那条。字段过白名单，不合法是 400。
    pub fn meegle_pin(&self, input: &MeeglePinInput) -> impl Future<Output = ApiResult<MeeglePin>> + Send + 'static {
        self.json(Method::POST, "/api/meegle/pins".to_owned(), json_body(input))
    }

    pub fn meegle_rename_pin(&self, id: &str, label: &str) -> impl Future<Output = ApiResult<MeeglePin>> + Send + 'static {
        #[derive(Serialize)]
        struct Body<'a> {
            label: &'a str,
        }
        self.json(Method::PATCH, format!("/api/meegle/pins/{}", seg(id)), json_body(&Body { label }))
    }

    pub fn meegle_unpin(&self, id: &str) -> impl Future<Output = ApiResult<OkResponse>> + Send + 'static {
        self.bare(Method::DELETE, format!("/api/meegle/pins/{}", seg(id)))
    }
}
