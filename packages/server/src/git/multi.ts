/**
 * 多仓库项目：成员清单校验与批量派生编排。
 *
 * 批量派生是**全有或全无**：任一成员失败，回滚已建好的 worktree（刚建的、
 * claimWorktree 确认过是我们自己建的），不留半成品项目行。回滚住在 remove.ts——
 * 那里仍是全仓库唯一删除用户可见路径的地方，这里只做编排。
 *
 * 预检（分支存在性、检出占用、目录占用）都在锁外做，是为了在**动手之前**把
 * 可预见的失败拦下来——拦住一个就省一轮回滚；真正的 TOCTOU 兜底仍是
 * addWorktree 的 classifyAddError，与单仓库派生"预检不是替代"的立场一致。
 */

import {
  MULTI_REPO_MAX,
  type MultiRepoMember,
  type MultiWorktreeInput,
} from "@falcon/shared";
import * as gc from "./command.js";
import { WorktreeError, worktreeFailureText } from "./error.js";
import type { GitHost } from "./host.js";
import { repoLockKey, withRepoLock } from "./lock.js";
import {
  isAbsolute,
  isAncestor,
  isUnc,
  memberWorktreePath,
  multiCentralPath,
  normalizeSep,
  pathDepth,
  vetoMultiRoots,
  vetoTargetDir,
  WINDOWS_PATH_BUDGET,
} from "./path.js";
import {
  addWorktree,
  batchGit,
  checkedOutMap,
  ensureVersion,
  mainWorktreeOf,
  pathExists,
  pathsExist,
  rootFrom,
} from "./repo.js";
import { rollbackCreatedWorktrees } from "./remove.js";

/**
 * 成员级失败：在 WorktreeError 上多带"是哪个成员"（容器里配置的路径，用户认得
 * 那一个）与回滚残留。路由据此组装 MultiDeriveError 响应体。
 */
export class MultiWorktreeError extends WorktreeError {
  constructor(
    reason: WorktreeError["reason"],
    message: string,
    detail: string | undefined,
    readonly memberDir: string,
    readonly leftover: string[] = []
  ) {
    super(reason, message, detail);
  }
}

/** 容器级校验失败（成员重复、basename 撞名、目录位置不合适……）：纯文本 409 */
export class MultiVetoError extends Error {}

/**
 * 成员清单的请求校验（创建/编辑容器时）。纯函数，宿主机 kind 未知——
 * 大小写别名与"同仓库两个子目录"这类要 rev-parse 才能判的，留给派生时的
 * vetoMultiRoots；嵌套成员（一个成员是另一个成员的子目录）合法：嵌套的独立仓库
 * 是真实存在的布局，同仓库子目录则会在派生时按"仓库根重复"拒掉。
 */
export function validateMemberList(
  repos: unknown
): { ok: true; repos: string[] } | { ok: false; error: string } {
  if (!Array.isArray(repos)) return { ok: false, error: "成员仓库必须是路径数组" };
  const out: string[] = [];
  for (const item of repos) {
    if (typeof item !== "string") return { ok: false, error: "成员仓库必须是路径数组" };
    const dir = item.trim().replace(/(?<=.)[\\/]+$/, "");
    if (!dir) return { ok: false, error: "成员仓库路径不能为空" };
    if (out.includes(dir)) return { ok: false, error: `成员仓库重复：${dir}` };
    out.push(dir);
  }
  if (out.length === 0) return { ok: false, error: "至少添加一个成员仓库" };
  if (out.length > MULTI_REPO_MAX) {
    return { ok: false, error: `成员仓库最多 ${MULTI_REPO_MAX} 个` };
  }
  return { ok: true, repos: out };
}

export interface MultiDeriveOutcome {
  centralDir: string;
  /** 与容器成员同序；dir = git 报的 worktree 路径，repoDir = 主 worktree 路径 */
  members: { dir: string; repoDir: string }[];
}

