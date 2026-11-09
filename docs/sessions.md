# 持久会话

持久会话由 falcon 自己的会话守护进程 falcon-sessiond 持有。宿主机（本地项目 = 后端所在机器，SSH 项目 = 远端机器）上的每个会话是一对进程：holder 持有 PTY，VT 进程（libghostty-vt）维护终端状态、回答终端查询、导出接回用的快照。

守护进程由后端推到宿主机的 `~/.falcon/bin/`（文件名带内容哈希，推完做握手校验）。宿主机不需要能出网，也不需要 curl 或 tar。推不上或跑不起来时降级为普通会话，界面上标明具体原因（目录不可写、`noexec` 挂载、架构不支持等）。

程序可以用 OSC 7501（Program Status Protocol）报告自己在工作、等授权、完成还是出错。Claude Code 会这么做。falcon 在侧栏和窗口标题栏上显示状态徽标；会话停在等授权或等回答时弹一次提醒，没人在看的会话也一样。

在宿主机上也能直接接回自己的会话：

```bash
~/.falcon/bin/falcon-sessiond-<版本>-<哈希> list
~/.falcon/bin/falcon-sessiond-<版本>-<哈希> attach <SID>
```

`attach` 缺省用 Ctrl+\ 断开，会话照常运行。

升级 falcon 时，新版本推上去的守护进程按内容另起文件名，旧文件不覆盖。已有会话照常运行、照常接回；只有 VT 进程会被热换成新版本（libghostty-vt 的二进制快照交接，换不了就用原始输出重建）。

协议与接入的设计见 [docs/design/session-daemon.md](./design/session-daemon.md)，决定见 [ADR 0020](./adr/0020-session-daemon-libghostty.md)。

## 首次部署与重试

首次在某台远端主机上创建会话时会询问一次授权，因为这会往那台机器写入可执行文件。授权按主机记（host + port + username），同一台机器上的后续项目不再询问。

探测和上传这类瞬时故障最多尝试 3 次（含首次），两次重试前分别等待 1.5 秒和 4 秒，进度里会说明正在第几次。三次都失败则给出远端的原始报错和一个重试按钮。跑不起来（`noexec`、架构不符）或目录不可写这类稳定的环境事实不做自动重试：先改环境，再点重试。

## 已知限制

- **Windows 宿主**：守护进程先用 `CREATE_BREAKAWAY_FROM_JOB` 脱离调用方所在的 Job Object（sshd 的会话、被进程管理器拉起的后端都在 Job 里，Job 关闭时整棵进程树会一起退出）。Job 不允许脱离时改经 WMI（`Win32_Process.Create`）拉起，依赖宿主机允许当前用户做 WMI 进程创建（默认允许）。远端首次使用时会做一次真实断线验证，验证不通过则标注为非持久。这条路径还没有在 Windows 真机上验证过。
- **Windows 上没有程序状态**：ConPTY 会吞掉它不认识的 OSC，OSC 7501 到不了守护进程。
- Windows ARM64 跑 x64 的构建，靠系统的 x64 模拟。能否跑起来由部署时的 `version` 握手决定。
- **派生超大仓库**：`git worktree add` 要物化整个工作区，几 GB 的仓库可能要几十秒。派生走普通 REST 请求，放在反向代理后面时请确认 idle timeout 够长。
- **Windows 上的派生路径长度**：目标路径超过 200 字符会被拒绝创建。建得出来却因为 `MAX_PATH` 删不掉，比一开始就拒绝更糟。

## 从 Zellij 版本升级

Zellij 已整体换成 falcon 自己的会话守护进程，旧会话不兼容。升级后旧的持久会话在列表里显示为已丢失，由你手动清除。会话里的程序其实还活在宿主机的 Zellij 里，falcon 不再管它们。想救可以 ssh 上去执行 `zellij attach mj-<…>`，用完自己结束（`zellij kill-all-sessions`）。
