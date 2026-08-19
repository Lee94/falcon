/**
 * git 命令构造与输出解析。纯函数，零 I/O。
 *
 * 与 zellij/command.ts 同一条规矩：**只产出 argv 数组与 env 映射**，转义交给
 * buildGitCommandLine 一处完成——远端 POSIX 与远端 Windows 的转义规则不同，
 * 绝不在这里拼字符串。
 *
 * 所有命令一律带 `-C <dir>` 而不依赖执行层的 cwd：SSH exec 根本没有 cwd 概念
 * （要另外套一层 `cd X &&`），localExec 走 shell 也没传 cwd。-C 是两种执行环境下
 * 唯一都成立的写法。同理，后端进程**永远不 process.chdir 进 worktree**——cwd 一旦
 * 落在里面，Windows 上这个目录就永久删不掉。
 */

import {
  buildCommandLine,
  encodePowerShell,
  quotePosix,
  quotePowerShell,
  type HostKind,
} from "../zellij/host.js";
import { sepFor } from "./path.js";

/** 所有 git 调用共用的环境变量 */
export const GIT_ENV: Record<string, string> = {
  // 错误分类靠解析 stderr，而 git 会按 locale 翻译错误消息——不锁死 C 的话，
  // 一台中文系统上 "is already used by worktree at" 这条判据直接失效，
  // 用户看到的会是笼统的"操作失败"而不是"这个分支已经在别处检出了"。
  LC_ALL: "C",
  // 需要凭据时绝不弹交互提示：SSH exec 没有 tty，git 会挂在那里等到天荒地老，
  // 表现为请求永远不返回——最难查的一类故障。localExec 与 SshLink.exec 都没有超时，
  // Fastify 也没配 request timeout，所以这条与下面的 askpass 是唯一的防线。
  GIT_TERMINAL_PROMPT: "0",
};

/** 只读查询额外带上：避免 status 去写 index.lock（只读挂载 / 并发时会失败） */
export const GIT_ENV_RO: Record<string, string> = {
  ...GIT_ENV,
  GIT_OPTIONAL_LOCKS: "0",
};

/**
 * 必须**删掉**（而不是置空）的环境变量。
 *
 * `localExec` 用 `spawn(shell:true)` 继承 `process.env`。拉起后端的进程——IDE、
 * 进程管理器、或者干脆是某个 git hook——若设了这些，所有 git 调用会静默作用到
 * 别的仓库上。askpass 同理：继承来的 helper 会让"绝不弹交互提示"落空。
 *
 * 曾经写成 `GIT_DIR: ""` 塞进 env 里，在 Windows 上一路绿灯——因为 PowerShell 的
 * `$env:X = ''` 恰好等价于删除。但 POSIX 侧 `env GIT_DIR='' git ...` 是**设成空串**，
 * 实测直接 `fatal: not a git repository: ''`；`GIT_INDEX_FILE=''` 更狠，git 会拿
 * `.lock` 当索引文件，`status` 报出一整片并不存在的删除。也就是说这个功能在
 * 每一台 POSIX 宿主上都是坏的，而只在 Windows 上测根本看不出来。
 */
export const GIT_UNSET: readonly string[] = [
  "GIT_DIR",
  "GIT_WORK_TREE",
  "GIT_INDEX_FILE",
  "GIT_ASKPASS",
  "SSH_ASKPASS",
];

/**
 * core.quotepath=false：让 git 原样输出非 ASCII 路径，不转成 \346\226\207。
 * --no-pager：非 tty 下 git 本就不分页，但 core.pager 被显式配成 `less -F` 之类时
 * 仍会挂住，加一道保险。
 */
const BASE = ["-c", "core.quotepath=false", "--no-pager"];

const at = (git: string, dir: string, ...args: string[]): string[] => [
  git,
  ...BASE,
  "-C",
  dir,
  ...args,
];

/** 探测 git 本身是否可用。必须先跑它，理由见 repo.ts 的 repoRoot 注释。 */
export function versionArgs(git: string): string[] {
  return [git, "--version"];
}

/**
 * 仓库根。
 *
 * --path-format=absolute（git ≥ 2.31）挡住 MSYS2 / Git-Bash 环境下吐出
 * /d/code/xxx 这种不能直接喂给 Windows API 的路径。注意即便如此，Windows 上返回的
 * 仍是正斜杠（D:/code/mojito）——调用方必须过一遍 normalizeSep。
 */
