//! 移植自 `packages/server/src/meegle/command.ts`；单测是 `command.test.ts` 与
//! `context.test.ts`（后者测的 normalizeFields / businessText / detailContext /
//! normalizeComments 等也都在 command.ts 里）的全部用例。
//!
//! meegle CLI（@lark-project/meegle）的命令构造与输出解析。
//!
//! 与 git/command.ts 同一分层：**纯函数，零 I/O**。不同的是这里只产出 argv 数组、
//! 从不拼命令行字符串——CLI 只在 falcon 后端所在的机器上跑（登录态存在那台机器的
//! 钥匙串里，远端主机上没有），直接 spawn 不经 shell，没有转义问题。
//!
//! 实测过的几条 CLI 脾气（1.0.9 与内置的 1.0.23 行为一致），代码里的判断都来自这些：
//! - 成功的 JSON 走 stdout；业务失败是 `{data:null, error:{code,message}}` 的 JSON，
//!   **打到 stderr**、退出码 1（`auth status` 未登录时同样退出码 1 但 JSON 在 stdout）。
//!   所以两条流都要试着当 JSON 解析，退出码只是旁证。
//! - 命令清单是登录后从服务端拉的（缓存在 ~/.meegle/cache）。**未登录时任何业务
//!   命令都报 `unknown command`**，与真正打错命令无法区分，得再问一次 auth status。
//! - `view search` 三个参数都必填，关键字是子串匹配，没有"列出全部视图"的路子。
//! - `mywork todo` 返回的 work_item_name 是空串，名字要靠 MQL 按 ID 补。
//! - MQL 的 FROM 可以直接用 project_key 与 type_key，不必换成空间名 / 类型名；
//!   单引号按 SQL 规矩双写即可。
//! - business 是工作项业务字段：get 给叶子 ID，meta-fields 的 list[].option 是
//!   option_id / option_name / children 树，MQL 给 cascade_key_label_value 的完整路径。
//!   未知 ID 不能当名称、也不能拿 owned_project 顶替。详情只按元数据挑排障字段。
//! - MQL LIMIT 100 仍只回 50 条；下一页用 session_id + group_id "1" / page_num 2。
//! - 用户输入一律走 `--flag=value` 形式：值以 `-` 开头时（关键字"-foo"）
//!   `--flag value` 会被 pflag 当成下一个 flag。
//!
//! # 移植约定
//!
//! - CLI 的原始输出用 `serde_json::Value` 读入，产出 falcon-proto 里的类型（与原生客户端
//!   同一份）。TS 里 `data: unknown` 的参数一律收 `&Value`：这些函数对 `undefined` 与
//!   `null` 走同一条分支，调用方拿不到值时传 `&Value::Null` 即可。
//! - TS 的默认参数（`page = 1`、`limit = MEEGLE_PAGE_SIZE`）在 Rust 里是 `Option`，`None`
//!   即默认值。
//! - `str()` 把数字转成字符串时照 JS 的 `String(number)` 走（见 [`js::number_to_string`]），
//!   工作项 id 常以数字形式出现在 CLI 输出里，这个格式必须一致。
//! - 正则：JS 的 `\s` / `\S` / `.` / `\d` 与 Rust 的 Unicode 语义不同，一律写成显式字符类
//!   （[`js::WS`] 等）；不带 `u` 标志的 `/…/i` 只把 ASCII 字母当成大小写不敏感，Rust 的
//!   `(?i)` 还会让 U+212A（开尔文符号）配上 `k`、U+017F（长 s）配上 `s`，所以经
//!   [`js::ci`] 把字母展开成 `[xX]`，不用 `(?i)`。
//! - JS 对象的键序：`Object.values` 先按数值升序给出数组下标形的键，再按插入序给出其余；
//!   serde_json 的 Map 在本 workspace 里随 feature 合并可能是插入序也可能是字典序
//!   （GPUI 会打开 preserve_order），所以 [`normalize_mql_rows`] 自己排。
//!
//! 不在纯函数层的（起进程 / 缓存 / 路由）：
//! - `client.ts` → [`super::client`]（`MeegleClient` 全部方法、`MeegleError`、`mapLimit`）；
//! - `bin.ts` → [`super::bin`]（`command.test.ts` 里测它的那条 "bundledBinName 与 npm 包的
//!   bin/ 命名一致" 也在那边；S7 内嵌 meegle 后解析方式会变）；
//! - `routes.ts`：`registerMeegleRoutes`、`queryOpts`（→ `MeegleQueryOpts::from_fresh_param`）、
//!   `pinLabel`、`optionalText`、`parsePinInput`、`pageOf` 随 api 层的路由一起移植。

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use falcon_proto::{
    MEEGLE_PAGE_SIZE, MeegleAttachment, MeegleComment, MeegleContextField, MeegleCurrentNode, MeeglePage, MeegleRole,
    MeegleSpace, MeegleTodoAction, MeegleTodoItem, MeegleUser, MeegleView, MeegleWorkItem, MeegleWorkItemDetail,
    MeegleWorkItemType, is_valid_meegle_key, is_valid_meegle_work_item_id,
};
use regex::Regex;
use serde_json::{Map, Value};

/// 交互式启动时的更新提示会往 stdout 里塞非 JSON，明确关掉
pub const CLI_ENV: &[(&str, &str)] = &[("MEEGLE_NO_UPDATE_CHECK", "1")];

/// CLI 的固定页长（project search / view get / mywork todo / MQL 都是 50）
pub const CLI_PAGE_SIZE: u32 = 50;

pub const TODO_ACTIONS: &[MeegleTodoAction] = MeegleTodoAction::ALL;

/// TS 的类型守卫 `isTodoAction`；要拿到枚举值用 [`MeegleTodoAction::from_wire`]
pub fn is_todo_action(v: &str) -> bool {
    MeegleTodoAction::from_wire(v).is_some()
}

/// 站点域名：只放行主机名字符，`meegle auth login --host` 后面不该出现别的
///
/// TS 收 `unknown`、非字符串一律 false；这里收 `&str`，非字符串由调用方的类型挡掉。
pub fn is_valid_host(v: &str) -> bool {
    // /…/i 只有 ASCII 字母大小写不敏感：显式写 a-zA-Z，不用 (?i)（见文件头）
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^[a-zA-Z0-9](?:[a-zA-Z0-9-]*[a-zA-Z0-9])?(?:\.[a-zA-Z0-9](?:[a-zA-Z0-9-]*[a-zA-Z0-9])?)+$")
            .unwrap()
    });
    RE.is_match(v)
}

/// 空间 key / 类型 key / 视图 id 都是 URL 安全的短串；放行别的就等于把任意 argv 交给 CLI
pub fn is_valid_key(v: &str) -> bool {
    is_valid_meegle_key(v)
}

pub fn is_valid_id(v: &str) -> bool {
    is_valid_meegle_work_item_id(v)
}

/// 粘贴进来的飞书项目链接：只收 http(s)，不含空白 / 控制字符，长度封顶
pub fn is_valid_url(v: &str) -> bool {
    // v.length <= 2048（UTF-16 码元）&& /^https?:\/\/[^\s\u0000-\u001f]+$/
    if js::utf16_len(v) > 2048 {
        return false;
    }
    let rest = v.strip_prefix("https://").or_else(|| v.strip_prefix("http://"));
    rest.is_some_and(|r| !r.is_empty() && r.chars().all(|c| !js::is_ws(c) && c > '\u{1f}'))
}

// ---- argv ----

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_string()).collect()
}

pub fn status_args() -> Vec<String> {
    argv(&["auth", "status", "--format", "json"])
}

pub fn version_args() -> Vec<String> {
    argv(&["version"])
}

/// device-code 模式：不开浏览器，把授权链接打到 stdout，后端抓出来交给前端
pub fn login_args(host: &str) -> Vec<String> {
    vec!["auth".into(), "login".into(), format!("--host={host}"), "--device-code".into()]
}

pub fn me_args() -> Vec<String> {
    argv(&["user", "search", "--user-keys=current_login_user()", "--format", "json"])
}

/// 不带关键字 = 当前用户最近访问过的空间（按访问时间由近及远）。`page` 缺省为 1
pub fn spaces_args(keyword: Option<&str>, page: Option<u32>) -> Vec<String> {
    let mut args = argv(&["project", "search"]);
    // if (keyword)：空串与缺省一样不带
    if let Some(k) = keyword.filter(|k| !k.is_empty()) {
        args.push(format!("--project-key={k}"));
    }
    args.push(format!("--page-num={}", page.unwrap_or(1)));
    args.extend(argv(&["--format", "json"]));
    args
}

pub fn types_args(space_key: &str) -> Vec<String> {
    vec!["workitem".into(), "meta-types".into(), format!("--project-key={space_key}"), "--format".into(), "json".into()]
}

pub fn fields_args(space_key: &str, type_key: &str, page: u32) -> Vec<String> {
    vec![
        "workitem".into(),
        "meta-fields".into(),
        format!("--project-key={space_key}"),
        format!("--work-item-type={type_key}"),
        format!("--page-num={page}"),
        "--format".into(),
        "json".into(),
    ]
}

pub fn view_search_args(space_key: &str, type_key: &str, keyword: &str) -> Vec<String> {
    vec![
        "view".into(),
        "search".into(),
        format!("--project-key={space_key}"),
        format!("--view-scope={type_key}"),
        format!("--key-word={keyword}"),
        "--format".into(),
        "json".into(),
    ]
}

pub fn view_items_args(space_key: &str, view_id: &str, page: u32) -> Vec<String> {
    vec![
        "view".into(),
        "get".into(),
        format!("--project-key={space_key}"),
        format!("--view-id={view_id}"),
        format!("--page-num={page}"),
        "--format".into(),
        "json".into(),
    ]
}

pub fn todo_args(action: MeegleTodoAction, page: u32) -> Vec<String> {
    vec![
        "mywork".into(),
        "todo".into(),
        format!("--action={}", action.as_str()),
        format!("--page-num={page}"),
        "--format".into(),
        "json".into(),
    ]
}

pub fn query_args(space_key: &str, mql: &str) -> Vec<String> {
    vec![
        "workitem".into(),
        "query".into(),
        format!("--project-key={space_key}"),
        format!("--mql={mql}"),
        "--format".into(),
        "json".into(),
    ]
}

/// 全景视图（multiProjectView）里当前用户有权限看到的工作项
pub fn multi_view_items_args(space_key: &str, view_id: &str, page: u32) -> Vec<String> {
    vec![
        "view".into(),
        "list-multi-project-workitems".into(),
        format!("--project-key={space_key}"),
        format!("--view-id={view_id}"),
        format!("--page-num={page}"),
        "--format".into(),
        "json".into(),
    ]
}

/// 纯本地解析飞书项目链接，不走网络；路由表在 CLI 里，别自己拆路径
pub fn url_decode_args(url: &str) -> Vec<String> {
    vec!["url".into(), "decode".into(), format!("--url={url}"), "--format".into(), "json".into()]
}

/// 显式字段只用于补业务与排障上下文；基础详情仍使用默认字段。
///
/// `fields` 为 `None` 或空表时不带 `--fields`（TS 的 `fields?.length`）。值是
/// `JSON.stringify(fields)`：字符串数组的紧凑 JSON，serde_json 的转义与它逐字节一致。
pub fn work_item_args(space_key: &str, id: &str, fields: Option<&[String]>) -> Vec<String> {
    let mut args =
        vec!["workitem".into(), "get".into(), format!("--project-key={space_key}"), format!("--work-item-id={id}")];
    if let Some(fields) = fields.filter(|f| !f.is_empty()) {
        let json = serde_json::to_string(fields).expect("字符串数组总能序列化");
        args.push(format!("--fields={json}"));
    }
    args.extend(argv(&["--format", "json"]));
    args
}

pub fn comment_args(space_key: &str, id: &str, page: u32) -> Vec<String> {
    vec![
        "comment".into(),
        "list".into(),
        format!("--project-key={space_key}"),
        format!("--work-item-id={id}"),
        format!("--page-num={page}"),
        "--format".into(),
        "json".into(),
    ]
}

// ---- MQL ----

const MQL_FIELDS: &str = "`work_item_id`, `name`, `work_item_status`, `updated_at`, `business`";

