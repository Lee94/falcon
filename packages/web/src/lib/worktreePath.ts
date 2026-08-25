/**
 * 派生目录的**预览**计算。
 *
 * 权威实现在服务端 git/path.ts（branchSlug / nameSlug / siblingWorktreePath /
 * multiCentralPath），这里只是让用户敲分支名时看得见结果。提交时预览值不上送
 * （单派生仅在用户手动改过目录时才发 dir），预览与真实值万一漂移也绝不会建到
 * 别处去。改动 slug 规则时请两边对照。
 */

/** 分支名 → 目录名后缀。镜像服务端 branchSlug */
export function branchSlugPreview(branch: string): string {
  const cleaned = branch
    .replace(/[/"<>|\s]+/g, "-")
    .replace(/-{2,}/g, "-")
    .replace(/^[-.]+/, "")
    .replace(/[-.]+$/, "");
  return (
    Array.from(cleaned)
      .slice(0, 48)
      .join("")
      .replace(/[-.]+$/, "") || "wt"
  );
}

/** 容器名 → 集中目录名前半段。镜像服务端 nameSlug（比 branchSlug 多挡 : * ? \） */
export function nameSlugPreview(name: string): string {
  const cleaned = name
    .replace(/[/\\:*?"<>|\s\u0000-\u001f]+/g, "-")
    .replace(/-{2,}/g, "-")
    .replace(/^[-.]+/, "")
    .replace(/[-.]+$/, "");
  return (
    Array.from(cleaned)
      .slice(0, 48)
      .join("")
      .replace(/[-.]+$/, "") || "multi"
  );
}

/** 单仓库派生：<仓库根父目录>/<仓库根基名>-<分支slug>。镜像 siblingWorktreePath */
export function previewDir(repoDir: string, branch: string): string {
  if (!repoDir || !branch) return "";
  const sep = repoDir.includes("\\") ? "\\" : "/";
  const root = repoDir.replace(/[\\/]+$/, "");
  const i = Math.max(root.lastIndexOf("\\"), root.lastIndexOf("/"));
  const parent = i <= 0 ? root : root.slice(0, i);
  const base = root.slice(i + 1);
  return `${parent}${sep}${base}-${branchSlugPreview(branch)}`;
}

/** 批量派生的集中目录：<baseDir>/<容器名slug>-<分支slug>。镜像 multiCentralPath */
export function previewMultiDir(baseDir: string, containerName: string, branch: string): string {
  if (!baseDir || !branch) return "";
  const sep = baseDir.includes("\\") ? "\\" : "/";
  const root = baseDir.replace(/[\\/]+$/, "");
  return `${root}${sep}${nameSlugPreview(containerName)}-${branchSlugPreview(branch)}`;
}
