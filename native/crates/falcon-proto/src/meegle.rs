//! 右侧「飞书项目」面板的数据协议（ADR 0010）。
//!
//! 数据源是宿主机（falcon 后端所在机器）上的 `meegle` CLI（@lark-project/meegle）：
//! 后端只负责起进程、解析 JSON、把几个接口的原始形状收敛成下面这些扁平结构；
//! 前端不认识 CLI 的任何原始字段。
//!
//! 业务请求撞上 409 就重新拉 `/api/meegle/status`，让面板自己切到安装 / 登录提示。

use serde::{Deserialize, Serialize};

use crate::wire::wire_enum;

/// 面板里可选的站点；自定义域名走手填
pub const MEEGLE_HOSTS: [&str; 2] = ["project.feishu.cn", "meegle.com"];

/// Meegle 标识符（空间 key、类型 key、视图 id）的边界：`^[A-Za-z0-9_][A-Za-z0-9_-]{0,63}$`。
///
/// CLI / REST / 浏览器拖放共用同一套边界，避免某一层静默拒绝合法工作项。
pub fn is_valid_meegle_key(value: &str) -> bool {
    let b = value.as_bytes();
    let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    match b.split_first() {
        Some((&first, rest)) => word(first) && rest.len() <= 63 && rest.iter().all(|&c| word(c) || c == b'-'),
        None => false,
    }
}

/// 工作项 id 的边界：`^\d{1,20}$`（只认 ASCII 数字，与 JS 的 `\d` 一致）。
pub fn is_valid_meegle_work_item_id(value: &str) -> bool {
    (1..=20).contains(&value.len()) && value.bytes().all(|c| c.is_ascii_digit())
}

wire_enum! {
    /// CLI 不可用的原因，随 409 一起回给前端；前端据此切到安装 / 登录提示。
    ///
    /// Rust 侧加了 `Unknown` 兜底：原因类（服务端内部其实还有 cli-error / bad-input
    /// 两个值，只是它们走 502 / 400，不该出现在 409 里）。
    pub enum MeegleUnavailableReason open {
        NotInstalled => "not-installed",
        NotAuthenticated => "not-authenticated",
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MeegleUser {
    pub key: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
}

/// 正在进行中的 device-code 登录：授权链接与授权码来自 CLI 的输出
/// （`POST /api/meegle/login` 的响应，也挂在 [`MeegleStatus::login`] 上）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MeegleLogin {
    pub host: String,
    pub url: String,
    pub code: String,
}

/// `GET /api/meegle/status`
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MeegleStatus {
    pub installed: bool,
    /// 实际拿来 spawn 的可执行文件路径，排查"为什么说没装"用
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub authenticated: bool,
    /// 已配置的站点（project.feishu.cn / meegle.com / 私有域名），未配置为 null
    #[serde(default)]
    pub host: Option<String>,
    /// CLI 原样报的数（服务端只查了 `typeof === "number"`），不保证是整数，所以用 f64
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in_minutes: Option<f64>,
    /// 登录后才有；取不到（网络等）时缺省，不影响 authenticated
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<MeegleUser>,
    /// 后端正拉着一个 `meegle auth login --device-code` 进程等用户授权
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<MeegleLogin>,
}

/// 空间（飞书项目里的 project），`GET /api/meegle/spaces` 的元素
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MeegleSpace {
    pub key: String,
    pub name: String,
    pub simple_name: String,
}

/// `GET /api/meegle/spaces/:key/types` 的元素
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MeegleWorkItemType {
    pub key: String,
    pub name: String,
    pub api_name: String,
    pub disabled: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MeegleView {
    pub id: String,
    pub name: String,
    pub type_key: String,
    pub type_name: String,
}

/// REST 列表的逻辑页长；CLI 仍固定每页 50 条。
pub const MEEGLE_PAGE_SIZE: u32 = 100;

/// 工作项（列表 / 最近 / 搜索结果的元素）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MeegleWorkItem {
    pub id: String,
    pub name: String,
    /// 工作项业务字段的可读名称；不是所属空间，无法解析时缺省
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub business: Option<String>,
    pub space_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_name: Option<String>,
    pub type_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// 详情页地址，缺少空间 simple_name 时没有
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// CLI 给的时间字符串，原样透传（不是毫秒数）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

wire_enum! {
    /// 待办页的四个分组，`GET /api/meegle/todo?action=`。客户端 → 服务端单向，闭集。
    pub enum MeegleTodoAction {
        Todo => "todo",
        ThisWeek => "this_week",
        Overdue => "overdue",
        Done => "done",
    }
}

/// 待办列表的元素。TS 里是 `extends MeegleWorkItem`，这里用 flatten 摊平，
/// 线上形状不变；`Deref` 到 [`MeegleWorkItem`]。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MeegleTodoItem {
    #[serde(flatten)]
    pub item: MeegleWorkItem,
    /// 节点流工作项当前停在我这里的节点
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_name: Option<String>,
    /// 状态流工作项当前状态
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule_start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule_end: Option<String>,
    /// 已办列表里的完成时间
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
}

impl std::ops::Deref for MeegleTodoItem {
    type Target = MeegleWorkItem;
    fn deref(&self) -> &MeegleWorkItem {
        &self.item
    }
}

/// 分页列表（视图条目、全景视图条目、待办）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MeeglePage<T> {
    pub items: Vec<T>,
    /// 从 1 起
    pub page: u32,
    pub has_more: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
}