/// [\u0000-\u001f\u007f]
fn is_mql_control(c: char) -> bool {
    c <= '\u{1f}' || c == '\u{7f}'
}

/// 字符串字面量：单引号双写；控制字符没有任何合法用途，换成空格
pub fn mql_literal(s: &str) -> String {
    let cleaned: String = s.chars().map(|c| if is_mql_control(c) { ' ' } else { c }).collect();
    format!("'{}'", cleaned.replace('\'', "''"))
}

/// 标识符：反引号包裹。key 都经过 is_valid_key，这里只是兜底把反引号剥掉
pub fn mql_ident(s: &str) -> String {
    let cleaned: String = s.chars().filter(|&c| c != '`' && !is_mql_control(c)).collect();
    format!("`{cleaned}`")
}

fn mql_from(space_key: &str, type_key: &str) -> String {
    format!("FROM {}.{}", mql_ident(space_key), mql_ident(type_key))
}

/// 按名称子串搜；`%` / `_` 是 LIKE 通配符，用户要是故意打了就让它当通配符。`limit` 缺省为
/// MEEGLE_PAGE_SIZE
pub fn mql_search(space_key: &str, type_key: &str, keyword: &str, limit: Option<usize>) -> String {
    format!(
        "SELECT {MQL_FIELDS} {} WHERE `name` LIKE {} ORDER BY `work_item_id` DESC LIMIT {}",
        mql_from(space_key, type_key),
        mql_literal(&format!("%{keyword}%")),
        clamp_limit(limit.unwrap_or(MEEGLE_PAGE_SIZE as usize))
    )
}

/// 类型下最新创建的工作项。id 单调递增，按它倒序就是"最近"。`limit` 缺省为 MEEGLE_PAGE_SIZE
pub fn mql_recent(space_key: &str, type_key: &str, limit: Option<usize>) -> String {
    format!(
        "SELECT {MQL_FIELDS} {} ORDER BY `work_item_id` DESC LIMIT {}",
        mql_from(space_key, type_key),
        clamp_limit(limit.unwrap_or(MEEGLE_PAGE_SIZE as usize))
    )
}

/// 按 ID 批量补名字 / 状态。ids 已经过 is_valid_id，非法的直接丢
pub fn mql_by_ids<S: AsRef<str>>(space_key: &str, type_key: &str, ids: &[S]) -> String {
    let clean: Vec<&str> = ids.iter().map(AsRef::as_ref).filter(|id| is_valid_id(id)).collect();
    format!(
        "SELECT {MQL_FIELDS} {} WHERE `work_item_id` IN ({}) LIMIT {}",
        mql_from(space_key, type_key),
        clean.join(", "),
        clamp_limit(clean.len())
    )
}

/// TS 是 `Math.max(1, Math.min(MEEGLE_PAGE_SIZE, Math.floor(n)))`；Rust 侧 limit 是整数，
/// floor 省掉
fn clamp_limit(n: usize) -> usize {
    n.clamp(1, MEEGLE_PAGE_SIZE as usize)
}

/// 按 `size` 切块。`size` 为 0 时 TS 会死循环，这里 panic（`slice::chunks` 的约定）
pub fn chunk<T: Clone>(arr: &[T], size: usize) -> Vec<Vec<T>> {
    arr.chunks(size).map(<[T]>::to_vec).collect()
}

// ---- 输出解析 ----

/// TS 的 `{ ok: false; code; message }`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliFailure {
    pub code: String,
    pub message: String,
}

impl CliFailure {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self { code: code.to_string(), message: message.into() }
    }
}

/// TS 的 `CliOutcome`：`{ ok: true; data }` 是 `Ok(data)`
pub type CliOutcome = Result<Value, CliFailure>;

/// TS 的 `str()`：非空字符串原样、数字照 `String(n)`，其余 undefined
fn str_of(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => n.as_f64().map(js::number_to_string),
        _ => None,
    }
}

/// `x.has_more === true`；`x` 不是对象时 `Value::get` 给 None，等价于 TS 的 `isObj(x) ? x : {}`
fn has_more_of(pagination: Option<&Value>) -> bool {
    matches!(pagination.and_then(|p| p.get("has_more")), Some(Value::Bool(true)))
}

/// `typeof x === "number" ? x : undefined`。线上类型（`MeeglePage::total`）是 u64：CLI 给的
/// total 总是非负整数；不是的话（TS 会原样透传 2.5 / -1）这里丢掉
fn total_of(v: Option<&Value>) -> Option<u64> {
    let n = v?.as_number()?;
    n.as_u64().or_else(|| n.as_f64().filter(|f| f.is_finite() && *f >= 0.0 && f.fract() == 0.0).map(|f| f as u64))
}

/// `/^https?:\/\//`
fn is_http_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

/// `.replace(/<!--[\s\S]*?-->/g, "")`
fn strip_html_comments(s: &str) -> String {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<!--.*?-->").unwrap());
    RE.replace_all(s, "").into_owned()
}

/// 字段元数据（`workitem meta-fields` 的一项）。只在服务端内部用，不上线
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldMetadata {
    pub key: String,
    pub name: String,
    /// TS 的 `type`
    pub field_type: String,
    /// option_id → 带父级的完整路径（`数据分析 / AI`）
    pub options: HashMap<String, String>,
}

/// 实测 meta-fields 返回 list + option 树；保留父级路径，不能用所属空间代替业务。
pub fn normalize_fields(data: &Value, page: u32) -> MeeglePage<FieldMetadata> {
    fn visit(nodes: Option<&Value>, parents: &[String], options: &mut HashMap<String, String>) {
        let Some(nodes) = nodes.and_then(Value::as_array) else { return };
        for node in nodes {
            if !node.is_object() {
                continue;
            }
            let name = str_of(node.get("option_name"));
            let id = str_of(node.get("option_id"));
            let mut path = parents.to_vec();
            if let Some(name) = &name {
                path.push(name.clone());
            }
            if let (Some(id), Some(_)) = (id, &name) {
                options.insert(id, path.join(" / "));
            }
            visit(node.get("children"), &path, options);
        }
    }

    let list = data.get("list").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    let mut items = Vec::new();
    for field in list {
        if !field.is_object() {
            continue;
        }
        // str() 通过了，String(field.field_key) 与 str() 的结果相同
        let Some(key) = str_of(field.get("field_key")) else { continue };
        let mut options = HashMap::new();
        visit(field.get("option"), &[], &mut options);
        items.push(FieldMetadata {
            name: str_of(field.get("field_name")).unwrap_or_else(|| key.clone()),
            field_type: str_of(field.get("field_type")).unwrap_or_default(),
            key,
            options,
        });
    }
    MeeglePage { items, page, has_more: has_more_of(data.get("pagination")), total: None }
}

/// IDs are only names when metadata proves it. MQL cascade labels are authoritative too.
///
/// `options` 为 `None` 即 TS 的默认空 Map。
pub fn business_text(value: Option<&Value>, options: Option<&HashMap<String, String>>) -> Option<String> {
    let lookup = |k: &str| options.and_then(|o| o.get(k)).cloned();
    match value? {
        Value::String(s) => lookup(s),
        Value::Number(n) => lookup(&js::number_to_string(n.as_f64()?)),
        Value::Array(items) => {
            let mut seen = HashSet::new();
            let labels: Vec<String> = items
                .iter()
                .filter_map(|v| business_text(Some(v), options))
                .filter(|s| !s.is_empty())
                // [...new Set(labels)]：去重，保留首次出现的顺序
                .filter(|s| seen.insert(s.clone()))
                .collect();
            (!labels.is_empty()).then(|| labels.join("、"))
        }
        Value::Object(o) => {
            if let Some(inner) = o.get("value").filter(|v| v.is_object()) {
                return business_text(Some(inner), options);
            }
            // value[key] !== undefined：键在（哪怕是 null）就交给它
            for key in ["cascade_key_label_value", "key_label_value", "key_label_value_list", "string_value"] {
                if let Some(v) = o.get(key) {
                    return business_text(Some(v), options);
                }
            }
            if let Some(label) = str_of(o.get("label")) {
                return Some(match business_text(o.get("children"), options) {
                    Some(children) => format!("{label} / {children}"),
                    None => label,
                });
            }
            lookup(&str_of(o.get("key")).unwrap_or_default())
        }
        _ => None,
    }
}

/// Only named diagnostic fields of safe types enter AI context; never stringify arbitrary objects.
pub fn is_context_field(field: &FieldMetadata) -> bool {
    static NAME: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(&js::ci(
            "复现|重现|预期|期望|实际|环境|版本|日志|堆栈|链接|repro|expected|actual|environment|version|logs?|stack.?trace|related.?links?",
        ))
        .unwrap()
    });
    static PERSON: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(&js::ci("人员|负责人|经办|邮箱|客户|用户|owner|operator|email|person|user")).unwrap()
    });
    static TYPE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            "^(text|multi-pure-text|multi-text|multi_text|rich_text|rich-text|textarea|select|multi-select|tree-select|tree-multi-select|cascade_select|link|url|number|workitem_related_(multi_)?select)$",
        )
        .unwrap()
    });
    NAME.is_match(&field.name)
        && field.key != "template_version"
        && !PERSON.is_match(&field.name.replace("客户环境", "环境"))
        && TYPE.is_match(&field.field_type)
}

pub fn is_attachment_field(field: &FieldMetadata) -> bool {
    static NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(&js::ci("附件|attachment")).unwrap());
    static TYPE: LazyLock<Regex> = LazyLock::new(|| Regex::new("^(multi-)?file$").unwrap());
    NAME.is_match(&field.name) && TYPE.is_match(&field.field_type)
}

/// 可能返回 `Some("")`（对象的 label 是空串时，与 TS 一致）；调用方按 JS 的真假把空串当没有
fn context_value(value: Option<&Value>, field: &FieldMetadata) -> Option<String> {
    match value? {
        Value::String(s) => {
            if field.field_type.contains("select") {
                return field.options.get(s).cloned();
            }
            Some(strip_html_comments(s)).filter(|s| !s.is_empty())
        }
        Value::Number(n) => n.as_f64().map(js::number_to_string),
        Value::Array(items) => {
            let parts: Vec<String> =
                items.iter().filter_map(|v| context_value(Some(v), field)).filter(|s| !s.is_empty()).collect();
            (!parts.is_empty()).then(|| parts.join("、"))
        }
        Value::Object(o) => {
            if field.field_type.starts_with("workitem_related_") {
                return str_of(o.get("name"));
            }
            if let Some(Value::String(label)) = o.get("label") {
                return Some(label.clone());
            }
            if let Some(Value::String(url)) = o.get("url")
                && is_http_url(url)
            {
                return Some(match o.get("name") {
                    Some(Value::String(name)) => format!("[{name}]({url})"),
                    _ => url.clone(),
                });
            }
            None
        }
        _ => None,
    }
}

/// [`detail_context`] 的结果，TS 里是 `Object.assign` 到详情上的三个字段
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DetailContext {
    pub business: Option<String>,
    pub context_fields: Vec<MeegleContextField>,
    pub attachments: Vec<MeegleAttachment>,
}

impl DetailContext {
    /// TS 的 `Object.assign(detail, detailContext(...))`：三个键总是在，`business` 为
    /// undefined 时也会把详情上原有的值盖掉；两个列表哪怕是空的也写上（序列化成 `[]`）
    pub fn assign_to(self, detail: &mut MeegleWorkItemDetail) {
        detail.item.business = self.business;
        detail.context_fields = Some(self.context_fields);
        detail.attachments = Some(self.attachments);
    }
}

