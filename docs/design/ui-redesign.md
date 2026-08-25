# Falcon UI / 交互重设计方案

> 状态：提案（2026-08-14） · 范围：`packages/web` 全部界面 · 目标形态：桌面宽屏，深色优先
>
> 术语一律沿用 [CONTEXT.md](../../CONTEXT.md)：Project / Terminal Session / Detach / Terminate / Viewer / Durable / Unverified。本文不新造词。

---

## 0. 这份文档是什么

它是一份可直接施工的重设计规格：诊断结论、信息架构、每块界面的布局与状态、视觉 token、组件规格、键盘方案、需要后端配合的接口缺口、分批落地清单与验收标准。

它**不**包含：像素级 Figma 稿、图标资源、浅色主题的完整色板（预留了结构，v1 不做）、移动端适配（本次明确不做）。

---

## 1. 设计立场

### 一句话主张

**把「会话是活的、跑在某台具体的机器上」变成界面的主角。**

Falcon 的硬承诺是"关掉网页会话不死"，但现在的界面完全没有表达它：首屏是一张空表格，持久性只在出问题时以一个黄色 `非持久` 徽标出现，终端满屏黑底里看不出自己连的是哪台机器。产品最贵的能力在 UI 上是隐形的，而最危险的信息（我在 prod 上）也是隐形的。

### 三条原则

1. **存活性可见（Liveness is the UI）**
   任何时刻，用户不用点任何东西就该知道：有几个会话在跑、跑在哪、哪些是持久的、哪些需要我处理。好消息和坏消息一样要显示——只标注"非持久"而不标注"持久"，等于让用户无法确认承诺已生效。

2. **身份先于内容（Know where you are before you type）**
   终端是一个可以执行任意破坏性命令的输入框。在敲下第一个字符之前，界面必须已经告诉你这是哪台主机、哪个目录。这是安全需求，不是装饰。

3. **克制的密度（Dense, not busy）**
   目标用户天天用 tmux / zellij / vim。信息密度要高、动效要少、键盘要能走完全流程。参照坐标是 Zed / Linear / Warp 的克制感，不是通用 SaaS 的圆角卡片堆叠。

---

## 2. 现状诊断

按严重度排序。每条标注了代码位置，便于施工时定位。

### S1 · 终端里看不出自己在哪台机器上

`TerminalView` 只渲染一个 banner（仅异常时出现）加一块全屏 xterm（[TerminalView.tsx:187](../../packages/web/src/components/TerminalView.tsx)）。会话名、所属项目、本地还是远端、哪个 host、工作目录、是否持久——全部不可见。多开几个 SSH tab 之后，本地和生产的终端长得一模一样。

这是本次重设计要解决的**第一优先级问题**，且是唯一一条带安全后果的。

### S2 · 刷新页面，工作台布局全丢

`tabs` 与 `active` 只存在 zustand 内存里（[store.ts:32-34](../../packages/web/src/store.ts)），没有持久化。刷新后回到空的会话总览。

这与产品承诺**自相矛盾**：会话确实还活着，但用户重新打开页面看到的是"什么都没有"，需要手动从侧栏逐个点回来。承诺兑现了，体感却是"没兑现"。

### S3 · 破坏性操作与安全操作长得一样

项目行上三个常驻 ghost 按钮：`✎` 编辑、`✕` 删除项目、`+` 新建终端（[Sidebar.tsx:108-128](../../packages/web/src/components/Sidebar.tsx)）。问题有三层：

- 删除项目会终止其下**全部会话**，却是一个和"编辑"同等视觉重量的常驻小图标；
- `✕`（删除项目，不可逆）与 tab 上的 `×`（关闭 tab，无副作用）**同形不同义**；
- 删除按钮排在最高频操作"新建终端"的**紧邻左侧**，误触代价极高。

### S4 · 系统对话全靠浏览器原生弹窗

`alert` 报错（[Sidebar.tsx:32](../../packages/web/src/components/Sidebar.tsx)、[SessionOverview.tsx:27](../../packages/web/src/components/SessionOverview.tsx)）、`confirm` 确认删除与终止、`prompt` 重命名会话（[SessionOverview.tsx:39](../../packages/web/src/components/SessionOverview.tsx)）。

除了粗糙，更实际的问题是**信息量不足**：`确定删除项目「prod-api」？其所有会话将被终止。` 没有告诉用户"其所有"到底是几个、叫什么、有没有正在跑的构建。

### S5 · Unverified 状态没有落在动作上

`unverified`（待接回）是这个产品最独特的状态，它天然需要一个动作。但在侧栏里，点击一个 unverified 会话只是打开 tab；「接回」按钮藏在会话总览表格里，或者要等 tab 打开后在顶部 banner 上找（[TerminalView.tsx:158-173](../../packages/web/src/components/TerminalView.tsx)）。

状态与它的补救动作被拆到了两个页面。

### S6 · 三处入口指向同一个会话，"我在哪"不成立

侧栏会话行、tabbar、总览表格的「打开」按钮都能打开会话，但三者的关系没有被表达出来。更别扭的是会话总览是个**伪 tab**——它不在 tabbar 里，靠侧栏 header 上一个 `≡` 按钮进入（[Sidebar.tsx:78-85](../../packages/web/src/components/Sidebar.tsx)），选中态只靠 `fontWeight: 700` 表示。

### S7 · tabbar 条件渲染导致布局跳动

`{tabs.length > 0 && <div className="tabbar">}`（[App.tsx:49](../../packages/web/src/components/App.tsx)）。从 0 个 tab 到 1 个 tab，整个主区下移一行；关掉最后一个 tab 又跳回来。

### S8 · Zellij 授权弹窗打断了错误的时刻，且拒绝之后没有回头路

用户点「新建终端」时的意图是"开个终端"，此刻却被要求做一个安全决策："允许 falcon 往我的服务器写可执行文件吗"（[ZellijInstallModal.tsx:147-170](../../packages/web/src/components/ZellijInstallModal.tsx)）。心智不匹配，用户倾向于随便点一个把弹窗消掉。

而"随便点一个"是有代价的：点「不安装」会写入 `authorized: false`，此后 `Sidebar` 的 `needsSetup` 判断（[Sidebar.tsx:45-47](../../packages/web/src/components/Sidebar.tsx)）两个分支都不再成立，弹窗永不再现，**UI 上没有任何地方能反悔**。这台主机就此只能开非持久会话。

重试本身已经做得不错（自动 + 手动分层、按 `canRetryInstall` 条件渲染、取消不等于拒绝），剩下的缺口是失败态不能改下载地址——而这恰恰是 `download-failed` 唯一有效的补救。详见 §7.4。

### S9 · 状态只用颜色编码

`.dot.active / .unverified / .dead` 三个类只改 `background`（[styles.css:199-209](../../packages/web/src/styles.css)），绿 / 黄 / 红。红绿色觉障碍用户在侧栏里分不出"运行中"和"已丢失"。`StatusDot` 只有 `title`，没有可访问文本（[StatusDot.tsx:6](../../packages/web/src/components/StatusDot.tsx)）。

### S10 · Modal 缺少对话框该有的一切

`Modal` 是一个裸 div（[Modal.tsx:3-14](../../packages/web/src/components/Modal.tsx)）：无 `role="dialog"` / `aria-modal`、无焦点陷阱、无 Esc 关闭、关闭后不还原焦点。并且**点遮罩即关**——用户在 `ProjectForm` 里填了一半 SSH 配置，手滑点到遮罩，全部丢失。

