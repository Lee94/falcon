# 环境与线程

环境是已有宿主机上的执行目录，可绑定单个或多个工作目录（普通目录与 Git 仓库均可混选）。界面不提供环境上下文输入，多文件夹的说明由用户自己写进成员目录的 `AGENTS.md`；终端与 Claude Code 都使用宿主机默认的登录 shell。`GET /api/environments/branches` 在目录所属节点查询 Git 分支，普通目录返回 `null`。`POST /api/environments` 在该节点验证工作目录：单目录环境支持选择基础分支，在节点数据目录 `environments/<环境 ID>/` 创建独立检出，源仓库保持原分支，环境删除后保留检出及其文件；多目录环境按 `repos` 校验并去重，不支持统一基础分支，删除前若尚未创建线程允许编辑目录列表。

`POST /api/projects/{环境 ID}/threads` 创建一个线程。新线程继承环境的工作目录与多目录成员列表，通过 `thread_environment_id` 关联环境；多目录线程在宿主机按线程生成虚拟项目目录（`AGENTS.md`、`CLAUDE.md` 与 `.claude/settings.json`）作为终端和 Claude 对话的工作目录，删除线程时清理该虚拟目录而不触碰成员目录。宿主机保存线程的 Claude 对话历史，桌面端打开对话窗口时加载历史并恢复 Claude Code 会话。两类创建接口支持 `Idempotency-Key`。环境下还有线程时，服务端拒绝删除该环境或更换其工作目录。

环境和线程复用项目库存的标识、文件接口和会话引擎，网关按所属节点重写标识。桌面和 Android 共用 `falcon-core::project_tree` 的分组。旧的多仓库环境和已有工作树线程仍可访问，旧工作树沿用原有删除护栏。

桌面侧栏以线程为第一层。默认视图平铺全部线程，默认按创建时间倒序排列（`project_tree::filter_and_sort_threads` / `project_tree::thread_list`）。列表参照 Claude 桌面版与 Orca：按时间排序时按本地日历日分为「今天 / 昨天 / 过去 7 天 / 过去 30 天 / 更早」，状态优先时按需关注 / 运行中 / 活跃中 / 闲置分段，按名称排序不分段（`project_tree::thread_sections`）。线程行分两行：第一行是行首状态记号（整条线程里最要紧的一个：在等你、出错、待接回、运行中、未读、开着、无）、名称与时间（按创建时间排序时显示创建时间，其余显示最近活动），悬停时时间换成 ＋ 与「…」；第二行淡色显示所属环境（远端线程附带机器名）、改动行数与可展开的会话数。完整的机器与工作目录写在悬停提示中。侧栏只提供「新建线程」一个新建入口，下方提供线程列表过滤（关键词、状态、机器、环境及已存档开关）与排序（最近创建、最近活跃、状态优先、名称 A–Z / Z–A、最早创建）；排序偏好随工作区持久化（`threadSort`），过滤条件保留在当前窗口内存中。底栏按钮切换到按机器分组的兼容视图，在其中编辑或删除环境、在机器上打开独立终端以及访问旧项目。

新建线程由 ⌘P（Windows 与 Linux 为 Alt+P）、侧栏顶部的「新建线程」或命令面板打开，界面为输入框式样：上方填写第一条消息，下方依次选择机器、环境和第一个动作，回车开始。「转到文件」相应改为 ⌘O（Windows 与 Linux 为 Ctrl+Shift+O）。机器和环境的规则如下：该机器上没有环境时，在对话框上叠开环境表单新建，创建成功后自动选中新环境；节点离线或连接中的主机不可选。

第一个动作可选 Claude 对话、普通终端或该机器上检测到的 Agent CLI，不选模型。它只决定线程开场时在画布上打开什么，之后可在侧栏线程行的 ＋ 中继续打开其他 Agent 或终端。线程名称取第一条消息的第一行（`project_tree::thread_name_from_prompt`），消息为空时使用默认名称。第一条消息的去向：