pub fn detail_context(data: &Value, metadata: &[FieldMetadata]) -> DetailContext {
    let fields = data.get("work_item_fields").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    // new Map(metadata.map(...))：同名 key 后者覆盖前者
    let by_key: HashMap<&str, &FieldMetadata> = metadata.iter().map(|f| (f.key.as_str(), f)).collect();
    let mut out = DetailContext::default();
    for field in fields {
        let Some(Value::String(key)) = field.get("key") else { continue };
        let meta = by_key.get(key.as_str()).copied();
        let value = field.get("value");
        if key == "business" {
            out.business = business_text(value, meta.map(|m| &m.options));
        }
        if let Some(meta) = meta
            && is_attachment_field(meta)
        {
            // Array.isArray(field.value) ? field.value : [field.value]
            let values: Vec<&Value> = match value {
                Some(Value::Array(items)) => items.iter().collect(),
                Some(v) => vec![v],
                None => Vec::new(),
            };
            for v in values {
                if !v.is_object() {
                    continue;
                }
                let Some(url) = str_of(v.get("url")).or_else(|| str_of(v.get("file_url"))) else { continue };
                if !is_http_url(&url) {
                    continue;
                }
                let name = str_of(v.get("name")).or_else(|| str_of(v.get("file_name")));
                out.attachments.push(MeegleAttachment { name, url });
            }
        }
        let Some(meta) = meta.filter(|m| is_context_field(m)) else { continue };
        if let Some(value) = context_value(value, meta).filter(|v| !v.is_empty()) {
            out.context_fields.push(MeegleContextField { name: meta.name.clone(), value });
        }
    }
    out
}

/// [`normalize_comments`] 的结果
#[derive(Debug, Clone, PartialEq)]
pub struct NormalizedComments {
    pub comments: Vec<MeegleComment>,
    /// `Math.max(1, Number(pagination.total_pages) || 1)`，照 TS 保留成 JS 的 number
    /// （可能是小数或 Infinity，调用方再与上限取小）
    pub total_pages: f64,
}

pub fn normalize_comments(data: &Value) -> NormalizedComments {
    let rows = data.get("comments").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    let mut comments = Vec::new();
    for row in rows {
        if !row.is_object() {
            continue;
        }
        let content = str_of(row.get("content")).map(|c| js::trim(&c).to_string()).unwrap_or_default();
        // Array.isArray(row.file_url) ? row.file_url : [row.file_url]
        let raw_files: Vec<Option<&Value>> = match row.get("file_url") {
            Some(Value::Array(items)) => items.iter().map(Some).collect(),
            other => vec![other],
        };
        let attachments: Vec<String> =
            raw_files.into_iter().filter_map(str_of).filter(|url| is_http_url(url)).collect();
        if content.is_empty() && attachments.is_empty() {
            continue;
        }
        comments.push(MeegleComment {
            content,
            created_at: str_of(row.get("created_at")),
            attachments: (!attachments.is_empty()).then_some(attachments),
        });
    }
    let n = js::to_number(data.get("pagination").and_then(|p| p.get("total_pages")));
    // Number(x) || 1：0、-0、NaN 都是假
    let n = if n == 0.0 || n.is_nan() { 1.0 } else { n };
    NormalizedComments { comments, total_pages: n.max(1.0) }
}

/// LIMIT doesn't change MQL's fixed 50-row transport page; use its opaque session to fetch page 2.
pub fn mql_next_args(space_key: &str, data: &Value) -> Option<Vec<String>> {
    let session = str_of(data.get("session_id"))?;
    Some(vec![
        "workitem".into(),
        "query".into(),
        format!("--project-key={space_key}"),
        format!("--session-id={session}"),
        r#"--group-pagination-list=[{"group_id":"1","page_num":2}]"#.into(),
        "--format".into(),
        "json".into(),
    ])
}

/// CLI 的两条输出流 → 结果或错误。
///
/// 成功是 stdout 上任意 JSON（对象 / 数组 / 甚至 `null`——查不到空间时就回 null）；
/// 失败是 stderr 上的 `{data:null, error:{code,message}}`（stdout 为空）；打错命令 /
/// 未登录是 stdout 上一行纯文本 `unknown command "x" for "meegle"`。先看 stdout，
/// 空了再看 stderr；两边都不是 JSON 才按文本报。
///
/// JSON.parse 与 serde_json 的差异只在边角：serde_json 有 128 层的嵌套上限、不收
/// `\ud800` 这类孤立代理的转义——那种输出会落到 BAD_OUTPUT。
pub fn parse_cli_json(stdout: &str, stderr: &str) -> CliOutcome {
    let out = js::trim(stdout);
    let text = if out.is_empty() { js::trim(stderr) } else { out };
    if !text.is_empty() {
        // 不是 JSON 就落到下面按文本处理
        if let Ok(data) = serde_json::from_str::<Value>(text) {
            if let Some(err) = data.get("error").filter(|e| e.is_object()) {
                return Err(CliFailure {
                    code: str_of(err.get("code")).unwrap_or_else(|| "CLI_ERROR".into()),
                    message: cli_error_text(&str_of(err.get("message")).unwrap_or_default()),
                });
            }
            // stderr 上的 JSON 只可能是错误信封；不是就当异常输出报出去
            if out.is_empty() {
                return Err(CliFailure::new("BAD_OUTPUT", first_line(text)));
            }
            return Ok(data);
        }
    }
    let line = first_line(text);
    // /^unknown command/i：纯 ASCII 前缀，按 ASCII 忽略大小写比
    if line.get(..15).is_some_and(|p| p.eq_ignore_ascii_case("unknown command")) {
        return Err(CliFailure::new("UNKNOWN_COMMAND", line));
    }
    if line.is_empty() {
        return Err(CliFailure::new("BAD_OUTPUT", "meegle 没有输出"));
    }
    Err(CliFailure::new("BAD_OUTPUT", line))
}

/// `text.trim().split(/\r?\n/)[0]?.trim() ?? ""`
pub fn first_line(text: &str) -> String {
    let first = js::trim(text).split('\n').next().unwrap_or("");
    js::trim(first.strip_suffix('\r').unwrap_or(first)).to_string()
}

/// 服务端错误长这样：`error=ErrViewNotExist,message=view not exist,retriable=false\nlogid: …`，
/// 业务错误再套一层 `message=Service Internal Error,biz error: xxx,retriable=true`。
/// 给用户看的是最里层那句；抽不出来就原样给第一行。
pub fn cli_error_text(raw: &str) -> String {
    static BIZ: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(&format!(r"biz error:{ws}*({dot}+?)(?:,retriable=|$)", ws = js::WS, dot = js::DOT)).unwrap()
    });
    static MSG: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(&format!(r"message=({dot}+?)(?:,retriable=|$)", dot = js::DOT)).unwrap());
    let line = first_line(raw);
    if let Some(m) = BIZ.captures(&line) {
        return js::trim(&m[1]).to_string();
    }
    if let Some(m) = MSG.captures(&line) {
        return js::trim(&m[1]).to_string();
    }
    line
}

/// [`parse_login_prompt`] 的结果：TS 的 `Omit<MeegleLogin, "host">`，S6 补上 host 即是
/// [`falcon_proto::MeegleLogin`]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginPrompt {
    pub url: String,
    pub code: String,
}

/// `auth login --device-code` 的提示：抓 URL 与授权码，二维码那堆块字符不要
pub fn parse_login_prompt(text: &str) -> Option<LoginPrompt> {
    static URL: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(&format!(r"URL:{ws}*(https?://{nws}+)", ws = js::WS, nws = js::NON_WS)).unwrap());
    static CODE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(&format!(r"Authorization code:{ws}*({nws}+)", ws = js::WS, nws = js::NON_WS)).unwrap()
    });
    let url = URL.captures(text)?[1].to_string();
    let code = CODE.captures(text).map(|m| m[1].to_string()).unwrap_or_default();
    Some(LoginPrompt { url, code })
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedStatus {
    pub authenticated: bool,
    pub host: Option<String>,
    /// CLI 原样报的数，不保证是整数（与 [`falcon_proto::MeegleStatus::expires_in_minutes`] 同）
    pub expires_in_minutes: Option<f64>,
}

pub fn parse_status(data: &Value) -> ParsedStatus {
    if !data.is_object() {
        return ParsedStatus { authenticated: false, host: None, expires_in_minutes: None };
    }
    ParsedStatus {
        authenticated: matches!(data.get("authenticated"), Some(Value::Bool(true))),
        host: str_of(data.get("host")),
        expires_in_minutes: data.get("expires_in_minutes").and_then(Value::as_f64),
    }
}

// ---- 归一化 ----

pub fn normalize_user(data: &Value) -> Option<MeegleUser> {
    // Array.isArray(data) ? data[0] : data
    let row = match data {
        Value::Array(items) => items.first()?,
        other => other,
    };
    if !row.is_object() {
        return None;
    }
    let key = str_of(row.get("user_key")).or_else(|| str_of(row.get("username")))?;
    Some(MeegleUser {
        name: str_of(row.get("name_cn")).or_else(|| str_of(row.get("name_en"))).unwrap_or_else(|| key.clone()),
        email: str_of(row.get("email")),
        avatar_url: str_of(row.get("avatar_url")),
        key,
    })
}

/// [`normalize_spaces`] 的结果
#[derive(Debug, Clone, PartialEq)]
pub struct NormalizedSpaces {
    pub spaces: Vec<MeegleSpace>,
    pub has_more: bool,
}

pub fn normalize_spaces(data: &Value) -> NormalizedSpaces {
    if !data.is_object() {
        return NormalizedSpaces { spaces: Vec::new(), has_more: false };
    }
    let list = data.get("projects").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    let mut spaces = Vec::new();
    for row in list {
        if !row.is_object() {
            continue;
        }
        let Some(key) = str_of(row.get("project_key")) else { continue };
        spaces.push(MeegleSpace {
            name: str_of(row.get("name")).unwrap_or_else(|| key.clone()),
            simple_name: str_of(row.get("simple_name")).unwrap_or_default(),
            key,
        });
    }
    NormalizedSpaces { spaces, has_more: has_more_of(data.get("pagination")) }
}

/// is_disable：实测 2 = 启用、1 = 停用（与字段名的字面意思相反，别猜）
pub fn normalize_types(data: &Value) -> Vec<MeegleWorkItemType> {
    let Some(list) = data.get("list").and_then(Value::as_array) else { return Vec::new() };
    let mut out = Vec::new();
    for row in list {
        if !row.is_object() {
            continue;
        }
        let Some(key) = str_of(row.get("type_key")) else { continue };
        out.push(MeegleWorkItemType {
            name: str_of(row.get("name")).unwrap_or_else(|| key.clone()),
            api_name: str_of(row.get("api_name")).unwrap_or_else(|| key.clone()),
            // row.is_disable === 1（JSON 的 1 与 1.0 在 JS 里是同一个数）
            disabled: row.get("is_disable").and_then(Value::as_f64) == Some(1.0),
            key,
        });
    }
    out
}

/// TS 的第二个参数是 `{ key, name }`（调用方传的是 MeegleWorkItemType），这里拆成两个
pub fn normalize_views(data: &Value, type_key: &str, type_name: &str) -> Vec<MeegleView> {
    let Some(list) = data.as_array() else { return Vec::new() };
    let mut out = Vec::new();
    for row in list {
        if !row.is_object() {
            continue;
        }
        let Some(id) = str_of(row.get("view_id")) else { continue };
        out.push(MeegleView {
            name: str_of(row.get("view_name")).unwrap_or_else(|| id.clone()),
            id,
            type_key: type_key.to_string(),
            type_name: type_name.to_string(),
        });
    }
    out
}

/// MQL 结果的一行。只在服务端内部用，不上线
#[derive(Debug, Clone, PartialEq)]
pub struct MqlRow {
    pub id: String,
    pub name: String,
    pub business: Option<String>,
    /// 仅内部使用：只有 MQL 未给 label 时才需要按空间×类型读一次元数据。
    pub business_value: Option<Value>,
    pub status: Option<String>,
    pub updated_at: Option<String>,
}

/// MQL 的值是带类型标签的信封：`{value_type:"string_value", value:{string_value:"…"}}`。
/// 这里把常见几种压成一段文本；没见过的类型返回 None，别硬 String() 出 [object Object]。
pub fn mql_value_text(cell: Option<&Value>) -> Option<String> {
    let v = cell?.get("value").filter(|v| v.is_object())?;
    if let Some(Value::String(s)) = v.get("string_value") {
        return Some(s.clone()).filter(|s| !s.is_empty());
    }
    if let Some(Value::Number(n)) = v.get("long_value") {
        return n.as_f64().map(js::number_to_string);
    }
    if let Some(Value::Number(n)) = v.get("double_value") {
        return n.as_f64().map(js::number_to_string);
    }
    if let Some(Value::Bool(b)) = v.get("bool_value") {
        return Some(if *b { "true" } else { "false" }.to_string());
    }
    if let Some(klv) = v.get("key_label_value").filter(|x| x.is_object()) {
        return str_of(klv.get("label"));
    }
    if let Some(Value::Array(list)) = v.get("key_label_value_list") {
        let labels: Vec<String> = list.iter().filter_map(|x| str_of(x.get("label"))).collect();
        return (!labels.is_empty()).then(|| labels.join("、"));
    }
    if let Some(user) = v.get("user_value").filter(|x| x.is_object()) {
        return str_of(user.get("name_cn")).or_else(|| str_of(user.get("name_en")));
    }
    if let Some(Value::Array(list)) = v.get("user_value_list") {
        let names: Vec<String> =
            list.iter().filter_map(|x| str_of(x.get("name_cn")).or_else(|| str_of(x.get("name_en")))).collect();
        return (!names.is_empty()).then(|| names.join("、"));
    }
    None
}