### S11 · 键盘操作接近于零

切 tab、新建终端、跳会话、接回，全部只能用鼠标。对一个终端工作台、对天天用 zellij 快捷键的目标用户，这是硬伤。

### S12 · 视觉系统缺少层次与状态

- 只有一档 `--border`、一档 `--text-dim`，无法表达"次要 / 更次要"；
- 无 spacing scale，间距是散落的魔法数（`4px 10px`、`5px 6px`、`8px 12px`、`4px 6px 4px 18px`）；
- **无 focus ring**：`input:focus` 只把 border 换成 accent 色（[styles.css:79-83](../../packages/web/src/styles.css)），button 完全没有 focus 样式，键盘用户在界面里是瞎的；
- 无 disabled 样式（`ProjectForm` 的提交按钮 disabled 时和可点时长得一样）；
- 等宽字体只在 xterm 里定义（[TerminalView.tsx:35](../../packages/web/src/components/TerminalView.tsx)），路径、主机名、版本号这些天生等宽的内容用的是比例字体。

### S13 · 术语与 i18n 有漏洞

- 项目类型硬编码中文，绕过 i18n：`{p.type === "ssh" ? "SSH" : "本地"}`（[Sidebar.tsx:106](../../packages/web/src/components/Sidebar.tsx)、[SessionOverview.tsx:68](../../packages/web/src/components/SessionOverview.tsx)）；
- tab 关闭按钮的 `title` 用了 `t("common.cancel")` → 显示"取消"（[App.tsx:63](../../packages/web/src/components/App.tsx)），语义错误，而这恰恰是最需要解释清楚的地方（关 tab 到底会不会终止会话）；
- `i18n.ts` 里 `session.nonDurableHint` 等文案措辞偏系统日志，非人话。

---

## 3. 信息架构

### 现在

```
Sidebar（项目树）        Main
  └ 项目                   ├ tabbar（条件出现）
     └ 会话                └ panes
                              ├ overview（伪 tab，从侧栏 header 进）
                              └ terminal × N
```

### 重设计后

```
┌─ Sidebar（可折叠） ──┬─ Stage ────────────────────────────┐
│                      │  TabBar（常驻，含「总览」固定首位）│
│  · 全局会话摘要      ├────────────────────────────────────┤
│  · 项目 › 会话 树    │  SessionHeader（会话身份条）        │
│  · 搜索 / 过滤       │  ────────────────────────────       │
│                      │  Terminal / Overview                │
│                      │                                     │
├──────────────────────┴────────────────────────────────────┤
│ StatusBar（22px）：链路 · 尺寸 · Viewer 数 · 持久性来源    │
└────────────────────────────────────────────────────────────┘
                 CommandPalette（⌘⇧P，覆盖层）
```

三个结构性变化：

1. **会话总览升级为 tabbar 里的固定首个 tab**（不可关闭）。它不再是"伪 tab"，`ActiveView` 的两种 kind 变成同一层级的东西，"我在哪"由 tabbar 单点表达。
2. **新增 SessionHeader**（会话身份条），解决 S1。
3. **新增 StatusBar**，把链路状态、终端尺寸、Viewer 数、持久性来源这些"随时想瞥一眼但不想被打扰"的信息放在开发者工具的传统位置。

### 导航模型（明确关系）

| 层 | 是什么 | 关闭它意味着 |
| --- | --- | --- |
| Sidebar 会话行 | **所有**会话的全集（存在性） | — |
| Tab | 我**正在关注**的会话子集（工作台布局） | Terminate，会话一起结束（Shift+关闭才是 Detach） |
| Stage | 当前**聚焦**的那一个 | — |

这个三层关系要靠文案讲出来，见 §7.3。

---

## 4. Sidebar 重设计

### 布局

```
┌────────────────────────────────────┐
│ Falcon                       ⌘⇧P ⚙ │  40px header
├────────────────────────────────────┤
│  ⦿ 5 运行中 · ◐ 1 待接回 · ○ 2 丢失│  全局摘要，点击 → 总览 tab
├────────────────────────────────────┤
│ 项目                            ＋ │  section label + 新建项目
│                                    │
│ ▾  falcon                本地  ⋯  │  ← 项目行（hover 出 ＋ 和 ⋯）
│      ⦿ dev server                  │
│      ⦿ build                       │
│                                    │
│ ▾ ▎prod-api          SSH ⛨  ⋯    │  ← ▎= 主机身份色；⛨ = 持久已就绪
│      ⦿ deploy                      │
│      ◐ migrate       待接回  接回 │  ← 状态需要动作时，动作就在行内
│      ○ old-shell     shell 已退出 ✕│  ← ✕ 在这里是"清除记录"，安全
│                                    │
│ ▸ ▎staging           SSH ⚠  ⋯    │  ← 折叠；⚠ = 该主机会话非持久
│                                    │
├────────────────────────────────────┤
│  ◍ 已连接 · v1.0.0                 │  footer（原「访问密码 / 退出」移入 ⚙）
└────────────────────────────────────┘
```

宽度 260px，可拖拽 220–420px，可折叠为 0（`⌘B`）。折叠状态持久化。

### 关键决策

**D1 · 项目行的操作收进溢出菜单，只留一个主操作。**
hover 时项目行右侧出现 `＋`（新建终端，最高频）和 `⋯`（溢出菜单）。菜单内容：

```
新建终端                  ⌘T
在总览中筛选此项目
──────────────
编辑项目…
持久会话设置…              ← SSH 项目才有，见 §7.4
──────────────
删除项目…                  ← 危险色，独立分组，永远在最下
```

删除从"常驻图标"降级到"菜单最底、有分隔线、危险色"，同时不再与 `＋` 相邻。解决 S3。

**D2 · 状态用形状 + 颜色双编码。**

| 状态 | 记号 | 颜色 | 语义 |
| --- | --- | --- | --- |
| Active | `⦿` 实心圆 | success | 后端确认存活 |
| Unverified | `◐` 半环 + 呼吸动画 | warning | 链路断了，可能还活着 |
| Dead | `○` 空心圆 | danger | 确认死亡，只剩记录 |

呼吸动画**只给 unverified**，因为它承载"系统正在/等待重连"的信息，不是装饰。`prefers-reduced-motion` 下降级为静态半环。解决 S9。

**D3 · Dead 会话降权而不隐藏。**
文字降到 tertiary 色、名称加删除线、右侧直接显示死因短语（"shell 已退出" / "后端重启" / "链路断开" / "Zellij 会话已不存在"）和一个「清除」`✕`。它不再和活着的会话共享同一套视觉重量。

**D4 · 主机身份色条。**
SSH 项目在名称左侧显示一条 3px 竖色条，颜色由 `host:port:username` 稳定哈希得出（算法见 §9.5）。本地项目不给色条——"有颜色 = 在别人的机器上"这个信号必须是独占的，才有警示价值。同一条色带会出现在 tab 和 SessionHeader 上，形成跨界面的身份链路。

**D5 · 持久性做正向显示。**
项目行的 `⛨` 表示"该宿主机的持久能力已就绪"，`⚠` 表示"该主机上的会话不持久"，hover 给出具体原因（复用现有 `reasonText`）。今天只显示坏消息，用户无法确认承诺生效。

