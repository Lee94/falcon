# Falcon

持久化终端工作台：以项目为单位管理终端会话。关掉客户端，会话仍在宿主机上运行。项目可以是本机上的一个目录，也可以连到预先保存的 SSH 远端主机。

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
