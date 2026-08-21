/**
 * git 执行层：把 argv 交给宿主机跑，把结果翻译成结构化事实或 WorktreeError。
 *
 * 核心约定（继承自 zellij/install.ts 的 ExecFn）：**非零退出码是正常返回值，
 * 绝不 reject**——localExec 连 spawn 的 error 事件都吞成 `{ code: null }`。
 * 所以这里判错一律显式查 res.code；只有 exec 本身 reject 才是链路故障。
 */

import type {
  GitChangeCounts,
  GitFileDiff,
  GitSnapshot,
  GitUnavailableReason,
  GitWorktreeRef,
  RepoBranch,
  RepoInfo,
  WorktreeFailure,
  WorktreeStatus,
} from "@mojito/shared";
import type { ExecResult } from "../zellij/install.js";
import * as gc from "./command.js";
import { WorktreeError, worktreeFailureText } from "./error.js";
import type { GitHost } from "./host.js";
import {
  canonKey,
  isAbsolute,
  normalizeSep,
  siblingWorktreePath,
} from "./path.js";

/**
 * 各类命令的超时。
 *
 * 必须有：localExec 与 SshLink.exec 都没有超时，Fastify 也没配 request timeout。
 * git 一旦停在那里（等凭据、stale NFS、半死的 SSH 通道），请求就永远不返回。
 * GIT_TERMINAL_PROMPT=0 挡住了最常见的一种，超时兜住其余。
 */
export const TIMEOUT_READ = 15_000;
export const TIMEOUT_ADD = 120_000;
export const TIMEOUT_REMOVE = 60_000;

interface RunOpts {
  timeoutMs?: number;
  signal?: AbortSignal;
}

/** 把外部 signal 与超时合成一个，跑完记得 dispose */
function deadline(opts?: RunOpts): { signal: AbortSignal; dispose(): void } {
  const ac = new AbortController();
  const ms = opts?.timeoutMs ?? TIMEOUT_READ;
  const timer = setTimeout(() => ac.abort(), ms);
  const onOuter = () => ac.abort();
  opts?.signal?.addEventListener("abort", onOuter, { once: true });
  return {
    signal: ac.signal,
    dispose() {
      clearTimeout(timer);
      opts?.signal?.removeEventListener("abort", onOuter);
    },
  };
}

/** 原样执行一条命令行（给 existsCommand / gitFileHeadCommand 这类非 git 命令用） */
export async function execRaw(
  host: GitHost,
  commandLine: string,
  opts?: RunOpts
): Promise<ExecResult> {
  const d = deadline(opts);
  try {
    return await host.exec(commandLine, d.signal);
  } catch (err) {
    throw new WorktreeError(
      "link-failed",
      worktreeFailureText("link-failed"),
      (err as Error).message
    );
  } finally {
    d.dispose();
  }
}

/** 跑一条 git 命令，**允许非零退出码**——退出码本身就是答案时用它 */
export async function probeGit(
  host: GitHost,
  argv: string[],
  env: Record<string, string> = gc.GIT_ENV_RO,
  opts?: RunOpts
): Promise<ExecResult> {
  return execRaw(host, gc.buildGitCommandLine(host.kind, argv, env), opts);
}

/**
 * 一次 exec 跑多条 git 命令（见 batchGitCommandLine），返回与入参同序的
 * 结果与整批的 stderr。SSH 上一条 exec 就是一次 channel 往返，快照类探测
 * 的往返数直接决定面板延迟。
 */
export async function batchGit(
  host: GitHost,
  argvs: string[][],
  env: Record<string, string> = gc.GIT_ENV_RO,
  opts?: RunOpts
): Promise<{ results: gc.BatchGitResult[]; stderr: string }> {
  const res = await execRaw(host, gc.batchGitCommandLine(host.kind, argvs, env), opts);
  return { results: gc.parseGitBatch(res.stdout, argvs.length), stderr: res.stderr };
}

type ProbeLike = { code: number | null; stdout: string };

/**
 * `git --version` 握手成功的缓存（key 是宿主机标识，只缓存成功）。
 * 握手的意义在区分"没装 git"与"不是仓库"，一台宿主机隔几分钟验一次足够，
 * 不必每个 repoRoot 都多付一个往返。
 */