**D6 · 搜索框按需出现。**
项目数 ≥ 6 或会话数 ≥ 10 时，header 下方出现过滤输入框；`⌘F` 聚焦。匹配项目名、会话名、host。少量项目时不占空间。

---

## 5. Stage：TabBar / SessionHeader / StatusBar

### 5.1 TabBar

```
┌──────────┬────────────────────┬───────────────────┬───┐
│ ▤ 总览   │ ⦿ dev server     × │ ▎⦿ deploy      × │ ＋│
└──────────┴────────────────────┴───────────────────┴───┘
     ↑ 固定首位，不可关闭        ↑ 色条 = SSH 主机身份
```

- **常驻渲染**，不再条件出现（解决 S7）。无 tab 时也保留高度，只显示「总览」和 `＋`。
- 每个 tab：`[主机色条] [状态记号] [会话名] [×]`。
- 关闭按钮的 tooltip 必须是 **「关闭标签页 · 结束会话」**，并带上第二行「按住 Shift 关闭 · 会话留在后台继续运行」（解决 S13 的认知问题）。
- 中键点击关闭；`⌘W` 关闭当前；两者都认 Shift（只 Detach、不杀）；tab 可拖拽排序。
- 溢出时横向滚动 + 两端渐隐遮罩，不做折叠菜单（tab 数量在这个产品里天然不多）。

**实现约束（重要）**：非活动 pane 必须继续保持 `visibility: hidden` 而不是卸载（当前 [App.tsx:80-92](../../packages/web/src/components/App.tsx) 的做法是对的）。xterm 实例和 WebSocket 一旦卸载就要重连重放，切 tab 会闪。重构布局时**不要**改成条件挂载。

### 5.2 SessionHeader（新增，解决 S1）

```
┌────────────────────────────────────────────────────────────────┐
│▎ deploy  ✎     prod-api · SSH   ubuntu@10.0.3.12:22   ⛨ 持久  ⋯│
│                                 ~/srv/api                       │
└────────────────────────────────────────────────────────────────┘
```

- 高度 44px，双行：主行是会话名 + 项目 + 连接串；次行是工作目录（等宽字体、tertiary 色、超长时中间省略）。
- 本地项目的连接串位置显示 `本机 · Windows` 之类，工作目录显示实际路径。
- **会话名就地重命名**：点击名称或 `✎` 进入 inline 编辑，Enter 提交 / Esc 取消。替换掉 `prompt()`（解决 S4 的一部分）。
- `⋯` 菜单：接回 / 复制连接串 / 在新 tab 中复制此会话（同项目新建会话）/ 清空回滚缓冲 / **终止会话**（危险分组）。
- 整条 header 的顶部有一条 2px 主机身份色（SSH）或中性色（本地）。**当会话属于 SSH 项目时，header 背景使用该身份色 8% 的叠加**——保证扫一眼就知道"这不是本机"。

### 5.3 StatusBar（新增）

22px，右对齐分段，全部为纯展示 + hover 详情：

```
                    ⦿ 已连接   80×24   👁 2   Zellij 0.44.4 · 持久
```

| 段 | 内容 | 说明 |
| --- | --- | --- |
| 链路 | `已连接` / `重连中 · 第 3 次` / `已断开` | 取代现在的红色 banner 常驻占位 |
| 尺寸 | `cols×rows` | 调整窗口时短暂高亮 |
| Viewer | `👁 2` | 多端同看时才显示；**需后端支持，见 §12** |
| 持久性 | `Zellij 0.44.4 · 持久` / `非持久 · 原因` | hover 展开完整原因 |

Banner 保留，但只用于**需要用户动作**的状态（unverified 待接回、dead、attach 失败）。纯信息性的状态下沉到 StatusBar，减少内容区被挤压。

---

## 6. 会话总览重设计

从裸表格升级为"舰队视图"。

```
会话总览

┌──────────────┐ ┌──────────────┐ ┌──────────────┐
│ ⦿ 运行中   5 │ │ ◐ 待接回   1 │ │ ○ 已丢失   2 │   ← 可点击筛选
└──────────────┘ └──────────────┘ └──────────────┘

[全部 ▾] [项目 ▾]                    [□ 全选]  [清除全部已丢失]

□ │ 状态   │ 会话        │ 项目 · 宿主机          │ 持久性      │ 空闲    │
──┼────────┼─────────────┼────────────────────────┼─────────────┼─────────┤
□ │ ⦿ 运行 │ dev server  │ falcon · 本机          │ ⛨ 持久      │ 活跃中  │ ⋯
□ │ ◐ 待接回│ migrate    │ prod-api · 10.0.3.12   │ ⛨ 持久      │ 12 分钟 │ 接回 ⋯
□ │ ○ 丢失 │ old-shell   │ staging · 10.0.4.9     │ ⚠ 非持久    │ 3 天    │ 清除 ⋯
```

变化：

1. **摘要卡**把"当前舰队健康度"提到最前，兼作筛选器。
2. **新增宿主机列**——总览的价值就在跨机器纵览。
3. **批量选择 + 批量操作**：`清除全部已丢失` 是刚需（dead 会话只能逐个清，[SessionOverview.tsx:116](../../packages/web/src/components/SessionOverview.tsx)）。批量终止走同一个确认框。
4. **行内主操作 + `⋯` 溢出**：现在一行最多铺 4 个按钮（打开/接回/重命名/终止），视觉噪音大且危险操作平铺。改为按状态给一个主操作，其余进菜单。
5. **空闲时间**显示相对值，`title` 给绝对时间戳。
6. **空状态**不是"没有会话"这四个字，见 §8。

---

## 7. 关键流程重设计

### 7.1 首次进入 / 空状态

| 情形 | 现在 | 重设计 |
| --- | --- | --- |
| 无项目 | 侧栏"还没有项目" + 空表格 | Stage 居中引导卡：两个大入口「本地文件夹」「远程 SSH」，下方一句 *"会话在后台持续运行，关掉这个网页也不会中断。"* ——第一次接触就讲清核心承诺 |
| 有项目无会话 | 空表格 | 总览显示项目卡片列表，每张卡一个「新建终端」主按钮 |
| 有会话 | 空表格（tab 全丢） | **恢复上次的 tab 集合与聚焦项**（解决 S2）；被恢复的会话若已 dead，tab 保留但显示 dead 态，不静默丢弃 |

布局持久化：`localStorage["falcon.workspace"] = { tabs: string[], active, sidebarWidth, sidebarCollapsed }`。恢复时按当前会话列表过滤掉已不存在的 id（`refreshSessions` 里已有类似逻辑，[store.ts:69-77](../../packages/web/src/store.ts)）。

### 7.2 新建会话

```
项目行 ＋  或  ⌘T  或  命令面板「新建终端」
        │
        ├─ 本地项目 ─────────────────────→ 立即创建 → 打开 tab
        │
        └─ SSH 项目
             ├─ 该主机已授权且已装 ──────→ 立即创建 → 打开 tab
             └─ 未决策 ──→ 持久会话抽屉（§7.4）
```

创建期间：tab 立刻出现并显示骨架 + "正在建立会话…"，而不是等 REST 返回后才出现。失败则该 tab 变为错误态并给「重试」，不再用 `alert`。

### 7.3 关 tab 教育（关 tab = 结束会话，Shift 才是 Detach）

