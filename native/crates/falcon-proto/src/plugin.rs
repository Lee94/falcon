//! 右侧栏插件（ADR 0025，作者手册 `docs/plugins.md`）：插件清单、面板文档（view）、
//! 动作，以及 falcon 交给命令插件的请求。
//!
//! 插件不往客户端里塞代码：它产出一份**声明式的面板文档**（[`PluginView`]：一串固定词汇的
//! 块——数字卡片、进度条、折线图、表格、列表、按钮、选项……），由客户端用自己的组件、按主题派生的
//! 语义色画出来。原生与浏览器版画的是同一份，插件不必知道客户端是什么；颜色只给语义
//! （[`PluginTone`]），不给色值，换主题插件跟着换。
//!
//! 这些形状有两个读者：客户端（服务端校验、裁剪过的），与**插件作者**（命令插件的 stdout 就是
//! [`PluginView`] 的 JSON）。后者是手写的 JSON，所以读法比别处宽：
//! - 枚举一律带 `Unknown` 兜底（`wire_enum! open`、块与动作的 `#[serde(other)]`）：插件写了个
//!   这版不认识的块 / 色调，那一块不画，别的照画；
//! - 表格单元格、键值对的值、数字卡片的值收字符串**也收数字与布尔**（[`lenient`]）：
//!   `{"value": 42}` 是最自然的写法，为这个让整块报错不值得；
//! - 整份文档逐块解析是服务端的事（`falcon-server` 的 `plugins/view.rs`）：某一块形状不对只把
//!   那一块换成一条提示，不连累整面板。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::wire::wire_enum;

/// 插件协议的版本。清单里的 `api` 与给命令插件的请求里的 `api` 都是它；
/// 不兼容的改动才加一，加块 / 加字段不算（老插件不写新字段，老客户端不画新块）。
pub const PLUGIN_API: u32 = 1;

wire_enum! {
    /// 插件从哪来。`builtin` 是 falcon-server 自己实现的（系统监控）；`command` 是用户放进
    /// `<数据目录>/plugins/<id>/` 的、由服务端起进程跑的。
    pub enum PluginKind open {
        Builtin => "builtin",
        Command => "command",
    }
}

wire_enum! {
    /// 插件看不看「当前项目」：`project` 的面板在侧栏换了焦点项目时重画一次（请求里带着那个
    /// 项目），`global` 的不管焦点。认不出的值按 `global` 处理。
    pub enum PluginScope open {
        Global => "global",
        Project => "project",
    }
}

wire_enum! {
    /// 语义色调。客户端映射到主题派生的 token（default = 正文色、muted = 次要字色、
    /// accent = 强调蓝、success / warning / danger = 成功 / 警示 / 危险色），插件不给色值。
    pub enum PluginTone open {
        Default => "default",
        Muted => "muted",
        Accent => "accent",
        Success => "success",
        Warning => "warning",
        Danger => "danger",
    }
}

wire_enum! {
    /// 表格列的对齐。数字列右对齐才好比大小。
    pub enum PluginAlign open {
        Left => "left",
        Right => "right",
    }
}

wire_enum! {
    /// 给命令插件的请求是哪一种：画面板，还是点了面板上的按钮。
    pub enum PluginEvent {
        Render => "render",
        Action => "action",
    }
}

/// `GET /api/plugins` 的一项：装了哪些插件（含停用的与清单写坏的——设置页要列出来说清原因，
/// 右侧栏只放 `enabled && error == null` 的）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginInfo {
    /// 命令插件就是目录名（`[a-z0-9][a-z0-9_-]{0,47}`），内置的是保留名
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Lucide 图标名（kebab-case，如 `activity`、`hard-drive`）；客户端认不出就用拼图块
    pub icon: String,
    pub kind: PluginKind,
    pub scope: PluginScope,
    /// 面板开着时多少秒重画一次；null = 只在打开 / 点刷新 / 点按钮时画
    pub refresh: Option<f64>,
    pub enabled: bool,
    /// 清单的毛病（JSON 不合法、缺 command…）。非 null 时插件不可用
    pub error: Option<String>,
    /// 命令插件所在的目录（后端那台机器上的绝对路径）；内置插件没有
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
}