const versionOkUntil = new Map<string, number>();
const VERSION_TTL_MS = 5 * 60_000;

function ensureVersion(host: GitHost, res: ProbeLike, stderr: string): void {
  if (res.code === 0 && /git version/i.test(res.stdout)) {
    versionOkUntil.set(host.key, Date.now() + VERSION_TTL_MS);
    return;
  }
  throw new WorktreeError(
    "git-missing",
    worktreeFailureText("git-missing"),
    stderr.trim() || res.stdout.trim() || `退出码 ${res.code}`
  );
}

function rootFrom(host: GitHost, res: ProbeLike, stderr: string): string {
  if (res.code !== 0) {
    throw new WorktreeError(
      "not-a-repo",
      worktreeFailureText("not-a-repo"),
      stderr.trim() || `退出码 ${res.code}`
    );
  }
  const root = normalizeSep(host.kind, res.stdout.trim());
  if (!root || !isAbsolute(host.kind, root)) {
    throw new WorktreeError("not-a-repo", "无法解析仓库根", res.stdout.trim());
  }
  return root;
}

/** 跑一条 git 命令，非零退出码翻译成带 reason 的 WorktreeError */
export async function runGit(
  host: GitHost,
  argv: string[],
  reason: WorktreeFailure,
  env: Record<string, string> = gc.GIT_ENV_RO,
  opts?: RunOpts
): Promise<ExecResult> {
  const res = await probeGit(host, argv, env, opts);
  if (res.code !== 0) {
    // 三级兜底与 install.ts 的 run() 一致：有些命令一个字都不往 stderr 写，
    // 只留一个退出码，而"该改环境还是该重试"全靠这一句话
    throw new WorktreeError(
      reason,
      worktreeFailureText(reason),
      res.stderr.trim() || res.stdout.trim() || `命令退出码 ${res.code}`
    );
  }
  return res;
}

export async function pathExists(host: GitHost, p: string, opts?: RunOpts): Promise<boolean> {
  const res = await execRaw(host, gc.existsCommand(host.kind, p), opts);
  return res.stdout.includes("yes");
}

/** 分片批量探测，返回与入参同序的布尔数组。任何一片失败就整体退化为"未知（false）" */
async function pathsExist(host: GitHost, paths: string[]): Promise<boolean[]> {
  const out: boolean[] = [];
  for (let i = 0; i < paths.length; i += gc.EXISTS_BATCH) {
    const slice = paths.slice(i, i + gc.EXISTS_BATCH);
    const res = await execRaw(host, gc.existsManyCommand(host.kind, slice));
    const parsed = gc.parseExistsMany(res.stdout);
    // 条数对不上说明输出被 profile 之类污染了，宁可当作"未知"也不要错位
    if (parsed.length !== slice.length) return paths.map(() => false);
    out.push(...parsed);
  }
  return out;
}

/**
 * 仓库根。
 *
 * 必须先握手 `git --version` 再解释 rev-parse 的失败：POSIX 上缺 git 是 127 +
 * "command not found"，而 Windows 的 `& 'git'` 在 git 缺失时抛
 * CommandNotFoundException、退出码 1、错误文本还是本地化的——跟"不是仓库"完全
 * 分不开。先握手一次就把两件事彻底分开，代价是一次往返。
 * （同一个套路在 zellij 那边叫 verify()，理由一模一样。）
 *
 * 裸仓库也走 not-a-repo：`--show-toplevel` 在裸仓库里直接 fatal，而"没有工作树的
 * 仓库"对派生来说与"不是仓库"是同一件事。
 */
export async function repoRoot(host: GitHost, dir: string, opts?: RunOpts): Promise<string> {
  if ((versionOkUntil.get(host.key) ?? 0) < Date.now()) {
    const ver = await probeGit(host, gc.versionArgs(host.git), gc.GIT_ENV, opts);
    ensureVersion(host, ver, ver.stderr);
  }
  const res = await probeGit(host, gc.repoRootArgs(host.git, dir), gc.GIT_ENV_RO, opts);
  return rootFrom(host, res, res.stderr);
}

