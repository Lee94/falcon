# 环境与线程

环境是已有宿主机上的执行目录和上下文。`GET /api/environments/branches` 在目录所属节点查询 Git 分支，普通目录返回 `null`。`POST /api/environments` 在该节点验证工作目录。选择基础分支时，在节点数据目录 `environments/<环境 ID>/` 创建独立检出，源仓库保持原分支。环境删除后保留检出及其文件。

`POST /api/projects/{环境 ID}/threads` 保存一个可恢复的 agent 对话。新线程共用环境工作目录，通过 `thread_environment_id` 关联环境。宿主机保存对话历史，桌面端打开线程时加载历史并恢复 Claude Code 会话。两类创建接口支持 `Idempotency-Key`。环境下还有线程时，服务端拒绝删除该环境。

环境和线程复用项目库存的标识、文件接口和会话引擎，网关按所属节点重写标识。桌面和 Android 共用 `falcon-core::project_tree` 的分组。旧的多仓库环境和已有工作树线程仍可访问，旧工作树沿用原有删除护栏。

局部验证在 `native/` 运行 `cargo test -p falcon-server --lib thread_`，协议样本通过 `cargo xtask fixtures --threads-only` 在隔离节点生成。