export function repoRootArgs(git: string, dir: string): string[] {
  return at(git, dir, "rev-parse", "--path-format=absolute", "--show-toplevel");
}

/** detached 时输出字面量 "HEAD" */
export function headBranchArgs(git: string, dir: string): string[] {
  return at(git, dir, "rev-parse", "--abbrev-ref", "HEAD");
}

export function headShortShaArgs(git: string, dir: string): string[] {
  return at(git, dir, "rev-parse", "--short", "HEAD");
}

export function remoteListArgs(git: string, repo: string): string[] {
  return at(git, repo, "remote");
}

/**
 * 分支列表。字段分隔用 %09（TAB）：git check-ref-format 禁止分支名含 ASCII
 * 控制字符，所以 TAB 是安全分隔符，而空格不是（分支名可以含空格）。
 */
export function branchListArgs(git: string, repo: string): string[] {
  return at(
    git,
    repo,
    "for-each-ref",
    "--format=%(refname:short)%09%(upstream:short)%09%(HEAD)",
    "refs/heads",
    "refs/remotes"
  );
}

export function worktreeListArgs(git: string, repo: string): string[] {
  return at(git, repo, "worktree", "list", "--porcelain");
}

/**
 * 新建分支并检出到新 worktree。
 *
 * startPoint 显式传（缺省由调用方填 "HEAD"）而不是省略：行为相同，但 argv 长度固定，
 * 日志里不会出现"这条为什么少一个参数"的疑问。
 *
 * 不传 --track：从 origin/x 起分支时 branch.autoSetupMerge 默认就会建跟踪关系；
 * 用户显式关掉了那是他的配置，我们不该覆盖。
 *
 * core.longpaths=true 只影响 Windows，POSIX 上是个无害的未知配置（git 会忽略）。
 */
export function worktreeAddNewArgs(
  git: string,
  repo: string,
  path: string,
  branch: string,
  startPoint: string
): string[] {
  return at(git, repo, "-c", "core.longpaths=true", "worktree", "add", "-b", branch, path, startPoint);
}

/** 把一条已存在的本地分支检出到新 worktree */
export function worktreeAddExistingArgs(
  git: string,
  repo: string,
  path: string,
  branch: string
): string[] {
  return at(git, repo, "-c", "core.longpaths=true", "worktree", "add", path, branch);
}

/**
 * remove 只给**一个** --force。
 *
 * 第二个 --force 才会无视 `git worktree lock`，而 lock 是用户明说的"别碰"——
 * 正是 ADR 0001 里"绝不接管用户自有的东西"那条原则。锁住了就报给用户，不自动解锁。
 */
export function worktreeRemoveArgs(git: string, repo: string, path: string): string[] {
  return at(git, repo, "worktree", "remove", "--force", path);
}

export function worktreePruneArgs(git: string, repo: string): string[] {
  return at(git, repo, "worktree", "prune");
}

// ---------------- 脏状态 ----------------
//
// 决策一律看退出码，不解析文本：Windows 远端上原生命令的输出按控制台代码页解码
// （zh-CN 默认 936），而 SshLink.exec 用 toString("utf8")，中文路径会变乱码。
// 解析只用于**展示**样例清单，读不到就退化成计数，不影响任何判断。

/** 工作区有未暂存改动则退出码 1 */
export function diffDirtyArgs(git: string, dir: string): string[] {
  return at(git, dir, "diff", "--quiet");
}

/** 暂存区有改动则退出码 1 */
export function diffCachedDirtyArgs(git: string, dir: string): string[] {
  return at(git, dir, "diff", "--cached", "--quiet");
}

/** 未跟踪文件清单（一行一个）。输出非空即有未跟踪文件 */
export function untrackedArgs(git: string, dir: string): string[] {
  return at(git, dir, "ls-files", "--others", "--exclude-standard");
}

/** 展示用：已跟踪改动 + 未跟踪文件 */
export function statusArgs(git: string, dir: string): string[] {
  return at(git, dir, "status", "--porcelain");
}

/**
 * 被 .gitignore 忽略的条目。必须单独取：status --porcelain 默认不含它们，
 * 但 .env、本地 sqlite、上传目录会跟着一起被删——.env 通常是全世界唯一一份。
 */
export function statusIgnoredArgs(git: string, dir: string): string[] {
  return at(git, dir, "status", "--porcelain", "--ignored=matching");
}

