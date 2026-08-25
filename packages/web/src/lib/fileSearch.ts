/**
 * Quick Open 的客户端过滤。后端给整份路径清单，这里按 VS Code 的直觉打分：
 * 文件名命中优于路径命中，前缀优于包含，子序列垫底。
 */

export const FILE_SEARCH_LIMIT = 50;

export function basename(path: string): string {
  const i = path.lastIndexOf("/");
  return i < 0 ? path : path.slice(i + 1);
}

export function dirname(path: string): string {
  const i = path.lastIndexOf("/");
  return i < 0 ? "" : path.slice(0, i);
}

function subseq(hay: string, needle: string): boolean {
  let i = 0;
  for (const c of hay) {
    if (c === needle[i]) i++;
    if (i === needle.length) return true;
  }
  return false;
}

/** 越小越靠前。对不上返回 null */
export function scorePath(path: string, needle: string): number | null {
  const q = needle.trim().toLowerCase();
  if (!q) return 0;
  const base = basename(path).toLowerCase();
  const lower = path.toLowerCase();
  if (base === q) return 0;
  if (base.startsWith(q)) return 1;
  if (base.includes(q)) return 2;
  if (lower.includes(q)) return 3;
  if (subseq(base, q)) return 4;
  if (subseq(lower, q)) return 5;
  return null;
}

export function filterFiles(
  paths: readonly string[],
  query: string,
  limit = FILE_SEARCH_LIMIT
): string[] {
  const needle = query.trim();
  if (!needle) return paths.slice(0, limit);
  const scored: { path: string; score: number }[] = [];
  for (const path of paths) {
    const score = scorePath(path, needle);
    if (score == null) continue;
    scored.push({ path, score });
  }
  scored.sort(
    (a, b) => a.score - b.score || a.path.length - b.path.length || a.path.localeCompare(b.path)
  );
  return scored.slice(0, limit).map((x) => x.path);
}
