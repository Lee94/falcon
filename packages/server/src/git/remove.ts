/**
 * 删除附属项目的 worktree 目录。
 *
 * 这是**全仓库唯一一处删除用户可控路径的地方**。在此之前后端的 TS 代码里零 fs 删除，
 * 唯一的 rm -rf 只作用于自己造的 .tmp-<uuid>。所以这个模块刻意做成单一入口：
 * 不导出任何裸的"删目录"函数，静态断言是一个纯函数（可以被单独盯着读、单独用手改的
 * 脏数据去打），动态取证与删除各自只有一条路径。
 *
 * 总的取舍：**DB 行无条件删，文件系统清理 best-effort**。留一条删不掉的项目行，
 * 用户唯一的出路是去改 SQLite；残留目录他自己删得掉——前提是我们把路径原样告诉他。
 * 所以这里的返回值是一串 warning，而不是异常。
 */

import fs from "node:fs";
import { MULTI_REPO_MAX, type MultiRepoMember } from "@falcon/shared";
import { Db, type ProjectRow } from "../db.js";
import { encodePowerShell, quotePosix, quotePowerShell } from "../zellij/host.js";
import type { HostKind } from "../zellij/host.js";
import * as gc from "./command.js";
import { gitErrorLine } from "./error.js";
import type { GitHost } from "./host.js";
import { repoLockKey, withRepoLock } from "./lock.js";
import {
  canonKey,
  isAbsolute,
  isAncestor,
  isUnc,
  normalizeSep,
  pathDepth,
  samePath,
} from "./path.js";
import { execRaw, listWorktrees, pathExists, probeGit, TIMEOUT_REMOVE } from "./repo.js";

/**
 * 删除前的静态否决。返回非 null 即"不许删"，字符串直接作为 warning 给用户。
 *
 * 纯函数、只吃行数据：不需要一台宿主机就能验证它。这是本功能里唯一一段
 * "写错了会删掉用户东西"的代码，它的可审查性比复用度重要——尤其在一个没有测试框架、
 * 唯一门禁是 tsc 的仓库里。
 *
 * otherDirs 是**其他**项目的工作目录，用来挡住"删掉别人正在用的目录"。
 */
export function vetoRemoval(
  kind: HostKind,
  row: ProjectRow,
  home: string,
  otherDirs: string[]
): string | null {
  const dir = row.working_dir;
  const repo = row.worktree_repo_dir;

  // ① 必须是附属项目
  if (!row.source_project_id) return `「${row.name}」不是附属项目，不清理任何目录`;

  // ② 必须是 falcon 建的目录。用户手工建的 worktree、以及将来"接管已有 worktree"的
  //    场景一律不删——对应 ADR 0001 的"绝不接管用户自有的 Zellij 会话"
  if (row.worktree_created_by_mojito !== 1) {
    return `${dir ?? "(空)"} 不是 Falcon 创建的，未删除`;
  }

  // ③ 路径必须非空且绝对。相对路径意味着这行是脏数据，宁可不删
  if (!dir || !isAbsolute(kind, dir)) return `工作目录不是绝对路径，未删除：${dir ?? "(空)"}`;
  if (!repo || !isAbsolute(kind, repo)) return `仓库根记录缺失，未删除：${dir}`;

  // ④ 深度门槛。真实的 worktree 都是"某个仓库目录的同级"，而仓库不会直接躺在
  //    盘符根上。门槛设在 2 能挡掉绝大多数脏数据造成的灾难（C:\ 、/ 、D:\x），
  //    代价只是拒绝一种没人真会用的布局
  if (pathDepth(kind, dir) < 2) return `路径过浅，拒绝删除：${dir}`;

  // ⑤ 绝不删仓库根本身，也绝不删包住仓库的任何一层
  if (samePath(kind, dir, repo)) return `目标就是仓库根，拒绝删除：${dir}`;
  if (isAncestor(kind, dir, repo)) return `目标包含仓库根，拒绝删除：${dir}`;

  // ⑥ 绝不删家目录本身或它的上层。注意 worktree **可以**在 home 里面
  //    （~/code/foo-feat-x 是最常见的布局），所以这里只挡"是 home"和"是 home 的祖先"
  if (home && (samePath(kind, dir, home) || isAncestor(kind, dir, home))) {
    return `目标是家目录或其上层，拒绝删除：${dir}`;
  }

  // ⑦ 别踩到其他项目的工作目录上
  const clash = otherDirs.find((o) => samePath(kind, dir, o) || isAncestor(kind, dir, o));
  if (clash) return `目标包含另一个项目的工作目录（${clash}），拒绝删除：${dir}`;

  return null;
}

