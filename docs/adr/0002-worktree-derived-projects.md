# 附属项目：把 git worktree 做成一等的 Project

用 git 的人常态是同时开几条分支。mojito 现在支持从一个 Project **派生**出附属项目：算出仓库根同级的目录 → `git worktree add` → 落一条新 Project 记录；删除附属项目时**连目录一起删**，删源项目则级联删掉它的全部附属项目（源项目自己的目录不动）。local 与 ssh 项目都支持。

这是 mojito 第一次删除用户可控路径——在此之前后端的 TS 代码里零 fs 删除，唯一的 `rm -rf` 只作用于自己造的 `.tmp-<uuid>`。所以下面大半篇幅是在讲**怎么保证只删该删的那个目录**，而不是怎么调 git。

## 决定一：附属项目是 Project，不是另一种实体

`projects` 表加四列（`source_project_id` / `worktree_branch` / `worktree_repo_dir` / `worktree_created_by_mojito`），不新建表，也不给 `ProjectType` 加第三个值。

附属项目在**运行时的每一个维度上都是 Project**：有名字、有宿主机、有 cwd、有会话、有 Zellij 布局、有主机授权。`sessions.project_id`、`SessionManager.getLink(ProjectRow)`、侧栏、总览、命令面板、HostDrawer 全部吃 `Project`。拆表等于把这些能力重新实现一遍；给 `ProjectType` 加值则会让每一处 `type === "ssh"` 都变成两条判断——「附属」与「local/ssh」是正交的两个维度。

**不可编辑性靠类型不可达保证**：`ProjectInput` 不含这四列，而 `PUT /api/projects/:id` 用的就是 `ProjectInput`，`Db.updateProject` 的 SQL 也不碰它们。于是"改一行 JSON 让 mojito 去 rm -rf 任意路径"这条攻击面从写入路径上就不存在。

唯一的漏点是 `working_dir`——它既是删除目标又本来就可改，所以 `PUT` 里显式拒绝改附属项目的工作目录。这条是第一道防线，后面还有四道。

附属项目的 `ssh_*` 五列从源项目**整行复制**（含密文，同一个 `SecretBox` 能解，全程不碰明文），不是解引用。这让 `SshLink` / `getLink` / `GET /host` 一处都不用改，也让删除清理不依赖源项目行还在不在；代价是源项目改配置时要手动传播一次（`updateChildrenSsh`）。顺带一个白捡的正确行为：Zellij 授权按 host+port+username 记，附属项目第一次开会话不会再弹一次安装授权。

## 决定二：DB 行无条件删，文件系统清理 best-effort

删除返回 `200 { ok: true, warnings?: string[] }`，而不是在清理失败时报错。

留一条删不掉的项目行，用户唯一的出路是去改 SQLite；而残留目录他自己删得掉——前提是我们把路径原样告诉他。所以清理失败一律降级成一条带绝对路径的 warning，UI 弹一个 sticky 的警告 toast。

代价是"磁盘上有目录、DB 里没记录"的孤儿理论上可能出现。我们不为它引入 `creating` / `deleting` 状态机 + 启动对账（那会带来三个 UI 状态和一整套永远走不到的代码），只在创建的链路故障分支上做一次 best-effort 回查：`worktree add` 抛链路错误时再 `worktree list` 一次，真落地了就认领。这与整体取向一致——出问题时把事实原样告诉用户，而不是替他记账。

## 删除护栏

分两层，**先纯函数、后 I/O**：静态断言（`vetoRemoval`）不需要一台宿主机就能验证，可以用手工构造的脏数据去打；这是全功能里唯一一段"写错了会删掉用户东西"的代码，在一个没有测试框架、唯一门禁是 `tsc` 的仓库里，它的可审查性比复用度重要。

静态断言（全部必须成立）：① 是附属项目；② `worktree_created_by_mojito === 1`；③ 路径与仓库根都非空且绝对；④ 深度 ≥ 2（真实的 worktree 都是某个仓库目录的同级，而仓库不会直接躺在盘符根上）；⑤ 不是仓库根、也不是仓库根的祖先；⑥ 不是家目录、也不是家目录的祖先（但**可以在家目录里面**——`~/code/foo-feat-x` 是最常见的布局）；⑦ 不包含任何其他项目的工作目录。