/// `GET /api/plugins`
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginList {
    /// 命令插件放在哪（后端那台机器上的绝对路径）：设置页照着告诉用户往哪放
    pub dir: String,
    pub plugins: Vec<PluginInfo>,
}

/// `POST /api/plugins/:id/render`
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PluginRenderInput {
    /// 侧栏此刻的焦点项目；没有就不带
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// 面板上 [`PluginBlock::Select`] 的当前取值（param → value）
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, String>,
}

/// `POST /api/plugins/:id/action`：点了 [`PluginAction::Invoke`]。回的是新的 [`PluginView`]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PluginActionInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, String>,
    /// 按钮上的动作 id
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

/// `PUT /api/plugins/:id/enabled`
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginEnabledInput {
    pub enabled: bool,
}

/// 面板文档：插件画出来的一整面。命令插件的 stdout 就是它的 JSON。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PluginView {
    /// 面板顶栏插件名后面的那行小字（"本机 · macOS 15.1"）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// 这一次画完之后多少秒再画（盖过清单里的 `refresh`）；0 / 负数 = 别自动重画
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<f64>,
    #[serde(default)]
    pub blocks: Vec<PluginBlock>,
    /// 弹一条通知，报按钮的结果用：只有 `action` 的回应里的会弹，画面板时带了也不弹
    /// （面板两秒重画一次，弹出来就是刷屏）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toast: Option<PluginToast>,
    /// 改写面板参数（param → value）：客户端照着改掉自己记的那份，之后的请求都带新值。
    /// 给没有 select 块露着的参数用——比如删掉了详情里的那个容器，把 `container` 改回空串、
    /// 回到列表；select 块的 `value` 是同一件事的特例
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginToast {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tone: Option<PluginTone>,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

/// 面板上的一块。词汇是固定的：客户端只会画这些，插件拿它们拼。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum PluginBlock {
    /// 分组小标题，右边可带一行次要文字
    Heading {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// 一段文字。`mono` = 等宽、保留换行与空格（命令输出原样贴进来）
    Text {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tone: Option<PluginTone>,
        #[serde(default, skip_serializing_if = "is_false")]
        mono: bool,
    },
    /// 带底色的提示条（告警、"此平台不支持"）
    Notice {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tone: Option<PluginTone>,
    },
    /// 一排数字卡片（窄栏里两个一行）
    Stats { items: Vec<PluginStat> },
    /// 进度条：`value / max`（max 缺省 100），右边是 `text`（缺省显示百分比）
    Meter {
        label: String,
        value: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tone: Option<PluginTone>,
    },
    /// 走势图：一条或几条等间距采样的折线，纵轴 0 → `max`（缺省取各条的最大值）
    Chart {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        /// 标题右边的读数（"12.3%"、"↓ 1.2 MB/s"）
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        series: Vec<PluginSeries>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<f64>,
        /// 横轴一共几格。点还没攒满时线从右边长出来（"最右边是现在"）；缺省 = 点数，铺满
        #[serde(default, skip_serializing_if = "Option::is_none")]
        slots: Option<u32>,
    },
    /// 键值对
    Kv { items: Vec<PluginKv> },
    /// 表格。单元格是文字（数字也收）
    Table {
        columns: Vec<PluginColumn>,
        #[serde(deserialize_with = "lenient::rows")]
        rows: Vec<Vec<String>>,
    },
    /// 列表：一行标题 + 次要文字 + 右侧徽标；带 `action` 的行可点
    List { items: Vec<PluginListItem> },
    /// 一排按钮
    Actions { items: Vec<PluginButton> },
    /// 选一个值。取值是面板的参数：客户端记着，之后每次请求都带上（`params[param]`），
    /// 换了就立刻重画；回来的文档里的 `value` 又覆盖客户端记的那份——插件说了算
    Select {
        param: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<String>,
        options: Vec<PluginOption>,
    },
    /// 分隔线
    Divider,
    /// 这版客户端不认识的块：不画
    #[serde(other)]
    Unknown,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginStat {
    pub label: String,
    #[serde(deserialize_with = "lenient::text")]
    pub value: String,
    /// 数字下面的一行小字（"8 核"、"共 32 GB"）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tone: Option<PluginTone>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginSeries {
    /// 图例（多条线时才显示）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 从旧到新
    pub values: Vec<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tone: Option<PluginTone>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginKv {
    pub key: String,
    #[serde(deserialize_with = "lenient::text")]
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tone: Option<PluginTone>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub mono: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginColumn {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub align: Option<PluginAlign>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginListItem {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub badge: Option<String>,
    /// 标题前的小圆点用这个色；不带就不画点
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tone: Option<PluginTone>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<PluginAction>,
    /// 行尾「⋯」按钮弹出的菜单：这一行能做的事（启停、删除、复制…）。`tone: "danger"` 的项标红，
    /// `icon` 不画（菜单里只有字）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub menu: Vec<PluginButton>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginButton {
    pub label: String,
    pub action: PluginAction,
    /// Lucide 图标名，同 [`PluginInfo::icon`]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// `accent` = 主按钮、`danger` = 危险按钮，其余是普通按钮
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tone: Option<PluginTone>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginOption {
    pub value: String,
    pub label: String,
}

/// 点了按钮 / 列表行之后做什么。只有 `invoke` 会回到插件；其余是客户端自己做
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PluginAction {
    /// 回到插件：命令插件以 `event = action` 再跑一次，回的文档换掉当前面板。
    /// `confirm` 非空时先弹确认框，正文就是它
    Invoke {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        confirm: Option<String>,
    },
    /// 用系统浏览器打开（只认 http / https，服务端校验过）
    OpenUrl { url: String },
    /// 写进剪贴板
    Copy { text: String },
    /// 立刻重画
    Refresh,
    /// 把一个面板参数改成 `value` 再重画（客户端自己做，与换了 select 的选项一样）。
    /// 进出详情就靠它：列表行点进去是 `{param: "container", value: "<id>"}`，「返回」是改回空串
    SetParam {
        param: String,
        #[serde(default, deserialize_with = "lenient::text")]
        value: String,
    },
    /// 这版客户端不认识的动作：按钮照画、点了不做事
    #[serde(other)]
    Unknown,
}

/// falcon 经 stdin 交给命令插件的请求（同样的内容也平铺进环境变量，见 `docs/plugins.md`）
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginRequest {
    pub api: u32,
    pub event: PluginEvent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default)]
    pub params: BTreeMap<String, String>,
    /// 焦点项目；没有焦点时为 null
    pub project: Option<PluginProject>,
}

/// [`PluginRequest::project`]：插件拿得到的项目信息（不含凭据）
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginProject {
    pub id: String,
    pub name: String,
    /// `local` / `ssh`
    #[serde(rename = "type")]
    pub project_type: String,
    /// 工作目录（在项目的宿主机上；SSH 项目就是远端路径）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    /// SSH 项目的 `用户@主机:端口`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh: Option<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// 手写 JSON 的宽松读法：字符串之外也收数字与布尔（写回去一律是字符串）
mod lenient {
    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer};
    use serde_json::Value;

    fn scalar<E: serde::de::Error>(v: Value) -> Result<String, E> {
        match v {
            Value::String(s) => Ok(s),
            Value::Number(n) => Ok(n.to_string()),
            Value::Bool(b) => Ok(b.to_string()),
            Value::Null => Ok(String::new()),
            other => Err(E::custom(format!("要文字或数字，拿到的是 {other}"))),
        }
    }

    pub fn text<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
        scalar(Value::deserialize(d)?)
    }

    pub fn rows<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Vec<String>>, D::Error> {
        let rows = Vec::<Vec<Value>>::deserialize(d).map_err(|e| D::Error::custom(format!("rows 要二维数组：{e}")))?;
        rows.into_iter().map(|row| row.into_iter().map(scalar).collect()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::roundtrip;

    #[test]
    fn plugin_list() {
        let list = roundtrip::<PluginList>(
            r#"{"dir":"/Users/fay/.falcon/plugins","plugins":[
                {"id":"sysmon","name":"系统监控","icon":"activity","kind":"builtin","scope":"project",
                 "refresh":2,"enabled":true,"error":null},
                {"id":"hello","name":"hello","description":"示例","icon":"puzzle","kind":"command","scope":"global",
                 "refresh":null,"enabled":false,"error":"plugin.json 缺少 command","dir":"/Users/fay/.falcon/plugins/hello"}
            ]}"#,
        );
        assert_eq!(list.plugins[0].kind, PluginKind::Builtin);
        assert_eq!(list.plugins[1].refresh, None);
    }

    #[test]
    fn view_with_every_block() {
        let v = roundtrip::<PluginView>(
            r#"{"title":"本机","refresh":2,"blocks":[
                {"type":"heading","text":"CPU","detail":"8 核"},
                {"type":"text","text":"a\nb","mono":true},
                {"type":"notice","text":"不支持","tone":"warning"},
                {"type":"stats","items":[{"label":"CPU","value":"12%","tone":"accent"}]},
                {"type":"meter","label":"内存","value":12.5,"max":32,"text":"12.5 / 32 GB"},
                {"type":"chart","label":"CPU","series":[{"values":[1,2,3]},{"name":"↑","values":[3,2],"tone":"success"}],"max":100,"slots":90},
                {"type":"kv","items":[{"key":"主机","value":"mac","mono":true}]},
                {"type":"table","columns":[{"title":"进程"},{"title":"CPU","align":"right"}],"rows":[["a","1%"]]},
                {"type":"list","items":[{"title":"x","badge":"1","tone":"danger","action":{"kind":"openUrl","url":"https://a.b"}},
                    {"title":"y","action":{"kind":"setParam","param":"container","value":"abc"},
                     "menu":[{"label":"删除","tone":"danger","action":{"kind":"invoke","id":"rm","value":"abc","confirm":"删？"}}]}]},
                {"type":"actions","items":[{"label":"重启","icon":"power","tone":"danger",
                    "action":{"kind":"invoke","id":"restart","value":"web","confirm":"确定？"}},
                    {"label":"刷新","action":{"kind":"refresh"}},{"label":"复制","action":{"kind":"copy","text":"x"}}]},
                {"type":"select","param":"machine","label":"机器","value":"local","options":[{"value":"local","label":"本机"}]},
                {"type":"divider"}
            ],"toast":{"tone":"success","title":"好了"},"params":{"container":""}}"#,
        );
        assert_eq!(v.blocks.len(), 12);
        assert_eq!(v.params.get("container").map(String::as_str), Some(""));
    }

    #[test]
    fn unknown_values_degrade_instead_of_failing() {
        let v: PluginView = serde_json::from_str(
            r#"{"blocks":[{"type":"hologram","x":1},{"type":"notice","text":"t","tone":"sparkly"},
                {"type":"actions","items":[{"label":"?","action":{"kind":"teleport"}}]},
                {"type":"table","columns":[{"title":"n","align":"center"}],"rows":[[1,true,null,"s"]]},
                {"type":"stats","items":[{"label":"n","value":42}]}]}"#,
        )
        .unwrap();
        assert_eq!(v.blocks[0], PluginBlock::Unknown);
        assert_eq!(v.blocks[1], PluginBlock::Notice { text: "t".into(), tone: Some(PluginTone::Unknown) });
        let PluginBlock::Actions { items } = &v.blocks[2] else { panic!() };
        assert_eq!(items[0].action, PluginAction::Unknown);
        let PluginBlock::Table { rows, columns } = &v.blocks[3] else { panic!() };
        assert_eq!(columns[0].align, Some(PluginAlign::Unknown));
        assert_eq!(rows[0], vec!["1", "true", "", "s"]);
        let PluginBlock::Stats { items } = &v.blocks[4] else { panic!() };
        assert_eq!(items[0].value, "42");
    }

    #[test]
    fn inputs_and_request() {
        roundtrip::<PluginRenderInput>(r#"{}"#);
        roundtrip::<PluginRenderInput>(r#"{"projectId":"p1","params":{"machine":"local"}}"#);
        roundtrip::<PluginActionInput>(r#"{"action":"restart","value":"web"}"#);
        roundtrip::<PluginEnabledInput>(r#"{"enabled":false}"#);
        roundtrip::<PluginRequest>(
            r#"{"api":1,"event":"action","action":"go","params":{},
                "project":{"id":"p","name":"n","type":"ssh","workingDir":"/srv","ssh":"u@h:22"}}"#,
        );
        roundtrip::<PluginRequest>(r#"{"api":1,"event":"render","params":{"a":"b"},"project":null}"#);
    }
}