/** 动态取证的结论。locked 只在 registered 时可知 */
interface Proof {
  kind: "registered" | "gitfile";
  locked: boolean;
}

/**
 * 证明 dir 此刻仍是 repo 的一棵 worktree。两条独立证据，任一成立即可。
 *
 * A) `git -C <repo> worktree list --porcelain` 里仍有这条路径。最强的一条：同时
 *    证明了目录是该仓库的 worktree、仓库还在、用户没在别处 worktree move 走。
 *
 * B) 目录里有一个 .git **文件**（不是目录）且以 "gitdir: " 开头。这是 linked
 *    worktree 独有的特征——普通仓库那里是目录，普通目录根本没有。留这条降级路径是
 *    因为最常见的破损场景是"用户把整个源仓库删了"：此时 A 永远失败，但目录本身仍然
 *    应该被清理掉。没有这条豁免，用户会被卡死——目录删不掉，也没法自己收拾。
 */
async function proveWorktree(host: GitHost, repo: string, dir: string): Promise<Proof | null> {
  try {
    const entries = await listWorktrees(host, repo, { timeoutMs: TIMEOUT_REMOVE });
    const hit = entries.find((e) => canonKey(host.kind, e.path) === canonKey(host.kind, dir));
    if (hit) return { kind: "registered", locked: hit.locked };
  } catch {
    // 仓库没了 / 链路抖了都落到证据 B
  }
  try {
    const res = await execRaw(host, gc.gitFileHeadCommand(host.kind, dir), {
      timeoutMs: TIMEOUT_REMOVE,
    });
    if (res.stdout.trimStart().startsWith("gitdir:")) return { kind: "gitfile", locked: false };
  } catch {
    // 读不到就是没证据
  }
  return null;
}

/** 被 lock 时给用户的说明。永远不自动解锁、永远不给第二个 --force */
function lockedWarning(dir: string): string {
  return `该 worktree 被 git worktree lock 锁定，目录未删除：${dir}（先 git worktree unlock 再重试）`;
}

/**
 * POSIX 远端：把校验与删除压成**一条**命令。
 *
 * 真正的风险不是注入（quotePosix 的单引号包裹让 $ / 反引号 / ; 全部惰性），
 * 而是"验证"与"删除"之间目标被换成符号链接——两次 SSH 往返之间的 TOCTOU。
 * 压成一条就没有那个窗口了。
 *
 * `--` 挡住以 - 开头的路径被 rm 当成选项；不带尾斜杠才不跟随最后一段的符号链接；
 * `cd -P && pwd -P` 确认它就是物理路径（我们记的是 git 报的路径，git 用 getcwd，
 * 拿到的本来就是物理路径）。
 */
function posixRemoveCommand(dir: string): string {
  const p = quotePosix(dir);
  return [
    `p=${p}`,
    `if [ ! -e "$p" ]; then printf gone; exit 0; fi`,
    `if [ -L "$p" ]; then printf symlink; exit 0; fi`,
    `if [ ! -d "$p" ]; then printf notdir; exit 0; fi`,
    `if [ "$(cd -P -- "$p" && pwd -P)" != "$p" ]; then printf notphysical; exit 0; fi`,
    `rm -rf -- "$p" && printf ok || printf failed`,
  ].join("; ");
}

/**
 * Windows 远端：同样一条命令里完成校验与删除。
 *
 * 用 .NET 的 Directory.Delete 而不是 Remove-Item：
 * - Remove-Item **-Path** 会先把路径当通配符去匹配。git refname 禁止 [ ] * ?，
 *   所以分支名安全，但**仓库目录名不受限**——D:\code\my[old]repo 派生出的路径含 [，
 *   -Path 匹配不到任何东西，实测退出码 0 而目录纹丝不动：静默报成功，我们据此删掉
 *   DB 行，目录就永远成了孤儿。（-LiteralPath 能解决这一条。）
 * - 但 PowerShell 5.1 的 Remove-Item -Recurse 对目录 junction **会递归进目标**，
 *   junction 指向 worktree 之外就会删到别处。encodePowerShell 调的正是 5.1。
 *
 * Directory.Delete 两个问题都没有：吃字面量字符串、且递归时不穿越 reparse point。
 * 代价是遇到只读文件会抛——那种情况报 warning 让用户自己收拾，比冒险强。
 *
 * 顶层是 reparse point 一票否决。注意**不能**一刀切"任何后代含 reparse point 就拒绝"：
 * Windows 上 pnpm 的 node_modules 遍地是 junction，但都指向内部的 node_modules/.pnpm。
 */