/** 未推送提交数。没有 upstream 时退出码 128，调用方按 null 处理，不算错误。 */
export function aheadArgs(git: string, dir: string): string[] {
  return at(git, dir, "rev-list", "--count", "@{upstream}..HEAD");
}

/** 未拉取提交数。与 ahead 一样，没有 upstream 时退出码 128。 */
export function behindArgs(git: string, dir: string): string[] {
  return at(git, dir, "rev-list", "--count", "HEAD..@{upstream}");
}

/** 当前分支跟踪的远程。没有 upstream 时退出码 128。 */
export function upstreamArgs(git: string, dir: string): string[] {
  return at(git, dir, "rev-parse", "--abbrev-ref", "@{upstream}");
}

export function remoteVerboseArgs(git: string, dir: string): string[] {
  return at(git, dir, "remote", "-v");
}

/**
 * 最近提交。%at 是 unix 秒——相对时间在前端按界面语言格式化，
 * 不拿 git 的 %ar（那会跟 LC_ALL=C 一起变成英文）。
 */
export function logArgs(git: string, dir: string, n = 12): string[] {
  return at(git, dir, "log", "-n", String(n), "--format=%h%x09%an%x09%at%x09%s");
}

// ---------------- 命令行拼装 ----------------

/**
 * 把 git argv 拼成一条可交给宿主机执行的命令行。
 *
 * 就是 buildCommandLine，只是把默认 env 换成 GIT_ENV。
 *
 * git 对底座有两条硬要求，都已经在 zellij/host.ts 里满足了，这里只记为什么必须有：
 * - **退出码要如实传出**：`diff --quiet` 的全部语义就在退出码里（1 = 有改动），
 *   `rev-list @{upstream}..` 的 128 表示"没有 upstream"而不是出错。压成 0/1 会让
 *   "有没有未提交改动"这类判断悄悄失真——见 powerShellScript 的注释。
 * - **输出编码要钉成 UTF-8**：含中文的路径经 936 代码页解码后是乱码，而护栏靠路径
 *   比对——见 encodePowerShell 的注释。
 */
export function buildGitCommandLine(
  kind: HostKind,
  argv: string[],
  env: Record<string, string> = GIT_ENV
): string {
  return buildCommandLine(kind, argv, env, GIT_UNSET);
}

/**
 * 探测一个路径是否存在。
 *
 * 退出码恒为 0、答案在 stdout 里：非零码在 ExecFn 的约定里留给"命令根本没跑起来"，
 * 混用会让"路径不存在"和"SSH 断了"变成同一个信号。
 *
 * 本地远端走同一条路径而不是本地用 fs.existsSync：占用检查的对象是**宿主机**的
 * 文件系统，开两条分叉只会让两边的语义悄悄漂移。
 */
export function existsCommand(kind: HostKind, p: string): string {
  if (kind === "windows") {
    // -LiteralPath：-Path 会做通配符展开，路径里出现一个 [ 就足以让它什么都匹配不到
    return encodePowerShell(
      `if (Test-Path -LiteralPath ${quotePowerShell(p)}) { 'yes' } else { 'no' }`
    );
  }
  return `if [ -e ${quotePosix(p)} ]; then printf yes; else printf no; fi`;
}

/**
 * 一次往返探测多个路径是否存在，输出与入参同序的 yes / no 行。
 *
 * 分支列表动辄几十条，每条都单发一次 exec 就是几十个 SSH 往返——一个下拉框不值这个价。
 * 命令行长度有上限（Windows 上尤其紧），所以调用方要自己分片，见 EXISTS_BATCH。
 */
export function existsManyCommand(kind: HostKind, paths: string[]): string {
  if (kind === "windows") {
    const arr = paths.map(quotePowerShell).join(",");
    return encodePowerShell(
      `@(${arr}) | ForEach-Object { if (Test-Path -LiteralPath $_) { 'yes' } else { 'no' } }`
    );
  }
  const arr = paths.map(quotePosix).join(" ");
  return `for p in ${arr}; do if [ -e "$p" ]; then echo yes; else echo no; fi; done`;
}

/** 单批最多探测多少条路径。超出的分片发送，别把命令行撑爆。 */
export const EXISTS_BATCH = 60;

export function parseExistsMany(stdout: string): boolean[] {
  return lines(stdout)
    .map((l) => l.trim())
    .filter((l) => l === "yes" || l === "no")
    .map((l) => l === "yes");
}