Tab 即会话：用户手动收起它就是不要它了，关 tab 默认 Terminate。留在后台跑是**另一条**动作，得让用户知道它存在：

- tab 关闭按钮 tooltip：**「关闭标签页 · 结束会话」** + 「按住 Shift 关闭 · 会话留在后台继续运行」
- **首次**关闭 tab 时，在「已终止「dev server」」这条 toast 上补一句：*"关闭标签页会结束会话。想让它继续在后台跑，按住 Shift 再关。"* 带一个「不再提示」。
- 不弹确认框：确认框挡在每一次关 tab 前面就成了噪音，用户会闭眼点。代价用事后 toast 兜。
- Shift 关闭（Detach）时弹一条 info toast：*"「dev server」仍在后台运行"*——让用户看到"它去哪了"。

### 7.4 持久会话授权与安装（重构 S8）

**把主机级决策从会话创建流程中移出。**

**主路径**——新建 SSH 项目时，`ProjectForm` 增加一节：

```
持久会话
┌──────────────────────────────────────────────────────┐
│ ☑ 在这台主机上启用持久会话                            │
│                                                       │
│   会在 10.0.3.12 上安装 Zellij 0.44.4（约 14 MB，由该 │
│   主机自行下载到 ~/.falcon/bin/）。启用后，SSH 断线或 │
│   falcon 重启都不会中断会话。                          │
│                                                       │
│   下载地址  [https://github.com/zellij-org/…      ]   │
│   ⚠ falcon 会执行从此地址下载的二进制且不校验完整性， │
│      请确保地址可信。                                  │
│                                                       │
│   授权按主机记（host + 端口 + 用户名），同一台机器上   │
│   的其他项目不再询问。                                 │
└──────────────────────────────────────────────────────┘
```

用户此刻正在配置这台机器，做这个决策心智是对的。安装在项目保存后于后台进行，进度显示在项目行（转圈）与 toast，**不阻塞**用户继续操作。

**兜底路径**——若创建会话时该主机仍未决策，弹出的对话框改为：

- 标题从"在这台主机上安装 Zellij"（讲实现）改为 **"为 10.0.3.12 启用持久会话？"**（讲收益）
- 两个按钮：`启用并安装` / `暂不启用，先开终端` —— 后者明确说明后果，而不是现在的"不安装（会话将非持久）"这种否定式默认
- 关闭对话框（Esc / 遮罩）等同于"暂不启用"，**只影响本次、不写库**，下次新建会话仍会问。当前实现已经是这样（`onClose → onSkip("not-authorized")` 只是本次会话的 `nonDurableReason`，写库的只有显式的 `deny()`）——保留这个取舍，但要让两条路径在措辞上可区分：显式按钮写「不启用（以后可在项目设置里改）」，把可逆性说出来，否则用户以为点下去就是永久的

#### 失败态与重试

**重试机制已经实现，本节只补它剩下的缺口。** 先明确现状，避免重复施工：

| 已具备 | 位置 |
| --- | --- |
| 后端两层退避自动重试（共 3 次尝试，退避 1.5s / 4s），只对瞬时故障生效 | `withInstallRetry` + `AUTO_RETRY_FAILURES` |
| 自动重试进度透传到 UI（"正在自动重试（第 n 次）…"） | `InstallServerMessage.stage.attempt` |
| 失败态的手动「重试」按钮，且按 `canRetryInstall(reason)` 条件渲染——架构不支持 / Job Object 不给按钮 | [ZellijInstallModal.tsx:196-200](../../packages/web/src/components/ZellijInstallModal.tsx) |
| 失败时告知"已自动重试 n 次仍失败，建议先检查网络或下载源" | `zellij.autoRetried` |
| 安装通道自身断开也归入可重试（`probe-failed` + `channelError`） | [ZellijInstallModal.tsx:110-117](../../packages/web/src/components/ZellijInstallModal.tsx) |
| 重试幂等：先关旧 WS，重置 stage / failure / attempt | [ZellijInstallModal.tsx:77-83](../../packages/web/src/components/ZellijInstallModal.tsx) |
| 「取消」只中断本次，不写入 `authorized: false` | [ZellijInstallModal.tsx:133-138](../../packages/web/src/components/ZellijInstallModal.tsx) |

这套分层是对的：机器该干的活（网络抖动）机器自己重试，人该做的决策（换个源、去装 curl）才交给人。下面四条是它仍然缺的。

**R1 · 失败态必须能改下载地址（最重要）**

`baseUrl` 输入框只存在于 `ask` 阶段（[ZellijInstallModal.tsx:153-160](../../packages/web/src/components/ZellijInstallModal.tsx)）。失败之后点「重试」，用的还是**同一个地址**——而后端刚刚已经用这个地址自动试过 3 次了。对 `download-failed`，这个按钮几乎注定再失败一轮。

`autoRetried` 的文案自己就承认了这点："建议先检查宿主机网络或**下载源**"——却没给改下载源的地方。内网用户的真实路径是「官方源不通 → 换内网镜像 → 再来」，现在他必须关掉弹窗、重新点新建终端、才能回到那个输入框。

失败态布局：

```
┌───────────────────────────────────────────────────────────┐
│ 未能在 10.0.3.12 上启用持久会话                            │
│                                                            │
│ ⚠ 下载 Zellij 失败                                         │
│   已自动重试 3 次仍失败，建议检查宿主机网络或换个下载源     │
│                                                            │
│ 下载地址                                                   │
│ [https://github.com/zellij-org/zellij/releases/…       ]   │
│                                                            │
│ ▸ 详细信息                       ← 折叠，展开是 detail 原文 │
│ ▸ 手动安装                       ← 折叠，展开是可复制命令   │
│                                                            │
│      [ 先不装（会话将非持久） ]         [ 重试 ]           │
└───────────────────────────────────────────────────────────┘
```

地址被改动过时，主按钮文案变为「用新地址重试」，并在 `startInstall()` 前先 `setHostAuthorization({ baseUrl })` 落库。只对 `download-failed` / `extract-failed` / `probe-failed` 显示这个输入框——`no-downloader`、`dir-not-writable`、`verify-failed` 换地址没有任何用，显示它只会误导。

**R2 · 手动安装指引要能复制**

`zellij.manualHint` 现在是一句无法执行的散文："也可手动将 Zellij {version} 二进制放到宿主机的 ~/.falcon/bin/"。这是内网 / 无出网远端用户唯一的出路，却要他自己去 releases 页面猜文件名、猜路径、猜要不要 `chmod`。

展开后给按远端实际 os/arch 拼好的命令 + 一个「复制」按钮：

```
mkdir -p ~/.falcon/bin
curl -L <按探测到的 target 拼好的完整 URL> | tar xz -C ~/.falcon/bin
mv ~/.falcon/bin/zellij ~/.falcon/bin/zellij-0.44.4
chmod +x ~/.falcon/bin/zellij-0.44.4
```

下方一行「装好后点这里重新检测」——`ensureZellij` 开头就是 `--version` 握手，已装好会直接返回（[install.ts:110-112](../../packages/server/src/zellij/install.ts)），所以「重新检测」复用同一条安装 WS 即可，**不需要新接口**。

**R3 · 拒绝之后没有回头路（死锁）**