function windowsRemoveCommand(dir: string): string {
  const p = quotePowerShell(dir);
  return encodePowerShell(
    [
      `$p = ${p}`,
      `if (-not (Test-Path -LiteralPath $p)) { 'gone'; exit 0 }`,
      `$i = Get-Item -LiteralPath $p -Force`,
      `if (-not ($i -is [System.IO.DirectoryInfo])) { 'notdir'; exit 0 }`,
      `if ($i.Attributes -band [System.IO.FileAttributes]::ReparsePoint) { 'symlink'; exit 0 }`,
      `try { [System.IO.Directory]::Delete($p, $true); 'ok' } catch { 'failed: ' + $_.Exception.Message }`,
    ].join("; ")
  );
}

/**
 * 本地删除**不走 shell**。
 *
 * fs.rm 不跟随符号链接与 junction（走 lstat + unlink）、没有通配展开、
 * 没有退出码传播问题，长路径也比 rd 好。只有 SSH 才需要命令行那一套——
 * 这一条把 Windows 本地的整类风险直接归零。
 *
 * 必须用异步版：worktree 里常有 node_modules（十万级文件），同步递归删除会把
 * 事件循环冻住几秒到几十秒，期间所有终端 WS 与 HTTP 全部停摆——而且每小时的
 * 存档清扫会在用户无感知时触发这条路径。
 *
 * maxRetries 是给 Windows 的：杀毒软件 / 索引服务偶尔会短暂持有句柄。
 */
async function removeLocalDir(dir: string): Promise<string | null> {
  try {
    const st = fs.lstatSync(dir);
    if (st.isSymbolicLink()) return `目标是符号链接，未删除：${dir}`;
    if (!st.isDirectory()) return `目标不是文件夹，未删除：${dir}`;
  } catch {
    return null; // 已经不在了
  }
  try {
    await fs.promises.rm(dir, { recursive: true, force: true, maxRetries: 3, retryDelay: 200 });
  } catch (err) {
    return `删除目录失败：${dir}（${(err as Error).message}）`;
  }
  return fs.existsSync(dir) ? `目录仍然存在，请手动清理：${dir}` : null;
}

const REMOTE_RESULT_TEXT: Record<string, (dir: string) => string> = {
  symlink: (d) => `目标是符号链接 / junction，出于安全没有删除：${d}`,
  notdir: (d) => `目标不是文件夹，未删除：${d}`,
  notphysical: (d) => `目标路径经过符号链接，出于安全没有删除：${d}`,
};

/** 远端删除：校验与删除在同一条命令里完成，消掉 TOCTOU 窗口 */
async function removeRemoteDir(host: GitHost, dir: string): Promise<string | null> {
  const cmd =
    host.kind === "windows" ? windowsRemoveCommand(dir) : posixRemoveCommand(dir);
  const res = await execRaw(host, cmd, { timeoutMs: TIMEOUT_REMOVE }).catch(
    (err: Error) => ({ code: null, stdout: "", stderr: err.message })
  );
  const out = res.stdout.trim();
  if (out === "ok" || out === "gone") return null;
  const known = REMOTE_RESULT_TEXT[out];
  if (known) return known(dir);
  return `删除目录失败：${dir}（${gitErrorLine(out || res.stderr) || `退出码 ${res.code}`}）`;
}

/**
 * 清理一个附属项目的 worktree。返回给用户看的 warning 列表（空 = 全部干净）。
 *
 * 顺序恒为 **取证 → worktree remove → 兜底删目录 → prune**：
 *
 * - 取证必须在 remove 之前。remove 成功后目录就没了，证据 A 自然不再成立，
 *   所以 proveWorktree 在整个函数里只求一次。
 * - 只删目录而不 worktree remove，会在 $GIT_DIR/worktrees/<id>/ 留一条 stale 管理
 *   记录；下次用同名路径再派生，git 会报 "is a missing but locked working tree"
 *   之类，而用户完全看不懂这跟上次删除有什么关系。
 * - prune **只在走了兜底删除时才跑**：它是仓库级的，会清掉 falcon 没创建的 stale
 *   条目（比如用户放在未挂载盘上的 worktree）。remove 成功时会自己清理管理项。
 */
