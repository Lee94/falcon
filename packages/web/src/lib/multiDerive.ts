/**
 * 批量派生表单的纯逻辑：按模式与分支名推演每个成员会发生什么。
 * 只是**预演**——权威判定在服务端（预检 + add 时的 TOCTOU 兜底），
 * 这里的结论用于提交前把可预见的失败摆在成员行上，别让用户白交一张表。
 */

import type { RepoInfo, WorktreeFailure } from "@falcon/shared";

export type MultiMode = "new-branch" | "existing-branch" | "auto";

export type MemberAction =
  /** 成员环境不可派生（没装 git / 不是仓库 / 连不上） */
  | { kind: "blocked"; reason?: WorktreeFailure; detail?: string }
  /** 将从该成员自己的 HEAD 新建分支 */
  | { kind: "create" }
  /** 将检出该成员已有的同名分支 */
  | { kind: "checkout" }
  /** new-branch 模式下同名分支已存在 */
  | { kind: "branch-exists" }
  /** existing-branch 模式下该成员没有这条分支 */
  | { kind: "branch-missing" }
  /** 分支已在该成员的另一棵 worktree 中检出 */
  | { kind: "branch-in-use"; at: string };

export function evaluateMember(info: RepoInfo, mode: MultiMode, branch: string): MemberAction {
  if (!info.derivable) return { kind: "blocked", reason: info.reason, detail: info.detail };
  const hit = info.branches.find((b) => !b.remote && b.name === branch);
  const effMode = mode === "auto" ? (hit ? "existing-branch" : "new-branch") : mode;
  if (effMode === "new-branch") {
    return hit ? { kind: "branch-exists" } : { kind: "create" };
  }
  if (!hit) return { kind: "branch-missing" };
  if (hit.checkedOutAt) return { kind: "branch-in-use", at: hit.checkedOutAt };
  return { kind: "checkout" };
}

/** 提交必须被拦下的动作（全有或全无：一个成员会失败就整单失败） */
export function actionBlocksSubmit(a: MemberAction): boolean {
  return a.kind !== "create" && a.kind !== "checkout";
}

/**
 * existing-branch 模式的候选：全体成员共有的本地分支交集。
 * 任一成员里已检出的标记 usedAt（下拉里禁用）。顺序取第一个成员的分支顺序。
 */
export function commonLocalBranches(
  members: { info: RepoInfo }[]
): { name: string; usedAt?: string }[] {
  const lists = members.map((m) => m.info.branches.filter((b) => !b.remote));
  if (lists.length === 0) return [];
  const nameSets = lists.map((l) => new Set(l.map((b) => b.name)));
  return lists[0]!
    .filter((b) => nameSets.every((s) => s.has(b.name)))
    .map((b) => {
      for (const l of lists) {
        const hit = l.find((x) => x.name === b.name);
        if (hit?.checkedOutAt) return { name: b.name, usedAt: hit.checkedOutAt };
      }
      return { name: b.name };
    });
}

/** 成员路径末段，成员行与脏文件样例的展示名。posix 与 windows 分隔符都认 */
export function memberBasename(dir: string): string {
  const trimmed = dir.replace(/[\\/]+$/, "");
  const i = Math.max(trimmed.lastIndexOf("/"), trimmed.lastIndexOf("\\"));
  return i >= 0 ? trimmed.slice(i + 1) : trimmed;
}
