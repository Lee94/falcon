# Falcon

Web 端持久化终端工作台：以项目为单位管理终端会话，会话不因网页关闭而终止；支持本地文件夹项目与远程 SSH 项目。

## Language

### 项目

**Project（项目）**:
终端会话的归属单位，统一概念，分 `local` 与 `ssh` 两种类型；都拥有名称与工作目录（终端的初始 cwd）。
_Avoid_: Workspace、Folder、Connection

**Local Project（本地项目）**:
以后端所在机器上的一个文件夹路径定义的 Project。

**SSH Host（远端主机）**:
预先保存的 SSH 连接配置（显示名称、主机、端口、用户名、凭据）。在设置里增删改，新建 SSH 项目时从列表里选一台，不用每次重填。项目行上的 ssh_* 是创建时从主机**复制**来的，不是解引用——改主机时再把五列刷回引用它的项目。存量项目可以没有绑定的远端主机，编辑时仍用手写字段。
_Avoid_: Connection、Server、Remote（太宽）、Host（单独说会和 Zellij 的「宿主机」撞名；带「远端」或写 SSH Host）

**SSH Project（SSH 项目）**:
以远程主机连接配置（主机、端口、凭据）定义的 Project；可选指定远端工作目录，不指定则使用登录默认目录。连接配置通常来自一台已保存的 SSH Host。

**Port Forward（端口转发）**:
挂在 SSH 项目上的 TCP 隧道，走该项目的 SshLink，与终端会话独立。本地转发（ssh -L）在 falcon 后端监听、打到远端能到达的地址；远端转发（ssh -R）在远端监听、打回后端能到达的地址。规则持久化，启用中的隧道在链路断开后随 SSH 一起重连。
_Avoid_: Tunnel（太宽）、Proxy / SOCKS（v1 不做动态转发）

**Docker Panel（Docker 面板）**:
挂在当前焦点项目上的宿主机 Docker 管理：容器、镜像、Compose。命令在项目的宿主机上执行（local = 后端机器，ssh = 远端经现有 SSH 链路），不是后端本机的 Docker（除非当前就是本地项目）。Compose 只在项目工作目录及往下两层子目录里发现 compose.yaml / docker-compose.yml。
_Avoid_: 本机 Docker（SSH 项目不是）、集群 / Swarm / Kubernetes

**Worktree Project（附属项目）**:
从一个 Project 派生出来的 Project，工作目录是源项目所在 git 仓库的一棵 worktree，检出另一条分支。与源项目共用同一台宿主机与同一份连接配置（local / ssh 跟着源项目走），但拥有独立的名称、独立的会话、独立的生命周期。目录由 falcon 在**仓库根**的同级创建（`<仓库基名>-<分支 slug>`），也只有 falcon 自己建的目录才会在删除时被清理。只能从普通 Project 派生一层。多仓库容器批量派生出的附属项目同时也是多仓库项目（multi 与 worktree 同时存在），删除按成员清单逐棵清理。
_Avoid_: 子项目（暗示嵌套，实际是磁盘上的同级）、分支项目（附属项目不等于分支，分支可以换）、Clone / 副本（只有一份仓库，什么都没有复制）

**Source Project（源项目）**:
被派生的那个 Project。派生的基准点是它工作目录所在的**仓库根**（`git rev-parse --show-toplevel`），不是工作目录本身——工作目录可能是仓库里的某个子目录。删除源项目会级联删除它的全部附属项目，但源项目自己的目录永远不删：那不是 falcon 建的。
_Avoid_: 父项目（"父/子"暗示所有权与嵌套，这里只有派生关系）、主项目、Origin（与 git remote 撞名）

**Derive（派生）**:
从源项目创建附属项目的动作：算出同级平铺目录 → `git worktree add` → 落一条 Project 记录。与 Terminate / Detach 一样是边界明确的动词：派生只新增一棵工作树，绝不改动源项目的工作区，也不改动它当前检出的分支。
_Avoid_: 克隆、复制、新建分支（派生也可以检出已有分支）

**Multi-repo Project（多仓库项目）**:
同时以 N 个成员仓库路径定义的 Project（`multi != null`），成员全部位于项目自己的宿主机上（local / ssh 都可以）。本身不是仓库：working_dir 只是可选的会话初始 cwd。与「附属」正交——multi 与 worktree 同时存在就是批量派生出的多仓库附属项目。
_Avoid_: Monorepo（那是一个仓库）、项目组 / Group（暗示管理层级，这是一个项目）、Workspace（已在 Project 的 Avoid 里）

**Member Repo（成员仓库）**:
多仓库项目里的一个 git 文件夹路径；可以指到仓库子目录，派生时才 rev-parse 出仓库根。容器的成员是用户的真仓库，永远不会被 falcon 删除；派生产物的成员是各棵 worktree（连同所属仓库根一起记录，供删除护栏比对）。
_Avoid_: 子仓库（与 submodule 撞名）、子项目（成员不是 Project）