export async function listWorktrees(
  host: GitHost,
  repo: string,
  opts?: RunOpts
): Promise<gc.GitWorktreeEntry[]> {
  const res = await runGit(
    host,
    gc.worktreeListArgs(host.git, repo),
    "worktree-add-failed",
    gc.GIT_ENV_RO,
    opts
  );
  return gc.parseWorktreeList(res.stdout);
}

/**
 * 主 worktree 的绝对路径。
 *
 * `worktree list` 的第一条永远是主 worktree。派生一律以它为基准，于是：
 * 工作目录指到仓库子目录时不会把 worktree 建进仓库内部；从附属项目再派生时
 * （虽然接口层已禁止）也不会长成 repo-a-b。
 */
export function mainWorktreeOf(entries: gc.GitWorktreeEntry[], fallback: string): string {
  return entries[0]?.path ?? fallback;
}

/** 分支 → 已检出它的 worktree 路径。git 不允许同一分支被检出两次 */
export function checkedOutMap(entries: gc.GitWorktreeEntry[]): Map<string, string> {
  const m = new Map<string, string>();
  for (const e of entries) if (e.branch) m.set(e.branch, e.path);
  return m;
}

/**
 * 源项目的仓库信息：能不能派生、有哪些分支、每条分支会建到哪里。
 *
 * 这是**探测**，不是操作：环境事实（没装 git、不是仓库）如实写进 reason 返回，
 * 由调用方决定是渲染成一句说明（GET）还是一个 409（POST）。
 */
export async function describeRepo(
  host: GitHost,
  workingDir: string,
  opts?: RunOpts
): Promise<RepoInfo> {
  // 第一批：握手 + 仓库根 + worktree 列表。后续命令要以主 worktree（列表第一条）
  // 为基准（headBranch 问的是主 worktree 的 HEAD，不一定等于 workingDir 的），
  // 所以拆成两批——两个往返，仍远好于原先的 7+。
  const git = host.git;
  const first = await batchGit(
    host,
    [gc.versionArgs(git), gc.repoRootArgs(git, workingDir), gc.worktreeListArgs(git, workingDir)],
    gc.GIT_ENV_RO,
    opts
  );
  const [verRes, rootRes, wtRes] = first.results;
  ensureVersion(host, verRes, first.stderr);
  const root = rootFrom(host, rootRes, first.stderr);
  if (wtRes.code !== 0) {
    throw new WorktreeError(
      "worktree-add-failed",
      worktreeFailureText("worktree-add-failed"),
      first.stderr.trim() || `退出码 ${wtRes.code}`
    );
  }
  const entries = gc.parseWorktreeList(wtRes.stdout);
  const main = mainWorktreeOf(entries, root);
  const checkedOut = checkedOutMap(entries);

  const second = await batchGit(
    host,
    [
      gc.remoteListArgs(git, main),
      gc.branchListArgs(git, main),
      gc.headBranchArgs(git, main),
      gc.headShortShaArgs(git, main),
    ],
    gc.GIT_ENV_RO,
    opts
  );
  const [remotesRes, branchRes, headRes, shaRes] = second.results;
  const remotes = remotesRes.stdout
    .split(/\r?\n/)
    .map((l) => l.trim())
    .filter(Boolean);

  if (branchRes.code !== 0) {
    throw new WorktreeError(
      "worktree-add-failed",
      worktreeFailureText("worktree-add-failed"),
      second.stderr.trim() || branchRes.stdout.trim() || `命令退出码 ${branchRes.code}`
    );
  }
  const parsed = gc.parseBranchList(branchRes.stdout, remotes);

  const headRaw = headRes.code === 0 ? headRes.stdout.trim() : "";
  const headBranch = headRaw && headRaw !== "HEAD" ? headRaw : undefined;
  const headSha = !headBranch && shaRes.code === 0 ? shaRes.stdout.trim() || undefined : undefined;

  // 远程分支的本地名：剥掉最长匹配的 remote 前缀（origin/feat/x → feat/x）
  const localNameOf = (name: string): string => {
    const hit = remotes
      .filter((r) => name.startsWith(`${r}/`))
      .sort((a, b) => b.length - a.length)[0];
    return hit ? name.slice(hit.length + 1) : name;
  };

  const draft = parsed.map((b) => {
    const localName = b.remote ? localNameOf(b.name) : undefined;
    return {
      name: b.name,
      remote: b.remote,
      localName,
      checkedOutAt: checkedOut.get(b.remote ? (localName ?? b.name) : b.name),
      head: b.head,
      suggestedDir: siblingWorktreePath(host.kind, main, localName ?? b.name),
    };
  });

  // 占用探测批量做：一个下拉框不值几十个 SSH 往返
  const dirs = draft.map((b) => b.suggestedDir);
  const occupied = dirs.length > 0 ? await pathsExist(host, dirs) : [];
  const branches: RepoBranch[] = draft.map((b, i) => ({
    ...b,
    dirOccupied: occupied[i] ?? false,
  }));

  return { derivable: true, repoDir: main, headBranch, headSha, branches };
}