在 `ask` 阶段点「不安装（会话将非持久）」会写入 `authorized: false`（[ZellijInstallModal.tsx:128-131](../../packages/web/src/components/ZellijInstallModal.tsx)）。而 `Sidebar` 判断是否需要安装的条件是：

```ts
authorized === null || (authorized === true && !installedVersion)
```

`authorized === false` 两个分支都不满足 → **弹窗再也不会出现，UI 上没有任何地方能反悔**。用户点错一次，这台主机就永远只能开非持久会话。

（对照：失败态或遮罩关闭走的是 `onSkip`，只影响本次、不写库，下次仍会问 —— 那条路径是对的。问题只出在显式的 `deny()` 上。）

补三个常态入口，任意一个都能解开这个死锁：

1. **项目行 `⋯` → 「持久会话设置…」**（SSH 项目）：主机级抽屉，显示授权状态、已装版本、下载地址、上次失败原因，带「安装 / 重新检测 / 撤销授权」。这是反悔的正规入口。
2. **`⚠ 非持久` 徽标可点**：侧栏会话行与总览里的徽标从纯提示变为可点击，点开直达上面的抽屉并定位到失败原因。看到问题的地方就是解决问题的地方。
3. **命令面板**：`>` 提供「为 {host} 启用持久会话」。

**R4 · 装好之后，已存在的会话不会变持久**

已经起来的非持久会话是裸 shell，外面没有 Zellij 包着，重试成功也救不回来。安装成功的提示必须说清楚，否则用户以为问题解决了，下次断线才发现工作没保住：

> 持久会话已在 10.0.3.12 上就绪。**现有的 2 个会话仍是非持久的**——新建的会话才会被 Zellij 接管。

这条 toast 上带一个「新建终端」按钮。

**R5 · 安装不该霸占整个界面（可选）**

当前安装是模态阻塞的，几十秒里用户什么都干不了。§7.4 主路径把安装挪到项目创建后的后台执行：进度显示在项目行（转圈）+ StatusBar，失败以 toast 呈现并带「查看详情」跳到主机抽屉。同一主机同时只允许一个安装在跑，第二个入口触发时**附着到进行中的那条通道**而不是重开一条。

优先级低于 R1–R4：它优化的是等待体验，而 R1–R4 修的是"用户被卡住出不去"。

### 7.5 断线与接回

| 阶段 | 现在 | 重设计 |
| --- | --- | --- |
| SSH 断线重连中 | 黄色 banner "第 N 次重连中…" | StatusBar 链路段变黄 + 计数；终端内容变 60% 不透明度（**输入被禁用这件事要看得见**）；≥3 次后升级为 banner |
| Unverified | banner + 「接回」 | banner 保留，同时**侧栏行内**也给「接回」（解决 S5） |
| 接回失败 | banner 里追加一行文字 | banner 内联错误 + 「重试」；错误详情可展开 |
| Dead | 红色 banner | banner 说明死因 + 两个出口：「用同样配置新建会话」/「清除记录」——现在是死路一条 |
| WS 断开 | "刷新页面重试" | 前端自动重连 WS（指数退避），只在多次失败后才建议刷新 |

### 7.6 破坏性操作确认（替换 `confirm`）

统一的 `ConfirmDialog`，规则：

- 标题是动作 + 对象："终止会话「migrate」？"
- 正文说明**具体后果**，不说套话："该会话中正在运行的命令会被杀掉。这个操作不可撤销。"
- 删除项目时**列出将被终止的会话**：
  ```
  删除项目「prod-api」？

  将同时终止 3 个会话：
    ⦿ deploy        活跃中
    ⦿ migrate       活跃 2 小时
    ⦿ log-tail      活跃 3 天

  远端 ~/.falcon/ 不会被清理。
  ```
- 主按钮是危险色且**默认焦点在取消上**；Enter = 取消，需要显式点击或 Tab 后 Enter 才能确认。
- Dead 会话的「清除」**不需要确认**——它只删一条记录，没有副作用。今天它和终止用了同一套心智，是过度确认。

---

## 8. 状态与文案系统

### 8.1 三态文案（替换现有 i18n 措辞）

| state | 标签 | 一句话解释（tooltip / banner） |
| --- | --- | --- |
| `active` | 运行中 | 后端确认这个会话活着。 |
| `unverified` | 待接回 | 链路断了，会话很可能还在跑，需要接回来确认。 |
| `dead` | 已丢失 | 确认已经结束了，这里只剩一条记录。 |

### 8.2 持久性文案

| 情形 | 标签 | 解释 |
| --- | --- | --- |
| durable | ⛨ 持久 | 断线或 falcon 重启后都能接回来。 |
| 非持久（有原因） | ⚠ 非持久 | 关掉网页没事，但链路断开或 falcon 重启后会丢。**原因：** {具体原因} |

`nonDurableHint` 当前文案"仅保证页面关闭不影响，不具备断线/重启存活能力"改为上表的说法：先说保住了什么，再说丢了什么。

### 8.3 错误呈现分级

| 级别 | 载体 | 用于 |
| --- | --- | --- |
| 瞬时 | Toast（4s，可带一个动作） | 操作失败：重命名失败、终止失败、创建失败 |
| 持续 | Banner（在 Stage 顶部） | 会话级异常状态：unverified / dead / attach 失败 |
| 阻塞 | Dialog | 需要用户抉择：授权、破坏性确认 |
| 环境 | StatusBar | 链路、持久性等常态信息 |

所有 `alert()` 调用点（`Sidebar.createAndOpen`、`Sidebar.deleteProject`、`SessionOverview.run`）改为 toast。

### 8.4 i18n 卫生

- 项目类型 `本地` / `SSH` 提取为 `project.typeLocalShort` / `project.typeSshShort`，不再硬编码。
- tab 关闭按钮的 `title` 换成新增 key `tab.closeHint`，删掉误用的 `common.cancel`。
- 新增 key 段：`tab.*`、`statusbar.*`、`palette.*`、`confirm.*`、`toast.*`、`empty.*`。

---

## 9. 视觉系统

深色为默认且唯一交付主题。所有颜色以 token 定义，浅色主题预留结构，v1 不实现。<br>
（2026-08-16 起浅色已实现，见下方变更注记。）

> **2026-08-15 变更**：token 的**命名与取值**已换成 shadcn/ui 的一套（`--background` / `--card` / `--popover` / `--primary` / `--muted` / `--destructive` / `--border` / `--ring`，base color: neutral，暗色由 `<html class="dark">` 固定），不再是下面 §9.1 的 `--surface-*` / `--text-*`。间距与圆角改用 Tailwind 的 scale。**下面 §9.1–9.4 保留为设计意图的记录，实际取值以 [`packages/web/src/styles.css`](../../packages/web/src/styles.css) 为准。** §9.5 的主机身份色算法未变，仍在 [`lib/hostColor.ts`](../../packages/web/src/lib/hostColor.ts)。