**Batch Derive（批量派生）**:
对容器全部成员按统一分支名各建一棵 worktree、集中放进一个新目录、产出**一个**多仓库附属项目的动作。**全有或全无**：任一成员失败即回滚已建好的 worktree，不留半成品。auto 模式 = 分支存在则检出、不存在则从各自 HEAD 新建。
_Avoid_: 批量克隆（什么都没复制）、同步（不动远端）

**Central Dir（集中目录）**:
批量派生新建的 `<容器名slug>-<分支slug>/`，N 棵 worktree 以各自仓库根 basename 平铺其中；就是派生产物的 working_dir。删除时它是唯一的包围盒——只有严格位于其内部的成员才会被清理，收尾只做空目录删除。
_Avoid_: 工作区目录（与 working_dir 混）、根目录

### 会话

**Terminal Session（终端会话）**:
一个由后端持有的运行中的 shell（PTY）。归属于某个 Project，一个 Project 可同时拥有多个。生命周期独立于任何浏览器页面。
_Avoid_: Tab（Tab 是前端视图，不是会话本身）、Terminal（指渲染组件时才用）

**Scrollback（历史输出）**:
会话在无人观看期间产生的输出记录，重新连接时先回放再接实时流。

**Detach（断开视图）**:
视图与会话断开、会话继续运行：离开或关闭页面，以及 Shift+关闭 Tab。会话仍留在会话列表里。
_Avoid_: 关闭（语义不明确）

**Terminate（终止）**:
显式销毁会话及其底层 shell / tmux 的动作，与 Detach 严格区分。**手动关闭 Tab 就是 Terminate**——Tab 即会话，用户收起它就是不要它了；离开页面则永远不是。
_Avoid_: 关闭、删除

**Viewer（观看端）**:
连接到某个会话的一个浏览器视图。一个会话可同时有多个 Viewer，输入均生效，终端尺寸以最后一次 resize 为准。

**Image Paste（图片粘贴）**:
把浏览器里的图片（截图粘贴或拖入文件）写到会话宿主机的 `<falcon 根>/paste/`，再把落盘路径粘进终端输入。终端字节流只有文本，图片本身永远不进 PTY——Claude Code 等 TUI 认的是"输入框里的图片路径"（拖文件进原生终端是同一个机制）。剪贴板同时有文本与位图时贴文本（Excel / 网页复制的常态），纯图片才走上传；旧图 24 小时后在下次粘贴时顺手清理。
_Avoid_: 上传图片（用户视角是粘贴，上传只是实现）、发送图片（不是聊天消息）

### 外观

**Theme（主题）**:
一套配色：底色、字色、光标色、选区色加 ANSI 16 色，格式与 Ghostty 主题文件完全一致（`background` / `foreground` / `cursor-color` / `cursor-text` / `selection-*` / `palette = N=#rrggbb`）。生效范围是**整个应用**：界面的语义色、语法高亮、终端画面都从同一套主题派生，没有"界面主题"与"终端主题"之分。内置目录是 Falcon 两套默认加 Ghostty 全部内置主题，名字与 `ghostty +list-themes` 一致；也可以贴一段 Ghostty 主题文本当自定义主题。
_Avoid_: 配色方案 / 终端配色（暗示只管终端）、皮肤

**Theme Slot（主题槽位）**:
浅色槽位与深色槽位各放一套主题，对应 Ghostty 的 `theme = light:X,dark:Y`。槽位里放什么样的主题不设限——给浅色槽位选一套深底主题也行。
_Avoid_: 浅色主题 / 深色主题（作为槽位名会和"主题本身是深是浅"混）

**Appearance Mode（明暗模式）**:
跟随系统 / 浅色 / 深色三档偏好，决定此刻用哪个槽位。与主题本身的深浅是两回事：主题的深浅按底色亮度判，决定 `.dark`、原生控件的 color-scheme 与给 PTY 的 COLORFGBG。
_Avoid_: 主题（这是切槽位，不是切主题）、暗黑模式

### 会话状态

**Active（活跃）**:
后端持有 PTY / 连接、确认存活的会话状态。有无 Viewer 在看不影响此状态。

**Unverified（待接回）**:
链路中断（SSH 断连或后端重启）后，持久会话理论上仍存活但尚未验证的状态。
_Avoid_: 断开（与 Detach 混淆）

**Dead（已丢失）**:
确认死亡的会话（shell 退出、tmux session 不存在、或非持久会话遭遇后端重启）。保留在列表中标注原因，由用户手动清除。

**Reattach（接回）**:
对 Unverified 会话重新建立链路、验证存活并恢复为 Active 的动作。后端自动接回（启动恢复、SSH 断线、本地 PTY 掉了都会自己试）；用户也可以手动再点一次。
_Avoid_: 懒惰验证（曾经是打开 tab 才接，已经改成自动）

### 存活性

**Page-close Survival（页面级存活）**:
所有会话的硬保证：关闭/断开浏览器页面绝不终止会话。

**Durable Session（持久会话）**:
在"后端与远端断连"或"后端重启"后仍能存活并被重新接回的会话。SSH 项目通过远端 tmux 包装实现；本地项目仅 macOS/Linux 支持，Windows 本地会话不具备（v1）。不具备持久能力的会话需在 UI 上明确标注。
_Avoid_: 持久化（易与数据存储混淆）
