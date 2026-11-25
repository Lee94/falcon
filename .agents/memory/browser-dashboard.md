# 多主机浏览器面板

入口是 Falcon 的 `/browser-dashboard`。桌面右侧活动栏的总览图标打开本机 Falcon 的面板，使用本机服务配置的地址。项目菜单打开当前连接服务端的总览，终端标题栏通过 `terminal` 和可选的 `browserSession` 查询参数选中实例。会话列表来自同源的 `GET /api/sessions`，最近的操作记录来自 `/api/sessions/{id}/browser/log`，实时画面来自 `/ws/browser/{id}?readOnly=true`。节点网关负责汇总和转发。网关返回的终端 ID 已包含所属节点，浏览器实例名按查询参数传递。桌面原生窗口继续提供接管功能。只读连接可以确认收到画面，不参与接管锁的计时。

总览的「历史会话」按宿主机与终端读取 `/api/sessions/{id}/browser/history`，再以返回的 `record` 分页读取操作记录。旧版命名实例仅有哈希，界面用短标识和修改时间区分；新版发现实例时在同一日志目录记录原名。清除终端时一并清理。远端节点须支持历史接口。

静态资源来自 vercel-labs/agent-browser 的 `packages/dashboard`，上游提交为 `d2dfc782ccdd79d25a9b4f0acced9321789cecb8`。Falcon 适配了会话来源、实时数据格式和只读界面。服务端二进制内含压缩后的面板资源，日常 Rust 编译直接使用这些文件。

更新面板时，从 Falcon 仓库根目录运行以下命令。构建依赖安装在临时上游检出目录。

```sh
dashboard_dir="$PWD/native/crates/falcon-server/dashboard"
upstream_dir="$(mktemp -d)"
git clone https://github.com/vercel-labs/agent-browser "$upstream_dir"
git -C "$upstream_dir" checkout d2dfc782ccdd79d25a9b4f0acced9321789cecb8
git -C "$upstream_dir" apply "$dashboard_dir/falcon.patch"
(cd "$upstream_dir" && pnpm install --filter dashboard... --frozen-lockfile --ignore-scripts)
(cd "$upstream_dir/packages/dashboard" && pnpm exec next build --webpack)
python3 "$dashboard_dir/package.py" "$upstream_dir/packages/dashboard/out"
```