/// 关键字搜索（`GET /api/meegle/spaces/:key/search`）：视图与工作项两路并行，
/// 各自的失败记在 errors 里，不拖垮另一路
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MeegleSearchResult {
    pub views: Vec<MeegleView>,
    pub items: Vec<MeegleWorkItem>,
    pub errors: Vec<String>,
}

/// 粘贴的飞书项目链接解析结果（`POST /api/meegle/resolve-url`）：后端用 CLI 的
/// `url decode` 认路由，再把 simple_name 换成 project_key。
///
/// 单个响应、按 `kind` 判别；认不出的 kind 解码失败即可（前端提示"不支持的链接"），
/// 不加 `Unknown` 兜底。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum MeegleUrlTarget {
    #[serde(rename = "workitem")]
    WorkItem {
        space_key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        space_name: Option<String>,
        type_key: String,
        id: String,
        url: String,
    },
    #[serde(rename = "view")]
    View {
        space_key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        space_name: Option<String>,
        view_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        type_key: Option<String>,
        url: String,
    },
    #[serde(rename = "multiProjectView")]
    MultiProjectView {
        space_key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        space_name: Option<String>,
        view_id: String,
        url: String,
    },
}

wire_enum! {
    /// 固定项的种类。
    ///
    /// Rust 侧加了 `Unknown` 兜底：固定项是存在服务端 SQLite 里的列表数据，新服务端
    /// 多一种可固定的东西时，老客户端不该因为一行认不出就整张固定列表解不出来。
    pub enum MeeglePinKind open {
        View => "view",
        MultiProjectView => "multiProjectView",
        WorkItem => "workitem",
    }
}

/// 面板里固定的视图 / 工作项（`GET /api/meegle/pins` 的元素），存在后端 SQLite 里：
/// 登录态是整台机器一份，固定列表也跟着机器走
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MeeglePin {
    pub id: String,
    pub kind: MeeglePinKind,
    pub space_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_name: Option<String>,
    /// 视图 id 或工作项 id
    pub target_id: String,
    /// 工作项才有
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_key: Option<String>,
    /// 用户可改的显示名；从链接打开的视图拿不到名字，默认就是 id
    pub label: String,
    /// 飞书里的地址，点外链用；搜出来的普通视图没有
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// unix 毫秒
    pub created_at: i64,
}