动态取证两条独立证据，任一成立即可：**A)** `worktree list --porcelain` 里仍有这条路径（最强，同时证明了仓库还在、没被 `worktree move` 走）；**B)** 目录里有一个 `.git` **文件**且以 `gitdir:` 开头（linked worktree 独有的特征）。留 B 是因为最常见的破损场景是"用户把整个源仓库删了"——此时 A 永远失败，但目录本身仍然应该被清理掉，没有这条豁免用户会被卡死。

取证必须在 `worktree remove` **之前**：remove 成功后目录就没了，证据 A 自然不再成立。

## 实现要点

大半是实测踩出来的。

- **Windows 上的退出码要自己传出，而且不能裸传**。这一条实测了两轮才落定。`powershell -EncodedCommand` 不写 `exit` 时只给 0 / 1：原生命令退 3 也会被压成 1，`curl` 的 6/7/22/28 全都分不出来；更糟的是原生命令一旦不是最后一条语句，失败会被完全吞掉退 0。但**裸写 `exit $LASTEXITCODE` 反而更危险**：`& 'C:\缺失.exe'` 抛的是 CommandNotFoundException，原生命令根本没跑，`$LASTEXITCODE` 从未被赋值，`exit $null` 等价于 `exit 0`——"跑不起来"被报成成功。所以 `powerShellScript` 预置哨兵 `$LASTEXITCODE = 127`（POSIX 的 command-not-found 约定）再 `exit`：跑起来了就被真实退出码覆盖，没跑起来就留 127。（订正：本 ADR 初稿说"共用路径退出码永远是 0"，实测是压成 1；`hasSession` / `verify` 这些既有判据因此一直是对的，只是丢了精度。）
- **输出编码必须钉成 UTF-8**。`[Console]::OutputEncoding` 默认是系统 OEM 代码页，简体中文机器上是 936；而 `SshLink.exec` 与 `localExec` 两边都是 `toString("utf8")`。在 936 机器上实测：`echo 中文路径` 不设这一行拿回的是 `d6d0cec4...`（GBK），解码即乱码；设了就是干净的 UTF-8，且重定向到管道时不写 BOM。含中文的路径、`dump-screen` 抓回的 Scrollback 全靠它，所以直接加进了 `encodePowerShell` 的前言而不是只给 git 用。
- **删除命令不用 `Remove-Item`**。`-Path` 会先把路径当通配符去匹配：git refname 禁止 `[ ] * ?` 所以分支名安全，但**仓库目录名不受限**——`D:\code\my[old]repo` 派生出的路径含 `[`。实测 `Remove-Item -Recurse -Force -EA SilentlyContinue -Path 'C:\...\rm[1]old'` **退出码 0、目录纹丝不动**，我们据此删掉 DB 行，目录就永远成了孤儿。`-LiteralPath` 能解决这一条（`install.ts` 的 `cleanup` 已按此修正），但 PowerShell 5.1（`encodePowerShell` 调的正是它）的 `Remove-Item -Recurse` 对目录 junction **会递归进目标**。所以 worktree 删除改用 .NET 的 `[System.IO.Directory]::Delete($p, $true)`：吃字面量、递归时不穿越 reparse point。顶层是 reparse point 一票否决；**不能**一刀切"任何后代含 reparse point 就拒绝"——Windows 上 pnpm 的 `node_modules` 遍地是 junction，但都指向内部的 `node_modules/.pnpm`。（`New-Item -Path` 不在此列：实测含 `[]` 的路径照样建得出来，创建不走通配匹配。）
- **"清掉继承来的环境变量"必须是 unset，不能是空串**。`GIT_DIR` / `GIT_WORK_TREE` / `GIT_INDEX_FILE` / askpass 要清掉，是因为 `localExec` 用 `spawn(shell:true)` 继承 `process.env`，后端若被某个 git hook 拉起就会静默作用到别的仓库上。初版把它们当成 `env` 的一部分设成 `""`，在 Windows 上一路绿灯——PowerShell 的 `$env:X = ''` 恰好等价于删除。但 POSIX 侧 `env GIT_DIR='' git ...` 是**设成空串**，实测直接 `fatal: not a git repository: ''`；`GIT_INDEX_FILE=''` 更狠，git 拿 `.lock` 当索引，`status` 报出一整片并不存在的删除。也就是说这个功能在每一台 POSIX 宿主上都是坏的，而只在 Windows 上测根本看不出来。`buildCommandLine` 因此多了一个显式的 `unset` 形参：POSIX 走 `env -u X`，Windows 走 `$env:X = $null`。
- **本地删除根本不走 shell**，直接 `fs.rmSync(dir, { recursive, force, maxRetries: 3 })`：不跟随符号链接与 junction、没有通配展开、没有退出码传播问题，长路径也比 `rd` 好。这一条把 Windows 本地的整类风险直接归零。
- **POSIX 远端把校验与删除压成一条命令**：`[ -d ] && [ ! -L ] && [ "$(cd -P && pwd -P)" = "$p" ] && rm -rf -- "$p"`。真正的风险不是注入（`quotePosix` 的单引号包裹让 `$` / 反引号 / `;` 全部惰性），而是两次 SSH 往返之间目标被换成符号链接的 TOCTOU。`--` 挡住以 `-` 开头的路径被当成选项。
- **被 `git worktree lock` 锁住时必须立刻收手**，不能落到兜底删除。第一版就栽在这里：`remove` 因为 lock 失败、记了 warning，然后兜底逻辑看见"目录还在"就把它删了——lock 是用户明说的"别碰"，这是实测才发现的。也因此绝不自动解锁、绝不给第二个 `--force`。
- **派生基准是仓库根，不是项目的 workingDir**。工作目录完全可能是仓库子目录（monorepo 里指到 `packages/server` 很常见），在那儿旁边建 worktree 会把它建进仓库自己里面——源仓库的 `git status` 里立刻多出一坨未跟踪文件。
- **永不创建 detached worktree**。检出远程分支一律走 `worktree add -b <local> <path> origin/<r>`，把 `origin/x` 直接当 commit-ish 会得到 detached HEAD，而 detached worktree 里的提交在 remove 之后立刻不可达、可被 gc 回收——那是真数据丢失。这也是"删 worktree 不删分支"的另一面：分支在，未推送的提交就永远在。
- **`worktree add` 的失败判据是 `is already used by worktree at`**，不是文档里更常见的 `is already checked out at`（后者是 `checkout` / `switch` 的说法）。git 2.48 实测。两条都认，因为老版本用过另一条。`GIT_ENV` 里锁 `LC_ALL=C` 就是为了让这些判据在中文系统上仍然成立。
- **git 路径要探测一次并缓存绝对路径**。SSH exec 是非登录 shell，`buildCommandLine` 不套 `sh -l -c`（只有 `buildPtyCommandLine` 套了），Homebrew / asdf / nix / `~/.local/bin` 下的 git 就是找不到——症状精确复刻 ADR 0001 描述过的那一类"终端里好好的、功能却说没装"。只在探测时套登录 shell，之后按绝对路径直调：stdout 干净、快。
- **必须先握手 `git --version` 再解释 `rev-parse` 的失败**。POSIX 上缺 git 是 127 + "command not found"，而 Windows 的 `& 'git'` 在 git 缺失时抛 CommandNotFoundException、退出码 1、错误文本还是本地化的——跟"不是仓库"完全分不开。
- **每条 git 调用都带超时**，外加 `GIT_TERMINAL_PROMPT=0` / `GIT_ASKPASS=` / `SSH_ASKPASS=`。`localExec` 与 `SshLink.exec` 都没有超时，Fastify 也没配 request timeout；git 一旦停在那里等凭据，请求就永远不返回。同时清空 `GIT_DIR` / `GIT_WORK_TREE` / `GIT_INDEX_FILE`：`localExec` 用 `spawn(shell:true)` 继承 `process.env`，拉起后端的进程若设了这些，所有 git 调用会静默作用到别的仓库上。
- **不用 `--porcelain -z`**。NUL 分隔要穿过 `powershell -EncodedCommand` 的输出管线，编码行为不可靠；改用默认格式 + 自己处理 C 风格引号，十行代码换掉一整类平台不确定性。所有输出按 `/\r?\n/` 切行——Windows 远端上 PowerShell 用 `\r\n` 拼接。
- **分支 slug 保留非 ASCII**。第一版写成"只保留 `[A-Za-z0-9._-]`"，结果 `功能/中文` 这类分支整条塌成同一个兜底值，两条中文分支互撞、目录名还完全看不出是哪条分支。CJK 在 NTFS 与 ext4 上都合法；只替换 `/` 与 Windows 独有的 `" < > |`（其余非法字符 git 的 refname 规则已经挡掉了）。尾部的 `.` 和空格必须去掉：Windows 创建时会**静默**吃掉它们，于是 DB 里记 `foo-feat.`、磁盘上是 `foo-feat`，护栏比对永远对不上。
- **落库的路径取自 git 自己的输出**，不是我们算出来的字符串。Windows 上 git 吐正斜杠、大小写也可能与用户输入不同，而护栏靠路径比对——归一化集中在 `canonKey` 一处，断言只比归一化后的形式。真正的危险不是"功能不工作"，而是有人为了让它工作去放宽断言。
- **并发锁按「宿主机 + 仓库根」**，不是 projectId：两个 Project 完全可能指向同一个仓库，按 projectId 加锁等于没加。