> **2026-08-16 变更**：浅色主题已实现（§15 中原列为"不做"）。深色仍是设计基准，但两套都交付：偏好为跟随系统 / 浅色 / 深色三档，存在 `falcon.theme`，落到 DOM 上就是 `<html>` 有没有 `.dark`。逻辑集中在 [`lib/theme.ts`](../../packages/web/src/lib/theme.ts)，入口在侧栏底部与设置页（另有命令面板项）。三处必须一起看：
>
> - **首帧**由 `index.html` 的内联脚本决定，否则深色用户会被白闪一下；它与 `lib/theme.ts` 必须用同一个 key、同一套判定。
> - **xterm 读不了 CSS 变量**，所以配色在 `TERM_THEMES` 里写死两份。浅色那份必须**整套**覆盖 ANSI 十六色（取 VS Code Light+ 的值）——xterm 默认的 white / brightWhite 接近纯白，在白底上直接消失。
> - **`--success` / `--warning` 两套取值差得远**：它们只作前景色用，深色下好看的 0.7 亮度黄绿放到白底上读不出来，浅色那套压到 0.52 / 0.53 才过 AA。

### 9.1 颜色（历史取值，已由 shadcn 主题取代）

```css
:root {
  /* ── 表面：四档层次，取代现在的两档 ── */
  --surface-0:  #0b0e14;   /* 应用底 / 终端底 */
  --surface-1:  #11151d;   /* 侧栏、tabbar、状态栏 */
  --surface-2:  #171c26;   /* 卡片、header、输入框底 */
  --surface-3:  #1f2632;   /* hover、选中、菜单 */
  --surface-overlay: #1a202b; /* modal / popover */

  /* ── 描边：三档 ── */
  --border-subtle:  #1e242e;  /* 分区线，几乎不可见 */
  --border-default: #2a3240;  /* 控件边框 */
  --border-strong:  #3d4757;  /* hover 边框、聚焦前置 */

  /* ── 文本：四档 ── */
  --text-primary:   #e4e9f0;
  --text-secondary: #9aa5b5;
  --text-tertiary:  #6b7688;
  --text-disabled:  #4a5364;
  --text-oncolor:   #ffffff;

  /* ── 强调 ── */
  --accent:         #3d8bfd;
  --accent-hover:   #5599ff;
  --accent-subtle:  #3d8bfd1f;   /* 12% 底 */

  /* ── 语义 ── */
  --success:        #45c163;   /* 运行中 */
  --success-subtle: #45c1631f;
  --warning:        #d9a441;   /* 待接回、非持久 */
  --warning-subtle: #d9a4411f;
  --danger:         #f0625c;   /* 已丢失、破坏性操作 */
  --danger-subtle:  #f0625c1f;

  /* ── 品牌点缀：仅用于 logo、空状态插图，禁止用于功能色 ── */
  --brand-mint:     #58d3a4;
}
```

对比度（WCAG AA，正文 4.5:1 / 大字与图形 3:1）：

| 组合 | 比值 | 判定 |
| --- | --- | --- |
| `--text-primary` on `--surface-0` | 14.8:1 | AAA |
| `--text-secondary` on `--surface-1` | 6.9:1 | AA |
| `--text-tertiary` on `--surface-1` | 3.9:1 | 仅可用于 ≥16px 或非正文（图标、装饰）|
| `--success` / `--warning` / `--danger` on `--surface-1` | ≥ 4.6:1 | AA |
| `--text-oncolor` on `--accent` | 4.6:1 | AA |

> `--text-tertiary` 不得用于正文，只用于分区标签、辅助元数据和图标。

### 9.2 排版

```css
--font-sans: -apple-system, "Segoe UI Variable Text", "Segoe UI",
             "PingFang SC", "Microsoft YaHei UI", system-ui, sans-serif;
--font-mono: "Cascadia Mono", "JetBrains Mono", "SF Mono", Consolas,
             "Noto Sans Mono CJK SC", monospace;

--text-xs:   11px;  /* badge、状态栏 */
--text-sm:   12px;  /* 标签、辅助文本 */
--text-base: 13px;  /* 侧栏、表格正文 —— 密度型界面的正文基准 */
--text-md:   14px;  /* 表单、对话框正文 */
--text-lg:   16px;  /* 对话框标题 */
--text-xl:   20px;  /* 页面标题 */

--leading-tight: 1.35;
--leading-normal: 1.55;
```

**等宽字体的使用规则**（新增，解决 S12）：文件路径、主机名、端口、版本号、shell 名、错误码一律 `--font-mono` + `--text-sm`。这些内容用户需要精确辨认字符，比例字体会让 `l/1/I`、`0/O` 混淆。

### 9.3 间距与圆角

```css
--space-1: 4px;   --space-2: 6px;   --space-3: 8px;
--space-4: 12px;  --space-5: 16px;  --space-6: 24px;  --space-7: 32px;

--radius-sm: 4px;   /* badge、状态记号、小图标按钮 */
--radius-md: 6px;   /* 按钮、输入框、行 */
--radius-lg: 10px;  /* 卡片、对话框、命令面板 */
```

行高规范：侧栏会话行 26px、项目行 30px、tab 34px、表格行 40px、按钮 28px（默认）/ 32px（对话框内）。

### 9.4 焦点、阴影、动效

```css
/* 双层 focus ring：内圈用底色隔开，保证在任何表面上都可见 */
--focus-ring: 0 0 0 2px var(--surface-0), 0 0 0 4px var(--accent);

--shadow-popover: 0 8px 24px -6px rgba(0,0,0,.55);
--shadow-modal:   0 24px 64px -12px rgba(0,0,0,.7);

--ease: cubic-bezier(.2,0,0,1);
--dur-fast: 90ms;    /* hover、按下 */
--dur-base: 150ms;   /* 展开、淡入 */
--dur-slow: 220ms;   /* 对话框、命令面板入场 */
```

规则：

- **所有**可聚焦元素使用 `:focus-visible { box-shadow: var(--focus-ring) }`。这是当前完全缺失的（S12）。
- 动效只用于位置和不透明度，不做尺寸弹跳。
- 唯一的循环动画是 unverified 的呼吸（2s ease-in-out 交替），因为它携带信息。
- `@media (prefers-reduced-motion: reduce)` 下所有 duration 归零，呼吸动画降级为静态。

### 9.5 主机身份色算法

```ts
/** 由 host:port:username 稳定映射到一个在深色底上可读的 hue */
export function hostHue(key: string): number {
  let h = 2166136261;                       // FNV-1a
  for (let i = 0; i < key.length; i++) {
    h ^= key.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  // 避开 100–150（绿，与 success 冲突）和 0–20（红，与 danger 冲突）
  const bands = [30, 50, 170, 190, 210, 230, 250, 270, 290, 310, 330];
  return bands[Math.abs(h) % bands.length];
}
// 使用：hsl(H 62% 58%) 作为色条；hsl(H 62% 58% / 8%) 作为 header 底
```

饱和度与亮度固定，保证任意 hue 下与 `--surface-*` 的对比度都达标，且不会与语义色混淆。

---

## 10. 组件规格

> **2026-08-15 变更**：本节原方案是"按需构建，全部无第三方 UI 库"。已改为 **Tailwind CSS v4 + shadcn/ui**，理由与代价见 §10.0。下面的规格表仍然有效——它描述的是**行为**，实现换成了 shadcn 的组件源码，差异逐条记在 §10.0。

### 10.0 采用 shadcn/ui（推翻原决策）

shadcn 不是运行时 UI 库，是把组件源码拷进仓库；真正引入的依赖是 Tailwind v4 与 Radix primitives。换来的是本节里最贵的那几项——焦点陷阱、还原焦点、菜单键盘遍历、`aria-*`、点外部关闭——不再由我们自己维护。

落地时的取舍：

