# Agent 的完整 GUI 交互

状态：2026-11-12 扩展 Android 的 GUI / TUI 双向转换。图形模式完全使用 GUI；只读时间线与原终端并排的实现不能满足这一要求。

## 问题

此前 Claude Code 的“图形”模式只读取固定 UUID 对应的 JSONL 对话记录。桌面在上方绘制消息，下方仍保留约 38% 高度的原终端，输入、权限审批和模型设置继续依赖官方交互界面。对话文件不包含完整的实时控制协议，不能通过给镜像加输入框就取得当前审批的请求标识或模型控制能力。

Claude Code 2.1.322 的公开 `--input-format stream-json` 与 `--output-format stream-json` 只适用于 `--print`。没有已核实的公开接口，可以把正在运行的官方交互式进程原地变为第三方双向结构化通道。`--resume` 只续接对话，不保留原进程及其活动轮次。

## 决定

1. **图形模式完整接管交互。** 使用现有 `AgentDriver` 的结构化双向通道。GUI 显示 Markdown、思考、工具结果、文件差异和计划；原生输入框负责发送，工具卡片负责审批，按钮负责中断，模型及思考程度菜单使用 CLI 返回的真实能力目录。图形模式不渲染原终端网格，也不提示用户到终端操作。
2. **默认启动仍遵循 ADR 0013。** 新建 Agent 会话默认运行官方交互式 CLI。已有结构化会话直接使用完整 GUI；其“紧凑 / 图形”仅为本地排版，不改变输入协议或 CLI 生命周期，运行中也可切换。
3. **首次进入 GUI 是明确的运行模式转换。** Claude 官方终端与旧 `terminalBacked` 镜像点击进入 GUI 时，应用先说明将退出当前 CLI 并续接同一段对话，未发送的终端草稿不会转入图形输入框。用户在应用内确认后才请求转换。不能把这一转换描述为同进程切换。
4. **转换不终止整个会话。** 服务端必须取得真实的空闲程序状态、前台 Claude 进程与固定 UUID 历史证据；首次提示词前，只有完整扫描能证明该 UUID 尚无记录且前台使用相同的 `--session-id` 时，才允许直接新建同 UUID 的图形对话。执行、等待审批、状态未知或证据不足时拒绝转换。转换期间禁止其它输入与并发转换。仅结束核实过的 CLI，等待原 Backend 的前台返回已知 shell，再在同一 PTY 内启动带 `--resume` 的结构化脚本，保留原 shell 与独立于 CLI 的后台任务。超时、前台变化或续接失败必须明确报错，不能把 JSON 注入未知进程。握手期间向全部 Viewer 发布不可交互状态，确认控制通道可用并解除转换锁后，再发布空闲快照；桌面与 Android 在未知状态下均禁用输入和控制，避免其它窗口提交后丢失提示词。
5. **新旧协议明确区分。** 保留 `POST /api/sessions/:id/agent-mode` 的旧 `gui` / `terminal` 镜像语义，避免旧客户端意外触发转换。完整 GUI 使用 `gui-control`，返回官方 TUI 使用 `terminal-control`；每次转换前分别检查 GET 能力的 `interactiveGui:true` 或 `interactiveTerminal:true`，能力不足时提示更新服务端且不发送 POST。原 `preservesProcess:true` 只描述旧镜像语义，不代表新的转换保留 CLI 进程。
6. **旧镜像不再冒充 GUI。** 新桌面收到 `terminalBacked:true` 时仍显示完整原终端；开启完整 GUI 必须通过上述控制模式转换。旧客户端仍可沿用既有镜像协议。
7. **模型与思考程度以 CLI 确认为准。** Claude 使用 `initialize`、`set_model`、`apply_flag_settings` 与 `get_settings` 校准实际设置；Codex 使用 `model/list` 目录并在 `turn/start` 提交选择。运行及等待审批期间禁用参数修改。`effortLevel:null` 清除覆盖，不用 token 预算代替思考程度，不按模型名猜测能力。只返回旧 `models` 名称列表的服务端仍可选模型，并明确显示未提供思考程度。
8. **不支持的控制请求必须可见且有应答。** 不把历史工具条目当成当前审批。结构化 CLI 请求的控制类型尚未支持时，明确提示并返回失败，避免移除终端后永久等待；不能自动批准未知请求。`AskUserQuestion` 暂不提供专用答案表单，会明确拒绝，并提示改用普通消息提问，用户仍在原生输入框回复。
9. **Android 支持返回官方 TUI。** 标题栏的 `TUI → GUI` / `GUI → TUI` 是真实运行协议转换，与 GUI 内的“紧凑 / 图形”排版按钮分开。返回 TUI 同样先确认，再由服务端核实驱动空闲、固定对话历史与进程身份，只退出已核实的结构化 Claude 进程，经退出握手返回原 shell，再在同一 PTY 中启动官方 CLI 并续接同一对话。启动脚本必须提供可校验的 CLI 进程证据；旧脚本证据不足时明确拒绝。首次提示词前且完整扫描证明历史为空时使用同 UUID 的 `--session-id`，其它情况严格使用 `--resume`，失败不自动新建对话。转换期间全部 Viewer 保持不可交互，待官方 CLI 前台与空闲状态核实后才发布终端快照并开放输入；不结束 holder、PTY、原 shell 或独立后台任务。官方续接失败且再次核实已返回原 shell 时，恢复可操作终端并显示失败原因；无法核实前台的超时仍保持未知状态。

