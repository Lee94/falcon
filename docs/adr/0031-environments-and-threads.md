# 环境与线程

环境是已有宿主机上的执行目录，可绑定单个或多个工作目录（普通目录与 Git 仓库均可混选）。界面不提供环境上下文输入，多文件夹的说明由用户自己写进成员目录的 `AGENTS.md`；终端与 Claude Code 都使用宿主机默认的登录 shell。`GET /api/environments/branches` 在目录所属节点查询 Git 分支，普通目录返回 `null`。`POST /api/environments` 在该节点验证工作目录：单目录环境支持选择基础分支，在节点数据目录 `environments/<环境 ID>/` 创建独立检出，源仓库保持原分支，环境删除后保留检出及其文件；多目录环境按 `repos` 校验并去重，不支持统一基础分支，删除前若尚未创建线程允许编辑目录列表。

`POST /api/projects/{环境 ID}/threads` 创建一个线程。新线程继承环境的工作目录与多目录成员列表，通过 `thread_environment_id` 关联环境；多目录线程在宿主机按线程生成虚拟项目目录（`AGENTS.md`、`CLAUDE.md` 与 `.claude/settings.json`）作为终端和 Claude 对话的工作目录，删除线程时清理该虚拟目录而不触碰成员目录。宿主机保存线程的 Claude 对话历史，桌面端打开对话窗口时加载历史并恢复 Claude Code 会话。两类创建接口支持 `Idempotency-Key`。环境下还有线程时，服务端拒绝删除该环境或更换其工作目录。

环境和线程复用项目库存的标识、文件接口和会话引擎，网关按所属节点重写标识。桌面和 Android 共用 `falcon-core::project_tree` 的分组。旧的多仓库环境和已有工作树线程仍可访问，旧工作树沿用原有删除护栏。

桌面侧栏以线程为第一层。默认视图平铺全部线程，默认按创建时间倒序排列（`project_tree::filter_and_sort_threads` / `project_tree::thread_list`），行尾只显示所属环境名称，机器写在悬停提示中。侧栏只提供「新建线程」一个新建入口，下方提供线程列表过滤（关键词、状态、机器、环境及已存档开关）与排序（最近创建、最近活跃、状态优先、名称 A–Z / Z–A、最早创建）；排序偏好随工作区持久化（`threadSort`），过滤条件保留在当前窗口内存中。底栏按钮切换到按机器分组的兼容视图，在其中编辑或删除环境、在机器上打开独立终端以及访问旧项目。

新建线程由 ⌘P（Windows 与 Linux 为 Alt+P）、侧栏顶部的「新建线程」或命令面板打开，界面为输入框式样：上方填写第一条消息，下方依次选择机器、环境和第一个动作，回车开始。「转到文件」相应改为 ⌘O（Windows 与 Linux 为 Ctrl+Shift+O）。机器和环境的规则如下：该机器上没有环境时，在对话框上叠开环境表单新建，创建成功后自动选中新环境；节点离线或连接中的主机不可选。

第一个动作可选 Claude 对话、普通终端或该机器上检测到的 Agent CLI，不选模型。它只决定线程开场时在画布上打开什么，之后可在侧栏线程行的 ＋ 中继续打开其他 Agent 或终端。线程名称取第一条消息的第一行（`project_tree::thread_name_from_prompt`），消息为空时使用默认名称。第一条消息的去向：

- Claude 对话：对话连上后自动发出。对话已有历史时只填入输入框。
- 能在交互模式开场时接收初始提示词的 CLI（`SessionAgent::accepts_prompt`：Claude Code、Codex、Grok、Devin，依据为 2026-11-27 核对的各 CLI 帮助信息）：`POST /api/projects/{id}/sessions` 的 `prompt` 字段经环境变量 `FALCON_AGENT_PROMPT` 交给启动脚本，脚本以 `-- <消息>` 作为单个参数传给 CLI，并在启动 CLI 前删除该变量。消息只在新建会话时传递，接回与原地重开不再传递，服务端不保存消息。Windows 宿主机的启动脚本不传递消息，原因是 cmd 展开变量后会再次解析特殊字符。
- 普通终端与其他 CLI：不接收消息，对话框提示这段文字只用作线程名称。

Claude 对话不再是独立的系统窗口，而是线程画布上的一种窗口（pane key `c:<项目 ID>`，`ActiveView::Chat`）。每个线程最多一扇，只在侧栏选中该线程时显示，与终端一样可拖动、最大化和关闭，并随工作区持久化。关闭窗口会断开连接，宿主机回收 Claude Code 进程，线程历史保留，下次打开时接回。线程删除后对话窗口随之移除；网关上的线程在节点离线期间保留窗口。

局部验证在 `native/` 运行 `cargo test -p falcon-server --lib thread_`，协议样本通过 `cargo xtask fixtures --threads-only` 在隔离节点生成。