/** 成员级失败的便捷构造：把底下抛上来的 WorktreeError 换成带成员归因的版本 */
function memberError(err: unknown, memberDir: string, leftover: string[] = []): MultiWorktreeError {
  if (err instanceof WorktreeError) {
    return new MultiWorktreeError(err.reason, err.message, err.detail, memberDir, leftover);
  }
  return new MultiWorktreeError(
    "link-failed",
    worktreeFailureText("link-failed"),
    (err as Error).message,
    memberDir,
    leftover
  );
}

/**
 * 批量派生：对容器全部成员按统一分支名各建一棵 worktree，集中放进一个新目录。
 *
 * 抛三类错误，路由分别映射：
 * - MultiVetoError → 409 纯文本（容器级校验）；
 * - MultiWorktreeError → 按 reason 映射状态码 + member 归因（leftover 非空 ⇒ 502）；
 * - 其余（不应出现）→ 502。
 */
export async function deriveMultiWorktrees(
  host: GitHost,
  container: { name: string; members: MultiRepoMember[] },
  input: MultiWorktreeInput
): Promise<MultiDeriveOutcome> {
  const git = host.git;
  const kind = host.kind;
  const members = container.members;
  const branch = input.branch;

  // ---- 预检批 1：握手 + 逐成员仓库根 ----
  const first = await batchGit(host, [
    gc.versionArgs(git),
    ...members.map((m) => gc.repoRootArgs(git, m.dir)),
  ]);
  const [verRes, ...rootResults] = first.results;
  ensureVersion(host, verRes!, first.stderr);
  const roots: string[] = [];
  rootResults.forEach((res, i) => {
    try {
      roots.push(rootFrom(host, res, first.stderr));
    } catch (err) {
      throw memberError(err, members[i]!.dir);
    }
  });

  // ---- 预检批 2：逐成员 worktree 列表 → 主 worktree（派生基准），顺带检出占用 ----
  // 成员完全可能本身指向一棵 linked worktree，基准必须取主 worktree（单派生同一条规矩）
  const second = await batchGit(host, roots.map((r) => gc.worktreeListArgs(git, r)));
  const mains: string[] = [];
  const checkedOut: Map<string, string>[] = [];
  second.results.forEach((res, i) => {
    if (res.code !== 0) {
      throw memberError(
        new WorktreeError(
          "worktree-add-failed",
          worktreeFailureText("worktree-add-failed"),
          second.stderr.trim() || `退出码 ${res.code}`
        ),
        members[i]!.dir
      );
    }
    const entries = gc.parseWorktreeList(res.stdout);
    mains.push(mainWorktreeOf(entries, roots[i]!));
    checkedOut.push(checkedOutMap(entries));
  });

  const vetoRoots = vetoMultiRoots(kind, mains);
  if (vetoRoots) throw new MultiVetoError(vetoRoots);

  // ---- 集中目录与逐成员落点的静态检查（照单派生 routes 的顺序与措辞） ----
  const central = normalizeSep(
    kind,
    input.dir?.trim() || multiCentralPath(kind, mains[0]!, container.name, branch)
  );
  if (!isAbsolute(kind, central)) throw new MultiVetoError("目标目录必须是绝对路径");
  if (isUnc(kind, central)) throw new MultiVetoError("暂不支持在 UNC 网络路径上派生");
  if (pathDepth(kind, central) < 2) {
    throw new MultiVetoError("目标目录过于靠近文件系统根，请换一个位置");
  }
  const targets = mains.map((root) => memberWorktreePath(kind, central, root));
  mains.forEach((root, i) => {
    if (isAncestor(kind, central, root)) {
      throw new MultiVetoError(`目标目录包含仓库根（${root}），拒绝创建`);
    }
    const veto = vetoTargetDir(kind, root, central);
    if (veto === "path-inside-repo") {
      throw memberError(
        new WorktreeError(veto, `${worktreeFailureText(veto)}：源仓库会把它当成一堆未跟踪文件`),
        members[i]!.dir
      );
    }
    // 长度预算按成员落点算——集中目录本身短不代表最长的成员落点也短
    if (kind === "windows" && targets[i]!.length > WINDOWS_PATH_BUDGET) {
      throw memberError(
        new WorktreeError(
          "path-too-long",
          `${worktreeFailureText("path-too-long")}：${targets[i]!.length} 字符，Windows 上建得出来却删不掉`
        ),
        members[i]!.dir
      );
    }
  });

  // 集中目录必须是全新的：占用语义与单派生锁内复查一致（"接管已存在目录"不是
  // 用户要的语义），也让回滚与删除的 rmdir 有据可依——目录一定是我们建的
  if (await pathExists(host, central)) {
    throw new WorktreeError(
      "path-occupied",
      `${worktreeFailureText("path-occupied")}：${central}`,
      central
    );
  }
  const occupied = await pathsExist(host, targets);
  const occupiedAt = occupied.findIndex(Boolean);
  if (occupiedAt >= 0) {
    throw memberError(
      new WorktreeError(
        "path-occupied",
        worktreeFailureText("path-occupied"),
        targets[occupiedAt]
      ),
      members[occupiedAt]!.dir
    );
  }

  // ---- 预检批 3：分支存在性，解析每个成员的实际模式 ----
  // auto = 存在则检出、不存在则从各自 HEAD 新建；new/existing 两种严格模式下
  // 这批结果用来在动手之前拦住必然的失败（拦住一个就省一轮回滚）
  const exists = await batchGit(host, roots.map((r) => gc.branchExistsArgs(git, r, branch)));
  const modes: ("new-branch" | "existing-branch")[] = [];
  exists.results.forEach((res, i) => {
    if (res.code == null) {
      throw memberError(
        new WorktreeError("link-failed", worktreeFailureText("link-failed"), exists.stderr.trim()),
        members[i]!.dir
      );
    }
    const has = res.code === 0;
    if (input.mode === "new-branch" && has) {
      throw memberError(
        new WorktreeError("branch-exists", worktreeFailureText("branch-exists"), branch),
        members[i]!.dir
      );
    }
    if (input.mode === "existing-branch" && !has) {
      throw memberError(
        new WorktreeError("branch-unknown", worktreeFailureText("branch-unknown"), branch),
        members[i]!.dir
      );
    }
    const mode = input.mode === "auto" ? (has ? "existing-branch" : "new-branch") : input.mode;
    if (mode === "existing-branch") {
      const at = checkedOut[i]!.get(branch);
      if (at) {
        throw memberError(
          new WorktreeError(
            "branch-in-use",
            worktreeFailureText("branch-in-use"),
            `已检出在 ${at}`
          ),
          members[i]!.dir
        );
      }
    }
    modes.push(mode);
  });

  // ---- 创建：逐成员顺序进锁（不持多锁；全有或全无本就串行） ----
  // 每条 add 最长 TIMEOUT_ADD（2 分钟），MULTI_REPO_MAX 同时是整个请求的耗时上限。
  // Fastify 没有 request timeout 是既有事实，这里不另开熔断。
  const created: { dir: string; repoDir: string }[] = [];
  for (let i = 0; i < members.length; i++) {
    try {
      const reported = await withRepoLock(repoLockKey(host, mains[i]!), async () => {
        // 锁内再查一次占用：预检与创建之间用户可能刚建了同名目录（空目录也拒绝，
        // "接管一个已存在的空目录"不该继承"删项目会删这个目录"的承诺）
        if (await pathExists(host, targets[i]!)) {
          throw new WorktreeError(
            "path-occupied",
            worktreeFailureText("path-occupied"),
            targets[i]
          );
        }
        return addWorktree(host, mains[i]!, {
          mode: modes[i]!,
          branch,
          dir: targets[i]!,
        });
      });
      created.push({ dir: reported, repoDir: mains[i]! });
    } catch (err) {
      // 集中目录是本次新建的（上面已确认原先不存在），回滚要一并收掉
      const leftover = await rollbackCreatedWorktrees(host, created, central, true).catch(() => [
        central,
      ]);
      throw memberError(err, members[i]!.dir, leftover);
    }
  }

  return { centralDir: central, members: created };
}