## 显示与滚动

桌面保留原有紧凑时间线：连续的思考、已完成工具、计划与普通通知默认归入可展开的“执行过程”。用户输入与正文展开；运行中的工具、失败、警告和待审批操作直接显示。工具输出先预览 12 行，完整内容可展开，文件差异不静默截断。

滚动视口限制高度，内层内容保持自然高度。接近底部时跟随新输出；上滚查看历史后保留当前位置。输入与流式 Markdown 使用各自的子视图，避免输入光标闪烁重排整条时间线。

Android 的结构化视图使用原生输入框与审批、参数控件，首次进入默认使用图形排版；解析 `terminalBacked`，旧只读镜像继续显示真实终端。界面模式以 WebSocket 快照为准，HTTP 成功与目标模式的就绪状态均已到达后才解除本次转换锁；迟到的 HTTP 响应不覆盖其它 Viewer 的新状态。服务端通过 `agentModeSnapshot:true` 承诺每次连接都发送权威模式快照，包括默认 TUI、等待尺寸、转换中与重启后的未知 GUI；Android 在首次同步完成前禁用输入，旧服务端保留原连接兼容行为，已有 GUI 重连仍须等待快照。成功后 5 秒仍缺目标状态时只重连重新同步，不通过超时放开旧模式。转换期间拦截终端输入与结构化控制请求，保留尺寸和外观上报。切换失败展示服务端给出的原因；GUI 草稿按会话保留，切回 GUI 可继续编辑，仅在提示词成功进入当前 WebSocket 发送队列后清空，确认框说明草稿不会自动转入另一种输入界面。

## 能力边界

首次从官方交互终端转换目前以 Claude Code 为范围。只有服务端可以核实目标进程与历史的宿主机才允许转换；不满足条件时保留原终端并说明原因。Codex 已有结构化进程可使用 GUI，但不假定本机 `codex --remote` 能直接接管任意已运行的官方终端。

转换绑定 Falcon 会话最初的固定 UUID。CLI 内通过 `/clear`、`/resume` 改换对话后，启动 argv 可能仍保留原 UUID；当前没有可靠的 runtime 对话身份接口，因此不会自动跟随新的对话。首次转换确认明确说明续接的是绑定对话，不能声称任意 CLI 内切换后的当前对话都会保留。

结构化进程的内存控制状态目前不能在服务端重启后仅从 VT 快照重建。继续明确拒绝向未知 PTY 注入控制协议，不把历史恢复或 `--resume` 宣称为活动轮次、待审批及进程状态恢复。

## 验证依据

测试使用独立数据目录与开发端口，不向模型发送真实请求，也不转换用户在用的会话。