export async function cleanupWorktree(
  row: ProjectRow,
  host: GitHost,
  otherDirs: string[]
): Promise<string[]> {
  // 多仓库派生行走成员清单逐棵清理；调用点（routes 的 DELETE 与 archive 清扫）不感知
  if (row.multi_repos != null) return cleanupMultiWorktree(row, host, otherDirs);

  const veto = vetoRemoval(host.kind, row, host.home, otherDirs);
  if (veto) return [veto];

  // vetoRemoval 已经保证这两个非空
  const dir = normalizeSep(host.kind, row.working_dir!);
  const repo = row.worktree_repo_dir!;

  const proof = await proveWorktree(host, repo, dir);
  if (!proof) {
    return [`无法确认 ${dir} 仍是 ${repo} 的 worktree，出于安全没有删除，请手动清理`];
  }

  // lock 是用户明说的"别碰"，兜底删除绝不能越过它——绝不自动解锁、
  // 绝不给第二个 --force，也绝不"remove 失败就直接 rm"。这条必须在动手之前拦住
  if (proof.locked) return [lockedWarning(dir)];

  return (await clearOneWorktree(host, repo, dir, proof)).warnings;
}

/**
 * 取证之后对单棵 worktree 的实际清理：worktree remove → 兜底删目录 → 兜底时 prune。
 * 单仓库与多仓库共用；调用方必须已经做完 veto、取证与 locked 拦截。
 * gone = 目录确认已不在磁盘上（多仓库版据此决定要不要删集中目录）。
 */
async function clearOneWorktree(
  host: GitHost,
  repo: string,
  dir: string,
  proof: Proof
): Promise<{ warnings: string[]; gone: boolean }> {
  const warn: string[] = [];

  if (proof.kind === "registered") {
    const res = await probeGit(host, gc.worktreeRemoveArgs(host.git, repo, dir), gc.GIT_ENV, {
      timeoutMs: TIMEOUT_REMOVE,
    }).catch((err: Error) => ({ code: null, stdout: "", stderr: err.message }));
    if (res.code !== 0) {
      const line = gitErrorLine(res.stderr || res.stdout);
      // list 与 remove 之间被 lock 上了：同样立刻收手，不落到兜底删除
      if (/locked working tree/i.test(line)) {
        return { warnings: [lockedWarning(dir)], gone: false };
      }
      warn.push(`git worktree remove 失败：${line || `退出码 ${res.code}`}`);
    }
  }

  // remove 成功时目录已经没了，这一步是幂等兜底
  let usedFallback = false;
  let gone = true;
  const stillThere = await pathExists(host, dir, { timeoutMs: TIMEOUT_REMOVE }).catch(() => true);
  if (stillThere) {
    usedFallback = true;
    const problem =
      host.key === "local" ? await removeLocalDir(dir) : await removeRemoteDir(host, dir);
    if (problem) {
      warn.push(problem);
      gone = false;
    }
  }

  if (usedFallback) {
    await probeGit(host, gc.worktreePruneArgs(host.git, repo), gc.GIT_ENV, {
      timeoutMs: TIMEOUT_REMOVE,
    }).catch(() => undefined);
  }

  return { warnings: warn, gone };
}

// ---------------- 多仓库派生行 ----------------

/**
 * 多仓库派生行的静态否决。与 vetoRemoval 同一地位：纯函数、只吃行数据，
 * 写错了会删掉用户东西的那一段，可审查性优先于复用度——所以不去改 vetoRemoval，
 * 而是给多仓库形态一份自己的完整断言清单。
 *
 * 多版最关键的新断言是**包围盒**：集中目录（working_dir）是唯一允许动手的范围，
 * 每个成员 worktree 必须严格在它内部——成员出圈即脏数据，整行拒删。
 */