/// 固定一项（`POST /api/meegle/pins`）。同一个东西再固定一次，服务端回已有的那条。
/// 各字段服务端都过白名单：它们之后会原样进 CLI 的 argv。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MeeglePinInput {
    pub kind: MeeglePinKind,
    pub space_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_name: Option<String>,
    pub target_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_key: Option<String>,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// 工作项附件字段中的文件（[`MeegleWorkItemDetail::attachments`] 的元素）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MeegleAttachment {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 可能仍要求飞书登录态
    pub url: String,
}

/// 评论（[`MeegleWorkItemDetail::comments`] 的元素）。人员标识不进入 AI 上下文。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MeegleComment {
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// 评论所带附件
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<String>>,
}

/// [`MeegleWorkItemDetail::context_fields`] 的元素。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MeegleContextField {
    pub name: String,
    pub value: String,
}

/// [`MeegleWorkItemDetail::current_nodes`] 的元素。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MeegleCurrentNode {
    pub name: String,
    pub owners: Vec<String>,
}

/// [`MeegleWorkItemDetail::roles`] 的元素。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MeegleRole {
    pub name: String,
    pub members: Vec<String>,
}

/// 工作项详情（`GET /api/meegle/spaces/:key/items/:id`）。TS 里是
/// `extends MeegleWorkItem`，这里用 flatten 摊平；`Deref` 到 [`MeegleWorkItem`]。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MeegleWorkItemDetail {
    #[serde(flatten)]
    pub item: MeegleWorkItem,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub simple_name: Option<String>,
    /// 节点流 / 状态流
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 保留图片、链接、代码的原始 Markdown，仅剥掉 HTML 注释
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description_markdown: Option<String>,
    /// 工作项附件字段中的文件；URL 可能仍要求飞书登录态
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<MeegleAttachment>>,
    /// 附件字段读取失败时保留基础详情，并在复制内容中明确提示
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments_unavailable: Option<bool>,
    /// 评论正文及评论所带附件；人员标识不进入 AI 上下文
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comments: Option<Vec<MeegleComment>>,
    /// 评论接口失败时保留基础详情，并在复制内容中明确提示
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comments_unavailable: Option<bool>,
    /// 按字段元数据筛选的排障信息，不包含人员字段
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_fields: Option<Vec<MeegleContextField>>,
    /// 基础详情可读但补充字段请求失败；复制上下文不能把这种降级当作完整读取。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_fields_unavailable: Option<bool>,
    /// CLI 给的时间字符串，原样透传
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_by: Option<String>,
    /// 当前进行中的节点及其负责人
    pub current_nodes: Vec<MeegleCurrentNode>,
    /// 当前负责人（current_status_operator）
    pub operators: Vec<String>,
    /// 有人的角色
    pub roles: Vec<MeegleRole>,
}