- `cargo test -p falcon-server --lib sessions::manager::tests::agent_mode --offline --locked -- --test-threads=4`：最终 15 passed、0 failed。真 PTY 用例覆盖首次提示词前进入 GUI、同 UUID 续接和历史保留、完整输入/审批/模型/思考/中断、原 Backend/shell 与独立后台继续运行、执行/审批/未知状态拒绝、启动失败与超时、转换锁和旧代回调。两个 Viewer 均先收到不可交互状态，再在解锁后收到空闲快照。未连接真实 SSH 宿主机或请求模型。
- `cargo test -p falcon-client --test rest --offline --locked`：19 passed，覆盖新能力协商、显式 `gui-control` 请求、旧服务端与仅镜像服务端拒绝转换。
- 最终 `cargo check --workspace --tests --offline --locked` 通过；`cargo test --workspace --offline --locked -- --test-threads=4` 合计 1420 passed、0 failed、4 ignored。服务端与桌面 `snapshot` 产物构建通过；Android `:app:compileDebugKotlin --offline --no-daemon` 在最终输入保护修改后再次通过（使用本机已安装的 JDK 21）。项目源码无编译警告，依赖 `block v0.1.6` 仍有既有的 Rust 未来兼容性提示。
- 最终界面产物通过 `127.0.0.1:4964` 隔离模拟服务与 Metal 回读截图验证：取消不发请求，确认后只发一次 `gui-control`；实际按键输入 `hello`，原生审批，切换模型与 `max` 思考程度，中断下一轮，往返紧凑/图形排版。始终只有一条会话 WebSocket，GUI 阶段未发送任何原始终端输入；截图中没有原终端区域。测试模型与能力仅为协议替身，不代表真实模型目录。额外将控制握手延迟 2 秒，期间尝试提交的 `x` 未发送；独立窗口直接处于未知状态时也未发送提示词或原始终端输入。
- `FALCON_FIXTURE_OUT=<隔离目录> cargo run -p xtask --offline --locked -- fixtures` 未能完成：既有生成器仍调用 `GET /api/projects/:id/git/commit?sha=zz`，现行路由仅支持 POST，实际得到 405、生成器期望 400。新增 GUI 能力与请求由独立路由、客户端和真 PTY 回归覆盖。
- 2026-11-12 服务端双向转换局部回归：`sessions::manager` 为 54 passed、3 ignored，`sessions::agent` 为 61 passed，`api::sessions::tests::agent_mode_route` 为 1 passed；不同用例合计 116 passed、0 failed、3 ignored。真 PTY 覆盖两轮 GUI/TUI 往返、首轮图形对话产生历史后从 `--session-id` 进程返回 `--resume` 官方终端、同 UUID 历史、原 shell 的 pid/变量/工作目录与独立后台存活、双 Viewer 和新 Viewer 在延迟启动期间禁用输入、前台/原 shell 身份变化、运行/审批/未知状态/坏历史拒绝、忽略 TERM 的超时，以及官方续接失败后实际输入验证原 shell 恢复。3 项忽略用例需要显式配置隔离 SSH 环境；本次仅使用 CLI 替身，没有请求真实模型或转换用户在用的会话。
- 2026-11-12 Android 双向转换：`:app:compileDebugKotlin --offline --no-daemon`、`:app:testDebugUnitTest --no-daemon` 与 `:app:assembleDebug --offline --no-daemon` 通过，安装包为 `android/app/build/outputs/apk/debug/app-debug.apk`。新增 26 项本机单元测试覆盖两种转换请求、各自能力检测、仅镜像/缺失路由/能力类型错误时不发 POST、服务端错误保留，以及结构化 GUI、旧镜像、退出快照的模式判定；另覆盖 HTTP 与 WebSocket 的两种到达顺序、未知状态、重连前禁用旧模式、其它 Viewer 改变模式、请求响应丢失后强制重新同步与旧服务端兼容。当前本机没有连接 Android 设备，也未安装模拟器，未执行手机上的触摸、软键盘和真实 Claude 对话转换。
