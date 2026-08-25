/**
 * git / worktree 操作的结构化错误。
 *
 * 形状与 zellij/install.ts 的 InstallError 完全一致：reason 是定义在 shared 的
 * 闭集联合类型，前后端共用，前端据此渲染具体说明——用户需要知道该去查什么，
 * 而不是收到一句"操作失败"。
 *
 * 单独一个文件是为了避免 host.ts ↔ repo.ts 的循环 import。
 */

import type { WorktreeFailure } from "@falcon/shared";

export class WorktreeError extends Error {
  constructor(
    readonly reason: WorktreeFailure,
    message: string,
    readonly detail?: string
  ) {
    super(message);
  }
}

const FAILURE_TEXT: Record<WorktreeFailure, string> = {
  "git-missing": "宿主机上没有 git，或 git 不在 PATH 中",
  "not-a-repo": "项目工作目录不在任何 git 仓库里",
  "no-working-dir": "项目没有指定工作目录，无从判断在哪个仓库里",
  "branch-in-use": "该分支已在另一个 worktree 中检出",
  "branch-exists": "已存在同名分支",
  "branch-unknown": "找不到这个分支或基点",
  "path-occupied": "目标目录已存在",
  "path-inside-repo": "目标目录落在仓库内部",
  "path-too-long": "目标路径过长",
  "worktree-add-failed": "git worktree add 失败",
  "link-failed": "命令没能在宿主机上跑起来",
};

export function worktreeFailureText(reason: WorktreeFailure): string {
  return FAILURE_TEXT[reason];
}

/**
 * 从 git 的多行输出里挑出真正有信息的那一行。
 *
 * 直接取第一行是不行的：`worktree add` 失败时 stderr 的第一行是
 * "Preparing worktree (checking out 'x')"，真正的原因在第二行的 `fatal:` 上。
 * 拿第一行给用户，等于告诉他"操作失败了，因为我们正在准备操作"。
 */
export function gitErrorLine(text: string): string {
  const lines = text
    .split(/\r?\n/)
    .map((l) => l.trim())
    .filter((l) => l.length > 0);
  return lines.find((l) => /^(fatal|error|warning):/i.test(l)) ?? lines[0] ?? "";
}