impl std::ops::Deref for MeegleWorkItemDetail {
    type Target = MeegleWorkItem;
    fn deref(&self) -> &MeegleWorkItem {
        &self.item
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::roundtrip;

    #[test]
    fn identifier_bounds_match_ts_regex() {
        assert!(is_valid_meegle_key("abc_1-x"));
        assert!(is_valid_meegle_key("_x"));
        assert!(is_valid_meegle_key(&"a".repeat(64)));
        assert!(!is_valid_meegle_key(&"a".repeat(65)));
        assert!(!is_valid_meegle_key("-x"));
        assert!(!is_valid_meegle_key(""));
        assert!(!is_valid_meegle_key("a b"));
        assert!(!is_valid_meegle_key("空间"));
        assert!(is_valid_meegle_work_item_id("6203847123"));
        assert!(is_valid_meegle_work_item_id(&"9".repeat(20)));
        assert!(!is_valid_meegle_work_item_id(&"9".repeat(21)));
        assert!(!is_valid_meegle_work_item_id(""));
        assert!(!is_valid_meegle_work_item_id("12a"));
        assert!(!is_valid_meegle_work_item_id("１２")); // 全角数字不算
    }

    #[test]
    fn status_states() {
        roundtrip::<MeegleStatus>(r#"{"installed":false,"authenticated":false,"host":null}"#);
        let s = roundtrip::<MeegleStatus>(
            r#"{"installed":true,"bin":"/opt/falcon/bin/meegle","version":"1.0.23","authenticated":true,
                "host":"project.feishu.cn","expiresInMinutes":1440,
                "user":{"key":"u1","name":"费","email":"fay@example.com","avatarUrl":"https://a/x.png"}}"#,
        );
        assert_eq!(s.expires_in_minutes, Some(1440.0));
        let s = roundtrip::<MeegleStatus>(
            r#"{"installed":true,"authenticated":false,"host":"meegle.com","expiresInMinutes":0.5,
                "login":{"host":"meegle.com","url":"https://meegle.com/device","code":"ABCD-1234"}}"#,
        );
        assert_eq!(s.login.unwrap().code, "ABCD-1234");
    }

    #[test]
    fn spaces_types_views() {
        roundtrip::<Vec<MeegleSpace>>(r#"[{"key":"sp1","name":"Falcon","simpleName":"falcon"}]"#);
        roundtrip::<Vec<MeegleWorkItemType>>(
            r#"[{"key":"story","name":"需求","apiName":"story","disabled":false}]"#,
        );
        roundtrip::<MeegleSearchResult>(
            r#"{"views":[{"id":"v1","name":"本周","typeKey":"story","typeName":"需求"}],
                "items":[{"id":"1001","name":"登录页","spaceKey":"sp1","typeKey":"story"}],
                "errors":["视图搜索失败：rate limited"]}"#,
        );
    }

