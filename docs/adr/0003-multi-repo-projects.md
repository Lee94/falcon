# 多仓库项目：N 个仓库一个项目，批量派生一个集中目录

用 git 的另一个常态是**一组相关仓库**（微服务、前后端分仓）同时开工。falcon 现在支持「多仓库项目」：一个 Project 同时包含 N 个成员仓库路径，一键在全部成员上按统一分支名各建一棵 worktree，集中放进一个新目录，产出**一个**可开会话的多仓库附属项目。术语见 CONTEXT.md（多仓库项目 / 成员仓库 / 批量派生 / 集中目录）。

## 决定一：判别式，不是新类型

`Project.multi?: MultiRepoInfo`（DB 一列 `multi_repos` JSON），**不给 `ProjectType` 加第三个值**——ADR 0002 决定一的理由原封不动适用：「多仓库」与「local/ssh」正交（成员全在项目自己的宿主机上，链路复用），与「容器/附属」也正交。四象限：

| | `multi == null` | `multi != null` |
|---|---|---|
| `worktree == null` | 普通项目 | **容器**（成员 dir = 用户选的路径） |
| `worktree != null` | 单仓库附属项目 | **多仓库附属项目**（成员 dir = worktree 路径 + repoDir = 仓库根） |

成员语义随行而变是刻意的：容器的成员是**用户的真仓库**（永不删除，反而进「别删到我」清单 `guardDirsOf`）；派生行的成员是**删除目标清单**（连同创建那一刻记下的仓库根，护栏比对不读容器行——它可改、可删）。派生行的 `worktree_repo_dir` 恒为 null：单值列对多仓库无意义。

写入护栏与 worktree 四列同罪三防线：`updateProject` 的 SQL 不碰 `multi_repos`；容器改成员走 `updateMultiRepos`，SQL 自带 `AND source_project_id IS NULL`（即使路由写错也够不到派生行，db.test.ts 有直接证据）；PUT 对派生行的 `repos` 显式 400。`parseMultiRepos` 对损坏 JSON 返回 null：UI 侧降级成普通项目外观（普通项目永不删目录，方向安全），删除侧直接 veto。

## 决定二：一次派生 = 一个项目、一个集中目录、一个会话

不是「每个成员各派生一个附属项目」——那样一次派生在侧栏多 N 行、会话按仓库各开各的、删除要点 N 次；而这个功能的使用单位就是「一条分支跨一组仓库」。集中目录 `<容器名slug>-<分支slug>/` 里以**仓库根 basename** 平铺 N 棵 worktree，默认建在第一个成员仓库根的父目录（基准是仓库根不是成员配置路径，理由同 0002 的 monorepo 子目录条目）；它就是派生行的 working_dir，会话 cwd、文件面板天然可用。

- `nameSlug` 比 `branchSlug` 更狠（`/ \ : * ? " < > |` 空白控制符全替换）：容器名是任意用户字符串，没有 refname 规则兜底。尾部 `.`/空格剥除与按码点截断的理由同 0002。
- 成员 basename 撞名（含 Windows 大小写别名）与同仓库出现两次在**派生时**拒（`vetoMultiRoots`，按 canonKey）——容器创建时成员可指仓库子目录、宿主 kind 未必已知，这两条只有 rev-parse 之后才判得出。
- 集中目录必须**原先不存在**（空目录也拒）：与单派生锁内复查同一语义，也让回滚与删除的空目录 rmdir 有据可依——目录一定是我们建的。
- `git worktree add` 会自建缺失的父目录（macOS git 2.x 实测；Windows 未验证，若不成立需在创建前显式 mkdir）。

## 决定三：全有或全无，回滚绝不裸删

任一成员失败 → `rollbackCreatedWorktrees`：对已建成员**逆序** `git worktree remove --force`（一个 `--force`，与删除链路同一条规矩），然后空目录删除收掉集中目录。回滚只碰"本请求刚建出、claimWorktree 确认过"的路径，**绝不落 rm 兜底**——刚建的树是干净的，remove 理应成功；万一失败（毫秒级内被 lock），残留路径进 `MultiDeriveError.leftover` 原样报给用户，HTTP 一律 502。失败归因带 `member.dir`（容器里配置的那个路径，用户认得）。回滚**不删分支**（0002 的铁律）：`add -b` 已建出的新分支留在原仓库里，指向 HEAD、无害；用 auto 重试会直接检出它。

预检（三批 batchGit：握手+仓库根 / worktree 列表 / 分支存在性）把可预见的失败在动手前拦下——拦住一个就省一轮回滚；auto 模式（存在则检出、不存在则从各自 HEAD 新建）也在这里解析。真正的 TOCTOU 兜底仍是 `classifyAddError`，与单派生"预检不是替代"的立场一致。

**auto 与"基点恒 HEAD"**：统一分支名跨 N 个仓库时"有的有这条分支、有的没有"是常态，没有 auto，全有或全无会让混合状态永远派生不出来。代价是 auto 不追踪远程分支：分支只在 origin 上存在时走"从 HEAD 新建"而不是从 origin/x——要动它得先解决 0002 的"永不 detached"约束，留给将来。

## 决定四：删除的包围盒断言