/**
 * 读 worktree 目录里的 .git **文件**头部。
 *
 * linked worktree 的 .git 是个文件（内容形如 `gitdir: /repo/.git/worktrees/x`），
 * 普通仓库那里是目录，普通目录根本没有。删除护栏拿它当降级证据——最常见的破损场景
 * 是"用户把整个源仓库删了"，此时 worktree list 永远失败，但目录本身仍该被清理。
 */
export function gitFileHeadCommand(kind: HostKind, dir: string): string {
  const f = `${dir}${sepFor(kind)}.git`;
  if (kind === "windows") {
    return encodePowerShell(
      `if (Test-Path -LiteralPath ${quotePowerShell(f)} -PathType Leaf) ` +
        `{ Get-Content -LiteralPath ${quotePowerShell(f)} -TotalCount 1 }`
    );
  }
  return `if [ -f ${quotePosix(f)} ]; then head -c 200 ${quotePosix(f)}; fi`;
}

// ---------------- 输出解析 ----------------
//
// 一律按 /\r?\n/ 切行：Windows 远端上 PowerShell 用 \r\n 拼接输出。

function lines(stdout: string): string[] {
  return stdout.split(/\r?\n/);
}

export interface GitWorktreeEntry {
  path: string;
  head: string;
  /** refs/heads/x 去前缀；detached 时为 undefined */
  branch?: string;
  bare: boolean;
  locked: boolean;
  prunable: boolean;
}

/**
 * 解析 `git worktree list --porcelain`。
 *
 * 格式：记录之间空行分隔，每条含 `worktree <path>` 与 `HEAD <sha>`，外加
 * `branch refs/heads/<n>` | `detached` 之一，可能还有 `bare` / `locked [原因]` /
 * `prunable [原因]`。第一条永远是主 worktree。
 *
 * 不用 -z：NUL 分隔要穿过 powershell -EncodedCommand 的输出管线，编码行为不可靠。
 * 改用默认格式 + 自己处理 C 风格引号——十行代码，换掉一整类平台不确定性。
 */