const GIT_FILE_CAP = 200;

function blankSnapshot(partial: Partial<GitSnapshot> & Pick<GitSnapshot, "available">): GitSnapshot {
  return {
    remotes: [],
    files: [],
    fileCount: 0,
    worktrees: [],
    commits: [],
    ...partial,
  };
}

export function unavailableSnapshot(
  reason: GitUnavailableReason,
  detail?: string
): GitSnapshot {
  return blankSnapshot({ available: false, reason, detail });
}

export function unavailableChanges(): GitChangeCounts {
  return { available: false, added: 0, deleted: 0 };
}

export function unavailableDiff(reason: GitUnavailableReason, detail?: string): GitFileDiff {
  return { available: false, reason, detail, diff: "", truncated: false };
}

export interface GitDiffTarget {
  /** 仓库根相对路径，前端从快照原样带回 */
  path: string;
  /** 重命名 / 复制前的路径 */
  origPath?: string;
  /** porcelain 的 ?? —— 未跟踪文件走 --no-index 伪 diff */
  untracked?: boolean;
}

/**
 * Git 面板里单个文件的 diff。
 *
 * 与 describeGit 同一类探测：环境事实抛 WorktreeError，由路由写成 200 +
 * available:false。基准取 HEAD（暂存 + 未暂存一起看）——面板列的是
 * `status --porcelain` 的合并视图，点开看到的也该是同一份合并答案。
 */
export async function describeGitDiff(
  host: GitHost,
  workingDir: string,
  target: GitDiffTarget,
  opts?: RunOpts
): Promise<GitFileDiff> {
  const root = await repoRoot(host, workingDir, opts);
  const ro = gc.GIT_ENV_RO;

  let res: ExecResult;
  if (target.untracked) {
    res = await probeGit(host, gc.diffUntrackedArgs(host.git, root, target.path), ro, opts);
    // 退出码 1 = 有差异，这就是预期答案；但"文件访问不了"也是 1，只是 stdout 为空
    const failed =
      (res.code !== 0 && res.code !== 1) ||
      (res.code === 1 && !res.stdout.trim() && res.stderr.trim().length > 0);
    if (failed) throw diffError(res);
  } else {
    res = await probeGit(
      host,
      gc.diffFileArgs(host.git, root, "HEAD", target.path, target.origPath),
      ro,
      opts
    );
    if (res.code !== 0) {
      // 最常见的失败是空仓库没有 HEAD（bad revision）：退回与空树比
      res = await probeGit(
        host,
        gc.diffFileArgs(host.git, root, gc.EMPTY_TREE, target.path, target.origPath),
        ro,
        opts
      );
      if (res.code !== 0) throw diffError(res);
    }
  }

  const { text, truncated } = gc.truncateDiff(res.stdout);
  return { available: true, diff: text, truncated };
}

function diffError(res: ExecResult): WorktreeError {
  return new WorktreeError(
    "link-failed",
    "git diff 失败",
    res.stderr.trim() || res.stdout.trim() || `退出码 ${res.code}`
  );
}

/**
 * 侧栏最后一层的 +N −M。只跑一条 status，SSH 上每棵检出都问一次也扛得住。
 * 读不到就 available:false，徽标不画——侧栏不能因为一台主机抖动整列空白。
 */