/// `Object.values(obj)` 的顺序：数组下标形的键（规范的十进制、小于 2^32 - 1）按数值升序
/// 在前，其余键按插入序在后。serde_json 的 Map 给不出插入序（见文件头），后一段退回 Map
/// 自身的顺序——CLI 实测只有一个分组 "1"。
fn js_object_values(map: &Map<String, Value>) -> Vec<&Value> {
    fn array_index(k: &str) -> Option<u32> {
        let canonical = k == "0" || (!k.starts_with('0') && !k.is_empty() && k.bytes().all(|b| b.is_ascii_digit()));
        k.parse::<u32>().ok().filter(|n| canonical && *n < u32::MAX)
    }
    let mut indexed: Vec<(u32, &Value)> = Vec::new();
    let mut rest: Vec<&Value> = Vec::new();
    for (k, v) in map {
        match array_index(k) {
            Some(i) => indexed.push((i, v)),
            None => rest.push(v),
        }
    }
    indexed.sort_by_key(|(i, _)| *i);
    indexed.into_iter().map(|(_, v)| v).chain(rest).collect()
}

/// `workitem query` 的 data 是 分组 id → 行数组；每行是 moql_field_list
pub fn normalize_mql_rows(data: &Value) -> Vec<MqlRow> {
    let Some(groups) = data.get("data").and_then(Value::as_object) else { return Vec::new() };
    let mut rows = Vec::new();
    for group in js_object_values(groups) {
        let Some(group) = group.as_array() else { continue };
        for row in group {
            let Some(Value::Array(fields)) = row.get("moql_field_list") else { continue };
            // 同名 key 后者覆盖前者（Map.set）
            let mut cells: HashMap<&str, &Value> = HashMap::new();
            for cell in fields {
                if let Some(Value::String(key)) = cell.get("key") {
                    cells.insert(key.as_str(), cell);
                }
            }
            let cell = |k: &str| cells.get(k).copied();
            let Some(id) = mql_value_text(cell("work_item_id")) else { continue };
            let business_value = cell("business");
            let business = business_text(business_value, None);
            rows.push(MqlRow {
                id,
                name: mql_value_text(cell("name")).unwrap_or_default(),
                status: mql_value_text(cell("work_item_status")),
                updated_at: mql_value_text(cell("updated_at")),
                // business ? {business} : businessValue ? {businessValue} : {}
                business_value: if business.is_some() { None } else { business_value.cloned() },
                business,
            });
        }
    }
    rows
}

pub fn work_item_url(host: Option<&str>, simple_name: Option<&str>, type_key: &str, id: &str) -> Option<String> {
    // !host || !simpleName：空串也算没有
    let host = host.filter(|h| !h.is_empty())?;
    let simple_name = simple_name.filter(|s| !s.is_empty())?;
    Some(format!("https://{host}/{simple_name}/{type_key}/detail/{id}"))
}

fn person_name(v: Option<&Value>) -> Option<String> {
    let v = v.filter(|v| v.is_object())?;
    str_of(v.get("name")).or_else(|| str_of(v.get("name_cn"))).or_else(|| str_of(v.get("name_en")))
}

fn person_names(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(items)) => items.iter().filter_map(|x| person_name(Some(x))).collect(),
        _ => Vec::new(),
    }
}

/// `work_item_attribute`（workitem get / view get 共用）→ 列表行
pub fn normalize_attribute(attr: &Value, host: Option<&str>) -> Option<MeegleWorkItem> {
    if !attr.is_object() {
        return None;
    }
    let id = str_of(attr.get("work_item_id"))?;
    // isObj(x) ? x : {}：Value::get 在非对象上给 None，效果相同
    let project = attr.get("owned_project");
    let typ = attr.get("work_item_type");
    let type_key = str_of(typ.and_then(|t| t.get("key"))).unwrap_or_default();
    Some(MeegleWorkItem {
        name: str_of(attr.get("work_item_name")).unwrap_or_default(),
        business: None,
        space_key: str_of(project.and_then(|p| p.get("key"))).unwrap_or_default(),
        space_name: str_of(project.and_then(|p| p.get("name"))),
        type_name: str_of(typ.and_then(|t| t.get("name"))),
        status: str_of(attr.get("work_item_status").and_then(|s| s.get("name"))),
        url: work_item_url(host, str_of(project.and_then(|p| p.get("simple_name"))).as_deref(), &type_key, &id),
        updated_at: str_of(attr.get("update_time")),
        type_key,
        id,
    })
}

pub fn normalize_view_items(data: &Value, host: Option<&str>, page: u32) -> MeeglePage<MeegleWorkItem> {
    if !data.is_object() {
        return MeeglePage { items: Vec::new(), page, has_more: false, total: None };
    }
    let list = data.get("work_item_list").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    let items = list
        .iter()
        .filter_map(|row| normalize_attribute(row.get("work_item_attribute").unwrap_or(&Value::Null), host))
        .collect();
    let pagination = data.get("pagination");
    MeeglePage {
        items,
        page,
        has_more: has_more_of(pagination),
        total: total_of(pagination.and_then(|p| p.get("total"))),
    }
}

/// mywork 的一行：名字是空的，得再补；这里先把它自己有的东西取干净
pub fn normalize_todo(data: &Value, page: u32) -> MeeglePage<MeegleTodoItem> {
    if !data.is_object() {
        return MeeglePage { items: Vec::new(), page, has_more: false, total: None };
    }
    let list = data.get("list").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    let mut items = Vec::new();
    for row in list {
        if !row.is_object() {
            continue;
        }
        let info = row.get("work_item_info");
        let Some(id) = str_of(info.and_then(|i| i.get("work_item_id"))) else { continue };
        let node = row.get("node_info");
        let state = row.get("state_info");
        let schedule = row.get("schedule");
        let finish = row.get("finish_time");
        items.push(MeegleTodoItem {
            item: MeegleWorkItem {
                id,
                name: str_of(info.and_then(|i| i.get("work_item_name"))).unwrap_or_default(),
                business: None,
                space_key: str_of(row.get("project_key")).unwrap_or_default(),
                space_name: str_of(row.get("project_name")),
                type_key: str_of(info.and_then(|i| i.get("work_item_type_key"))).unwrap_or_default(),
                type_name: None,
                status: None,
                url: None,
                updated_at: None,
            },
            node_name: str_of(node.and_then(|n| n.get("node_name"))),
            state_name: str_of(state.and_then(|s| s.get("start_state_key_name"))),
            schedule_start: str_of(schedule.and_then(|s| s.get("start_time"))),
            schedule_end: str_of(schedule.and_then(|s| s.get("end_time"))),
            finished_at: str_of(finish.and_then(|f| f.get("finish_time"))),
        });
    }
    let raw_total = data.get("total").and_then(Value::as_f64);
    // 没有 has_more 字段：满一页且 total 说后面还有，才算有下一页
    let has_more = list.len() >= CLI_PAGE_SIZE as usize
        && raw_total.is_none_or(|t| f64::from(page) * f64::from(CLI_PAGE_SIZE) < t);
    MeeglePage { items, page, has_more, total: total_of(data.get("total")) }
}

/// `view list-multi-project-workitems`：每行只有名字 / 空间 / id / 类型 key，状态要另补
pub fn normalize_multi_view_items(data: &Value, page: u32) -> MeeglePage<MeegleWorkItem> {
    if !data.is_object() {
        return MeeglePage { items: Vec::new(), page, has_more: false, total: None };
    }
    let list = data.get("data").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    let mut items = Vec::new();
    for row in list {
        if !row.is_object() {
            continue;
        }
        let Some(id) = str_of(row.get("work_item_id")) else { continue };
        items.push(MeegleWorkItem {
            id,
            name: str_of(row.get("name")).unwrap_or_default(),
            business: None,
            space_key: str_of(row.get("project_key")).unwrap_or_default(),
            space_name: None,
            type_key: str_of(row.get("work_item_type_key")).unwrap_or_default(),
            type_name: None,
            status: None,
            url: None,
            updated_at: None,
        });
    }
    let pagination = data.get("pagination");
    MeeglePage {
        items,
        page,
        has_more: has_more_of(pagination),
        total: total_of(pagination.and_then(|p| p.get("total"))),
    }
}

/// `url decode` 认出来的三类目标（TS 的 `ParsedUrl`）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedUrl {
    WorkItem { host: String, simple_name: String, type_key: String, id: String },
    View { host: String, simple_name: String, view_id: String, type_key: Option<String> },
    MultiProjectView { host: String, simple_name: String, view_id: String },
}

/// 图表 / 甘特 / 空间总览也带 view_id，但 `view get` 取不出工作项，不当视图开。
/// 面板能开的只有三类：工作项详情、按类型的视图（storyView / issueView / workObjectView）、全景视图。
const VIEW_KINDS_NOT_OPENABLE: [&str; 3] = ["view_chart", "view_user_gantt", "view_project_overview"];

/// `url decode` 的输出 → 面板认识的三类目标；认不出或不支持就给一句能看懂的原因
/// （TS 的 `{ ok: false; message }` 是 `Err(message)`）
pub fn parse_url_target(decoded: &Value) -> Result<ParsedUrl, String> {
    if !decoded.is_object() {
        return Err("链接解析失败".into());
    }
    let kind = str_of(decoded.get("url_kind")).unwrap_or_else(|| "unknown".into());
    let host = str_of(decoded.get("host")).unwrap_or_default();
    let simple_name = str_of(decoded.get("simple_name"));
    let view_id = str_of(decoded.get("view_id"));
    let id = str_of(decoded.get("work_item_id"));
    let type_key = str_of(decoded.get("work_item_type"));
    if kind == "workitem_detail"
        && let (Some(simple_name), Some(id), Some(type_key)) = (&simple_name, &id, &type_key)
    {
        return Ok(ParsedUrl::WorkItem {
            host,
            simple_name: simple_name.clone(),
            type_key: type_key.clone(),
            id: id.clone(),
        });
    }
    if kind == "view_multi_project"
        && let (Some(simple_name), Some(view_id)) = (&simple_name, &view_id)
    {
        return Ok(ParsedUrl::MultiProjectView { host, simple_name: simple_name.clone(), view_id: view_id.clone() });
    }
    if kind.starts_with("view_")
        && !VIEW_KINDS_NOT_OPENABLE.contains(&kind.as_str())
        && let (Some(simple_name), Some(view_id)) = (simple_name, view_id)
    {
        return Ok(ParsedUrl::View { host, simple_name, view_id, type_key });
    }
    if kind == "unknown" {
        return Err("认不出这个链接指向什么".into());
    }
    Err(format!("这类页面打不开（{kind}）"))
}

/// [`group_for_lookup`] 的一组
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LookupGroup {
    pub space_key: String,
    pub type_key: String,
    pub ids: Vec<String>,
}

/// 按 空间 × 类型 分组，MQL 一次只能查一张表。
///
/// TS 收 `{ spaceKey, typeKey, id }[]`（调用方传 MeegleWorkItem）；这里收
/// `(space_key, type_key, id)` 三元组。组按首次出现的顺序，组内 id 去重保序。
pub fn group_for_lookup<'a>(items: impl IntoIterator<Item = (&'a str, &'a str, &'a str)>) -> Vec<LookupGroup> {
    let mut groups: Vec<LookupGroup> = Vec::new();
    let mut index: HashMap<(&str, &str), usize> = HashMap::new();
    for (space_key, type_key, id) in items {
        if !is_valid_key(space_key) || !is_valid_key(type_key) || !is_valid_id(id) {
            continue;
        }
        let i = *index.entry((space_key, type_key)).or_insert_with(|| {
            groups.push(LookupGroup { space_key: space_key.into(), type_key: type_key.into(), ids: Vec::new() });
            groups.len() - 1
        });
        let g = &mut groups[i];
        if !g.ids.iter().any(|x| x == id) {
            g.ids.push(id.to_string());
        }
    }
    groups
}