## 明确不做

沿用 ADR 0001 那条"mojito 只管理自己创建的会话，绝不接管用户自有的"：

- **不接管 mojito 没创建的 worktree**。用户手动把普通项目指向一个已存在的 worktree，mojito 就当它是普通项目，不提供"删除时一并删目录"。
- **不删分支**、不自动 unlock、不给 `worktree add --force`、不给 `-B`、不给第二个 `--force`。
- **`worktree prune` 只在走了兜底删除时才跑**：它是仓库级的，会清掉 mojito 没创建的 stale 条目。`worktree remove` 成功时会自己清理管理项。
- **不允许二级派生**。git 本身允许，但侧栏的两级树会变成任意深度、删除级联要递归，而收益是零——从源项目派生完全等价。将来若要放开，把 source 取成 `row.source_project_id ?? row.id` 重新挂到根即可，树仍是两级。
- **不做定时 GC、不做启动自动清理**。每一次删除都必须由用户的一次点击直接触发。
- **代码里不出现 `git clean` 的命令模板**，哪怕是 `-n` 的。在一个没有测试的仓库里，一个 `-n` 打错就是 `git clean -fdX`。
- **不为派生开 WebSocket**。Zellij 安装用 WS 是因为它同时满足"耗时数十秒 + 有分阶段进度 + 必须能取消"三条；`worktree add` 一条都不满足，取消到一半反而留下半棵 worktree。