export async function describeGitChanges(
  host: GitHost,
  workingDir: string,
  opts?: RunOpts
): Promise<GitChangeCounts> {
  const res = await probeGit(host, gc.statusArgs(host.git, workingDir), gc.GIT_ENV_RO, opts);
  if (res.code !== 0) return unavailableChanges();
  const { added, deleted } = gc.countStatusChanges(gc.parseStatusEntries(res.stdout));
  return { available: true, added, deleted };
}

/**
 * 同一台宿主机上多棵检出的 +N −M，一次 exec 全拿。
 * 侧栏轮询按主机分组后调它：同主机 N 个项目从 N 条 SSH channel 压成 1 条。
 * 任何一条失败只影响自己那格（unavailable），不拖累同批的其他项目。
 */
export async function describeGitChangesMany(
  host: GitHost,
  dirs: string[],
  opts?: RunOpts
): Promise<GitChangeCounts[]> {
  if (dirs.length === 0) return [];
  const { results } = await batchGit(
    host,
    dirs.map((d) => gc.statusArgs(host.git, d)),
    gc.GIT_ENV_RO,
    opts
  );
  return results.map((res) => {
    if (res.code !== 0) return unavailableChanges();
    const { added, deleted } = gc.countStatusChanges(gc.parseStatusEntries(res.stdout));
    return { available: true, added, deleted };
  });
}

/**
 * 右侧 Git 面板的仓库快照。
 *
 * 跟 describeRepo 一样是探测：环境事实（没装 git、不是仓库）抛 WorktreeError，
 * 由路由写成 200 + available:false。
 *
 * 全部命令批成**一次** exec：SSH 一条连接默认 MaxSessions=10，逐条串行虽然
 * 躲开了并发上限，但 100ms RTT 的链路上十来条就是 1.5s+，而这个快照每 5 秒
 * 轮询一次。批量后既没有并发压力也没有串行延迟。所有命令用 -C workingDir
 * 而不是先等仓库根：porcelain / rev-parse / log / worktree list 在仓库内
 * 任何目录下答案都一样（porcelain 路径本就相对仓库根），根从批里那条
 * rev-parse 拿。
 */
export async function describeGit(
  host: GitHost,
  workingDir: string,
  opts?: RunOpts
): Promise<GitSnapshot> {
  const git = host.git;

  const { results, stderr } = await batchGit(
    host,
    [
      gc.versionArgs(git),
      gc.repoRootArgs(git, workingDir),
      gc.headBranchArgs(git, workingDir),
      gc.headShortShaArgs(git, workingDir),
      gc.upstreamArgs(git, workingDir),
      gc.aheadArgs(git, workingDir),
      gc.behindArgs(git, workingDir),
      gc.statusArgs(git, workingDir),
      gc.remoteVerboseArgs(git, workingDir),
      gc.logArgs(git, workingDir),
      gc.worktreeListArgs(git, workingDir),
    ],
    gc.GIT_ENV_RO,
    opts
  );
  const [
    verRes,
    rootRes,
    headBranchRes,
    headShaRes,
    upstreamRes,
    aheadRes,
    behindRes,
    statusRes,
    remotesRes,
    logRes,
    wtRes,
  ] = results;

  ensureVersion(host, verRes, stderr);
  const root = rootFrom(host, rootRes, stderr);

  const headRaw = headBranchRes.code === 0 ? headBranchRes.stdout.trim() : "";
  const detached = !headRaw || headRaw === "HEAD";
  const headBranch = detached ? undefined : headRaw;
  const headSha = headShaRes.code === 0 ? headShaRes.stdout.trim() || undefined : undefined;

  const upstreamRaw = upstreamRes.code === 0 ? upstreamRes.stdout.trim() : "";
  const upstream = upstreamRaw && upstreamRaw !== "HEAD" ? upstreamRaw : undefined;

  const countOrNull = (res: { code: number | null; stdout: string }): number | null =>
    res.code === 0 && /^\d+$/.test(res.stdout.trim()) ? Number(res.stdout.trim()) : null;

  const files = statusRes.code === 0 ? gc.parseStatusEntries(statusRes.stdout) : [];
  const remotes = remotesRes.code === 0 ? gc.parseRemotes(remotesRes.stdout) : [];
  const commits = logRes.code === 0 ? gc.parseLog(logRes.stdout) : [];

  let worktrees: GitWorktreeRef[] = [];
  if (wtRes.code === 0) {
    worktrees = gc.parseWorktreeList(wtRes.stdout).map((e) => ({
      path: normalizeSep(host.kind, e.path),
      branch: e.branch,
      head: e.head.slice(0, 7),
      current: pathEq(host, e.path, root),
    }));
  }

  return blankSnapshot({
    available: true,
    repoDir: root,
    workDir: normalizeSep(host.kind, workingDir),
    headBranch,
    headSha,
    detached,
    upstream,
    ahead: upstream ? countOrNull(aheadRes) : null,
    behind: upstream ? countOrNull(behindRes) : null,
    remotes,
    files: files.slice(0, GIT_FILE_CAP),
    fileCount: files.length,
    worktrees,
    commits,
  });
}