/// 描述是飞书富文本编辑器导出的 Markdown 方言：图片是 `![](url)` 后面紧跟一段
/// `<!-- image:{…} -->` 注释，面板窄、又不渲染 Markdown，原样显示就是一坨 URL。
/// 图片换成占位、注释剥掉、多余空行压一压；其余标记（加粗、链接）留着，看得懂。
pub fn plain_description(md: &str) -> String {
    static IMAGE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"!\[[^\]]*\]\([^)]*\)").unwrap());
    // /[ \t]+$/gm：多行模式的 $ 停在四种行终止符前（Rust 的 (?m)$ 只认 \n），
    // 所以把终止符一起吃进来再原样写回
    static TRAILING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[ \t]+([\n\r\x{2028}\x{2029}]|$)").unwrap());
    static BLANKS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n{3,}").unwrap());
    let s = strip_html_comments(md);
    let s = IMAGE.replace_all(&s, "[图片]");
    let s = TRAILING.replace_all(&s, "$1");
    let s = BLANKS.replace_all(&s, "\n\n");
    js::trim(&s).to_string()
}

pub fn normalize_detail(data: &Value, host: Option<&str>) -> Option<MeegleWorkItemDetail> {
    if !data.is_object() {
        return None;
    }
    let attr = data.get("work_item_attribute").filter(|a| a.is_object())?;
    let base = normalize_attribute(attr, host)?;

    // 同名 key 后者覆盖前者（Map.set）
    let mut fields: HashMap<&str, &Value> = HashMap::new();
    if let Some(Value::Array(list)) = data.get("work_item_fields") {
        for f in list {
            if let (Some(Value::String(key)), true) = (f.get("key"), f.is_object()) {
                // f.value 缺省是 undefined；这里没有值就不放，get 时同样拿到 None
                if let Some(v) = f.get("value") {
                    fields.insert(key.as_str(), v);
                } else {
                    fields.remove(key.as_str());
                }
            }
        }
    }
    let field = |k: &str| fields.get(k).copied();
    let priority = field("priority");
    let description = match field("description") {
        Some(Value::String(d)) => Some(d.as_str()),
        _ => None,
    };

    let mut current_nodes = Vec::new();
    if let Some(Value::Array(list)) = data.get("work_item_current_node") {
        for n in list {
            if !n.is_object() {
                continue;
            }
            if let Some(name) = str_of(n.get("name")) {
                current_nodes.push(MeegleCurrentNode { name, owners: person_names(n.get("owners")) });
            }
        }
    }

    let mut roles = Vec::new();
    if let Some(Value::Array(list)) = attr.get("role_members") {
        for r in list {
            if !r.is_object() {
                continue;
            }
            let members = person_names(r.get("members"));
            if let Some(name) = str_of(r.get("name"))
                && !members.is_empty()
            {
                roles.push(MeegleRole { name, members });
            }
        }
    }

    let project = attr.get("owned_project");
    Some(MeegleWorkItemDetail {
        item: MeegleWorkItem { business: business_text(field("business"), None), ..base },
        simple_name: str_of(project.and_then(|p| p.get("simple_name"))),
        mode: str_of(attr.get("work_item_mod")),
        template: str_of(attr.get("template").and_then(|t| t.get("name"))),
        priority: match priority {
            Some(p) if p.is_object() => str_of(p.get("label")),
            p => str_of(p),
        },
        description: description.map(plain_description).filter(|d| !d.is_empty()),
        description_markdown: description.map(strip_html_comments).filter(|d| !d.is_empty()),
        attachments: None,
        attachments_unavailable: None,
        comments: None,
        comments_unavailable: None,
        context_fields: None,
        context_fields_unavailable: None,
        created_at: str_of(attr.get("create_time")),
        created_by: person_name(attr.get("create_by")),
        updated_by: person_name(attr.get("updated_by")),
        current_nodes,
        operators: person_names(field("current_status_operator")),
        roles,
    })
}

/// 照抄 TS 语义要用到的 JavaScript 行为。falcon-core 有一份同类的 `js` 模块，但那是它的
/// 私有模块；falcon-server 还没有公共的落脚点（lib.rs 不归本轮改），先各自带一份。
/// `trim` 开到 crate 内：client.rs 的关键字也照 JS 的 `trim()` 处理。
pub(crate) mod js {
    use serde_json::Value;

    /// ECMAScript 的 WhiteSpace + LineTerminator：`trim()` 与正则 `\s` 用的就是这一套
    /// （Rust 的 `char::is_whitespace` 不含 U+FEFF、多一个 U+0085）。
    pub(super) fn is_ws(c: char) -> bool {
        matches!(
            c,
            '\u{9}'..='\u{d}'
                | ' '
                | '\u{a0}'
                | '\u{1680}'
                | '\u{2000}'..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
        )
    }

    /// `String.prototype.trim`
    pub(crate) fn trim(s: &str) -> &str {
        s.trim_matches(is_ws)
    }

    /// 正则里的 `\s`
    pub(super) const WS: &str =
        r"[\t\n\x0B\x0C\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}]";
    /// 正则里的 `\S`
    pub(super) const NON_WS: &str =
        r"[^\t\n\x0B\x0C\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}]";
    /// 正则里的 `.`（不带 s 标志）：不配四种行终止符
    pub(super) const DOT: &str = r"[^\n\r\x{2028}\x{2029}]";

    /// 不带 `u` 标志的 `/…/i` 的源码 → Rust 正则：ASCII 字母展开成 `[xX]`，`.` 换成 [`DOT`]，
    /// `\` 转义原样保留。不支持字符类与 `\s` `\d` 这类转义（它们在 Rust 里语义不同），
    /// 需要时由调用方拼 [`WS`] 等常量。
    pub(super) fn ci(pattern: &str) -> String {
        let mut out = String::with_capacity(pattern.len() * 4);
        let mut chars = pattern.chars();
        while let Some(c) = chars.next() {
            match c {
                '\\' => {
                    out.push(c);
                    out.extend(chars.next());
                }
                '.' => out.push_str(DOT),
                c if c.is_ascii_alphabetic() => {
                    out.push('[');
                    out.push(c.to_ascii_lowercase());
                    out.push(c.to_ascii_uppercase());
                    out.push(']');
                }
                c => {
                    debug_assert!(c != '[', "ci() 不支持字符类");
                    out.push(c);
                }
            }
        }
        out
    }

    /// `s.length`：UTF-16 码元数
    pub(super) fn utf16_len(s: &str) -> usize {
        s.chars().map(char::len_utf16).sum()
    }

    /// `String(n)`（Number::toString）：整数不带 `.0`，|n| ≥ 1e21 或 < 1e-6 走指数表示
    /// （`1e+21`、`1.5e-7`）。Rust 的 `{}` / `{:e}` 与 JS 一样给最短的可回读位数。
    pub(super) fn number_to_string(v: f64) -> String {
        if v.is_nan() {
            return "NaN".into();
        }
        if v.is_infinite() {
            return if v > 0.0 { "Infinity" } else { "-Infinity" }.into();
        }
        if v == 0.0 {
            // -0 也是 "0"
            return "0".into();
        }
        if (1e-6..1e21).contains(&v.abs()) {
            return format!("{v}");
        }
        let s = format!("{v:e}");
        match s.split_once('e') {
            Some((m, e)) if !e.starts_with('-') => format!("{m}e+{e}"),
            _ => s,
        }
    }

    /// `Number(v)`：JSON 值（`None` = undefined）转数字。
    ///
    /// 数组先 `join(",")` 成字符串再转（`[] → 0`、`[5] → 5`、`[1,2] → NaN`），对象是 NaN。
    pub(super) fn to_number(v: Option<&Value>) -> f64 {
        match v {
            None => f64::NAN,
            Some(Value::Null) => 0.0,
            Some(Value::Bool(b)) => f64::from(u8::from(*b)),
            Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
            Some(Value::String(s)) => string_to_number(s),
            Some(Value::Array(items)) => string_to_number(&array_to_string(items)),
            Some(Value::Object(_)) => f64::NAN,
        }
    }

    /// `Array.prototype.toString`（= `join(",")`，null 元素写成空串）
    fn array_to_string(items: &[Value]) -> String {
        items
            .iter()
            .map(|v| match v {
                Value::Null => String::new(),
                Value::String(s) => s.clone(),
                Value::Bool(b) => b.to_string(),
                Value::Number(n) => n.as_f64().map(number_to_string).unwrap_or_default(),
                Value::Array(inner) => array_to_string(inner),
                Value::Object(_) => "[object Object]".to_string(),
            })
            .collect::<Vec<_>>()
            .join(",")
    }

    /// `Number(string)`（ECMAScript StringToNumber）：首尾空白、空串为 0、十六进制 / 八进制 /
    /// 二进制前缀、`Infinity`；`str::parse::<f64>` 这些一样都不认，反而认 `inf` / `nan`。
    fn string_to_number(s: &str) -> f64 {
        let t = trim(s);
        if t.is_empty() {
            return 0.0;
        }
        let bytes = t.as_bytes();
        if bytes.len() > 2 && bytes[0] == b'0' {
            let radix = match bytes[1] {
                b'x' | b'X' => 16,
                b'o' | b'O' => 8,
                b'b' | b'B' => 2,
                _ => 0,
            };
            if radix != 0 {
                let mut v = 0.0f64;
                for c in t[2..].chars() {
                    match c.to_digit(radix) {
                        Some(d) => v = v * f64::from(radix) + f64::from(d),
                        None => return f64::NAN,
                    }
                }
                return v;
            }
        }
        let (neg, body) = match bytes[0] {
            b'-' => (true, &t[1..]),
            b'+' => (false, &t[1..]),
            _ => (false, t),
        };
        let v = if body == "Infinity" {
            f64::INFINITY
        } else if is_decimal_literal(body) {
            body.parse::<f64>().unwrap_or(f64::NAN)
        } else {
            return f64::NAN;
        };
        if neg { -v } else { v }
    }