export function vetoMultiRemoval(
  kind: HostKind,
  row: ProjectRow,
  members: MultiRepoMember[] | null,
  home: string,
  otherDirs: string[]
): string | null {
  const central = row.working_dir;

  // ① 必须是派生行。容器的成员是用户的真仓库，绝不进删除路径
  if (!row.source_project_id) return `「${row.name}」不是附属项目，不清理任何目录`;

  // ② 必须是 falcon 建的目录
  if (row.worktree_created_by_mojito !== 1) {
    return `${central ?? "(空)"} 不是 Falcon 创建的，未删除`;
  }

  // ③ 成员清单必须完好。JSON 损坏 / 空清单 / 超上限都视为脏数据——
  //    清单就是删除目标，读不出清单等于不知道该删什么
  if (!members || members.length === 0 || members.length > MULTI_REPO_MAX) {
    return `成员记录损坏或异常，未删除任何目录：${central ?? "(空)"}`;
  }

  // ④ 集中目录非空、绝对、非 UNC、深度门槛（与单版 ③④ 同理）
  if (!central || !isAbsolute(kind, central)) {
    return `集中目录不是绝对路径，未删除：${central ?? "(空)"}`;
  }
  if (isUnc(kind, central)) return `集中目录是 UNC 路径，拒绝删除：${central}`;
  if (pathDepth(kind, central) < 2) return `路径过浅，拒绝删除：${central}`;

  // ⑤ 绝不删家目录本身或它的上层（可以在 home 里面，同单版 ⑥）
  if (home && (samePath(kind, central, home) || isAncestor(kind, central, home))) {
    return `集中目录是家目录或其上层，拒绝删除：${central}`;
  }

  for (const m of members) {
    // ⑥ 包围盒：每个成员必须严格在集中目录内部
    if (!m.dir || !isAbsolute(kind, m.dir)) {
      return `成员路径不是绝对路径，未删除任何目录：${m.dir || "(空)"}`;
    }
    if (!isAncestor(kind, central, m.dir)) {
      return `成员 ${m.dir} 不在集中目录 ${central} 内部，记录异常，未删除任何目录`;
    }

    // ⑦ 仓库根记录必须完好，且成员不得等于/包住任何成员的仓库根
    if (!m.repoDir || !isAbsolute(kind, m.repoDir)) {
      return `成员 ${m.dir} 缺少仓库根记录，未删除任何目录`;
    }
    for (const other of members) {
      if (!other.repoDir) continue;
      if (samePath(kind, m.dir, other.repoDir) || isAncestor(kind, m.dir, other.repoDir)) {
        return `成员 ${m.dir} 覆盖仓库根 ${other.repoDir}，拒绝删除`;
      }
    }

    // ⑧ 集中目录不得等于/包住任何仓库根——仓库先于集中目录存在，落进去只可能是脏数据
    if (samePath(kind, central, m.repoDir) || isAncestor(kind, central, m.repoDir)) {
      return `集中目录 ${central} 包含仓库根 ${m.repoDir}，拒绝删除`;
    }

    // ⑨ 成员别踩到其他项目的目录上。⑥ 保证成员在集中目录内部，检查集中目录本可覆盖
    //    成员，但这里仍逐成员各查一遍——将来若有人放宽 ⑥，这条不至于跟着失守
    const memberClash = otherDirs.find(
      (o) => samePath(kind, m.dir, o) || isAncestor(kind, m.dir, o)
    );
    if (memberClash) {
      return `成员 ${m.dir} 包含另一个项目的目录（${memberClash}），拒绝删除`;
    }
  }

  // ⑨ 集中目录别踩到其他项目的目录上
  const clash = otherDirs.find(
    (o) => samePath(kind, central, o) || isAncestor(kind, central, o)
  );
  if (clash) return `集中目录包含另一个项目的目录（${clash}），拒绝删除：${central}`;

  return null;
}

/**
 * 多仓库派生行的清理：逐成员 取证 → 清理，最后收拾集中目录。
 *
 * locked 的成员**成员级保留**、其余照删（与"DB 行无条件删、清理 best-effort"一致：
 * 整体收手会留下 N 个目录让用户手工收拾，更糟）。集中目录只走**空目录删除**——
 * 里面可能有用户放的别的东西，非空一律留下 + warning 带路径。
 */
async function cleanupMultiWorktree(
  row: ProjectRow,
  host: GitHost,
  otherDirs: string[]
): Promise<string[]> {
  const members = Db.parseMultiRepos(row);
  const veto = vetoMultiRemoval(host.kind, row, members, host.home, otherDirs);
  if (veto) return [veto];

  const warn: string[] = [];
  let allGone = true;
  for (const m of members!) {
    const dir = normalizeSep(host.kind, m.dir);
    const repo = m.repoDir!; // veto ⑦ 已保证非空
    const proof = await proveWorktree(host, repo, dir);
    if (!proof) {
      warn.push(`无法确认 ${dir} 仍是 ${repo} 的 worktree，出于安全没有删除，请手动清理`);
      allGone = false;
      continue;
    }
    if (proof.locked) {
      warn.push(lockedWarning(dir));
      allGone = false;
      continue;
    }
    const res = await clearOneWorktree(host, repo, dir, proof);
    warn.push(...res.warnings);
    allGone &&= res.gone;
  }

  const central = normalizeSep(host.kind, row.working_dir!);
  if (allGone) {
    const problem = await removeEmptyDir(host, central);
    if (problem) warn.push(problem);
  } else {
    warn.push(`集中目录未删除（内有残留）：${central}`);
  }
  return warn;
}