    #[test]
    fn pages() {
        let p = roundtrip::<MeeglePage<MeegleWorkItem>>(
            r#"{"items":[{"id":"1001","name":"登录页","business":"支付","spaceKey":"sp1","spaceName":"Falcon",
                          "typeKey":"story","typeName":"需求","status":"开发中",
                          "url":"https://project.feishu.cn/falcon/story/detail/1001","updatedAt":"2026-09-20 10:00"}],
                "page":1,"hasMore":true,"total":250}"#,
        );
        assert_eq!(p.total, Some(250));
        let t = roundtrip::<MeeglePage<MeegleTodoItem>>(
            r#"{"items":[{"id":"1002","name":"修 bug","spaceKey":"sp1","typeKey":"issue","nodeName":"开发",
                          "scheduleStart":"2026-09-20","scheduleEnd":"2026-09-25"},
                         {"id":"1003","name":"已完成","spaceKey":"sp1","typeKey":"issue","stateName":"已关闭",
                          "finishedAt":"2026-09-19"}],
                "page":2,"hasMore":false}"#,
        );
        assert_eq!(t.items[0].name, "修 bug"); // 经 Deref
        assert_eq!(t.items[1].state_name.as_deref(), Some("已关闭"));
        assert_eq!(MeegleTodoAction::ThisWeek.as_str(), "this_week");
    }

    #[test]
    fn url_target_every_kind() {
        let w = roundtrip::<MeegleUrlTarget>(
            r#"{"kind":"workitem","spaceKey":"sp1","spaceName":"Falcon","typeKey":"story","id":"1001",
                "url":"https://project.feishu.cn/falcon/story/detail/1001"}"#,
        );
        assert!(matches!(w, MeegleUrlTarget::WorkItem { .. }));
        roundtrip::<MeegleUrlTarget>(
            r#"{"kind":"view","spaceKey":"sp1","viewId":"v1","typeKey":"story","url":"https://x/view/v1"}"#,
        );
        roundtrip::<MeegleUrlTarget>(r#"{"kind":"view","spaceKey":"sp1","viewId":"v1","url":"https://x/v"}"#);
        let m = roundtrip::<MeegleUrlTarget>(
            r#"{"kind":"multiProjectView","spaceKey":"sp1","spaceName":"Falcon","viewId":"mv1","url":"https://x/mv"}"#,
        );
        assert!(matches!(m, MeegleUrlTarget::MultiProjectView { .. }));
    }

    #[test]
    fn pins() {
        let list = roundtrip::<Vec<MeeglePin>>(
            r#"[{"id":"pin1","kind":"workitem","spaceKey":"sp1","spaceName":"Falcon","targetId":"1001",
                 "typeKey":"story","label":"登录页","url":"https://x/1001","createdAt":1758700000000},
                {"id":"pin2","kind":"multiProjectView","spaceKey":"sp1","targetId":"mv1","label":"mv1",
                 "createdAt":1758700000001},
                {"id":"pin3","kind":"view","spaceKey":"sp1","targetId":"v1","label":"本周","createdAt":1758700000002}]"#,
        );
        assert_eq!(list[1].kind, MeeglePinKind::MultiProjectView);
        let odd = serde_json::from_str::<MeeglePin>(
            r#"{"id":"p","kind":"chart","spaceKey":"s","targetId":"c1","label":"c","createdAt":0}"#,
        )
        .unwrap();
        assert_eq!(odd.kind, MeeglePinKind::Unknown);
        roundtrip::<MeeglePinInput>(
            r#"{"kind":"view","spaceKey":"sp1","targetId":"v1","label":"本周"}"#,
        );
        roundtrip::<MeeglePinInput>(
            r#"{"kind":"workitem","spaceKey":"sp1","spaceName":"Falcon","targetId":"1001","typeKey":"story",
                "label":"登录页","url":"https://x/1001"}"#,
        );
    }

    #[test]
    fn work_item_detail() {
        let d = roundtrip::<MeegleWorkItemDetail>(
            r#"{"id":"1001","name":"登录页","spaceKey":"sp1","spaceName":"Falcon","typeKey":"story",
                "typeName":"需求","status":"开发中","url":"https://x/1001","updatedAt":"2026-09-20",
                "simpleName":"falcon","mode":"节点流","template":"默认","priority":"P1",
                "description":"做个登录页","descriptionMarkdown":"做个**登录页**\n![](https://x/img.png)",
                "attachments":[{"name":"设计稿.fig","url":"https://x/a"},{"url":"https://x/b"}],
                "comments":[{"content":"看下这个","createdAt":"2026-09-21","attachments":["https://x/c"]},
                            {"content":"好"}],
                "contextFields":[{"name":"环境","value":"staging"}],
                "createdAt":"2026-09-01","createdBy":"费","updatedBy":"费",
                "currentNodes":[{"name":"开发","owners":["费"]}],"operators":["费"],
                "roles":[{"name":"PM","members":["甲"]}]}"#,
        );
        assert_eq!(d.name, "登录页");
        assert_eq!(d.attachments.as_ref().unwrap()[1].name, None);
        // 降级：附件 / 评论 / 补充字段读取失败，只剩基础详情
        roundtrip::<MeegleWorkItemDetail>(
            r#"{"id":"1002","name":"x","spaceKey":"sp1","typeKey":"issue",
                "attachmentsUnavailable":true,"commentsUnavailable":true,"contextFieldsUnavailable":true,
                "currentNodes":[],"operators":[],"roles":[]}"#,
        );
    }

    #[test]
    fn unavailable_reason() {
        assert_eq!(
            roundtrip::<MeegleUnavailableReason>(r#""not-authenticated""#),
            MeegleUnavailableReason::NotAuthenticated
        );
        assert_eq!(
            serde_json::from_str::<MeegleUnavailableReason>(r#""cli-error""#).unwrap(),
            MeegleUnavailableReason::Unknown
        );
    }
}
