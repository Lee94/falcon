# Native Shell 界面迁移与截图核对

## 已迁移的主要结构

- 桌面采用 6px 岛间距、12px 岛圆角、独立服务端身份栏及右侧活动栏；单画布隐藏画布切换栏。
- 侧栏复用 `falcon_core::project_tree::group_servers`，按宿主机、项目、检出、会话分组。Detach 只断开 Viewer 并移除布局，接回重建 Viewer；Terminate 保留忙会话确认。
- 工作区复用 `falcon_core::workspace` 的 GPUI 持久化形状，按 profile id 保存列、尺寸、折叠、选中项目和面板。自定义服务端不能覆盖本机 profile 的 URL；恢复后聚焦会话须同步当前画布 id。
- 文件、Git 和声明式插件使用实际服务端数据；在线初始状态不展示离线 demo。插件首开加载、按声明间隔刷新，并只合并无动作的在途渲染请求。
- 设置页采用左右分栏。主题双槽位、应用内置图标、终端偏好、通知、主机、中转、插件和关于页已接入相应数据或操作。终端偏好同步到画布字体、光标与网格尺寸。

## 局部验证

NSH 0.5.1 的依赖要求 Rust 1.97。本机 Homebrew Rust 1.96 不满足；已安装独立 rustup 1.97.0，未更改全局默认版本。运行 Cargo 时从 `native/` 进入，避免忽略 `.cargo/config.toml` 的源配置而重复下载及编译依赖。若 Cargo 由 rustup 启动但 PATH 仍先找到 Homebrew rustc，需显式指定 `RUSTC`。

```sh
cd native
RUSTC="$HOME/.rustup/toolchains/1.97.0-aarch64-apple-darwin/bin/rustc" rustup run 1.97.0 cargo test -p falcon-nsh --lib --locked
RUSTC="$HOME/.rustup/toolchains/1.97.0-aarch64-apple-darwin/bin/rustc" rustup run 1.97.0 cargo check -p falcon-app --tests --locked
```

本次测试覆盖桌面导航及侧栏调整、Detach/接回与终止确认、移动与 TUI 布局、主题底色判定、图标、插件签名防回滚，以及终端高吞吐基准。未执行全工作区检查或各端安装包构建。

## 截图方式

- 原 GPUI 参考版本为提交 `1614baa`（0.3.28），应在独立 worktree 编译，不能恢复到当前工作区覆盖迁移代码。
- 当前 macOS 提供可选 `snapshot` feature，正常发布构建不启用。AppKit 缓存视图为 PNG，锁屏时仍可截图。环境变量：`FALCON_SNAPSHOT` 为目标路径，`FALCON_SNAPSHOT_DELAY_MS` 默认为 8000，`FALCON_SNAPSHOT_QUIT=1` 截完退出。
- `FALCON_SNAPSHOT_SCENE` 可选 `terminal`、`overview`、`files`、`changes`、`sysmon`、`docker`、`tokens`、`appearance`、`terminal-settings`、`notifications`、`hosts`、`relays`、`plugins`、`about`、`palette`、`profiles`、`project`、`file`、`diff`。`FALCON_SNAPSHOT_PROJECT` 按项目名选择场景。
- 场景驱动会聚焦会话并发送清行、清屏控制字符，只能对隔离的审查会话使用。服务端应另选端口及数据目录；偏好由 `FALCON_NATIVE_DATA_DIR` 隔离。使用 `--server` 指向测试服务端，勿修改 `FALCON_LOCAL_URL`，以免触及日用服务。
- GPUI 的连接管理是独立窗口，须截该窗口，不能拿服务端主窗口当作连接页。锁屏时 GPUI 插件会因为窗口不可见而暂停更新，临时自动化驱动需确保可见性判断及手动画帧，不应将此修改带进正式渲染代码。
- 本次本地截图、并排比较与检查说明保存在被忽略的 `target/nsh-gpui-review/`。CPU、内存和 Agent 用量是动态数据，不计算像素一致率。GPUI 主窗口截图为 2880×1802，NSH 为 2880×1800，展示时归一到逻辑尺寸；原图同时保留。

## 当前覆盖与仍有限制

- 主机表单支持新增、编辑、删除和 Tailscale 设备预填；Tailscale 仍使用普通 SSH 认证，不会自动配置凭据。
- 项目表单支持本地/SSH 项目、多仓库及 worktree 创建、编辑和派生，并保护映射中的文件、保存和 Git 操作。工作目录与 shell 命令仍是手动文本输入，没有目录选择器或 shell 自动发现。
- 文件面板已接入新建目录、重命名、删除、上传和下载，并保护打开的文件及在途操作。文件对话框和传输能力依平台后端而异；不支持的平台应显示不可用反馈。
- 会话菜单支持重命名非 dead 会话、清除 dead 会话记录；清除记录不会终止进程。中转页支持公网发布与端口转发；Web/WASM 上明确显示不可用并禁用操作。项目映射写入及 worktree 操作在不支持的目标上也会返回显式 unsupported 状态。
- 外观设置包括应用图标选择/上传、终端偏好和窗口背景模式；背景效果按平台能力降级，不应把内容透明度当作窗口透明/模糊实现。Git 已有统一/并排差异视图和冲突操作，但高级批量流程仍需逐项对照 GPUI。
- 截图对照目前只覆盖 macOS；其他桌面、移动和 Web 后端仍需实机验证，不能据此宣称所有平台完全一致。
