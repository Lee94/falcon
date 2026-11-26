# 环境与线程验证

环境和线程由所属节点的 SQLite 数据库管理。目录规则与接口见[架构决定](../../docs/adr/0031-environments-and-threads.md)。

从 `native/` 运行以下局部检查：

- `cargo test -p falcon-server --lib thread_` 验证工作树、上下文清理、重复请求以及两个网关访问同一宿主机。
- `cargo test -p falcon-core --lib project_tree` 验证环境下的线程分组和旧项目分组。
- `cargo xtask fixtures --threads-only` 使用已构建的服务端生成环境与线程协议样本。服务端更新后先执行 `cargo build -p falcon-server`。

检查桌面入口时，以 `FALCON_LOCAL_URL` 指向隔离服务端，用 `FALCON_NATIVE_DATA_DIR` 指定临时客户端目录。线程根目录应包括 `AGENTS.md`、`CLAUDE.md` 和 `projects/`，打开终端后可用 `pwd` 核对。