/**
 * 附属项目的工作区状态：删除前的预检。
 *
 * 脏与否**只看退出码**，不解析文本——Windows 远端上原生命令的输出按控制台代码页
 * 解码，中文路径会变乱码。文本解析只用来给出样例清单，读不到就退化成计数，
 * 不影响任何判断。
 */
export async function worktreeStatus(
  host: GitHost,
  dir: string,
  opts?: RunOpts
): Promise<WorktreeStatus> {
  const present = await pathExists(host, dir, opts);
  if (!present) {
    return { present: false, dirtyCount: 0, dirtySample: [], ignoredCount: 0, ahead: null };
  }
  try {
    const [unstaged, staged, untracked] = await Promise.all([
      probeGit(host, gc.diffDirtyArgs(host.git, dir), gc.GIT_ENV_RO, opts),
      probeGit(host, gc.diffCachedDirtyArgs(host.git, dir), gc.GIT_ENV_RO, opts),
      probeGit(host, gc.untrackedArgs(host.git, dir), gc.GIT_ENV_RO, opts),
    ]);
    const untrackedLines = untracked.stdout.split(/\r?\n/).filter((l) => l.trim().length > 0);
    const dirtyByCode =
      unstaged.code === 1 || staged.code === 1 || untrackedLines.length > 0;

    // 展示用清单：拿不到就只报"有改动"，不阻断
    let dirtyCount = dirtyByCode ? Math.max(1, untrackedLines.length) : 0;
    let dirtySample: string[] = [];
    const st = await probeGit(host, gc.statusArgs(host.git, dir), gc.GIT_ENV_RO, opts);
    if (st.code === 0) {
      const parsed = gc.parseStatus(st.stdout);
      dirtyCount = parsed.count;
      dirtySample = parsed.files;
    }

    const ign = await probeGit(host, gc.statusIgnoredArgs(host.git, dir), gc.GIT_ENV_RO, opts);
    const ignoredCount = ign.code === 0 ? gc.countIgnored(ign.stdout) : 0;

    // 没有 upstream 时 rev-list 退出码 128，这不是错误，只是"无从比较"
    const aheadRes = await probeGit(host, gc.aheadArgs(host.git, dir), gc.GIT_ENV_RO, opts);
    const ahead =
      aheadRes.code === 0 && /^\d+$/.test(aheadRes.stdout.trim())
        ? Number(aheadRes.stdout.trim())
        : null;

    return { present: true, dirtyCount, dirtySample, ignoredCount, ahead };
  } catch (err) {
    // 读不到状态不阻断删除，只是让确认框改口说"无法确认里面有没有未保存的东西"
    return {
      present: true,
      dirtyCount: 0,
      dirtySample: [],
      ignoredCount: 0,
      ahead: null,
      error: err instanceof WorktreeError ? (err.detail ?? err.message) : (err as Error).message,
    };
  }
}

export interface AddWorktreeOptions {
  mode: "new-branch" | "existing-branch";
  /** 目标分支的本地名 */
  branch: string;
  /** mode=new-branch 的基点，缺省 HEAD */
  startPoint?: string;
  /** 目标目录（调用方已做过静态否决与占用探测） */
  dir: string;
}

