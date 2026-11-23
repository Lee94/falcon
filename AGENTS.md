# 项目指引与共享记忆

本项目的架构、构建测试命令与协作约定见 [CLAUDE.md](CLAUDE.md)，进入项目时须读取该文件。

本地提交与 push 只做改动相关的局部测试或编译检查；全量编译检查与测试放在 GitHub 发版流程中执行，失败则禁止发布。具体命令与例外见 CLAUDE.md 的「验证」一节。

项目记忆：

- [本地原生客户端更新](.agents/memory/native-client-update.md)：客户端单独更新、应用备份与签名校验，以及服务端版本探测的注意事项。

- [本地 coding agent CLI 检测](.agents/memory/agent-cli-detection.md)：检测范围、刷新与菜单共用规则，以及新增 CLI 的修改位置。

- [多主机浏览器面板](.agents/memory/browser-dashboard.md)：会话路由、官方面板版本与静态资源更新步骤。

Android 终端使用 Google vtfx，最低支持 Android 10（API 29）。生产源码、许可证、上游提交和本地适配记录在 `android/vtfx/`。修改终端时可运行 Android Gradle 任务 `:app:testDebugUnitTest --tests '*VtfxSessionTest'`。构建前通过 `cargo xtask android --jni-only` 准备字体和原生库。
