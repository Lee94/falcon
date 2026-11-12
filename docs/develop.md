# 开发

仓库里只有 Rust。`native/` 是一个 Cargo workspace：服务端 `falcon-server`，界面 `falcon-ui`，原生入口 `falcon-desktop`。构建、打包与资源生成都是 `cargo xtask`（别名在 `native/.cargo/config.toml`；不在 `native/` 下就 `cargo run --manifest-path native/Cargo.toml -p xtask -- <命令>`）。

给代理与贡献者的模块约定、测试命令与架构说明在仓库根的 [CLAUDE.md](../CLAUDE.md)。

## Amp orb 环境

`.agents/setup` 为 x86_64 Debian orb 安装 Rust stable、会话守护进程的交叉编译工具、Linux 原生库与资源生成工具，以及 Android 的 Java 17、SDK 35、build-tools 34.0.0 和 platform-tools。Rust 额外目标只预装本机的 linux-musl 与两个 Android 目标；其余发行目标按需安装，发版 CI 仍负责完整的五目标工具链。它按 Cargo 锁文件预取依赖，经 `cargo xtask sessiond` 准备本机守护进程（Zig / Ghostty 的下载与校验仍由 xtask 管），并预编译 Android 的 Kotlin 与单元测试代码以缓存 Gradle 依赖，不打安装包。

冷启动先补下载工具与 Java，再并行准备 Rust / 原生库与 Android，等两组都成功后才退出。NDK 的冷下载太大，可能拖过 Amp 的 20 分钟 setup 时限，故不自动安装；需要 `cargo xtask android` 打 JNI / APK 时，先用 `sdkmanager 'ndk;27.2.12479018'` 按需补齐。Kotlin 编译与 JVM 单元测试不需要 NDK。

工具链路径与 Android 环境写入 orb 的登录 shell 配置，后续 Amp 命令与 supervised services 都能使用。`.agents/resume` 只做快速工具检查，不联网安装、不启动服务。无需数据库服务或额外密钥；SQLite 由 Rust 依赖内嵌。开发服务端仍须显式使用隔离的端口与数据目录，不能把 `~/.falcon` 当测试数据。

setup 可重复运行，已有系统包、Rust 目标、SDK 组件与 xtask / Gradle 缓存会复用；日志输出各步骤耗时。Amp 只有在这些脚本进入项目的默认分支后，才会用它们刷新并复用新 orb 的环境快照。macOS / Windows 安装包仍需相应平台，orb 不安装 Android 模拟器。

### 国内网络

setup 的 Debian 包使用中科大镜像，保留 Debian 签名校验，仅对本次 apt 调用生效，不覆盖机器已有的源。Rust 工具链与 rustup 安装器使用 rsproxy；Cargo 继续复用 `native/.cargo/config.toml` 的 rsproxy 配置。cargo-zigbuild 优先取官方预编译包，下载超时或失败时，改用 rsproxy 编译同一锁定版本；哈希校验失败则直接报错，不降级绕过校验。

Android 命令行工具与 sdkmanager 的仓库基址使用 Google 官方国内域名 `https://googledownloads.cn/android/repository/`。Gradle wrapper 改从华为云镜像下载同一 8.9 版本，并按 Gradle 官方公布的 SHA-256 校验；Android 的插件与 Maven 依赖优先从阿里云镜像取，缺包时仍保留官方仓库兜底。这两项 Gradle 仓库配置也用于普通 Android 开发与 CI，不只用于 setup。

`RUSTUP_DIST_SERVER`、`RUSTUP_UPDATE_ROOT`、`SDK_TEST_BASE_URL` 会写入 orb 登录环境，已有显式配置优先；覆盖 SDK 基址时须保留末尾 `/`。Zig 仍从 ziglang.org 下载，Ghostty 源码直连 GitHub 官方 codeload，二者继续校验仓库钉死的哈希；没有引入第三方 GitHub 代理。如果这些官方域名也被阻断，仍需 orb 的网络提供可用的 HTTPS 出口。下载器增加了连接超时、低速检测与重试，但不能保证任意国内线路都能完成全新环境安装。

## 结构

```
native/
├── crates/
│   ├── falcon-proto/      协议类型（服务端与客户端共用）
│   ├── falcon-server/     服务端：axum + portable-pty + russh + rusqlite
│   ├── falcon-client/     REST / WebSocket
│   ├── falcon-term/       终端：alacritty_terminal + 按键 / 鼠标 / 滚轮
│   ├── falcon-theme/      主题（Ghostty 主题、界面色派生）
│   ├── falcon-core/       与界面框架无关的纯逻辑
│   ├── falcon-platform/   平台能力接口
│   ├── falcon-ui/         界面
│   └── falcon-desktop/    原生客户端入口与平台实现
├── sessiond/              会话守护进程（独立工作区）
├── xtask/                 构建 / 打包 / 资源生成
└── assets/fonts/          内嵌字体
```

会话核心跑在服务端一条专用线程上。持久会话的设计见 [docs/design/session-daemon.md](./design/session-daemon.md)。

## 常用命令

在 `native/` 下：

```bash
cargo xtask --help
cargo run -p falcon-server
cargo run -p falcon-desktop
cargo check --workspace --tests
cargo test --workspace
```

`cargo xtask` 的发布与打包命令见 [deploy.md](./deploy.md)。其余命令：

| 命令 | 作用 |
| --- | --- |
| `sessiond [--targets …] [--check\|--test]` | 编 falcon-sessiond。开发构建的服务端从 `native/target-sessiond` 取二进制 |
| `ghostty [--targets …]` | 按锁定版本用 Zig 编 libghostty-vt 静态库 |
| `ghostty-bindgen` | 升 pin 时重新生成绑定 |
| `icons` | 生成内置应用图标 |
| `vendor-fonts` / `vendor-themes` | 重新生成已入库的字体与主题 |
| `fixtures` / `compare-fixtures` | 从真服务端落盘协议响应，或比较两套响应的形状 |

改了守护进程要重跑 `cargo xtask sessiond`。远端用的目标加 `--targets all`。同版本号不会因为文件名相同而跳过推送：文件名按内容哈希，新编的会被当成新文件。

## 升级 libghostty-vt

改 `native/sessiond/ghostty.pin` 的 commit 与源码包 sha256，然后：

```bash
cd native
cargo xtask ghostty --targets all
cargo xtask ghostty-bindgen
cargo xtask sessiond --check --targets all && cargo xtask sessiond --test
```

`ghostty` 会先打上 `native/sessiond/ghostty-patches/` 里的补丁。改补丁时要同步 pin 文件里的补丁记录。`ghostty-bindgen` 的 diff 就是这次的 ABI 变化。`sessiond --test` 含真 PTY 用例和接回比对语料。

宿主机上已有的会话不受影响：新版 falcon 接回时把它们的 VT 进程热换成新版本。
