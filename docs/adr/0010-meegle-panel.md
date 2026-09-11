# 飞书项目面板：宿主机上的 meegle CLI 当数据源

终端工作台里跑着的活多半对应飞书项目（Meegle）里的一条需求 / 缺陷 / 任务，看它的状态、节点、描述得切去浏览器翻。右侧栏加一格「飞书项目」：看当前用户的待办（待办 / 本周 / 逾期 / 已办）、选空间搜视图与工作项、点开看详情，再一键跳去飞书。

数据源不是飞书 OpenAPI，而是 [meegle-cli](https://github.com/larksuite/meegle-cli)（npm 包 `@lark-project/meegle`，随服务内置，见决定四）。它把 OAuth、token 续期、钥匙串存储、命令清单都自己管了；我们只起进程、解析 JSON。

## 决定一：只在 falcon 后端所在的机器上跑 CLI，不跟项目走

Git / 文件面板按"当前项目的宿主机"执行命令，本地项目走本机、SSH 项目走远端。飞书项目的登录态是**人**的，不是项目的：token 存在跑 CLI 的那台机器的钥匙串里，远端主机上既没有 CLI 也没有登录。所以 `meegle` 一律在 falcon 后端本机 spawn，面板也不随焦点项目切换。

这也意味着不经 shell：直接 `spawn("meegle", argv)`，`meegle/command.ts` 只产 argv 数组，没有 POSIX / Windows 两套转义。环境用本地 PTY 同一份登录环境（`sessions/loginEnv`），不然 launchd 拉起的后端 PATH 里没有 `/opt/homebrew/bin`，CLI 就"未安装"。

用户输入一律 `--flag=value`：值以 `-` 开头时 `--flag value` 会被 pflag 当成下一个 flag。空间 key / 类型 key / 视图 id / 工作项 id 都过白名单正则，站点域名只放行主机名字符——放行别的就等于把任意 argv 交给 CLI。

## 决定二：CLI 的输出约定是实测出来的，全写在 command.ts 顶部

几条脾气（1.0.9 摸出来的，内置的 1.0.23 逐条复核过一致），每一条都对应代码里一个分支：

- 成功的 JSON 在 stdout；业务失败是 `{data:null, error:{code,message}}`，**打到 stderr**、退出码 1。`auth status` 未登录时退出码也是 1 但 JSON 在 stdout。所以两条流都当 JSON 试，退出码只是旁证。
- 命令清单是登录后从服务端拉的（`~/.meegle/cache/tools.json`）。**未登录时任何业务命令都报 `unknown command`**，与打错命令无法区分，得再问一次 `auth status` 才知道是没登录。
- `view search` 的空间、类型、关键字三个参数都必填，关键字是子串匹配。**没有"列出全部视图"的接口**，视图只能搜；`--view-scope=_all` 返回空。
- `mywork todo` 返回的 `work_item_name` 是空串，名字要靠 MQL 按 ID 批量补（`WHERE work_item_id IN (…)`，一次 50 个，按 空间 × 类型 分组因为 MQL 一次只能查一张表）。
- MQL 的 FROM 可以直接用 `project_key`.`type_key`，不必换成空间名 / 类型名；单引号按 SQL 规矩双写。`ORDER BY work_item_id DESC` 就是"最近"。
- `view search` 按租户限 5 qps，超了报 `rate limit, … qps: 5`。按类型扇出时并发 3、撞了退避重试两次，十几个类型的空间搜一次刚好不撞。
- 错误文案层层套：`error=ErrX,message=Service Internal Error,biz error: 真正的原因,retriable=true\nlogid: …`，给用户看的是最里层那句。
- `is_disable`：2 是启用、1 是停用，与字段名字面意思相反。

## 决定三：登录走 device-code，由后端拉起进程、前端给链接

`meegle auth login` 默认开浏览器做 OAuth——开在**后端那台机器**上；后端在 Mac mini 上跑、用户在别处的浏览器里用，就看不见。`--device-code` 模式把授权链接与授权码打到 stdout，后端抓出来交给前端显示，用户在自己的浏览器里点开授权；CLI 进程留着轮询，授权完成自己退出，面板每 2 秒问一次状态，切成正文。

同一时刻只有一个登录进程：再点一次给同一份链接，别起第二个把第一个的 code 作废。十分钟没授权就把进程收掉。登录进程退出时把查询缓存整表清掉——换人了。

## 决定四：CLI 随服务内置，不让用户另装

面板要用就得先 `npm install -g @lark-project/meegle`，对一个"打开就能用"的工作台来说是多余的一步；服务跑在别的机器上时更没人记得去那台机器装。`@lark-project/meegle` 的 npm 包本身就带着六个平台的静态 Go 二进制（`bin/meegle-<platform>-<arch>`，各约 11 MB，没有安装脚本），所以直接把它作为 server 的**锁定版本**依赖：

- pnpm 安装的开发 / dist 运行方式：`meegle/bin.ts` 从依赖包里取本平台的二进制直接 spawn，不经它的 `meegle.js` 包装（省一个 node 进程，也躲开包装脚本里的更新提示）。
- 单文件发布：`scripts/build-binary.mjs` 按目标把对应二进制作为资产 `bin/meegle` 封进 SEA blob，bootstrap 释放到 runtime 目录后经 `FALCON_MEEGLE_BIN` 指入——bundle 里没有 node_modules 可解析。父实例传下来的陈旧路径与 `FALCON_WEB_DIST` 同一套处理：指向的文件不存在就换成自己的。
- `FALCON_MEEGLE_BIN` 显式指定优先，PATH 上的 `meegle` 只是兜底；面板的"没找到 CLI"提示会把实际尝试的路径打出来。

CLI 的登录态（`~/.meegle` 与钥匙串）与二进制来源无关，用户之前用全局安装登录过的，内置版本直接沿用。锁定版本升级是一次有意的动作：换版本前把 `command.ts` 顶部那几条实测脾气重新跑一遍（1.0.9 → 1.0.23 已核对一致）。

## 决定五：后端归一化，前端不认识 CLI 的原始字段

`/api/meegle/*` 回的是 `@falcon/shared` 里的扁平结构（`MeegleWorkItem` / `MeegleTodoItem` / `MeegleView` …），CLI 那套 `work_item_attribute` / `moql_field_list` / 带类型标签的值信封（`{value_type:"key_label_value_list", value:{…}}`）都在 `command.ts` 里压平。CLI 升级改了形状只动一处，而且归一化是纯函数，能拿真实样本做单测。

错误码：CLI 没装 / 没登录是 409 带 `reason`（前端据此切到安装 / 登录提示，用户动手就能过）；CLI 跑了但飞书报错是 502；参数不合法 400。前端任何业务请求撞上 409 就重新拉一次状态。

## 决定六：粘贴链接打开，固定列表存 SQLite

飞书里那个「快速访问」是个人收藏，CLI 与 OpenAPI 都没开出来，面板拿不到；能做的是让用户把常用的东西钉在 Falcon 里。两步：

- **链接 → 目标**：`POST /api/meegle/resolve-url` 走 CLI 的 `url decode`（纯本地解析，路由表在它那边，**别自己拆路径**），只认三类 `url_kind`：`workitem_detail`、`view_story` / `view_issue` / `view_workitem`（`storyView` / `issueView` / `workObjectView/<type>` 三种路径，都带 `work_item_type`）、`view_multi_project`（`multiProjectView/<id>`）。图表 / 甘特 / 空间总览也带 `view_id` 但 `view get` 取不出工作项，按原因拒掉。`simple_name` 必须再经 `project search` 换成权威 `project_key`（同名空间可能有多个无权限，只认精确匹配），链接的站点与登录的站点不一致也拒。
- **全景视图**：`view list-multi-project-workitems` 只回名字 / 空间 / id / 类型 key，状态与外链走和待办同一个 `enrich()`。URL 里的 `node=` 是页面左侧树的定位，CLI 没有对应入参，开的是整个视图。
- **固定**：`meegle_pins` 表（kind / space_key / target_id / label / url…）。登录态是整台机器一份，固定列表也跟着机器走，不放 localStorage——换个浏览器还在。同一个 kind+空间+id 只存一条，再固定回已有的；名字用户可改（从链接打开的视图拿不到名字，默认就是 id）。所有字段过白名单再入库：它们之后会原样进 CLI 的 argv。

面板里「固定」页顶部一个链接输入框（粘贴即开），「空间」页的搜索框也认链接；下钻页顶栏有图钉。

## 决定七：两层 TTL 缓存，刷新才打穿

待办首页 50 条要 2–4 秒、空间搜索按类型扇出 4–6 秒，都是 CLI 的网络往返，单次请求省不掉。面板每次切走都会卸载（右侧栏一次只挂一个），不缓存就每次重来。

- **服务端** `TtlCache`（`client.ts`，默认 5 分钟；状态 30 秒、类型 10 分钟）：挡住重复的 meegle 进程。同一 key 并发合并成一次。`?fresh=1` 或 `POST /api/meegle/cache/clear` 跳过 / 清空。登录进程退出也清空。
- **前端** `lib/meegleCache.ts`（同一套 `TtlCache`，不进 localStorage）：面板卸载后再挂立刻画出上次的列表；30 秒内不打网络，之后后台静默再拉。顶栏刷新清两边。
- **右侧栏开着时切走不卸载飞书面板**（`display:none`），关掉右侧栏才卸——下钻栈和滚动还在；卸了也有前端缓存兜着。

## 面板结构

三页：「待办」是 mywork 四个列表加一个只筛已加载条目的输入框；「空间」是空间下拉 + 搜索框 + 类型 chip；「固定」是链接输入框 + 固定列表。有关键字就搜视图与工作项（两路并行，一路里某个类型失败只记一条错误，其余照常出结果）；没关键字但选了类型就列该类型最近 30 条；都没有就只给提示。点视图 / 工作项在面板内下钻（栈），顶上返回键；工作项详情页与列表行都带"在飞书项目中打开"外链，URL 形状 `https://<host>/<simple_name>/<type_key>/detail/<id>`（`url decode` 认 type_key，自定义类型也一样）。

详情描述是飞书富文本编辑器导出的 Markdown 方言，图片是 `![](url)` 紧跟 `<!-- image:{…} -->`；面板不渲染 Markdown，图片换成 `[图片]`、注释剥掉，其余标记原样留着。

## 工作项分组与复制上下文

工作项列表按 **业务线 → 工作项类型 → 当前状态** 三级折叠显示。业务线取工作项的
`business` 字段，不是空间名；级联选择值须解析成人可读名称，未知值不能直接拿内部 ID
冒充名称。分组计数只统计当前页，未知业务线 / 类型 / 状态有明确的兜底分组。

待办、普通视图、全景视图的一个面板页对应最多 100 条，即两个连续的 CLI 50 条页；
保留 CLI 返回顺序确定页边界，再在页内分组。CLI 不提供三字段全局排序，因此这不是
全量分组后的分页，同一分组可能出现在多页；界面明确标注“当前页分组”。
不为了制造全局排序而在首次打开时遍历整个租户。最近列表与搜索结果仍是有界结果集。

工作项行、工作项固定项和详情支持右键菜单（也支持键盘菜单键 / Shift+F10）：

- **复制工作项 Key**：复制飞书界面使用的 `<模板前缀>-<work_item_id>`。CLI 没有暴露
  模板前缀配置，当前按已确认的模板名映射；未知模板退回纯 `work_item_id`，不能按工作项
  类型猜测（同一个「产研任务」类型下的不同模板会使用不同前缀）。
- **复制 AI 修复上下文**：用户选择后才重新获取详情，复制结构化 Markdown。
  只包含标题、原始描述、附件和评论，不再复制空间、类型、状态、节点、优先级等基础
  元数据，也不复制人员标识。评论通过 `comment list` 分页获取，最多 200 条；失败或超过
  上限会在正文中明确标注，不拿列表摘要冒充完整详情。

`description` 继续供面板纯文本预览，`descriptionMarkdown` 保留图片引用与代码块，
两者不能混用。附件只复制飞书返回的名称与 URL，不下载文件内容，来源正文
不是可信指令；也提醒用户粘贴前检查敏感信息。此功能只写剪贴板，不调用 AI 服务。
浏览器支持时在菜单点击栈内提交带 Promise 的 `ClipboardItem`，避免 Safari 在异步
网络请求后丢失用户激活；权限拒绝明确提示重试。

真机回归：待办 / 视图翻到一整页底部，再点下一页，成功后应回到新页顶部；
断网翻页应保留原页、原滚动位置，恢复网络后“重试”仍请求刚才失败的目标页。
搜索 / 最近列表的本地分页也应回顶。分别在工作项行、固定工作项和详情中验证右键、
Shift+F10、Key 复制、含图片 / 代码块的上下文复制；剪贴板拒绝或登录过期不应显示成功。

## 决定八：工作项拖到源项目，只预填派生表单

工作项行、详情和工作项固定项在同时具有原始 `work_item_id` 与空间 key 时可拖动。侧栏只把
普通项目 / 多仓库容器的源项目行显示为接收目标；附属项目不是目标，保持 ADR 0002
“只能派生一层”的约束。浏览器拖放只认 Falcon 私有 MIME 和经过版本、类型、ID、空间 key
校验的 JSON，不接收 `text/plain` / URL，也不因为外部页面恰好拖来一段 JSON 就触发。

松手不创建任何目录或分支，只打开既有派生表单供用户复核。项目名和分支名预填原始
`work_item_id`；单仓库与多仓库都明确选择“新建分支”。单仓库基点是目标源项目探测到的
当前 `HEAD`；多仓库按 ADR 0003 从每个成员自己的 `HEAD` 新建同名分支。若同名分支已经
存在，多仓库表单会在逐仓库预检时阻止提交；单仓库由创建端点返回 `branch-exists`，两者都
不会静默改成检出已有分支。菜单入口仍打开原来的空表单。

详情正文需要能正常框选，所以只让 Key 旁的抓手可拖，不把整个滚动详情设为 `draggable`；
列表行和工作项固定行以抓手光标及 tooltip 提示拖放。真机回归至少覆盖 Chromium / Safari：
从三种入口拖到普通源项目和多仓库容器，确认表单预填 Key、明确为新建分支、基点为目标
`HEAD`；拖到附属项目、终端或外部文本目标不得创建；取消表单后原菜单派生仍是空表单。

## 已知取舍

- 视图只能按关键字搜，没有"这个空间的全部视图"。
- 待办名字与业务线要多一轮 MQL，每个 100 条逻辑页由两个 CLI 页组成；空间搜索按类型扇出。第一次（或刷新后）仍要等这些网络往返，之后 5 分钟内走缓存。
- 只做只读：不改字段、不流转节点。要写就去终端里用 `meegle` 或去飞书。
- 未在 Windows 后端上验过（npm 包里有 win32 二进制，spawn 路径不经 shell 应当一样；单文件发布本来就不支持 Windows）；SSH 远端上的 CLI 不支持，见决定一。
- 工作项搜索结果有数量上限；视图搜索每类型最多 CLI 给多少算多少，不代表空间内的全量结果。