    /// StrUnsignedDecimalLiteral：`digits [. digits] [e[+-]digits]`，小数点两边至少一边有数字
    fn is_decimal_literal(s: &str) -> bool {
        let b = s.as_bytes();
        let int_digits = b.iter().take_while(|c| c.is_ascii_digit()).count();
        let mut i = int_digits;
        let mut frac_digits = 0;
        if i < b.len() && b[i] == b'.' {
            i += 1;
            frac_digits = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
            i += frac_digits;
        }
        if int_digits == 0 && frac_digits == 0 {
            return false;
        }
        if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
            i += 1;
            if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
                i += 1;
            }
            let exp_digits = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
            if exp_digits == 0 {
                return false;
            }
            i += exp_digits;
        }
        i == b.len()
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use serde_json::json;

        // 以下是 Rust 侧补的：JS 语义对照
        #[test]
        fn number_to_string_matches_js() {
            assert_eq!(number_to_string(7112390164.0), "7112390164");
            assert_eq!(number_to_string(1.5), "1.5");
            assert_eq!(number_to_string(-0.0), "0");
            assert_eq!(number_to_string(1e21), "1e+21");
            assert_eq!(number_to_string(1.5e-7), "1.5e-7");
            assert_eq!(number_to_string(0.000001), "0.000001");
            // 超过 2^53 的整数：JS 给最短可回读位数，不是精确展开
            assert_eq!(number_to_string(12345678901234567890.0), "12345678901234567000");
        }

        #[test]
        fn to_number_matches_js() {
            assert_eq!(to_number(Some(&json!("3"))), 3.0);
            assert_eq!(to_number(Some(&json!(" 0x10 "))), 16.0);
            assert_eq!(to_number(Some(&json!(null))), 0.0);
            assert_eq!(to_number(Some(&json!([5]))), 5.0);
            assert!(to_number(Some(&json!("abc"))).is_nan());
            assert!(to_number(None).is_nan());
            assert!(to_number(Some(&json!({}))).is_nan());
        }

        #[test]
        fn ci_is_ascii_only() {
            let re = regex::Regex::new(&ci("stack.?trace")).unwrap();
            assert!(re.is_match("Stack Trace"));
            assert!(!re.is_match("stack\ntrace"));
            let re = regex::Regex::new(&ci("logs?")).unwrap();
            assert!(re.is_match("LOGS"));
            // (?i) 会让开尔文符号配上 k、长 s 配上 s；JS 不带 u 的 /i 不会
            assert!(!regex::Regex::new(&ci("k")).unwrap().is_match("\u{212A}"));
            assert!(!regex::Regex::new(&ci("s")).unwrap().is_match("\u{17F}"));
        }

        #[test]
        fn trim_uses_js_whitespace() {
            assert_eq!(trim("\u{feff} a \u{3000}"), "a");
            assert_eq!(trim("\u{85}a"), "\u{85}a");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_string()).collect()
    }

    // ======== command.test.ts ========

    /// comment list argv carries the project, item and page
    #[test]
    fn comment_list_argv_carries_the_project_item_and_page() {
        assert_eq!(
            comment_args("p1", "123", 2),
            s(&["comment", "list", "--project-key=p1", "--work-item-id=123", "--page-num=2", "--format", "json"])
        );
    }

    /// argv 用 --flag=value，用户输入以 - 开头也不会被当成 flag
    #[test]
    fn argv_uses_flag_equals_value() {
        assert_eq!(
            view_search_args("p1", "story", "-foo"),
            s(&["view", "search", "--project-key=p1", "--view-scope=story", "--key-word=-foo", "--format", "json"])
        );
        assert_eq!(spaces_args(None, None), s(&["project", "search", "--page-num=1", "--format", "json"]));
        assert_eq!(
            spaces_args(Some("FX"), Some(2)),
            s(&["project", "search", "--project-key=FX", "--page-num=2", "--format", "json"])
        );
        assert_eq!(login_args("meegle.com"), s(&["auth", "login", "--host=meegle.com", "--device-code"]));
        assert_eq!(query_args("p", "SELECT 1")[3], "--mql=SELECT 1");
    }

    /// 参数白名单
    #[test]
    fn argument_whitelist() {
        assert!(is_valid_host("project.feishu.cn"));
        assert!(is_valid_host("my-tenant.example.com"));
        // TS 里还有一个非字符串的 1：Rust 的签名只收 &str，由类型挡掉
        for bad in ["", "localhost", "a b.com", "-x.com", "x.com/", "http://x.com"] {
            assert!(!is_valid_host(bad), "{bad}");
        }
        assert!(is_valid_key("67d7ba04296cba3d3ece0694"));
        assert!(is_valid_key("26oZ-EaHR"));
        assert!(is_valid_key("j__7WfhHRN"));
        let long = "a".repeat(65);
        for bad in ["", "a b", "--x", "x`y", long.as_str(), "x/y"] {
            assert!(!is_valid_key(bad), "{bad}");
        }
    }

    /// MQL 字面量：单引号双写、控制字符变空格
    #[test]
    fn mql_literal_doubles_quotes_and_blanks_controls() {
        assert_eq!(mql_literal("it's"), "'it''s'");
        assert_eq!(mql_literal("a\nb\u{0}c"), "'a b c'");
        assert_eq!(
            mql_search("p1", "issue", "登录'", Some(20)),
            "SELECT `work_item_id`, `name`, `work_item_status`, `updated_at`, `business` FROM `p1`.`issue` \
             WHERE `name` LIKE '%登录''%' ORDER BY `work_item_id` DESC LIMIT 20"
        );
        assert!(mql_recent("p1", "story", Some(999)).ends_with("LIMIT 100"));
        // 非数字 id 直接丢，LIMIT 跟着有效数量走
        assert_eq!(
            mql_by_ids("p1", "issue", &["1", "x", "2"]),
            "SELECT `work_item_id`, `name`, `work_item_status`, `updated_at`, `business` FROM `p1`.`issue` \
             WHERE `work_item_id` IN (1, 2) LIMIT 2"
        );
    }

    /// parseCliJson：成功 / 信封错误 / unknown command / 纯文本
    #[test]
    fn parse_cli_json_outcomes() {
        assert_eq!(parse_cli_json(r#"{"a":1}"#, ""), Ok(json!({ "a": 1 })));
        assert_eq!(parse_cli_json("null", ""), Ok(Value::Null));
        assert_eq!(parse_cli_json("[]", ""), Ok(json!([])));
        // 错误信封在 stderr 上，stdout 是空的
        let err = parse_cli_json(
            "",
            &json!({
                "data": null,
                "error": {
                    "code": "SERVER_CALL_FAILED",
                    "message": "error=ErrViewNotExist,message=view not exist,retriable=false\nlogid: 20260910110143F01F",
                    "retryable": true,
                },
                "meta": {},
            })
            .to_string(),
        );
        assert_eq!(err, Err(CliFailure::new("SERVER_CALL_FAILED", "view not exist")));
        // stderr 上不是信封的 JSON 也不能当成功
        assert_eq!(parse_cli_json("", r#"{"a":1}"#), Err(CliFailure::new("BAD_OUTPUT", r#"{"a":1}"#)));
        assert_eq!(
            parse_cli_json("unknown command \"project\" for \"meegle\"\n", ""),
            Err(CliFailure::new("UNKNOWN_COMMAND", "unknown command \"project\" for \"meegle\""))
        );
        assert_eq!(parse_cli_json("", "boom\nmore"), Err(CliFailure::new("BAD_OUTPUT", "boom")));
        let empty = parse_cli_json("", "");
        assert!(matches!(&empty, Err(e) if e.message == "meegle 没有输出"));
    }

    /// cliErrorText 取最里层的业务错误
    #[test]
    fn cli_error_text_takes_innermost_business_error() {
        assert_eq!(
            cli_error_text(
                "error=ErrServiceInternalError,message=Service Internal Error,biz error: project_key, view_scope and key_word are required,retriable=true\nlogid: x"
            ),
            "project_key, view_scope and key_word are required"
        );
        assert_eq!(
            cli_error_text(
                "error=ErrMetadataError,message=metadata error,project access denied (Code: 3005) | Context: no permission,retriable=false"
            ),
            "metadata error,project access denied (Code: 3005) | Context: no permission"
        );
        assert_eq!(cli_error_text("plain text\nsecond"), "plain text");
    }

    /// parseLoginPrompt 从 device-code 输出里抓链接与授权码
    #[test]
    fn parse_login_prompt_grabs_url_and_code() {
        let text = "\n  Please scan the QR code with your phone, or open the following URL in a browser:\n\
                    \x20 URL: https://project.feishu.cn/b/auth/mcp?channel=meegle-cli&mode=device&usercode=6YJBH-TLS5W\n\
                    \x20 Authorization code: 6YJBH-TLS5W\n\n████\n";
        assert_eq!(
            parse_login_prompt(text),
            Some(LoginPrompt {
                url: "https://project.feishu.cn/b/auth/mcp?channel=meegle-cli&mode=device&usercode=6YJBH-TLS5W".into(),
                code: "6YJBH-TLS5W".into(),
            })
        );
        assert_eq!(parse_login_prompt("  Please scan the QR code"), None);
    }

    /// parseStatus 三种形态
    #[test]
    fn parse_status_three_shapes() {
        assert_eq!(
            parse_status(&json!({ "authenticated": true, "expires_in_minutes": 119, "host": "project.feishu.cn" })),
            ParsedStatus {
                authenticated: true,
                host: Some("project.feishu.cn".into()),
                expires_in_minutes: Some(119.0)
            }
        );
        assert_eq!(
            parse_status(&json!({ "authenticated": false, "host": null, "reason": "no local token" })),
            ParsedStatus { authenticated: false, host: None, expires_in_minutes: None }
        );
        assert_eq!(
            parse_status(&json!("garbage")),
            ParsedStatus { authenticated: false, host: None, expires_in_minutes: None }
        );
    }

    /// normalizeUser / normalizeSpaces / normalizeTypes / normalizeViews
    #[test]
    fn normalize_user_spaces_types_views() {
        assert_eq!(
            normalize_user(&json!([{
                "avatar_url": "https://x/a.png",
                "email": "fay@example.com",
                "name_cn": "fay-李丰豪",
                "name_en": "fay",
                "user_key": "7481570191529279507",
                "username": "7481570191529279507",
            }])),
            Some(MeegleUser {
                key: "7481570191529279507".into(),
                name: "fay-李丰豪".into(),
                email: Some("fay@example.com".into()),
                avatar_url: Some("https://x/a.png".into()),
            })
        );
        assert_eq!(normalize_user(&json!([])), None);

        assert_eq!(
            normalize_spaces(&json!({
                "pagination": { "has_more": false, "page_num": 1, "page_size": 50, "total": 1 },
                "projects": [{ "name": "FX", "project_key": "67e9", "simple_name": "fanruan-fx" }],
            })),
            NormalizedSpaces {
                spaces: vec![MeegleSpace { key: "67e9".into(), name: "FX".into(), simple_name: "fanruan-fx".into() }],
                has_more: false,
            }
        );
        // 查不到空间时 CLI 直接回 null
        assert_eq!(normalize_spaces(&Value::Null), NormalizedSpaces { spaces: vec![], has_more: false });

        assert_eq!(
            normalize_types(&json!({
                "list": [
                    { "api_name": "story", "is_disable": 2, "name": "需求", "type_key": "story" },
                    { "api_name": "sprint", "is_disable": 1, "name": "迭代", "type_key": "sprint" },
                ],
            })),
            vec![
                MeegleWorkItemType {
                    key: "story".into(),
                    name: "需求".into(),
                    api_name: "story".into(),
                    disabled: false
                },
                MeegleWorkItemType {
                    key: "sprint".into(),
                    name: "迭代".into(),
                    api_name: "sprint".into(),
                    disabled: true
                },
            ]
        );

        assert_eq!(
            normalize_views(&json!([{ "view_id": "26oZ-EaHR", "view_name": "全部" }]), "story", "需求"),
            vec![MeegleView {
                id: "26oZ-EaHR".into(),
                name: "全部".into(),
                type_key: "story".into(),
                type_name: "需求".into(),
            }]
        );
        assert_eq!(normalize_views(&json!({ "data": null }), "story", "需求"), vec![]);
    }

    /// MQL 信封值与行归一化
    #[test]
    fn mql_envelope_values_and_rows() {
        let t = |v: Value| mql_value_text(Some(&v));
        assert_eq!(t(json!({ "value_type": "string_value", "value": { "string_value": "x" } })).as_deref(), Some("x"));
        assert_eq!(
            t(json!({ "value_type": "long_value", "value": { "long_value": 7112390164u64 } })).as_deref(),
            Some("7112390164")
        );
        assert_eq!(
            t(json!({ "value_type": "key_label_value", "value": { "key_label_value": { "key": "issue", "label": "缺陷" } } }))
                .as_deref(),
            Some("缺陷")
        );
        assert_eq!(
            t(json!({
                "value_type": "key_label_value_list",
                "value": { "key_label_value_list": [{ "key": "a", "label": "组员开发" }, { "key": "b", "label": "验收" }] },
            }))
            .as_deref(),
            Some("组员开发、验收")
        );
        assert_eq!(
            t(json!({ "value_type": "user_value", "value": { "user_value": { "name_cn": "赵兴佩", "name_en": "zxp" } } }))
                .as_deref(),
            Some("赵兴佩")
        );
        assert_eq!(t(json!({ "value_type": "weird", "value": { "blob": {} } })), None);

        let rows = normalize_mql_rows(&json!({
            "data": {
                "1": [
                    {
                        "moql_field_list": [
                            { "key": "name", "value": { "string_value": "登录问题" }, "value_type": "string_value" },
                            {
                                "key": "work_item_status",
                                "value": { "key_label_value_list": [{ "key": "_v", "label": "组员开发" }] },
                                "value_type": "key_label_value_list",
                            },
                            { "key": "work_item_id", "value": { "long_value": 7112390164u64 }, "value_type": "long_value" },
                            { "key": "updated_at", "value": { "string_value": "2026-09-10" }, "value_type": "string_value" },
                        ],
                    },
                    { "moql_field_list": [{ "key": "name", "value": { "string_value": "没 id 的行" }, "value_type": "string_value" }] },
                ],
            },
            "list": null,
        }));
        assert_eq!(
            rows,
            vec![MqlRow {
                id: "7112390164".into(),
                name: "登录问题".into(),
                business: None,
                business_value: None,
                status: Some("组员开发".into()),
                updated_at: Some("2026-09-10".into()),
            }]
        );
        // 空结果：data 是 {}
        assert_eq!(normalize_mql_rows(&json!({ "data": {}, "list": null })), vec![]);
    }

    fn attr() -> Value {
        json!({
            "create_by": { "email": "roxy@example.com", "key": "7496", "name": "Roxy-杨子静" },
            "create_time": "2025-08-08T15:36:08+08:00",
            "owned_project": { "key": "67d7", "name": "一体化产研团队", "simple_name": "b2rl2h" },
            "role_members": [
                { "key": "role_fe0eb9", "name": "测试者" },
                { "key": "role_a367e7", "members": [{ "email": "fay@example.com", "key": "7481", "name": "fay-李丰豪" }], "name": "开发组长" },
            ],
            "template": { "id": 2771065, "name": "改良" },
            "update_time": "2026-09-10T07:49:09+08:00",
            "updated_by": { "email": "lipei@example.com", "key": "7526", "name": "Lipei-李培" },
            "work_item_id": "6453102417",
            "work_item_mod": "节点流",
            "work_item_name": "图表模糊问题",
            "work_item_status": { "key": "0CRPpMACM", "name": "开发组长" },
            "work_item_type": { "key": "67da6360e9d810fd8008b7a4", "name": "产研任务" },
        })
    }

    /// work_item_attribute → 列表行 / 详情
    #[test]
    fn work_item_attribute_to_row_and_detail() {
        assert_eq!(
            normalize_attribute(&attr(), Some("project.feishu.cn")),
            Some(MeegleWorkItem {
                id: "6453102417".into(),
                name: "图表模糊问题".into(),
                business: None,
                space_key: "67d7".into(),
                space_name: Some("一体化产研团队".into()),
                type_key: "67da6360e9d810fd8008b7a4".into(),
                type_name: Some("产研任务".into()),
                status: Some("开发组长".into()),
                url: Some("https://project.feishu.cn/b2rl2h/67da6360e9d810fd8008b7a4/detail/6453102417".into()),
                updated_at: Some("2026-09-10T07:49:09+08:00".into()),
            })
        );
        // 站点未知就没有链接，别拼出 https://null/…
        assert_eq!(normalize_attribute(&attr(), None).unwrap().url, None);
        assert_eq!(work_item_url(Some("h"), Some(""), "t", "1"), None);

        let detail = normalize_detail(
            &json!({
                "work_item_attribute": attr(),
                "work_item_current_node": [
                    { "actual_begin_time": "2025-11-06T10:05:38+08:00", "id": "state_0", "name": "开发组长", "owners": [{ "key": "7481", "name": "fay-李丰豪" }] },
                ],
                "work_item_fields": [
                    { "key": "business", "name": "业务线", "value": "67ece7e8" },
                    { "key": "current_status_operator", "name": "当前负责人", "value": [{ "key": "7481", "name": "fay-李丰豪" }] },
                    { "key": "description", "name": "描述", "value": "运营反馈有图表模糊<!-- x -->" },
                    { "key": "priority", "name": "优先级", "value": { "label": "一般紧急", "value": "option_2" } },
                ],
            }),
            Some("project.feishu.cn"),
        )
        .expect("detail");
        assert_eq!(detail.simple_name.as_deref(), Some("b2rl2h"));
        assert_eq!(detail.mode.as_deref(), Some("节点流"));
        assert_eq!(detail.template.as_deref(), Some("改良"));
        assert_eq!(detail.priority.as_deref(), Some("一般紧急"));
        assert_eq!(detail.description.as_deref(), Some("运营反馈有图表模糊"));
        assert_eq!(detail.created_by.as_deref(), Some("Roxy-杨子静"));
        assert_eq!(detail.updated_by.as_deref(), Some("Lipei-李培"));
        assert_eq!(
            detail.current_nodes,
            vec![MeegleCurrentNode { name: "开发组长".into(), owners: vec!["fay-李丰豪".into()] }]
        );
        assert_eq!(detail.operators, vec!["fay-李丰豪".to_string()]);
        // 没人的角色不列
        assert_eq!(detail.roles, vec![MeegleRole { name: "开发组长".into(), members: vec!["fay-李丰豪".into()] }]);
        assert_eq!(normalize_detail(&json!({ "work_item_attribute": null }), Some("h")), None);
    }

    /// plainDescription 把富文本 Markdown 里的图片与注释收掉
    #[test]
    fn plain_description_drops_images_and_comments() {
        let md = "![](https://project.feishu.cn/goapi/v1/tos/file/x.png?isSaas=1)<!--image:{\"width\":790,\"uuid\":\"79BD\"} -->\n\n\n\n这里不是删除字段，是将其移除字段栏  \n**加粗**保留";
        assert_eq!(plain_description(md), "[图片]\n\n这里不是删除字段，是将其移除字段栏\n**加粗**保留");
        assert_eq!(plain_description("  \n"), "");
    }

    /// view get 分页
    #[test]
    fn view_get_pagination() {
        let page = normalize_view_items(
            &json!({
                "pagination": { "has_more": true, "page_num": 1, "page_size": 50, "total": 3576 },
                "work_item_list": [{ "work_item_attribute": attr() }, { "work_item_attribute": null }],
            }),
            Some("project.feishu.cn"),
            1,
        );
        assert_eq!(page.items.len(), 1);
        assert!(page.has_more);
        assert_eq!(page.total, Some(3576));
        assert_eq!(
            normalize_view_items(&Value::Null, Some("h"), 3),
            MeeglePage { items: vec![], page: 3, has_more: false, total: None }
        );
    }

    /// mywork 行归一化与翻页判断
    #[test]
    fn mywork_rows_and_paging() {
        let row = |id: u64, extra: Value| {
            let mut base = json!({
                "node_info": { "node_name": "开发组长", "node_state_key": "node_state_0" },
                "project_key": "67d7",
                "project_name": "一体化产研团队",
                "schedule": { "end_time": "", "start_time": "" },
                "state_info": { "end_state_key_name": "", "start_state_key_name": "开发评估" },
                "work_item_info": { "work_item_id": id, "work_item_name": "", "work_item_type_key": "issue" },
            });
            // { ...base, ...extra }
            if let (Some(b), Some(e)) = (base.as_object_mut(), extra.as_object()) {
                for (k, v) in e {
                    b.insert(k.clone(), v.clone());
                }
            }
            base
        };
        let one = normalize_todo(
            &json!({ "list": [row(1, json!({ "schedule": null, "finish_time": { "finish_time": "2026-09-09 17:59" } }))], "total": 132 }),
            1,
        );
        assert_eq!(
            one.items,
            vec![MeegleTodoItem {
                item: MeegleWorkItem {
                    id: "1".into(),
                    name: "".into(),
                    business: None,
                    space_key: "67d7".into(),
                    space_name: Some("一体化产研团队".into()),
                    type_key: "issue".into(),
                    type_name: None,
                    status: None,
                    url: None,
                    updated_at: None,
                },
                node_name: Some("开发组长".into()),
                state_name: Some("开发评估".into()),
                schedule_start: None,
                schedule_end: None,
                finished_at: Some("2026-09-09 17:59".into()),
            }]
        );
        assert!(!one.has_more);
        assert_eq!(one.total, Some(132));

        let fifty =
            |total: u64| json!({ "list": (1..=50).map(|i| row(i, json!({}))).collect::<Vec<_>>(), "total": total });
        assert!(normalize_todo(&fifty(132), 1).has_more);
        assert!(!normalize_todo(&fifty(100), 2).has_more);
        // this_week 没数据时 list 是 null
        assert_eq!(
            normalize_todo(&json!({ "list": null, "total": 0 }), 1),
            MeeglePage { items: vec![], page: 1, has_more: false, total: Some(0) }
        );
    }

    /// groupForLookup 按 空间×类型 分组并去重 / chunk
    #[test]
    fn group_for_lookup_and_chunk() {
        let groups = group_for_lookup([
            ("p1", "issue", "1"),
            ("p1", "issue", "1"),
            ("p1", "story", "2"),
            ("p2", "issue", "3"),
            ("bad key", "issue", "4"),
            ("p1", "issue", "abc"),
        ]);
        let g = |space: &str, typ: &str, ids: &[&str]| LookupGroup {
            space_key: space.into(),
            type_key: typ.into(),
            ids: s(ids),
        };
        assert_eq!(groups, vec![g("p1", "issue", &["1"]), g("p1", "story", &["2"]), g("p2", "issue", &["3"])]);
        assert_eq!(chunk(&[1, 2, 3, 4, 5], 2), vec![vec![1, 2], vec![3, 4], vec![5]]);
        assert_eq!(chunk::<i32>(&[], 2), Vec::<Vec<i32>>::new());
    }

    // "bundledBinName 与 npm 包的 bin/ 命名一致" 测的是 bin.ts，随 bin.ts 留到 S6（见文件头）。

    /// isValidUrl 只收干净的 http(s) 链接
    #[test]
    fn is_valid_url_only_clean_http_links() {
        assert!(is_valid_url("https://project.feishu.cn/b2rl2h/story/detail/1?node=2"));
        let long = format!("http://{}", "a".repeat(2100));
        // TS 里还有一个非字符串的 1：Rust 的签名只收 &str，由类型挡掉
        for bad in ["", "ftp://x", "https://a b", "https://x\ny", long.as_str()] {
            assert!(!is_valid_url(bad), "{bad}");
        }
    }

    /// parseUrlTarget 认三类页面，其余给原因
    #[test]
    fn parse_url_target_three_kinds() {
        let dec = |extra: Value| {
            let mut base = json!({ "host": "project.feishu.cn", "simple_name": "b2rl2h" });
            for (k, v) in extra.as_object().unwrap() {
                base.as_object_mut().unwrap().insert(k.clone(), v.clone());
            }
            base
        };
        assert_eq!(
            parse_url_target(&dec(
                json!({ "url_kind": "workitem_detail", "work_item_type": "story", "work_item_id": "7072286406" })
            )),
            Ok(ParsedUrl::WorkItem {
                host: "project.feishu.cn".into(),
                simple_name: "b2rl2h".into(),
                type_key: "story".into(),
                id: "7072406".replace("406", "286406"),
            })
        );
        assert_eq!(
            parse_url_target(&dec(json!({ "url_kind": "view_multi_project", "view_id": "3oX4fFoDg" }))),
            Ok(ParsedUrl::MultiProjectView {
                host: "project.feishu.cn".into(),
                simple_name: "b2rl2h".into(),
                view_id: "3oX4fFoDg".into(),
            })
        );
        // storyView / issueView / workObjectView 都是按类型的视图，带 work_item_type
        assert_eq!(
            parse_url_target(&dec(
                json!({ "url_kind": "view_issue", "view_id": "uFdSs-8DR", "work_item_type": "issue" })
            )),
            Ok(ParsedUrl::View {
                host: "project.feishu.cn".into(),
                simple_name: "b2rl2h".into(),
                view_id: "uFdSs-8DR".into(),
                type_key: Some("issue".into()),
            })
        );
        assert!(
            parse_url_target(&dec(json!({ "url_kind": "view_workitem", "view_id": "abc", "work_item_type": "67da" })))
                .is_ok()
        );
        // 图表 / 甘特 / 总览带 view_id 但开不了
        for kind in ["view_chart", "view_user_gantt", "view_project_overview"] {
            let r = parse_url_target(&dec(json!({ "url_kind": kind, "view_id": "x" })));
            assert!(matches!(&r, Err(m) if m.contains(kind)), "{kind}");
        }
        let unknown = parse_url_target(&json!({ "url_kind": "unknown", "host": "project.feishu.cn" }));
        assert_eq!(unknown, Err("认不出这个链接指向什么".to_string()));
        let home = parse_url_target(&dec(json!({ "url_kind": "workitem_homepage", "work_item_type": "story" })));
        assert!(matches!(&home, Err(m) if m.contains("workitem_homepage")));
        assert!(parse_url_target(&json!("garbage")).is_err());
    }

    /// normalizeMultiViewItems：只有骨架，状态留给 enrich
    #[test]
    fn normalize_multi_view_items_skeleton_only() {
        let page = normalize_multi_view_items(
            &json!({
                "data": [
                    { "name": "图表增量更新", "project_key": "67d7", "work_item_id": 7043546793u64, "work_item_type_key": "67da" },
                    { "name": "no id" },
                ],
                "pagination": { "has_more": false, "page_num": 1, "page_size": 50, "total": 2 },
            }),
            1,
        );
        assert_eq!(
            page,
            MeeglePage {
                items: vec![MeegleWorkItem {
                    id: "7043546793".into(),
                    name: "图表增量更新".into(),
                    business: None,
                    space_key: "67d7".into(),
                    space_name: None,
                    type_key: "67da".into(),
                    type_name: None,
                    status: None,
                    url: None,
                    updated_at: None,
                }],
                page: 1,
                has_more: false,
                total: Some(2),
            }
        );
        assert_eq!(
            normalize_multi_view_items(&Value::Null, 2),
            MeeglePage { items: vec![], page: 2, has_more: false, total: None }
        );
    }

    // ======== context.test.ts ========

    fn metadata() -> MeeglePage<FieldMetadata> {
        normalize_fields(
            &json!({
                "list": [
                    { "field_key": "business", "field_name": "业务线", "field_type": "_business", "option": [
                        { "option_id": "parent", "option_name": "数据分析", "children": [
                            { "option_id": "leaf", "option_name": "AI" },
                            { "option_id": "old", "option_name": "历史业务", "disabled": true },
                        ] },
                    ] },
                    { "field_key": "steps", "field_name": "复现步骤", "field_type": "multi-text" },
                    { "field_key": "expected", "field_name": "预期结果", "field_type": "text" },
                    { "field_key": "actual", "field_name": "实际结果", "field_type": "text" },
                    { "field_key": "env", "field_name": "客户环境", "field_type": "select",
                      "option": [{ "option_id": "cloud", "option_name": "公有云" }] },
                    { "field_key": "version", "field_name": "影响版本", "field_type": "workitem_related_multi_select" },
                    { "field_key": "logs", "field_name": "日志", "field_type": "multi-text" },
                    { "field_key": "links", "field_name": "相关链接", "field_type": "link" },
                    { "field_key": "multi_attachment", "field_name": "附件", "field_type": "multi-file" },
                    { "field_key": "owner", "field_name": "环境负责人", "field_type": "user" },
                    { "field_key": "email", "field_name": "邮箱", "field_type": "text" },
                    { "field_key": "users", "field_name": "复现用户", "field_type": "multi-user" },
                    { "field_key": "template_version", "field_name": "流程版本", "field_type": "number" },
                ],
                "pagination": { "has_more": true },
            }),
            1,
        )
    }

    /// business resolves only authoritative option IDs / MQL cascade labels, never space or guessed names
    #[test]
    fn business_resolves_only_authoritative_names() {
        let metadata = metadata();
        let options = Some(&metadata.items[0].options);
        let b = |v: Value, o: Option<&HashMap<String, String>>| business_text(Some(&v), o);
        assert!(metadata.has_more);
        assert_eq!(b(json!("leaf"), options).as_deref(), Some("数据分析 / AI"));
        assert_eq!(b(json!("old"), options).as_deref(), Some("数据分析 / 历史业务"));
        assert_eq!(b(json!("unknown"), options), None);
        assert_eq!(b(json!("业务名称也不能凭字符串猜"), None), None);
        assert_eq!(
            b(
                json!({ "value": { "cascade_key_label_value": {
                    "key": "parent", "label": "数据分析", "children": [{ "key": "leaf", "label": "AI", "children": null }],
                } } }),
                None
            )
            .as_deref(),
            Some("数据分析 / AI")
        );
        assert_eq!(b(json!({ "name": "所属空间" }), None), None);
        assert_eq!(b(json!(["leaf", "leaf"]), options).as_deref(), Some("数据分析 / AI"));
    }

    /// detail context retains diagnosis text, code and image links, excludes personnel and unsupported objects
    #[test]
    fn detail_context_keeps_diagnosis_excludes_personnel() {
        let metadata = metadata();
        let markdown = "步骤\n![screen](https://example.com/s.png)<!-- image:private -->\n```js\nfail()\n```";
        let result = detail_context(
            &json!({ "work_item_fields": [
                { "key": "business", "value": "leaf" },
                { "key": "steps", "value": markdown },
                { "key": "expected", "value": "成功" },
                { "key": "actual", "value": "失败" },
                { "key": "env", "value": { "label": "公有云", "value": "cloud" } },
                { "key": "version", "value": [{ "id": 123, "name": "v1.2.3" }] },
                { "key": "logs", "value": "```text\nError: boom\n```" },
                { "key": "links", "value": { "name": "trace", "url": "https://example.com/trace" } },
                { "key": "multi_attachment", "value": [
                    { "file_name": "screen.png", "file_url": "https://example.com/screen.png" },
                    { "name": "trace.txt", "url": "https://example.com/trace.txt" },
                ] },
                { "key": "owner", "value": { "name": "某人", "email": "private@example.com" } },
                { "key": "email", "value": "private@example.com" },
                { "key": "users", "value": [{ "name": "某人", "email": "private@example.com" }] },
                { "key": "template_version", "value": 4 },
                { "key": "unknown", "value": { "email": "private@example.com" } },
            ] }),
            &metadata.items,
        );
        assert_eq!(result.business.as_deref(), Some("数据分析 / AI"));
        assert_eq!(result.context_fields.len(), 7);
        assert_eq!(
            result.attachments,
            vec![
                MeegleAttachment { name: Some("screen.png".into()), url: "https://example.com/screen.png".into() },
                MeegleAttachment { name: Some("trace.txt".into()), url: "https://example.com/trace.txt".into() },
            ]
        );
        assert_eq!(result.context_fields[0].value, strip_html_comments(markdown));
        assert_eq!(
            result.context_fields.iter().find(|f| f.name == "影响版本"),
            Some(&MeegleContextField { name: "影响版本".into(), value: "v1.2.3".into() })
        );
        // JSON.stringify(result) 里不能有：DetailContext 不上线，拿 Debug 文本代替
        assert!(!format!("{result:?}").contains("private@example.com"));
        assert!(
            metadata.items.iter().filter(|f| is_context_field(f)).all(|f| ![
                "owner",
                "email",
                "users",
                "template_version"
            ]
            .contains(&f.key.as_str()))
        );
        assert_eq!(metadata.items.iter().filter(|f| is_attachment_field(f)).count(), 1);
        assert_eq!(
            detail_context(
                &json!({ "work_item_fields": [{ "key": "logs", "value": { "email": "x" } }] }),
                &metadata.items
            )
            .context_fields,
            vec![]
        );
    }

    /// comments retain only content, time and attachment URLs
    #[test]
    fn comments_retain_only_content_time_and_attachments() {
        assert_eq!(
            normalize_comments(&json!({
                "comments": [
                    {
                        "content": "  已复现\n",
                        "created_at": "2026-09-11 10:53:20",
                        "creator": "private-user-key",
                        "file_url": "https://example.com/comment.png",
                    },
                    { "content": "", "file_url": "" },
                ],
                "pagination": { "total_pages": 3 },
            })),
            NormalizedComments {
                comments: vec![MeegleComment {
                    content: "已复现".into(),
                    created_at: Some("2026-09-11 10:53:20".into()),
                    attachments: Some(vec!["https://example.com/comment.png".into()]),
                }],
                total_pages: 3.0,
            }
        );
    }

    /// descriptionMarkdown strips only HTML comments, legacy description stays compact
    #[test]
    fn description_markdown_strips_only_comments() {
        let md = "\n![screen](https://example.com/s.png)<!-- image:data -->\n\n\n```ts\nx();  \n```\n";
        let detail = normalize_detail(
            &json!({
                "work_item_attribute": { "work_item_id": "1", "owned_project": { "name": "空间" } },
                "work_item_fields": [{ "key": "description", "value": md }],
            }),
            None,
        )
        .unwrap();
        assert_eq!(detail.description_markdown.as_deref(), Some(md.replacen("<!-- image:data -->", "", 1).as_str()));
        assert!(detail.description.as_deref().is_some_and(|d| d.contains("[图片]")));
        assert_eq!(detail.business, None);
    }

    /// new argv remain literal argv, MQL second page uses opaque session and fixed group
    #[test]
    fn new_argv_and_mql_second_page() {
        assert!(
            work_item_args("p", "1", Some(&s(&["business", "steps"])))
                .contains(&r#"--fields=["business","steps"]"#.to_string())
        );
        assert!(fields_args("p", "issue", 2).contains(&"--page-num=2".to_string()));
        assert!(mql_recent("p", "issue", None).ends_with("LIMIT 100"));
        assert!(mql_search("p", "issue", "bug", None).ends_with("LIMIT 100"));
        assert_eq!(
            mql_next_args("p", &json!({ "session_id": "-session" })),
            Some(s(&[
                "workitem",
                "query",
                "--project-key=p",
                "--session-id=-session",
                r#"--group-pagination-list=[{"group_id":"1","page_num":2}]"#,
                "--format",
                "json",
            ]))
        );
        assert_eq!(mql_next_args("p", &json!({})), None);
    }

    /// MQL list business uses labeled hierarchy or defers IDs to batched metadata resolution
    #[test]
    fn mql_business_label_or_deferred_id() {
        let metadata = metadata();
        let data = |value: Value| {
            json!({ "data": { "1": [{ "moql_field_list": [
                { "key": "work_item_id", "value": { "long_value": 1 } },
                { "key": "business", "value": value },
            ] }] } })
        };
        assert_eq!(
            normalize_mql_rows(&data(json!({ "key_label_value": { "key": "leaf", "label": "AI" } })))[0]
                .business
                .as_deref(),
            Some("AI")
        );
        let unresolved = normalize_mql_rows(&data(json!({ "string_value": "leaf" }))).remove(0);
        assert_eq!(unresolved.business, None);
        assert_eq!(
            business_text(unresolved.business_value.as_ref(), Some(&metadata.items[0].options)).as_deref(),
            Some("数据分析 / AI")
        );
    }

    // ======== 以下是 Rust 侧补的：JS 语义边角 ========

    #[test]
    fn object_values_order_follows_js() {
        // Object.values：数组下标形的键按数值升序（"2" 在 "10" 前），与 Map 的遍历顺序无关
        let rows = normalize_mql_rows(&json!({ "data": {
            "10": [{ "moql_field_list": [{ "key": "work_item_id", "value": { "long_value": 10 } }] }],
            "2": [{ "moql_field_list": [{ "key": "work_item_id", "value": { "long_value": 2 } }] }],
        } }));
        assert_eq!(rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["2", "10"]);
    }

    #[test]
    fn plain_description_trailing_blanks_before_any_line_terminator() {
        // 多行模式的 $ 也停在 \r 前：CRLF 行尾的空格同样剥掉
        assert_eq!(plain_description("a  \r\nb\t\u{2028}c"), "a\r\nb\u{2028}c");
    }

    #[test]
    fn comments_total_pages_follows_number_coercion() {
        let total = |v: Value| normalize_comments(&json!({ "pagination": { "total_pages": v } })).total_pages;
        assert_eq!(total(json!("4")), 4.0);
        assert_eq!(total(json!(0)), 1.0);
        assert_eq!(total(json!("abc")), 1.0);
        assert_eq!(total(json!(-3)), 1.0);
        assert_eq!(normalize_comments(&json!({})).total_pages, 1.0);
    }

    #[test]
    fn detail_context_assign_overwrites_like_object_assign() {
        let mut detail = normalize_detail(
            &json!({
                "work_item_attribute": { "work_item_id": "1" },
                "work_item_fields": [{ "key": "business", "value": { "key_label_value": { "label": "支付" } } }],
            }),
            None,
        )
        .unwrap();
        assert_eq!(detail.business.as_deref(), Some("支付"));
        detail_context(&json!({}), &[]).assign_to(&mut detail);
        assert_eq!(detail.business, None);
        assert_eq!(detail.context_fields, Some(vec![]));
        assert_eq!(detail.attachments, Some(vec![]));
    }
}
