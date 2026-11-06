# 在 Tern 中访问 Falcon

Tern 插件展示 Falcon 的主机、项目目录、附属项目、多仓库成员与终端会话，可直接创建普通终端或 OMP 会话。点击创建按钮或已有会话，宿主机插件直接在发起操作的客户端打开并聚焦终端；配置本机数据目录后，本机持久会话直接连接 `falcon-sessiond`，其他会话通过 Falcon 的现有 REST API 与 WebSocket 接入。普通终端访问复用现有服务；OMP 原生图形界面需要包含本节实现的 Falcon 服务端与守护进程。

## 目录展示

分组复用 `falcon-core::project_tree::group_servers`，与 Falcon 侧栏保持一致：

- 第一层区分 Falcon 后端本机、已保存的远端主机、未绑定主机的存量 SSH 项目。
- 项目显示名称和完整工作目录。同名文件夹由完整路径与所属主机区分；路径在窄屏换行。
- 附属项目挂在源项目的“附属项目”分组下，显示分支及其自己的完整目录。这是项目关系，实际目录仍按显示的路径定位。
- 多仓库项目展开“成员仓库”，逐项显示成员目录。没有指定工作目录时标明使用多仓库虚拟目录，不把成员误画为同一物理目录的子文件夹。
- 会话挂在实际所属项目下，显示标题、状态和持久性。没有会话的项目保留；独立会话单列，已存档项目通过“显示存档”查看。
- 项目、成员仓库、附属项目及会话分组可折叠，折叠状态与存档开关在插件重载后保留。打开窗格时读取一次快照，之后点击“刷新”或按 `R` 更新列表；终端连接保持实时传输与自动重连。

## 构建与安装

在仓库根目录执行：

```sh
cargo run --manifest-path native/Cargo.toml -p xtask -- tern
```

也可以在 `native/` 执行 `cargo xtask tern`。产物为 `release/falcon-tern-v<版本>-<平台>`，Windows 带 `.exe` 后缀。连接程序只依赖 Falcon 的客户端和纯逻辑 crate，不依赖桌面界面的 GPUI。

把连接程序放到宿主机上的固定位置，例如 macOS/Linux 的 `~/.local/bin/falcon-tern`；随后安装插件：

```sh
tern plugin link /absolute/path/to/mojito/integrations/tern
```

`link` 从源码目录加载；需要独立副本时使用 `tern plugin install /absolute/path/to/mojito/integrations/tern`。插件包是整个 `integrations/tern/` 目录，包含 `plugin.toml`、三个 Luau 文件和样式表。

## 配置

