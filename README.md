# Falcon

Falcon 是按**机器 → 环境 → 线程**组织工作的持久化终端工作台。接入本机或通过 SSH（安全外壳协议）连接远端主机后，在机器节点新建环境，选择该机上的一个或多个 Git 仓库。

从环境新建线程，填写线程名称和任务说明。所属主机上的 Falcon 将每个仓库的当前提交检出到独立工作树，并在 `<数据目录>/threads/<线程 ID>/` 下生成 `AGENTS.md`、引用它的 `CLAUDE.md` 和 `projects/`。选择线程可打开终端或查看文件。关闭终端、断开连接或修改环境后，已有线程仍保留原目录和上下文。

桌面侧栏底部的「旧项目与独立终端」入口可显示原有项目和会话。旧项目的右键菜单提供「以此项目创建环境」。

界面是原生客户端，服务端负责会话、文件与中转。术语见 [CONTEXT.md](./CONTEXT.md)，架构决策见 [docs/adr/](./docs/adr/)。

## 能力

| 场景 | 保证 |
| --- | --- |
| 关掉客户端或断开连接 | 会话继续运行，再次打开时回放历史输出 |
| SSH 链路断开 | 持久会话里的命令继续运行，自动指数退避重连并接回 |
| 后端进程重启 | 持久会话自动接回，历史从宿主机上的终端状态重建；非持久会话标记为已丢失 |

持久会话的机制、限制，以及从旧的 Zellij 版本升级，见 [docs/sessions.md](./docs/sessions.md)。

SSH 主机、中转、粘贴图片、路径与附属项目见 [docs/using.md](./docs/using.md)。

## 跑起来

需要 Rust 工具链。在 `native/` 下：

```bash
cd native
cargo run -p falcon-server      # 服务端，默认 127.0.0.1:4923，数据目录 ~/.falcon
cargo run -p falcon-desktop     # 原生客户端，连本机服务
```

不带参数时用的是默认数据目录 `~/.falcon`，和已经装好的服务是同一个实例。试跑另起一个时加上 `--port` 与 `--data-dir`。

启动参数、安装包、系统服务与更新见 [docs/deploy.md](./docs/deploy.md)。仓库结构与构建任务见 [docs/develop.md](./docs/develop.md)。

## 文档

| 文档 | 内容 |
| --- | --- |
| [CONTEXT.md](./CONTEXT.md) | 术语 |
| [docs/sessions.md](./docs/sessions.md) | 持久会话、守护进程、已知限制 |
| [docs/using.md](./docs/using.md) | SSH、中转、粘贴、路径、附属项目 |
| [docs/deploy.md](./docs/deploy.md) | 启动参数、打包、常驻、更新、安全、卸载 |
| [docs/develop.md](./docs/develop.md) | 工程结构、常用命令、升级 libghostty-vt |
| [docs/plugins.md](./docs/plugins.md) | 右侧栏插件作者手册 |
| [docs/adr/](./docs/adr/) | 架构决策 |

### iPad 客户端

用 Xcode 打开 `ipad/Falcon.xcodeproj`，选择 Falcon 和 iPad 运行目标。真机安装时在 Signing & Capabilities 中选择开发团队。应用支持 iPadOS 17 及以上版本。首次打开时输入 iPad 可访问的 Falcon 服务端地址，例如 `http://falcon.local:4923`。侧栏按项目列出会话，点按加号可新建终端；窗口变窄时通过系统返回按钮切换会话。
