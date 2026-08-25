/**
 * 多仓库容器表单里的路径小工具。纯函数、零 I/O。
 *
 * 只做**建议值**：算出来的结果显式填进表单字段、随请求原样上送，
 * 服务端不重算——所以这里的分隔符猜测（看第一条路径长什么样）漂移无害。
 */

/**
 * 一组成员路径的最近公共父目录，给「会话初始 cwd」当建议值。
 * 没有有用的公共父目录（不同盘符、只剩文件系统根）返回 ""——
 * 填一个 "/" 进去当 cwd 还不如留空走家目录。
 */
export function commonParentDir(paths: string[]): string {
  const clean = paths.map((p) => p.trim().replace(/[\\/]+$/, "")).filter(Boolean);
  if (clean.length === 0) return "";
  const windows = clean[0]!.includes("\\") || /^[A-Za-z]:/.test(clean[0]!);
  const norm = (s: string) => (windows ? s.toLowerCase() : s);
  const segLists = clean.map((p) => p.split(/[\\/]+/));
  const first = segLists[0]!;
  let common = first.length;
  for (const segs of segLists.slice(1)) {
    let i = 0;
    while (i < common && i < segs.length && norm(segs[i]!) === norm(first[i]!)) i++;
    common = i;
  }
  // 保留第一条路径的原始大小写；posix 的首段是 ""（根），windows 是盘符
  const segs = first.slice(0, common);
  if (segs.length < 2) return "";
  return segs.join(windows ? "\\" : "/");
}
