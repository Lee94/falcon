# 本地 coding agent CLI 检测

- 新建会话的 CLI 清单由 `falcon-proto::SessionAgent` 定义；启动命令名与 wire 标识一致，新增工具同时补 `falcon-ui/locales/zh-CN.json` 的 `agent.<标识>` 文案。
- `falcon-server/src/sessions/agent_detect.rs` 检查后端本机 PATH 内的可执行文件，使用 `local::resolve_base_env` 重新读取交互登录 shell 环境，30 秒缓存并合并并发探测；不运行 CLI、不自动安装。
- `GET /api/agents` 返回 `Vec<SessionAgent>`；工作区登录后立即请求并每 30 秒刷新。侧栏、画布、项目菜单与命令面板使用 `Workspace::session_agents`，本地与 SSH 均只显示目标机器的检测结果；SSH 请求带 `hostId` / `projectId`，在远端交互登录 shell 内检查命令，禁止退回完整清单。
- 已加入 Antigravity CLI（`agy`）与 Oh My Pi（`omp`），沿用普通 CLI 的启动与退出后回到 shell 的行为。
- macOS 上使用 Codex 时，若会话守护进程记录 shell 退出码 130，可检查前台程序是否因中断信号（SIGINT）退出。交互式 `zsh -c` 此时可能跳过后续命令。POSIX 启动脚本在调用 CLI 前设置 `trap : INT`，使 CLI 可以接收中断，退出后仍返回登录 shell。回归用例为 `interrupted_codex_returns_to_interactive_zsh`。
- ACP 助手配置独立于会话开场 CLI；不能仅因命令存在就假定支持 ACP。
- 验证：`cargo check --workspace --tests`、`cargo test --workspace`；检测回归测试在 `sessions::agent_detect`，覆盖执行权限、符号链接、目录、卸载、去重与 Windows PATHEXT。