多仓库派生行不复用 `vetoRemoval`（那段被 0002 点名"可审查性优先于复用度"，一字不动），另写 `vetoMultiRemoval`：除单版七条的逐成员适配外，核心新断言是**包围盒**——集中目录是唯一允许动手的范围，每个成员必须严格在它内部，成员出圈即脏数据整行拒删。另有：成员不得等于/包住任何仓库根、集中目录不得包住任何仓库根、集中目录与每个成员都不得踩到其他项目的 guardDirsOf。

清理顺序：逐成员 取证（`proveWorktree` 证据 A/B 原样复用）→ `clearOneWorktree`（从单版抽出的共用段）→ 全部确认消失才对集中目录做**空目录删除**（posix `rmdir` / .NET `Directory.Delete($p,$false)` / 本地 `fs.rmdirSync`，非递归在类别上无法毁数据）。集中目录非空一律留下 + warning 带路径——里面可能有用户放的东西。

**locked 成员成员级保留**、其余照删：整体收手会留 N 个目录让用户手工收拾，与"DB 行无条件删、清理 best-effort"的总取舍一致。存档到期清扫零改动获得正确语义：locked ⇒ 集中目录没删掉 ⇒ `pathExists` 判"还在" ⇒ 行保留下一轮重试。

## 顺手修掉的锁键分歧

`withRepoLock` 的键此前三处手拼：派生用 `host.key + '::' + canonKey(root)`，commit 与 pull/push 用单冒号——两个键空间不相交，**同一仓库上的派生与 pull/push 从来就不互斥**。统一为 `repoLockKey()`（`::`，因为 ssh 的 host.key 本身含 `:`）。这是修 bug：行为变化是 commit/pull/push 与派生现在正确互斥，别把它"恢复"回去。批量派生**逐成员顺序进锁**、不持多锁——全有或全无本就串行，也就没有死锁问题。

## 面板与徽标

- git 面板类端点加 `?repo=<成员 dir 原文>`：必须精确等于某个成员（前端从 `project.multi.repos` 原样带回，不做归一化比较——不匹配就是请求造错了）。读端点缺省回退第一个成员，**写端点（commit/pull/push）缺省 400**：写操作不猜。前端的成员选择放 store（`multiRepo`）：History / 修改 / diff tab 必须看同一个成员。
- 侧栏 ± 徽标对多仓库行**刻意不显示**：一个 projectId 对 N 个仓库，求和徽标说不清是哪个仓库脏，误导大于信息。按 (id, repo) 键控留给将来。
- `/files*` 与 ⌘P 零改动：集中目录不是仓库，`listRepoFiles` 返回 null 自动落 fs 遍历。

## 明确不做

- 不给容器嵌套容器、不允许跨宿主成员（成员只是路径，宿主由容器的 type/ssh_* 决定，跨宿主在构造上不存在）。
- 不提供成员级单独派生 / 单独删除——从容器再派生一次、或删掉整个派生行，完全等价。
- 派生行的成员清单不可编辑（它是删除目标）；容器编辑成员**不影响**已派生的子项目（快照语义，"判据取创建那一刻的值"）。
- 部分成功不落库：宁可回滚重来，不留一个缺了成员的派生行——没有"补建缺失成员"的流程，半成品只会腐烂。
- 上限 `MULTI_REPO_MAX = 16`：逐成员串行、每条 add 最长 2 分钟，上限同时是请求耗时上限（Fastify 无 request timeout 是既有事实）。

## 验证

纯函数层有测试（nameSlug / multiCentralPath / vetoMultiRoots / vetoMultiRemoval 全套脏数据 / validateMemberList / parseMultiRepos / updateMultiRepos 的 SQL 护栏 / guardDirsOf / 前端 preview 与交集逻辑）。端到端照 0002 先例手工验，macOS 本地 + git 2.x 经 REST 跑通：

- 容器创建（workingDir 留空）与探测（GET /repos 逐成员 derivable + baseDir）。
- auto 混合态批量派生（一仓库已有 feat/x、一仓库没有）：中文容器名的集中目录、两棵 worktree 各自检出 feat/x、DB 行 multi+worktree 形状与逐成员 repoDir 正确。
- 预检拦截：分支已被别的 worktree 检出 → 409 + member 归因，**零创建**。
- 真回滚：srv 的 .git 置只读让第二个成员在 add 时才失败 → web 已建成的 worktree 被 remove、集中目录消失、无半成品行、无 leftover；web 上 `add -b` 建出的分支按铁律保留。
- 护栏：PUT 派生行的 repos → 400；`?repo=` 命中成员 / 非成员 400 / 写操作缺 repo 400。
- 删除：合并预检 dirtySample 带 `srv/` 前缀；删派生行后 worktree 逐棵注销、分支保留、集中目录消失、成员仓库纹丝不动；集中目录塞用户文件 → 成员删光、目录保留 + warning 带确切路径；删容器级联清掉派生行与其集中目录。

**未验证**：SSH 远端（POSIX 与 Windows）全部路径、Windows 本地（尤其 `worktree add` 自建父目录、`[]` 路径、长路径预算）、locked 成员的删除分支、存档到期清扫的多仓库行、多仓库项目的会话/Zellij 路径（沿用现有 cwd 机制，理论上零改动）。