/**
 * 只删**空**目录：posix rmdir / windows [IO.Directory]::Delete($p, $false) /
 * 本地 fs.rmdirSync。非递归在类别上无法毁数据（最多删掉一个空壳），但它仍然
 * 住在这个文件里——remove.ts 继续是全仓库唯一删除用户可见路径的地方。
 * 返回 null = 删掉了或本来就不在；非 null 是给用户的 warning（自带路径）。
 */
export async function removeEmptyDir(host: GitHost, dir: string): Promise<string | null> {
  const failText = (detail: string) =>
    `集中目录未删除（可能有残留文件）：${dir}（${detail}）`;
  if (host.key === "local") {
    try {
      fs.rmdirSync(dir);
      return null;
    } catch (err) {
      const e = err as NodeJS.ErrnoException;
      return e.code === "ENOENT" ? null : failText(e.message);
    }
  }
  const cmd =
    host.kind === "windows"
      ? encodePowerShell(
          [
            `$p = ${quotePowerShell(dir)}`,
            `if (-not (Test-Path -LiteralPath $p)) { 'gone'; exit 0 }`,
            `try { [System.IO.Directory]::Delete($p, $false); 'ok' } catch { 'failed: ' + $_.Exception.Message }`,
          ].join("; ")
        )
      : [
          `p=${quotePosix(dir)}`,
          `if [ ! -e "$p" ]; then printf gone`,
          `elif rmdir -- "$p" 2>/dev/null; then printf ok`,
          `else printf failed; fi`,
        ].join("; ");
  const res = await execRaw(host, cmd, { timeoutMs: TIMEOUT_REMOVE }).catch((err: Error) => ({
    code: null,
    stdout: "",
    stderr: err.message,
  }));
  const out = res.stdout.trim();
  if (out === "ok" || out === "gone") return null;
  return failText(gitErrorLine(out || res.stderr) || `退出码 ${res.code}`);
}

/**
 * 批量派生失败后的回滚。只对"本请求刚建出、claimWorktree 已确认"的成员动手：
 * 逆序逐个 `git worktree remove --force`（一个 --force，与删除链路同一条规矩），
 * 失败就记 leftover——刚建的树是干净的，remove 理应成功；万一失败（比如毫秒级内
 * 被 lock），把路径原样报给用户比再开一条裸删路径便宜且诚实。绝不落 rm 兜底。
 *
 * 返回残留的绝对路径（空 = 回滚干净）。
 */
export async function rollbackCreatedWorktrees(
  host: GitHost,
  created: { dir: string; repoDir: string }[],
  centralDir: string,
  centralCreatedByUs: boolean
): Promise<string[]> {
  const leftover: string[] = [];
  for (const c of [...created].reverse()) {
    const res = await withRepoLock(repoLockKey(host, c.repoDir), () =>
      probeGit(host, gc.worktreeRemoveArgs(host.git, c.repoDir, c.dir), gc.GIT_ENV, {
        timeoutMs: TIMEOUT_REMOVE,
      })
    ).catch((err: Error) => ({ code: null, stdout: "", stderr: err.message }));
    if (res.code !== 0) {
      leftover.push(c.dir);
      // remove 失败会留 stale 管理项，prune 一下别让下次同名派生报莫名其妙的错
      await probeGit(host, gc.worktreePruneArgs(host.git, c.repoDir), gc.GIT_ENV, {
        timeoutMs: TIMEOUT_REMOVE,
      }).catch(() => undefined);
    }
  }
  if (centralCreatedByUs) {
    if (leftover.length === 0) {
      const problem = await removeEmptyDir(host, centralDir);
      if (problem) leftover.push(centralDir);
    } else {
      // 成员还在里面，集中目录必然留着——一并列出来，用户照单收拾
      leftover.push(centralDir);
    }
  }
  return leftover;
}
