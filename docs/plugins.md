# 写一个右侧栏插件

falcon 右侧栏内置四格之后可以挂插件：一个插件一格，面板内容由插件给（内置的「系统监控」与「Docker」也是插件，走的是同一套面板文档）。插件就是 falcon **后端那台机器上**的一个程序——任何语言都行——它把面板内容以 JSON 打到标准输出，falcon 用自己的组件、按当前主题画出来，原生客户端与浏览器版一模一样。设计取舍见 [ADR 0025](adr/0025-right-panel-plugins.md)。

例子在 [`docs/plugins/examples/`](plugins/examples/)：

| 例子 | 演示 |
|---|---|
| `uptime` | 最省事的写法：一条命令，`output: "text"`，输出原样贴进面板 |
| `hello` | POSIX sh：选项参数、按钮动作、确认框、通知、用 `FALCON_PLUGIN_DATA` 存状态 |
| `todos` | Python：跟着焦点项目走（`scope: "project"`）、读 stdin 的请求、列表行、复制 |

## 三分钟上手

1. 在设置 → 插件里看「插件目录」（缺省是 `~/.falcon/plugins/`，在**后端那台机器上**）。
2. 建一个文件夹，**文件夹名就是插件 id**（小写字母数字开头，`[a-z0-9_-]`，最长 48）：

   ```
   ~/.falcon/plugins/uptime/plugin.json
   ```

   ```json
   { "name": "运行时间", "icon": "clock", "command": "uptime", "output": "text", "refresh": 30 }
   ```

3. 设置 → 插件 → 重新扫描。右侧栏多出一个时钟图标。

改 `plugin.json` 或脚本不用重启 falcon：清单每次请求都重新读，点面板右上角的刷新就是新的。

## plugin.json