配置位于 Tern **宿主机**。macOS 的插件数据目录为 `~/Library/Application Support/Tern/plugin-data/falcon/`；Linux 为 `${XDG_STATE_HOME:-$HOME/.local/state}/tern/plugin-data/falcon/`，Windows 为 `%LOCALAPPDATA%\Tern\plugin-data\falcon\`。设置了 `TERN_CONFIG_DIR` 时，数据目录为该目录下的 `plugin-data/falcon/`。

在插件数据目录创建 `config.json`，`bridge` 与 `config` 必须是绝对路径：

```json
{
  "bridge": "/absolute/path/to/falcon-tern",
  "config": "/absolute/path/to/bridge.json",
  "tern": "/Applications/Tern.app/Contents/MacOS/tern"
}
```

`tern` 指定宿主机上的 Tern 命令行程序；未配置时使用 `PATH` 中的 `tern`（Windows 为 `tern.exe`）。移动端点击会话时由宿主机运行此程序，将来源窗格映射到客户端窗口，并在该窗口的项目树右侧分栏打开、聚焦终端。

`bridge.json` 指定 Falcon 服务地址。地址应使用实际安装的端口；以下 `4923` 为默认端口：

```json
{
  "url": "http://127.0.0.1:4923",
  "passwordFile": "/absolute/private/falcon-password"
}
```

`passwordFile` 的内容为访问密码，只去掉文件末尾的换行，不去掉密码中的空格。macOS 也可以引用 Falcon 桌面客户端已经保存的密码，无需复制密码：

```json
{
  "url": "http://127.0.0.1:4923",
  "desktopPasswords": "/Users/your-name/Library/Application Support/com.falcon.Falcon/passwords.json",
  "profile": "local"
}
```

`profile` 默认 `local`；同时提供时 `passwordFile` 优先。凭据文件的相对路径相对于 `bridge.json` 所在目录解析。未设置密码的回环服务可以省略凭据字段。URL 只允许 HTTP(S)，不可包含用户名、密码、查询参数或片段。

凭据保留在宿主机，快照和手机链接均不包含密码。密码文件应只允许当前用户读写，例如 Unix 上设置 `chmod 600`。连接程序复用 Falcon 客户端的自动登录机制，后端重启导致登录失效时自动重新登录。

本机持久会话可在 `bridge.json` 中增加 `"sessiondRoot": "/absolute/path/to/falcon-data"`，路径必须是当前 Falcon 的实际 `--data-dir`。例如服务使用 `--data-dir /Users/your-name/.mojito`，这里就填该目录。该选项要求 Falcon URL 为回环地址；连接程序先请求服务准备会话，核对会话元数据中的完整 ID，再读取 `sessiond/binary.json` 中由服务端握手验证的部署路径并调用 `attach`。不需要手动创建 `bin/falcon-sessiond`。它省去 WebSocket 转发和服务端的 16 毫秒输出合并窗口；SSH 会话和非持久会话仍使用原连接方式。路径相对于配置文件解析。更新连接程序或修改这项配置后，已打开的接入窗口需用 `Ctrl+\` 断开并重新接入，底层会话继续运行。

配置后选择 Tern 命令面板的 **Open Falcon projects**，或 **New Falcon Projects block**。修改配置后点击刷新；修改插件代码后执行 `tern plugin reload falcon`。

## 终端操作

点击会话行或标题后，连接程序在项目树所在的 Tern 宿主机运行，终端在该项目树右侧分栏打开并自动取得焦点。左侧保留 Falcon Projects 入口，不为终端新增标签；用户自行设置的项目树标签名保持不变。同一客户端窗口内，相同连接配置与会话 ID 的终端只打开一份，再次点击会直接聚焦已有视图，插件重载后也会复用；此前在独立标签中的终端会移回项目树右侧。Falcon 的 SSH 项目仍通过 Falcon 后端接入，无需在手机重新填写 SSH 凭据。

每个未存档项目的“会话”分组，以及主机下的“独立会话”分组，都提供一个 **新建会话** 下拉菜单：

- **普通终端**：在对应项目的工作目录启动普通 shell；独立会话使用该主机的登录目录。
- **OMP**：在对应宿主机启动 OMP，仅在检测到该主机安装了 `omp` 后显示。SSH 项目检测远端，不使用后端本机的检测结果。OMP 原生图形界面的扩展前提见下文。

选择菜单项后创建会话，成功后立即更新列表并接入右侧分栏；创建期间入口显示“正在创建…”并禁用，避免重复提交。创建失败显示实际错误；若创建成功但接入失败，会话仍保留在列表中，可点击重新接入。刷新项目树时会重新检测 OMP；检测按主机合并，异步进行，不阻塞普通终端的创建。已存档或所属项目已删除的条目不提供创建入口。

- 本机直连复用守护进程的输入、回放与尺寸处理；WebSocket 会话在重连时回放历史画面。
- `Ctrl+\` 断开视图；关闭接入终端同样保留 Falcon 会话。
- WebSocket 未连接时不缓存键盘输入，避免重连后把旧输入交给状态已变化的程序。
- 会话已结束、登录失败或快照获取失败时显示实际错误。快照失败时停止显示旧项目树，可点击重试。
- WebSocket 链路的宿主机密码询问使用隐藏输入；`Ctrl+C` 取消询问。

命令行可独立检查连接：

```sh
falcon-tern --config /absolute/path/to/bridge.json snapshot
falcon-tern --config /absolute/path/to/bridge.json attach SESSION_ID
falcon-tern --config /absolute/path/to/bridge.json agents PROJECT_ID
falcon-tern --config /absolute/path/to/bridge.json create PROJECT_ID
falcon-tern --config /absolute/path/to/bridge.json create PROJECT_ID --agent omp
```

也可通过 `FALCON_TERN_CONFIG` 指定配置文件。

创建与检测命令的 `PROJECT_ID` 也接受 `standalone:local` 或 `standalone:host:<主机 ID>`，用于独立会话。创建命令向标准输出返回会话 JSON，不自动附着；插件随后调用 `attach` 接入。

## OMP 原生图形界面

持久会话会保留 Tern Surface Protocol（TSP，Tern 图形界面协议）的文档、样式、图片数据与未完成消息；断开接回和 VT（虚拟终端）热升级时重新构造图形界面。普通终端能力查询仍由守护进程统一回答；TSP hello 后的设备属性回包按顺序等待协商完成，避免后续查询提前结束原生检测。

在 macOS/Linux 的 Tern 中接入 OMP 时，Falcon 通过宿主机上的私有 Unix 套接字请求 OMP 扩展执行显示停止与重新启动。此操作不发送终端按键或进程信号，保留同一 OMP 进程、对话和已输入的草稿。`Ctrl+\` 断开后重新探测，恢复该进程的字符界面。按单端操作使用；图形界面期间，Falcon 的字符终端不提供同步图形显示。

首次接入会将内置 `integrations/omp/falcon-tern.ts` 安装到 OMP 的 `extensions/` 目录。**启动时未加载该扩展的 OMP 必须重新启动并恢复会话一次；`/reload` 不会导入新扩展。** 首次重启的草稿恢复取决于 OMP 自身的持久化行为。也可在启动时显式指定 `omp --extension /absolute/path/to/integrations/omp/falcon-tern.ts`；使用 `--no-extensions` 时仍需显式指定该扩展。

已确认的 OMP 启动参数中的 `--profile` 优先于 `OMP_PROFILE`、`PI_PROFILE`。默认 profile 使用连接程序环境的 `PI_CODING_AGENT_DIR`，否则使用 `~/.omp/agent`；命名 profile 使用 `~/.omp/profiles/<name>/agent`，`PI_CONFIG_DIR` 可修改 `.omp` 配置根目录。连接程序环境与目标 shell 自行修改的环境不同时，应显式加载扩展；不能从 shell/script wrapper 的参数可靠确定目标 profile。扩展与凭据均留在会话宿主机，手机无需安装扩展。

Windows 的 OMP 显示重探尚不支持；非持久会话不提供守护进程的原生图形快照。移动端是否支持具体图形控件取决于其 Tern 客户端版本。

## 移动端与网页客户端

1. 在运行 Falcon 连接程序的机器上安装并配置上述 Tern 插件。
2. 用 Tern 的远程主机连接或移动端配对入口连接到这台 **Tern 宿主机**，打开其 Falcon Projects 窗格。
3. 展开主机、项目与会话，直接点击会话行或标题，即可进入终端。

会话操作由宿主机插件处理，不依赖窗口端 Luau 或自定义链接路由。手机无需复制插件目录、安装 Rust 程序或保存 Falcon 密码；电脑窗口关闭后，宿主机守护进程仍可处理其他客户端的操作。远程访问的网络可达性与身份验证由 Tern 配置。

Tern 的客户端能力见[插件架构](https://docs.stencil.so/tern/concepts/architecture.md)、[支持矩阵](https://docs.stencil.so/tern/concepts/support-matrix.md)及[插件目录说明](https://docs.stencil.so/tern/concepts/packages.md)。

## 验证范围

本次实现已在 macOS 的实际 Tern 窗口验证目录树、390 像素窄屏的路径换行、折叠状态重载、点击接入、中文输入与关闭视图后的会话存活。另用插件目录为空的客户端验证点击会话行和标题后，在来源窗口打开并聚焦终端；关闭电脑端窗口后，客户端仍能进入会话并执行命令。真实 Tern 网页客户端已在 390 像素窄屏下验证从宿主机命令创建项目树、点击会话直接进入，以及终端自动聚焦和历史回放。隔离的真实 Falcon 服务验证了跨读取边界的 UTF-8 输入、尺寸更新、断开后的终端模式恢复，以及带密码的后端重启后的自动登录和会话接回。Windows 与 iOS 真机交互尚未验证；窄屏验证使用桌面窗口和浏览器，不等同于 iOS 真机验证。

本机直连优化使用同一个隔离的真实持久会话测量 40 次逐键回显：原 WebSocket 路径中位数 19.23 毫秒、95 分位数 19.98 毫秒；直连中位数 0.34 毫秒、95 分位数 0.48 毫秒。这是连接程序到会话的往返耗时，不包含手机网络与屏幕显示耗时。已验证中文输入、断开后的终端模式恢复和会话存活，并用回归测试阻止完整 ID 不同但短 ID 相同的目录被错误接入。

OMP 原生界面已在 macOS 的真实 Tern 0.9.12 与 OMP 18.16.0 中验证：显示完整原生输入框与模型选择栏，输入中文和表情草稿，以 `Ctrl+\` 断开后恢复字符界面，重启隔离的 Falcon 服务，再由正式连接程序通过 `sessiond/binary.json` 发现部署路径并接回图形界面。还验证了原生界面运行期间的 VT 热升级，升级后仍可继续编辑。上述切换期间 OMP 的 PID 保持不变，草稿内容保留。Windows 与 iOS 的原生 OMP 界面未验证。

WebSocket 路径也已用实际 Tern 验证原生输入框和中文、表情输入。连接程序过滤 Tern 带末尾省略参数的设备属性回包，并在原生模式保留主屏，使 inline 图形界面可见；来宾的全屏程序仍由其自身的 VT 输出控制。

终端分栏与复用已在 macOS 的实际 Tern 窗口验证：切换到竖向侧栏后，只保留项目树入口；连续点击同一会话五次，窗格 ID 与标签数量保持不变。另已验证含空格和单引号的配置路径、插件重载后的复用、关闭视图后的重新接入、旧独立标签迁回分栏、自定义标签名保留，以及通过接入终端执行命令并读取实际输出。

创建入口已在 macOS 实际 Tern 窗口验证：从空项目创建普通终端并执行命令；创建真实 OMP 18.16.0 会话并输入中文、表情草稿；从主机分组创建独立终端。上述操作均自动聚焦右侧终端，左侧标签数量保持不变。命令行无效参数与不存在的项目均返回错误且未增加会话。

下拉菜单已在实际 Tern 窗口验证展开和两项创建操作：普通终端能执行命令，OMP 原生界面能输入中文；创建后左侧标签数量保持不变。