## 验证

无测试框架（v1 已明确不写自动化测试），照 ADR 0001 的先例做手工端到端验证，在 Windows 本地 + `git 2.48.1` 上跑通：

- 派生（新建分支 / 检出本地分支 / 检出远程分支 / 中文分支 / 子目录项目基准落在仓库根同级）与各类拒绝（分支已被检出、分支已存在、基点不存在、目录被占用、目标落在仓库内部、非仓库项目、二级派生）。
- 删除：脏工作区的确认框内容（含被忽略的 `.env` 单列）、目录真的消失、分支保留、worktree 注销。
- 护栏：21 条静态断言用例（盘符根 / home / 仓库根 / 仓库根上层 / 相对路径 / 踩到别的项目 / 大小写差异 / POSIX 侧的 `/`、`/home`、反斜杠文件名）；外加直接改 SQLite 把 `working_dir` 指向 `C:\`、家目录、仓库根、仓库上层、无关目录——每条都拒删目录、给出具体原因、DB 行照删。
- 破损场景：手工删掉 worktree 目录后再删项目（幂等）、把主仓库整个改名后再删附属项目（走 `.git` 文件嗅探仍清理成功）、`worktree lock` 后删除（目录保留 + warning）。
- 级联：源项目 + 2 个附属项目 + 1 个活跃会话，不带 `force` 得 409（会话数含附属项目的），带 `force` 后两个 worktree 目录消失、源项目目录纹丝不动、会话全部终止。

**未验证**：SSH 远端（POSIX 与 Windows）的全部路径、macOS 宿主。远端的命令构造与本地共用同一套代码，但 `-EncodedCommand` 下的退出码传播与编码、以及 POSIX 侧压成一条的删除命令都只在本地推演过。