export function parseWorktreeList(stdout: string): GitWorktreeEntry[] {
  const out: GitWorktreeEntry[] = [];
  let cur: Partial<GitWorktreeEntry> | null = null;
  const flush = () => {
    if (cur?.path) {
      out.push({ head: "", bare: false, locked: false, prunable: false, ...cur } as GitWorktreeEntry);
    }
    cur = null;
  };
  for (const line of lines(stdout)) {
    if (!line.trim()) {
      flush();
      continue;
    }
    const sp = line.indexOf(" ");
    const key = sp < 0 ? line.trim() : line.slice(0, sp);
    const val = sp < 0 ? "" : line.slice(sp + 1).trim();
    if (key === "worktree") {
      flush();
      cur = { path: unquoteCPath(val) };
    } else if (!cur) {
      continue;
    } else if (key === "HEAD") cur.head = val;
    else if (key === "branch") cur.branch = val.replace(/^refs\/heads\//, "");
    else if (key === "bare") cur.bare = true;
    else if (key === "locked") cur.locked = true;
    else if (key === "prunable") cur.prunable = true;
  }
  flush();
  return out;
}

/**
 * git 的 C 风格路径引号：整体加双引号，内部 \\ \" \t \n 与 \NNN（八进制）。
 * 我们已经加了 core.quotepath=false，但含双引号或控制字符的路径仍会被引起来。
 */
export function unquoteCPath(s: string): string {
  if (!s.startsWith('"')) return s;
  const body = s.slice(1, s.endsWith('"') ? -1 : undefined);
  let out = "";
  for (let i = 0; i < body.length; i++) {
    if (body[i] !== "\\") {
      out += body[i];
      continue;
    }
    const c = body[++i];
    if (c === "n") out += "\n";
    else if (c === "t") out += "\t";
    else if (c === "r") out += "\r";
    else if (c && c >= "0" && c <= "7") {
      out += String.fromCharCode(parseInt(body.slice(i, i + 3), 8));
      i += 2;
    } else out += c ?? "";
  }
  return out;
}

export interface ParsedBranch {
  name: string;
  remote: boolean;
  upstream?: string;
  head: boolean;
}

export function parseBranchList(stdout: string, remotes: string[]): ParsedBranch[] {
  const res: ParsedBranch[] = [];
  for (const line of lines(stdout)) {
    if (!line.trim()) continue;
    const [name, upstream, head] = line.split("\t");
    if (!name) continue;
    const remote = remotes.some((r) => name === r || name.startsWith(`${r}/`));
    // origin/HEAD 是指向默认分支的 symref，不是可检出的目标，列出来只会误导
    if (remote && name.endsWith("/HEAD")) continue;
    res.push({ name, remote, upstream: upstream || undefined, head: head === "*" });
  }
  return res;
}

/** git status --porcelain 的行数与前若干条路径（展示用，读不到不影响判断） */
export function parseStatus(stdout: string, sample = 8): { count: number; files: string[] } {
  const entries = parseStatusEntries(stdout);
  return { count: entries.length, files: entries.slice(0, sample).map((e) => e.path) };
}

export interface GitStatusEntry {
  path: string;
  origPath?: string;
  index: string;
  work: string;
}

/**
 * 侧栏 +N −M：文件还在的改动算 added，工作树里消失的算 deleted。
 *
 * 删了又加回来（AD）或重命名（R/C）不算删除——人看见的是一个还在的文件。
 */
export function countStatusChanges(entries: GitStatusEntry[]): {
  added: number;
  deleted: number;
} {
  let added = 0;
  let deleted = 0;
  for (const e of entries) {
    if (isStatusDeleted(e)) deleted++;
    else added++;
  }
  return { added, deleted };
}

function isStatusDeleted(e: GitStatusEntry): boolean {
  if (e.index !== "D" && e.work !== "D") return false;
  return (
    e.index !== "A" &&
    e.work !== "A" &&
    e.index !== "R" &&
    e.work !== "R" &&
    e.index !== "C" &&
    e.work !== "C"
  );
}

/**
 * 完整解析 `git status --porcelain`。
 *
 * 前两列永远是 XY，第三列是空格，后面才是路径。重命名 / 复制是
 * `XY orig -> new`；只有 XY 里真有 R/C 才按箭头拆，免得路径里刚好有 ` -> `。
 */
export function parseStatusEntries(stdout: string): GitStatusEntry[] {
  const out: GitStatusEntry[] = [];
  for (const line of lines(stdout)) {
    if (line.length < 3) continue;
    const index = line[0] ?? " ";
    const work = line[1] ?? " ";
    const rest = unquoteCPath(line.slice(3).trim());
    if (!rest) continue;
    const renamed = index === "R" || index === "C" || work === "R" || work === "C";
    const arrow = renamed ? rest.indexOf(" -> ") : -1;
    if (arrow >= 0) {
      out.push({
        path: unquoteCPath(rest.slice(arrow + 4).trim()),
        origPath: unquoteCPath(rest.slice(0, arrow).trim()),
        index,
        work,
      });
    } else {
      out.push({ path: rest, index, work });
    }
  }
  return out;
}

/** `git remote -v`：每个 remote 只取 fetch 那一行 */
export function parseRemotes(stdout: string): { name: string; url: string }[] {
  const seen = new Set<string>();
  const out: { name: string; url: string }[] = [];
  for (const line of lines(stdout)) {
    const m = /^(\S+)\s+(\S+)\s+\((fetch|push)\)/.exec(line.trim());
    if (!m || m[3] !== "fetch" || seen.has(m[1]!)) continue;
    seen.add(m[1]!);
    out.push({ name: m[1]!, url: m[2]! });
  }
  return out;
}

export interface GitLogEntry {
  sha: string;
  author: string;
  authoredAt: number;
  subject: string;
}

/** `git log --format=%h\\t%an\\t%at\\t%s`。subject 里可能有 TAB，只切前三列。 */
export function parseLog(stdout: string): GitLogEntry[] {
  const out: GitLogEntry[] = [];
  for (const line of lines(stdout)) {
    if (!line) continue;
    const [sha, author, at, ...rest] = line.split("\t");
    if (!sha || !author || !at) continue;
    const sec = Number(at);
    out.push({
      sha,
      author,
      authoredAt: Number.isFinite(sec) ? sec * 1000 : 0,
      subject: rest.join("\t"),
    });
  }
  return out;
}

/** `status --porcelain --ignored=matching` 里 `!!` 开头的那些 */
export function countIgnored(stdout: string): number {
  return lines(stdout).filter((l) => l.startsWith("!!")).length;
}