- **配色改用 shadcn 默认暗色（base color: neutral）**，不再是 §9.1 的 `#0b0e14` 系。§9.1 的色板作为历史记录保留，实际取值以 `packages/web/src/styles.css` 的 `:root` / `.dark` 为准。`--success` / `--warning` 是 shadcn 没有、这个产品必须有的两个语义色，另行定义。
- **Esc 仍由 App 统一分发**。所有 Radix 浮层一律 `onEscapeKeyDown={(e) => e.preventDefault()}`，否则多层浮层时 Radix 和 App 会各关一层。
- **`lockOverlay` 保留**（`AppDialog`）：表单类对话框点遮罩不关闭，改为抖动。这是 Radix 没有、但 §10 明确要求的行为。
- **React 升到 19**。shadcn 现在的组件是按"ref 即 props"写的，没有 `forwardRef`；React 18 下 Radix 往这些包装组件传 ref 会静默失败（`Presence` 的进出场与卸载都挂在这条 ref 上）。
- Toast 换成 **sonner**，`store.toast()` 作为唯一入口保留——调用点只描述发生了什么。
- 命令面板用 **cmdk**，但 `shouldFilter={false}`：`>` / `@` / `#` 前缀是这个产品自己的语义，交给 cmdk 打分会把前缀当普通字符匹配。
- shadcn 组件里硬编码的 `Close` 文案已接到 i18n（`common.close`）。**每次 `shadcn add` 之后要重新检查这一条。**

### 10.1 Button

| variant | 默认 | hover | active | focus-visible | disabled |
| --- | --- | --- | --- | --- | --- |
| `primary` | `--accent` 底 / `--text-oncolor` | `--accent-hover` | 亮度 ×.94 | ring | 40% 不透明 + `not-allowed` |
| `secondary` | `--surface-3` 底 / `--border-default` | `--border-strong` | 底色 ×.96 | ring | 同上 |
| `ghost` | 透明 / 无边框 | `--surface-3` | 底色 ×.96 | ring | 同上 |
| `danger` | 透明 / `--danger` 字 | `--danger-subtle` 底 | — | ring | 同上 |
| `danger-solid` | `--danger` 底 | 亮度 ×1.08 | — | ring | 同上 |

尺寸：`sm` 24px / `md` 28px / `lg` 32px。图标按钮为正方形，`aria-label` **必填**（当前 ghost 图标按钮只有 `title`，屏幕阅读器读不到）。

### 10.2 StatusIndicator

替代 `StatusDot`。props：`state`、`size: sm|md`、`showLabel?: boolean`。

- 渲染 `<span role="img" aria-label="运行中">`，形状 + 颜色双编码；
- `showLabel` 时在右侧渲染文本标签（总览表格用）；
- unverified 带呼吸动画。

### 10.3 其余组件清单

| 组件 | 关键要求 |
| --- | --- |
| `Input` / `Select` | 尺寸统一 28px；`--font-mono` 变体给路径/主机；错误态红边 + 下方错误文本（关联 `aria-describedby`） |
| `Segmented` | 已有 `.seg`，补 focus ring 与 `role="radiogroup"` |
| `Badge` | 变体 neutral / success / warning / danger；`--radius-sm`；仅 11px |
| `Tooltip` | 400ms 延迟，Esc 关闭，跟随 focus 出现（键盘可达） |
| `Menu`（溢出菜单） | 键盘上下选择、Esc 关、危险项独立分组置底、点外部关闭 |
| `Dialog` | **修复 S10**：`role="dialog"` + `aria-modal` + 焦点陷阱 + Esc 关 + 关闭后还原焦点；**表单类对话框点击遮罩不关闭**（改为轻微抖动提示），非表单类可关 |
| `ConfirmDialog` | 见 §7.6，默认焦点在取消 |
| `Toast` | 右下角堆叠，最多 3 条，4s 自动消失，可带一个动作按钮，hover 暂停计时，`aria-live="polite"` |
| `CommandPalette` | 见 §11 |
| `EmptyState` | 图标 + 一句话说明 + 一个主操作，禁止只放一句"没有数据" |
| `Banner` | 变体 info / warn / error；右侧动作区；可关闭的与不可关闭的两种 |
| `Table` | 表头 sticky，行 hover，支持多选列 |

---

## 11. 键盘方案

### 11.1 硬约束：终端会吃掉按键

xterm.js 聚焦时几乎吞掉所有 `Ctrl+*` 组合键（`Ctrl+C`、`Ctrl+D`、`Ctrl+R`、`Ctrl+W` 全是 shell 语义）。全局快捷键**必须**避开它们，否则会破坏终端本身的可用性——这对目标用户是不可接受的。

安全区：

- macOS：`⌘` 系列（终端不用 Cmd 作控制键）
- Windows / Linux：`Ctrl+Shift+*`（终端惯例上留给复制粘贴等应用级操作）与 `Alt+数字`
- 两平台：`Alt+数字`

实现方式：`term.attachCustomKeyEventHandler()` 里判断是否命中全局快捷键表，命中则 `return false` 阻止 xterm 处理并派发到应用层。

### 11.2 快捷键表

| 动作 | macOS | Win / Linux |
| --- | --- | --- |
| 命令面板 | `⌘⇧P` / `F1` | `Ctrl+Shift+P` / `F1` |
| 转到文件 | `⌘P` | `Alt+P` |
| 新建终端（当前项目） | `⌘T` | `Ctrl+Shift+T` |
| 关闭当前 tab（Terminate） | `⌘W` | `Ctrl+Shift+W` |
| 下一个 / 上一个 tab | `⌘⇧]` / `⌘⇧[` | `Ctrl+Tab` / `Ctrl+Shift+Tab` |
| 切到第 N 个 tab | `⌘1..9` | `Alt+1..9` |
| 切到会话总览 | `⌘0` | `Alt+0` |
| 折叠 / 展开侧栏 | `⌘B` | `Ctrl+Shift+B` |
| 聚焦搜索 | `⌘F` | `Ctrl+Shift+F` |
| 接回当前会话 | `⌘R` | `Ctrl+Shift+R` |

`Esc` **永远归终端**（vim 用户），仅在对话框 / 命令面板 / 菜单打开时被上层拦截——此时终端必定已失焦。

**实现补充（2026-08-15）**：上表的键位里有一部分是浏览器的保留快捷键，页面 `preventDefault` 无效——`⌘T` / `⌘W`（开/关浏览器标签页）、`Ctrl+Shift+T` / `Ctrl+Shift+W`、`Ctrl+Tab`、`⌘1..9`。它们照常注册（在 Electron 之类的宿主里能用），同时每个命令额外给一个浏览器不碰的 Alt 别名，界面上显示的仍是上表的主键位：

| 命令 | Alt 别名 |
| --- | --- |
| 命令面板 | `⌘/Ctrl+K`、`F1` |
| 转到文件 | `Alt+P` |
| 新建终端 / 关闭 tab | `Alt+T` / `Alt+W` |
| 折叠侧栏 / 接回 | `Alt+B` / `Alt+R` |
| 下一个 / 上一个 tab | `Alt+]` / `Alt+[` |
| 第 N 个 tab / 总览 | `Alt+1..9` / `Alt+0` |

判定用 `event.code`（`Alt+字母` 在 mac 上会把 `event.key` 变成特殊字符），拿不到 `code` 的环境退回按 `event.key` 推。见 [shortcuts.ts](../../packages/web/src/lib/shortcuts.ts)。

