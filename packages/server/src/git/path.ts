/**
 * 宿主机路径运算与分支 slug。全部是纯函数，零 I/O。
 *
 * **不用 node:path**——与 zellij/host.ts 手工拼 sep 的决定同一个理由：后端跑在
 * Windows 上也可能要为一台 Linux 远端构造路径，node:path 会用后端自己的平台规则，
 * 方向直接反了。kind 一律由 localKind() 或 SshLink 的探测给出。
 *
 * 这里还住着删除护栏的静态断言（vetoRemoval，见 remove.ts 调用处）所依赖的全部
 * 比较原语。它们必须能被单独盯着读、单独手工构造脏数据去打，所以刻意保持纯净：
 * 在一个没有测试框架、唯一门禁是 tsc 的仓库里，可审查性比复用度重要。
 */

import type { WorktreeFailure } from "@falcon/shared";
import type { HostKind } from "../zellij/host.js";

export function sepFor(kind: HostKind): string {
  return kind === "windows" ? "\\" : "/";
}

/**
 * 分隔符归一。
 *
 * windows：/ → \ 并合并重复分隔符（开头的 \\ 是 UNC，保留两根）。
 * posix：**什么都不做**——反斜杠在 Linux 上是合法文件名字符，顺手把它换成 /
 * 会当场把一个叫 `a\b` 的目录变成 `a/b`。
 */