| 字段 | 类型 | 缺省 | 说明 |
|---|---|---|---|
| `command` | 字符串 或 字符串数组 | **必填** | 要跑的程序与参数（argv，**不经 shell**）。字符串就是一个程序名，不按空格拆。`./run.sh`、`bin/x` 这类带路径分隔符的相对插件目录；裸名字（`python3`）走 PATH——PATH 是登录 shell 的那份，与终端里一致 |
| `name` | 字符串 | 目录名 | 显示名（活动栏的提示、面板顶栏、设置页） |
| `description` | 字符串 | — | 设置页里的一行说明 |
| `icon` | 字符串 | `puzzle` | [Lucide](https://lucide.dev/icons/) 图标名（kebab-case，如 `activity`、`hard-drive`、`list-checks`），认不出就是拼图块 |
| `output` | `json` / `text` | `json` | `text`：整段 stdout 贴成一块等宽文字，插件没有按钮 |
| `scope` | `global` / `project` | `global` | `project`：侧栏换了焦点项目就重画一次 |
| `refresh` | 秒 | 不自动重画 | 面板开着时多久重画一次，最小 1。面板不露着、窗口最小化时不画 |
| `timeout` | 秒 | 10 | 一次运行的时限，最大 120。超时整个进程组被结束 |
| `api` | 数字 | 1 | 插件协议版本；比这版 falcon 新时插件不可用（设置页会说） |

清单写错了插件不会出现在右侧栏，设置 → 插件里会用红字写明原因。

## falcon 怎么叫你的插件

每次要画面板（打开、到了 `refresh`、换了选项、点了刷新）或点了按钮，falcon 就跑一次你的程序：工作目录是插件目录，请求同时经两条路给你——

**stdin**：一份 JSON

```json
{
  "api": 1,
  "event": "render",
  "params": { "step": "5" },
  "project": { "id": "…", "name": "mojito", "type": "local", "workingDir": "/Users/me/mojito" }
}
```

点了按钮时 `event` 是 `"action"`，另带 `"action": "<按钮的 id>"` 与 `"value"`。没有焦点项目时 `project` 是 `null`；SSH 项目还带 `"ssh": "用户@主机:端口"`（不含凭据，工作目录是远端路径——插件跑在后端本机，读不到它）。

**环境变量**：同样的内容，给没有 JSON 解析器的 shell 脚本

| 变量 | 内容 |
|---|---|
| `FALCON_PLUGIN_EVENT` | `render` / `action` |
| `FALCON_PLUGIN_ACTION` / `FALCON_PLUGIN_VALUE` | 点的按钮的 id 与 value（只在 action 时有） |
| `FALCON_PARAM_<名字大写>` | 每个面板参数，如 `FALCON_PARAM_STEP` |
| `FALCON_PROJECT_ID` / `_NAME` / `_TYPE` / `_DIR` / `_SSH` | 焦点项目（有才设） |
| `FALCON_PLUGIN_ID` / `FALCON_PLUGIN_DIR` | 插件 id 与目录 |
| `FALCON_PLUGIN_DATA` | 给插件存状态的目录（`<数据目录>/plugin-data/<id>/`，已建好） |
| `FALCON_PLUGIN_API` | `1` |

插件进程不常驻，每次都是从头跑；要记东西就写进 `FALCON_PLUGIN_DATA`。

## 你的输出：面板文档

stdout 打一个 JSON 对象：

```json
{
  "title": "面板顶上的一行小字（可省）",
  "refresh": 5,
  "blocks": [ { "type": "stats", "items": [ { "label": "计数", "value": 3 } ] } ],
  "toast": { "tone": "success", "title": "好了" },
  "params": { "item": "" }
}
```

- `refresh`（可省）盖过清单：这一次之后多少秒再画；`0` = 别自动重画。
- `toast`（可省）只在点按钮的回应里弹；画面板时带了也不弹。
- `params`（可省）改写面板参数（见下面的「参数」），比如删掉了详情页里的那一项、把 `item` 改回空串回到列表。
- 退出码非零、超时、输出不是合法 JSON：面板上显示错误与 stderr 的最后几行，上一次画好的内容留着。**能预料的问题（没装某个命令、不适用于这个项目）请画成一条 `notice`、正常退出**，面板照样能看。
- 某一块写错了只有那一块变成红色提示（"第 3 块解析失败：…"），其余照画。

### 色调

插件不给色值，只给语义色调，跟着用户的主题走：`default`（正文色）、`muted`（次要）、`accent`（强调蓝）、`success`、`warning`、`danger`。

### 块

每块一个 `type`。数值字段（`value`、单元格）写数字或字符串都行。

| `type` | 字段 | 画出来 |
|---|---|---|
| `heading` | `text`，`detail?` | 分组小标题，右边一行次要文字 |
| `text` | `text`，`tone?`，`mono?` | 一段文字；`mono` = 等宽、保留换行与空格 |
| `notice` | `text`，`tone?` | 带底色的提示条 |
| `stats` | `items: [{label, value, detail?, tone?}]` | 数字卡片，两个一行 |
| `meter` | `label`，`value`，`max?`（缺省 100），`text?`，`tone?` | 进度条；右边 `text` 缺省显示百分比 |
| `chart` | `label?`，`text?`，`series: [{name?, values: [数…], tone?}]`，`max?`，`slots?` | 走势图（从旧到新），线下淡填充；`max` 缺省按数据自动；`slots` = 横轴格数，点没攒满时线从右边长出来；多条带 `name` 的线有图例 |
| `kv` | `items: [{key, value, tone?, mono?}]` | 键值对 |
| `table` | `columns: [{title, align?: "left"\|"right"}]`，`rows: [[单元格…]…]` | 表格；第一列宽一些，数字列请右对齐 |
| `list` | `items: [{title, detail?, badge?, tone?, action?, menu?}]` | 列表；`tone` 画成标题前的小圆点，带 `action` 的行可点；`menu` 是行尾「⋯」弹出的菜单，每项同 `actions` 的按钮（`label`、`action`、`tone`，`danger` 标红，`icon` 不画） |
| `actions` | `items: [{label, action, icon?, tone?}]` | 一排按钮；`tone: "accent"` 主按钮、`"danger"` 危险按钮 |
| `select` | `param`，`value?`，`label?`，`options: [{value, label}]` | 选一个值（≤ 3 个短选项是分段控件，否则下拉），见下 |
| `divider` | — | 分隔线 |

不认识的 `type` 不画（老版本的 falcon 遇到新块不会报错）。

### 动作

按钮与列表行的 `action`：

| `kind` | 字段 | 做什么 |
|---|---|---|
| `invoke` | `id`，`value?`，`confirm?` | 以 `event: "action"` 再跑一次你的插件，你的输出换掉当前面板。`confirm` 非空时先弹确认框，正文就是它。`id` 用 `[A-Za-z0-9_.:-]` |
| `openUrl` | `url` | 用浏览器打开（只认 http / https） |
| `copy` | `text` | 写进剪贴板 |
| `refresh` | — | 马上重画 |
| `setParam` | `param`，`value` | 把一个面板参数改成 `value` 再重画（客户端自己做，不回插件）。做"列表 → 详情 → 返回"就靠它：行的 `action` 是 `{"kind":"setParam","param":"item","value":"42"}`，详情页上「返回」按钮把它改回 `""` |

### 参数（select）

`select` 的取值是面板的**参数**：用户选了什么，falcon 记着，之后每次请求都在 `params` / `FALCON_PARAM_*` 里带上，并立刻重画一次。你输出的 `value` 又会覆盖 falcon 记的那份——插件说了算，比如把一个已经不存在的选项改回缺省。

参数不一定要有 select 露着：`setParam` 动作也能设（用户点了哪一行），文档顶层的 `params` 能改写任何一个（两处都说了的以 `params` 为准）。参数名要小写字母开头的 `[a-z0-9_]`（最长 32），最多 16 个、每个值最长 1024；参数只在面板开着时存在，重启客户端后回到你的缺省。

## 上限

插件的输出会被裁剪：最多 200 块；每块最多 200 项（列表行的菜单 32 项）、表格 500 行 12 列、走势 4 条线各 600 个点；短字符串 512 个字符，一块文字 32K；stdout 最多 1 MB（超了进程被结束）。

## 调试

面板就是最快的回路：改脚本 → 点刷新。不想开客户端时直接打 API（后端绑在回环上且没设密码时不用登录；否则先在浏览器里登录，带上 `falcon_token` cookie）：

```sh
curl -s -X POST localhost:4923/api/plugins/<id>/render -H 'content-type: application/json' \
  -d '{"params":{"step":"5"}}' | jq .
curl -s localhost:4923/api/plugins | jq '.plugins[] | {id, enabled, error}'
```

也可以照着 falcon 的样子手动跑：

```sh
cd ~/.falcon/plugins/hello
echo '{"api":1,"event":"render","params":{},"project":null}' | \
  env FALCON_PLUGIN_EVENT=render FALCON_PLUGIN_DATA=/tmp/hello-data ./run.sh | jq .
```

## 安全

插件以 falcon 后端的身份运行，能做 falcon 能做的一切。只放你信任的代码。falcon 不提供"从网上安装插件"：插件只能由能登录后端那台机器的人放进插件目录；能用 falcon 的人只能触发已经装好的插件，传过去的只有面板参数与按钮 id。