/**
 * 建一棵新的 worktree，返回 **git 自己报的**绝对路径。
 *
 * 返回值必须取自 `worktree list --porcelain` 而不是我们算出来的字符串：删除护栏靠
 * 路径比对，落库的那一份就得是宿主机的权威说法（Windows 上 git 吐正斜杠、大小写
 * 也可能与用户输入不同）。
 *
 * 断链认领：worktree add 完全可能在宿主机上**已经成功、只是响应丢了**（SSH 抖动）。
 * 那样磁盘上多一个目录而 DB 里什么都没有——不可见的孤儿，而删除护栏全靠 DB 记录，
 * 我们再也无法安全清理它。所以链路故障时 best-effort 回查一次：真落地了就认领。
 * 这比引入 creating/deleting 状态机 + 启动对账便宜得多，取向也一致：
 * 出问题时把事实原样告诉用户，而不是替他记账。
 */
export async function addWorktree(
  host: GitHost,
  main: string,
  o: AddWorktreeOptions
): Promise<string> {
  const argv =
    o.mode === "new-branch"
      ? gc.worktreeAddNewArgs(host.git, main, o.dir, o.branch, o.startPoint || "HEAD")
      : gc.worktreeAddExistingArgs(host.git, main, o.dir, o.branch);

  let res: ExecResult;
  try {
    res = await probeGit(host, argv, gc.GIT_ENV, { timeoutMs: TIMEOUT_ADD });
  } catch (err) {
    const claimed = await claimWorktree(host, main, o).catch(() => null);
    if (claimed) return claimed;
    throw err instanceof WorktreeError
      ? new WorktreeError(
          err.reason,
          err.message,
          `${err.detail ?? ""}（如果宿主机上已经建出了 ${o.dir}，请手动检查）`.trim()
        )
      : err;
  }

  if (res.code !== 0) {
    const reason = classifyAddError(res.stderr);
    throw new WorktreeError(
      reason,
      worktreeFailureText(reason),
      res.stderr.trim() || res.stdout.trim() || `git worktree add 退出码 ${res.code}`
    );
  }

  const claimed = await claimWorktree(host, main, o);
  if (!claimed) {
    // add 报成功却查不到，只可能是路径归一化出了岔子。宁可报错也不要落一条
    // 指向未知位置的记录——那条记录将来会被拿去当删除目标。
    throw new WorktreeError(
      "worktree-add-failed",
      worktreeFailureText("worktree-add-failed"),
      `已创建但在 worktree 列表中找不到 ${o.dir}`
    );
  }
  return claimed;
}

/** 目标目录是否已经是本仓库的一棵 worktree 且分支相符；是则返回 git 报的路径 */
async function claimWorktree(
  host: GitHost,
  main: string,
  o: AddWorktreeOptions
): Promise<string | null> {
  const entries = await listWorktrees(host, main);
  const hit = entries.find((e) => pathEq(host, e.path, o.dir));
  if (!hit) return null;
  return hit.branch === o.branch ? hit.path : null;
}

/**
 * `git worktree add` 的失败分类。
 *
 * 预检与 add 之间用户可能刚在别的终端检出了同一分支，所以这些判据是 TOCTOU 兜底，
 * 不是"预检的替代"。判据全是英文原文——GIT_ENV 里锁了 LC_ALL=C 就是为了这一刻。
 */
export function classifyAddError(stderr: string): WorktreeFailure {
  const s = stderr.toLowerCase();
  // 两种措辞都要认：worktree add 说的是 "is already used by worktree at"
  // （git 2.48 实测），而 "is already checked out at" 是 checkout/switch 的说法，
  // 老版本的 worktree add 也用过。只认一条会把 branch-in-use 降级成
  // 一句笼统的 worktree-add-failed，用户就不知道该去哪个目录看了。
  if (s.includes("is already used by worktree at") || s.includes("is already checked out at")) {
    return "branch-in-use";
  }
  if (/a branch named .* already exists/.test(s)) return "branch-exists";
  if (s.includes("invalid reference") || s.includes("not a valid object name"))
    return "branch-unknown";
  if (s.includes("already exists")) return "path-occupied";
  return "worktree-add-failed";
}

/** 两个路径是否指同一处（归一化后比较，Windows 大小写不敏感） */
export function pathEq(host: GitHost, a: string, b: string): boolean {
  return canonKey(host.kind, a) === canonKey(host.kind, b);
}