export function normalizeSep(kind: HostKind, p: string): string {
  if (kind !== "windows") return p;
  const unc = p.startsWith("\\\\") || p.startsWith("//");
  const body = p.replace(/\//g, "\\").replace(/\\{2,}/g, "\\");
  return unc ? `\\${body}` : body;
}

export function isAbsolute(kind: HostKind, p: string): boolean {
  if (kind === "windows") {
    const n = normalizeSep("windows", p);
    return /^[A-Za-z]:\\/.test(n) || n.startsWith("\\\\");
  }
  return p.startsWith("/");
}

/** UNC 路径（\\server\share\...）。v1 不支持派生：\\server\share 才是"根"，深度规则不同 */
export function isUnc(kind: HostKind, p: string): boolean {
  return kind === "windows" && normalizeSep("windows", p).startsWith("\\\\");
}

/** 去掉尾部分隔符后按 sep 切成非空段。windows 的第一段是盘符（`C:`） */
export function segments(kind: HostKind, p: string): string[] {
  const n = normalizeSep(kind, p).replace(/[\\/]+$/, "");
  return n.split(kind === "windows" ? "\\" : "/").filter(Boolean);
}

/**
 * 盘符 / 根之后还剩几段。C:\ → 0，C:\a → 1，/ → 0，/home → 1。
 *
 * 删除护栏的深度门槛用它：真实的 worktree 目录都是"某个仓库目录的同级"，
 * 而仓库不会直接躺在盘符根上。门槛设在 2 能挡掉绝大多数脏数据造成的灾难，
 * 代价只是拒绝一种没人真会用的布局。
 */
export function pathDepth(kind: HostKind, p: string): number {
  const segs = segments(kind, p);
  return kind === "windows" ? Math.max(0, segs.length - 1) : segs.length;
}

export function basenameOf(kind: HostKind, p: string): string {
  const segs = segments(kind, p);
  return segs[segs.length - 1] ?? "";
}

export function dirnameOf(kind: HostKind, p: string): string {
  const n = normalizeSep(kind, p).replace(/[\\/]+$/, "");
  const sep = sepFor(kind);
  const i = n.lastIndexOf(sep);
  if (i < 0) return n;
  if (kind === "posix") return i === 0 ? "/" : n.slice(0, i);
  // windows：C:\a → C:\（保留根的那根分隔符），C:\a\b → C:\a
  const head = n.slice(0, i);
  return /^[A-Za-z]:$/.test(head) ? `${head}\\` : head;
}

export function joinPath(kind: HostKind, ...parts: string[]): string {
  const sep = sepFor(kind);
  const joined = parts
    .filter((s) => s.length > 0)
    .map((s, i) => (i === 0 ? s.replace(/[\\/]+$/, "") : s.replace(/^[\\/]+|[\\/]+$/g, "")))
    .join(sep);
  return normalizeSep(kind, joined);
}

/**
 * 路径相等。windows 上**大小写不敏感**，尾分隔符不计。
 *
 * 必须有：git 在 Windows 上返回正斜杠（D:/code/falcon），而 DB 里存的是用户输入的
 * D:\code\falcon，直接 === 恒不成立。真正的危险不是功能不工作，而是有人为了让它
 * 工作去放宽断言——所以归一化集中在这里一处，断言只比归一化后的形式。
 */
export function samePath(kind: HostKind, a: string, b: string): boolean {
  return canonKey(kind, a) === canonKey(kind, b);
}

/** 比较用的规范形式。只用于比较，不要拿它当路径发给宿主机。 */
export function canonKey(kind: HostKind, p: string): string {
  const n = normalizeSep(kind, p).replace(/[\\/]+$/, "");
  return kind === "windows" ? n.toLowerCase() : n;
}

/** ancestor 是不是 child 的**严格**祖先（相等返回 false） */
export function isAncestor(kind: HostKind, ancestor: string, child: string): boolean {
  const a = segments(kind, canonKey(kind, ancestor));
  const c = segments(kind, canonKey(kind, child));
  if (a.length >= c.length) return false;
  return a.every((s, i) => s === c[i]);
}

/**
 * 分支名 → 目录名后缀。
 *
 * 只替换**文件系统真的不接受**的字符，不做 ASCII 白名单。
 * git 的 check-ref-format 已经挡掉了空格、`~ ^ : ? * [`、反斜杠和控制字符，
 * 剩下必须自己处理的只有 `/`（路径分隔符）和 Windows 独有的 `" < > |`。
 *
 * 曾经写成"只保留 [A-Za-z0-9._-]"，结果 `功能/中文` 这类纯非 ASCII 分支名会整条
 * 塌成同一个兜底值——两条中文分支互撞、目录名还完全看不出是哪条分支。CJK 在 NTFS
 * 与 ext4 上都是合法文件名；命令行方向经 quotePosix / -EncodedCommand 都是字节安全的，
 * 而回读方向即使真的乱码，落库的也是 **git 自己报的路径**（见创建流程），
 * 于是护栏比对仍然自洽——最坏是拒删并告警，不会删错。
 *
 * 尾部的 . 和空格必须去掉：Windows 创建时会**静默**吃掉它们，于是 DB 里记的是
 * `foo-feat.`、磁盘上是 `foo-feat`，删除护栏拿路径比对就永远对不上，最后表现为
 * "目录删不掉，只给了个 warning"。截断之后要再去一次，否则截口可能又露出一个 -。
 * 截断按**码点**而不是 UTF-16 单元，免得把一个代理对劈成半个字符。
 *
 * 注意这个映射**不是单射**：feature/foo 与 feature-foo 会撞同一个目录名。撞了就
 * 报 path-occupied 让用户自己改名——自动加后缀会让目录名与分支名失去对应关系。
 */
export function branchSlug(branch: string): string {
  const cleaned = branch
    .replace(/[/"<>|\s]+/g, "-")
    .replace(/-{2,}/g, "-")
    .replace(/^[-.]+/, "")
    .replace(/[-.]+$/, "");
  const s = Array.from(cleaned)
    .slice(0, 48)
    .join("")
    .replace(/[-.]+$/, "");
  return s || "wt";
}

/**
 * 同级平铺路径：<仓库根的父目录>/<仓库根基名>-<分支 slug>
 *
 * 基准点是**仓库根**，不是项目的 workingDir。项目工作目录完全可能是仓库里的一个
 * 子目录（monorepo 里指到 packages/server 很常见），在那儿旁边建 worktree 会把它
 * 建进仓库自己里面——源仓库的 git status 里立刻多出一坨未跟踪文件。
 */
export function siblingWorktreePath(
  kind: HostKind,
  repoRoot: string,
  branch: string
): string {
  const root = normalizeSep(kind, repoRoot).replace(/[\\/]+$/, "");
  return joinPath(kind, dirnameOf(kind, root), `${basenameOf(kind, root)}-${branchSlug(branch)}`);
}

/**
 * 容器名 → 集中目录名的前半段。比 branchSlug 更狠：分支名有 git 的
 * check-ref-format 兜底（空格、`~ ^ : ? * [`、反斜杠、控制字符都进不来），
 * 而容器名是任意用户字符串，什么都可能有——所以 POSIX/Windows 两边的非法与
 * 危险字符（`/ \ : * ? " < > |`、空白、控制字符）全部替换成 -。
 * 尾部 . 与空格必须去掉、截断按码点，理由同 branchSlug（Windows 静默吞字符、
 * 代理对别劈成两半）。空了兜底 "multi"。
 */
export function nameSlug(name: string): string {
  const cleaned = name
    .replace(/[/\\:*?"<>|\s\u0000-\u001f]+/g, "-")
    .replace(/-{2,}/g, "-")
    .replace(/^[-.]+/, "")
    .replace(/[-.]+$/, "");
  const s = Array.from(cleaned)
    .slice(0, 48)
    .join("")
    .replace(/[-.]+$/, "");
  return s || "multi";
}

/**
 * 批量派生的集中目录默认位置：<第一个成员仓库根的父目录>/<容器名slug>-<分支slug>。
 * 基准取仓库根而不是成员配置的路径，与 siblingWorktreePath 的理由相同。
 */
export function multiCentralPath(
  kind: HostKind,
  firstRepoRoot: string,
  containerName: string,
  branch: string
): string {
  const root = normalizeSep(kind, firstRepoRoot).replace(/[\\/]+$/, "");
  return joinPath(kind, dirnameOf(kind, root), `${nameSlug(containerName)}-${branchSlug(branch)}`);
}

/** 成员 worktree 的落点：<集中目录>/<仓库根 basename>。basename 不做 slug——它已经是一个真实存在的目录名 */
export function memberWorktreePath(kind: HostKind, centralDir: string, repoRoot: string): string {
  return joinPath(kind, centralDir, basenameOf(kind, repoRoot));
}

/**
 * 批量派生前对 N 个已解析出的仓库根做的容器级否决。返回给用户看的一句话，null = 通过。
 *
 * - 仓库判重按 canonKey：两个成员配置成同一仓库的不同子目录、或 Windows 上的
 *   大小写别名，rev-parse 之后都会在这里现形。
 * - basename 撞名也按 canonKey：两棵 worktree 要以仓库根 basename 平铺进同一个
 *   集中目录，Windows 上 Repo 与 repo 是同一个目录。
 * 这两条都只能在派生时判——容器创建时成员可以是仓库子目录、宿主 kind 也未必已知。
 */
export function vetoMultiRoots(kind: HostKind, roots: string[]): string | null {
  const seenRoot = new Map<string, string>();
  const seenBase = new Map<string, string>();
  for (const root of roots) {
    if (!isAbsolute(kind, root)) return `仓库根不是绝对路径：${root}`;
    const rootKey = canonKey(kind, root);
    const dupRoot = seenRoot.get(rootKey);
    if (dupRoot) return `两个成员指向同一个仓库（${dupRoot}），请去掉一个`;
    seenRoot.set(rootKey, root);
    const base = basenameOf(kind, root);
    const baseKey = canonKey(kind, base);
    const dupBase = seenBase.get(baseKey);
    if (dupBase) {
      return `成员仓库目录同名（${dupBase} 与 ${root}），无法在同一个集中目录里平铺，请先给仓库目录改名`;
    }
    seenBase.set(baseKey, root);
  }
  return null;
}

/**
 * Windows 路径长度上限。
 *
 * <repo>-<长分支名>\node_modules\... 极易超过 MAX_PATH(260)，而超了之后 rd /s 直接
 * 失败——宁可在创建时就拒绝，也不要建完发现删不掉。留 60 字符给仓库内部的深路径。
 */
export const WINDOWS_PATH_BUDGET = 200;

/**
 * 目标目录的静态否决。返回 null 表示可以建。
 *
 * 只管"这个位置本身合不合适"，不碰文件系统——占用检查是另一回事（要发命令）。
 * 与删除护栏共用同一套比较原语，免得两边的"相等/包含"语义悄悄漂移。
 */
export function vetoTargetDir(
  kind: HostKind,
  mainWorktree: string,
  dir: string
): WorktreeFailure | null {
  // 建在仓库里面：源仓库的 git status 里会立刻多出一坨未跟踪文件。
  // 等于仓库根更糟——那是要把仓库自己盖掉。
  if (samePath(kind, dir, mainWorktree) || isAncestor(kind, mainWorktree, dir)) {
    return "path-inside-repo";
  }
  if (kind === "windows" && dir.length > WINDOWS_PATH_BUDGET) return "path-too-long";
  return null;
}