- Claude 对话：对话连上后自动发出。对话已有历史时只填入输入框。
- 能在交互模式开场时接收初始提示词的 CLI（`SessionAgent::accepts_prompt`：Claude Code、Codex、Grok、Devin，依据为 2026-11-27 核对的各 CLI 帮助信息）：`POST /api/projects/{id}/sessions` 的 `prompt` 字段经环境变量 `FALCON_AGENT_PROMPT` 交给启动脚本，脚本以 `-- <消息>` 作为单个参数传给 CLI，并在启动 CLI 前删除该变量。消息只在新建会话时传递，接回与原地重开不再传递，服务端不保存消息。Windows 宿主机的启动脚本不传递消息，原因是 cmd 展开变量后会再次解析特殊字符。
- 普通终端与其他 CLI：不接收消息，对话框提示这段文字只用作线程名称。

Claude 对话不再是独立的系统窗口，而是线程画布上的一种窗口（pane key `c:<项目 ID>`，`ActiveView::Chat`）。每个线程最多一扇，只在侧栏选中该线程时显示，与终端一样可拖动、最大化和关闭，并随工作区持久化。关闭窗口会断开连接，宿主机回收 Claude Code 进程，线程历史保留，下次打开时接回。线程删除后对话窗口随之移除；网关上的线程在节点离线期间保留窗口。

对话窗口按轮画：右侧的提示词气泡、过程（文字与工具步骤卡片）、最终回复。工具卡片的标题由宿主机按工具名从参数里提炼（Bash 的 description 与命令、文件路径、搜索词……，`api/claude.rs::tool_call`），工具结果截成开头 12 行随 `toolResult` 事件推来，连同宿主机量的耗时一起写进线程历史，接回的对话与当场看着跑完的一样；一轮的总耗时取 Claude Code 的 `result.duration_ms`。跑着的一轮全展开并计时，结束后过程收进「工作了 …」，没有最终回复的一轮默认展开。发送钮在跑着时变成停止，发 `interrupt` 控制请求打断这一轮。事件只增不改：旧历史只有工具名，照样画成卡片；客户端跳过认不出的事件，节点比客户端新时不断开对话。整理事件流的纯逻辑在 `falcon-core::claude_transcript`。

输入框照 Claude 桌面版的式样，除语音输入外的几项都接在 Claude Code 的 stream-json 控制协议上（字段形状按 2.1 实测）：

- **附件**（「＋」、拖入、粘贴文件或截图）：客户端读进内存，发送时经 `upload` 按 512K 字符分段传 base64（WS 单帧与网关都限 1 MiB），`prompt` 按 id 引用。宿主机把图片（png / jpeg / gif / webp，≤ 5 MB）交成图片块、PDF（≤ 20 MB）交成文档块、UTF-8 文本（≤ 512 KB）解码后包上文件名成文字块。附件只有文件名进线程历史；提示词交不出去（附件没传完、类型不对）时宿主机回一个出错的 `done` 让这一轮收尾，不进历史。
- **权限模式**（盾牌）：`set_permission_mode`。Claude Code 启动时带 `--allow-dangerously-skip-permissions`，否则切不到「跳过权限确认」；它只是让这一项可选，默认不生效。Claude Code 自己换模式（批准计划后退出计划模式）经 `system/status` 的 `permissionMode` 跟上。
- **模型与推理强度**：模型列表与每个模型支持的 effort 档位取自 `initialize` 的 `models`；换模型走 `set_model`（空串 = 回到默认），effort 没有专门的控制请求，走会话级的 `apply_flag_settings {effortLevel}`，不写用户的 settings.json；实际生效的模型与 effort 由 `get_settings` 的 `applied` 报回。别名指向自定义模型时显示名就是原始 id，界面写成「别名 · 原始 id」。
- **上下文用量**（圆环）：Ready 后、每轮结束后、换模型后各发一次 `get_context_usage`，推 `context { used, max }`。

线程上选过的模型、effort、权限模式存在宿主机的 `claude-threads/<id>.options.json`，接回时 Ready 之后用控制请求恢复（而不是启动参数：存着的模型下线了只是一条 `warning`，不至于线程起不来）；「跳过权限确认」不存，重新打开回到询问。设置被拒绝推 `warning`，界面在输入框上方提示。输入框底下一行写这条线程在哪：机器（本机 / 主机名）、环境名与主工作树 / 工作树、分支，只读。

局部验证在 `native/` 运行 `cargo test -p falcon-server --lib thread_`，协议样本通过 `cargo xtask fixtures --threads-only` 在隔离节点生成。
