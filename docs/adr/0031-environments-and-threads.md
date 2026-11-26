# 环境与线程

环境和线程复用宿主机节点的项目标识、文件接口和会话引擎。节点数据库的 `projects.environment_context` 可空。旧项目保留空值，环境保存用户填写的文本（允许空串），线程保存创建时生成的完整上下文。线程的 `source_project_id` 指向环境。入口按节点重写两个标识，远端机器离线时沿用节点离线状态。

`POST /api/environments` 在所属主机验证成员路径、解析仓库根，并按主工作树判重。`POST /api/projects/{环境 ID}/threads` 在该主机记录各仓库的起始提交，创建独立分支及工作树。工作树放在节点数据目录的 `threads/<线程 ID>/projects/` 下，线程根目录同时作为默认终端目录。线程上下文首次写入后可由用户编辑。两类创建接口支持 `Idempotency-Key`。

线程成员及其源仓库记录在创建时固定。环境更换成员或修改上下文只影响新线程。创建中途失败会撤销已登记的工作树并报告残留。与既有工作树删除机制一致，Git 分支保留。显式删除线程前可通过 `GET /api/projects/{线程 ID}/worktree` 检查未提交修改。清理只移除内容与初始快照相同的上下文文件，用户编辑过的内容留在原目录。删除仍有线程的环境时，服务端提示先处理各线程。

桌面和 Android 共用 `falcon-core::project_tree` 的主机和父子关系。环境内创建线程预选该环境，桌面全局入口列出「主机 / 环境」。原项目、原工作树和独立终端继续使用旧入口。

局部验证在 `native/` 运行 `cargo test -p falcon-server --lib thread_`，协议样本通过 `cargo xtask fixtures --threads-only` 在隔离节点生成。