### 11.3 命令面板

`⌘⇧P` / `Ctrl+Shift+P` 唤起（VS Code 同键；另有 `F1` 与旧的 `⌘K`），模糊匹配，分组结果：

```
┌────────────────────────────────────────────┐
│ >                                          │
├────────────────────────────────────────────┤
│ 跳转到会话                                  │
│   ⦿ dev server        falcon · 本机        │
│   ◐ migrate           prod-api · 10.0.3.12 │
│ 项目                                        │
│   ＋ 在 falcon 中新建终端                   │
│   ＋ 新建项目…                              │
│ 操作                                        │
│   接回「migrate」                           │
│   终止当前会话…                             │
│   设置访问密码…                             │
└────────────────────────────────────────────┘
```

前缀语法：`>` 命令、`@` 跳转会话、`#` 跳转项目。默认（无前缀）同时搜会话与命令。危险命令（终止、删除）在面板中带 `…` 后缀，选中后仍走 `ConfirmDialog`。

它同时是 S11 的解药：所有功能都能纯键盘走完。

---

## 12. 需要后端配合的改动

设计里有几处依赖当前 API 没有的数据。诚实列出，可按需裁剪：

| 需求 | 现状 | 建议 |
| --- | --- | --- |
| StatusBar 的 Viewer 数 | 后端持有 viewer 集合，但不下发 | `ServerMessage` 增加 `{ type: "viewers", count: number }`，viewer 增减时广播；**若不做，去掉 StatusBar 的 Viewer 段即可，不影响其余设计** |
| 会话列表实时性 | 前端 5s 轮询（[App.tsx:34](../../packages/web/src/components/App.tsx)） | 可选：加一条 `/ws/events` 推送会话增删与状态变更，去掉轮询。轮询在 UI 上的表现是状态点最多滞后 5s |
| 持久性来源展示 | `SessionWithProject` 无 Zellij 版本 | 可从 `SystemInfo` / `HostZellijStatus` 取，无需改协议 |
| 批量清除已丢失 | 只有单条 `DELETE /api/sessions/:id` | 前端循环调用即可，无需新接口 |
| 创建会话时的默认命名 | `name` 可选，由后端生成 | 保持现状；UI 允许创建后 inline 重命名 |

除 Viewer 数外，本设计不强依赖任何新接口。

---

## 13. 落地批次

按"先补安全与信任，再补导航效率，最后统一视觉"的顺序。每批可独立发布。

### 批次 1 · 身份与安全（最高优先）

1. `SessionHeader` + 主机身份色条 + SSH 会话的 header 着色 → 解决 S1
2. tab / 侧栏 / 总览的状态记号改为形状+颜色双编码 → 解决 S9
3. `ConfirmDialog` 替换全部 `confirm()`，删除项目时列出受影响会话 → 解决 S3/S4
4. `Toast` 替换全部 `alert()`
5. 项目行操作收入 `⋯` 菜单，删除项目降级并远离 `＋` → 解决 S3
6. tab 关闭 tooltip 与首次关 tab 提示 → 解决 S13 的语义错误
7. `tabbar` 常驻渲染 → 解决 S7
8. **主机抽屉「持久会话设置…」+ `⚠ 非持久` 徽标可点** → 解开 `authorized: false` 死锁（R3）。这是 bug 级问题，不该等到打磨批次

### 批次 2 · 导航与效率

9. **失败态内联下载地址 + 「用新地址重试」** → R1，内网用户唯一的出路
10. 工作台布局持久化（tabs / active / 侧栏宽度）→ 解决 S2
11. 会话总览提升为固定首个 tab，废除 `≡` 伪导航 → 解决 S6
12. 命令面板 + 快捷键表 → 解决 S11
13. 侧栏重构：全局摘要、行内接回、dead 降权、持久性正向标记 → 解决 S5
14. `StatusBar`
15. 会话名 inline 重命名，替换 `prompt()`

### 批次 3 · 系统化与打磨

16. 视觉 token 全量替换 `styles.css`，建立 spacing / typography scale
17. `:focus-visible` ring 全覆盖、disabled 样式、等宽字体规则 → 解决 S12
18. `Dialog` 无障碍改造（focus trap / Esc / 遮罩策略）→ 解决 S10
19. 会话总览升级为舰队视图（摘要卡、宿主机列、批量操作）
20. 可复制的手动安装指引 + 「重新检测」（R2）、安装成功后的"旧会话仍非持久"提示（R4）
21. 持久会话决策前置到 `ProjectForm`、安装后台化（R5）→ 解决 S8 的时机问题
22. 空状态与首次引导
23. i18n 卫生：硬编码提取、误用 key 修正、文案重写 → 解决 S13

---

## 14. 验收标准

可测的判据，每条对应一个诊断项：

1. 任意打开一个 SSH 会话，**不滚动、不 hover、不点击**，能读到：会话名、项目名、`user@host:port`、工作目录、持久与否。（S1）
2. 打开 3 个 tab → 刷新页面 → 3 个 tab 与聚焦项原样恢复。（S2）
3. 侧栏截图转灰度后，仍能区分运行中 / 待接回 / 已丢失。（S9）
4. 删除一个有 3 个会话的项目时，确认框列出这 3 个会话的名字。（S3/S4）
5. 界面上不存在任何 `alert` / `confirm` / `prompt` 调用。（S4）
6. 一个 unverified 会话，从侧栏可以在**一次点击内**触发接回。（S5）
7. 全程只用键盘：新建项目 → 新建终端 → 切换会话 → 接回 → 终止。（S11）
8. Tab 键遍历全部界面，每一个可聚焦元素都有可见的 focus ring。（S12）
9. 在终端里 `Ctrl+C` / `Ctrl+R` / `Ctrl+W` / `Esc` 行为与原生终端完全一致，没有被应用层截走。（§11.1）
10. Zellij 安装失败后，**不关闭对话框**就能改下载地址重试；主按钮文案随地址改动变为「用新地址重试」。（R1）
11. 失败态能复制到一段直接可执行的手动安装命令，URL 已按远端 os/arch 拼好。（R2）
12. 曾经点过「不安装」的主机，能纯从 UI 重新启用持久会话——不需要查文档，也不需要动数据库。（R3）
13. 安装成功后，用户被明确告知**已存在的会话仍是非持久的**。（R4）
14. 首次关闭 tab 时，用户被明确告知会话已被结束，以及 Shift 可以让它继续跑。（§7.3）
15. 无项目的新用户，在首屏就能读到"关掉网页也不会中断"这句承诺。（S1 的正向表达）

---

## 15. 明确不做

- **移动端 / 响应式**：本次范围为桌面宽屏。窄于 1024px 时侧栏自动折叠即可，不做移动端交互。
- ~~**浅色主题**：token 结构预留，v1 不实现配色。~~ → **2026-08-16 已实现**（跟随系统 / 浅色 / 深色三档，见 §9 变更注记）。
- **终端分屏 / 平铺布局**：Zellij 自己就能分屏，在 Web 层再做一套是重复建设，且会与 Zellij 的键位打架。
- **主题自定义 / 终端配色方案切换**：v2 议题。
- **多语言**：i18n 框架已就位，v1 仍只交付中文，但本次要修复硬编码使其可扩展。
