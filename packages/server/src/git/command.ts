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
 * 仍是正斜杠（D:/code/falcon）——调用方必须过一遍 normalizeSep。
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
 *
 * 末列 %(symref) 是用来认出 origin/HEAD 的。不能按名字认：
 * `refs/remotes/origin/HEAD` 的 `%(refname:short)` 是 **origin**（git 取的是
 * 最短无歧义名），不是 origin/HEAD——于是下拉里会冒出一条叫 "origin" 的
 * 假分支，检出它必然失败。symref 只有符号引用才非空，判据是准的。
 */
export function branchListArgs(git: string, repo: string): string[] {
  return at(
    git,
    repo,
    "for-each-ref",
    "--format=%(refname:short)%09%(upstream:short)%09%(HEAD)%09%(symref)",
    "refs/heads",
    "refs/remotes"
  );
}

export function worktreeListArgs(git: string, repo: string): string[] {
  return at(git, repo, "worktree", "list", "--porcelain");
}

/**
 * 本地分支是否存在：退出码即答案（0 = 存在）。批量派生的 auto 模式与预检用。
 * --verify 拒绝前缀匹配之类的猜测，--quiet 让"不存在"保持静默、不往 stderr 写 fatal。
 */
export function branchExistsArgs(git: string, repo: string, branch: string): string[] {
  return at(git, repo, "rev-parse", "--verify", "--quiet", `refs/heads/${branch}`);
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

/**
 * Quick Open 的文件清单：已跟踪 + 未跟踪，排除 gitignore。
 * 路径相对 `-C` 的目录（工作目录），正斜杠分隔。
 */
export function lsFilesIndexArgs(git: string, dir: string): string[] {
  return at(git, dir, "ls-files", "-co", "--exclude-standard");
}

/** 一行一个相对路径。空行丢掉；Windows 偶发反斜杠收成 `/` */
export function parseLsFiles(stdout: string): string[] {
  const out: string[] = [];
  for (const raw of stdout.split(/\r?\n/)) {
    const line = raw.replace(/\r$/, "");
    if (!line) continue;
    out.push(line.replaceAll("\\", "/"));
  }
  return out;
}

/** 展示用：已跟踪改动 + 未跟踪文件 */
export function statusArgs(git: string, dir: string): string[] {
  return at(git, dir, "status", "--porcelain");
}

/**
 * 同上，但把未跟踪**目录**展开成一个个文件。
 *
 * 默认的 porcelain 会把整个未跟踪目录折叠成一条 `?? sub/`：那既算不出行数，
 * 在目录树视图里也会变成一个名字带斜杠的假"文件"。
 *
 * 侧栏那条轮询（describeGitChanges）**不用**这个：它每 8 秒问一遍所有项目，
 * 而展开一个巨大的未跟踪目录（漏进来的 node_modules 之类）要枚举几万条。
 * 代价是面板的文件数可能比侧栏徽标大，那是面板更准确，不是它算错了。
 */
export function statusAllArgs(git: string, dir: string): string[] {
  return at(git, dir, "status", "--porcelain", "--untracked-files=all");
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
 * git 内建的空树对象。仓库还没有任何提交（无 HEAD）时拿它当 diff 基准，
 * 语义不变：工作区对比"一无所有"。
 */
export const EMPTY_TREE = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/**
 * 单个文件相对 base（HEAD 或空树）的 diff：暂存 + 未暂存合在一起看。
 *
 * 重命名要把新旧两个路径都传进 pathspec。实测 worktree↔tree 的 diff 不做
 * rename 配对（那是 --cached 视图的事），出来的是删除 + 新增两段——两半都在，
 * 这就是如实的答案；只传新路径会丢掉删除那一半。
 */
export function diffFileArgs(
  git: string,
  dir: string,
  base: string,
  path: string,
  origPath?: string
): string[] {
  const paths = origPath ? [origPath, path] : [path];
  return at(git, dir, "diff", base, "--", ...paths);
}

/**
 * 未跟踪文件的伪 diff：与空文件比对。
 *
 * `/dev/null` 是 git 在 diff --no-index 里特判的字面量（当空输入），Windows 上
 * 同样成立，不必换成 NUL。有差异时退出码 1——这是答案不是错误，调用方用 probeGit；
 * 空的未跟踪文件两边相同，退出码 0、无输出。
 */
export function diffUntrackedArgs(git: string, dir: string, path: string): string[] {
  return at(git, dir, "diff", "--no-index", "--", "/dev/null", path);
}

/**
 * 最近提交。%at 是 unix 秒——相对时间在前端按界面语言格式化，
 * 不拿 git 的 %ar（那会跟 LC_ALL=C 一起变成英文）。
 */
export function logArgs(git: string, dir: string, n = 12): string[] {
  return at(git, dir, "log", "-n", String(n), "--format=%h%x09%an%x09%at%x09%s");
}

// ---------------- History 面板 ----------------

/**
 * 只看这三类 ref，**不用 `--all`**。
 *
 * --all 会把 refs/ 下的一切都算进来，包括 IDE 写的私有 ref——JetBrains 的
 * Local History 就挂在 refs/jb/* 下，每按几下保存就是一条提交。实测本仓库
 * `git log --all` 的头几条全是 "Local History"，把真实历史挤到了看不见的地方。
 * refs/stash 同理。
 */
const HISTORY_REFS = ["--branches", "--remotes", "--tags"];

/** History 每页条数与作者聚合的采样深度 */
export const LOG_PAGE = 60;
export const AUTHOR_SAMPLE = 400;

export interface LogQuery {
  /** 只看某条分支（Branch 下拉）。缺省看 HISTORY_REFS */
  rev?: string;
  /** 提交信息搜索（字面量，不是正则） */
  grep?: string;
  /** 作者筛选（字面量） */
  author?: string;
  skip?: number;
  limit?: number;
}

/**
 * History 列表。
 *
 * `-F`（--fixed-strings）对 --grep 与 --author 同时生效，把用户在搜索框里
 * 敲的东西一律当字面量——不加它，一个 `(` 就够 git 报 "Unmatched ( or \("
 * 然后整页空白。`-i` 对固定字符串同样生效。
 *
 * rev 走 `<rev> --` 的位置参数：分支名以 `-` 开头时（git 允许 `--` 之后的
 * refname 长这样）不加终结符会被当成选项解析。
 */
export function logPageArgs(git: string, dir: string, q: LogQuery = {}): string[] {
  const limit = q.limit ?? LOG_PAGE;
  const args = [
    "log",
    // %P 空 = 根提交；%D 空 = 这条上没有任何 ref。subject 放最后一列，
    // 它是唯一可能含 TAB 的字段（ref 名与作者名都禁止控制字符）
    "--format=%H%x09%h%x09%an%x09%ae%x09%at%x09%P%x09%D%x09%s",
    // 多取一条探"还有没有下一页"，比 rev-list --count 便宜得多
    "-n",
    String(limit + 1),
  ];
  if (q.skip) args.push(`--skip=${q.skip}`);
  if (q.grep || q.author) args.push("--fixed-strings", "--regexp-ignore-case");
  if (q.grep) args.push(`--grep=${q.grep}`);
  if (q.author) args.push(`--author=${q.author}`);
  // 指定了分支就只看它，否则看全部分支/远程/标签——两者都要 --date-order，
  // 不然多分支并行时提交会按拓扑挤成一坨，画出来的图与时间轴对不上
  args.push("--date-order");
  if (q.rev) args.push(q.rev, "--");
  else args.push(...HISTORY_REFS);
  return at(git, dir, ...args);
}

/** 作者下拉的候选：采样近若干条提交的 %an，去重与排序在解析侧做 */
export function logAuthorsArgs(git: string, dir: string, n = AUTHOR_SAMPLE): string[] {
  return at(git, dir, "log", "-n", String(n), "--format=%an", ...HISTORY_REFS);
}

/**
 * 作者下拉里「我」是谁。没配 user.name 时退出码 1、无输出——那是正常状态
 * （这台机器上还没设过身份），不是错误。
 */
export function configUserNameArgs(git: string, dir: string): string[] {
  return at(git, dir, "config", "--get", "user.name");
}

/**
 * 合并提交的 diff 取 first-parent。
 *
 * 不加这个的话 `git show <merge>` 一个文件都不输出（默认 --diff-merges=off），
 * 面板上看起来就像"这次合并什么都没改"。git ≥ 2.31，与 --path-format=absolute
 * 是同一代要求。
 */
const FIRST_PARENT = "--diff-merges=first-parent";

/** 提交元数据（一行）。字段顺序与 parseCommitMeta 一一对应 */
export function commitMetaArgs(git: string, dir: string, sha: string): string[] {
  return at(
    git,
    dir,
    "show",
    "-s",
    "--format=%H%x09%h%x09%an%x09%ae%x09%at%x09%cn%x09%ct%x09%P%x09%D",
    sha,
    "--"
  );
}

/** 完整提交信息。单独一条命令：%B 含换行，塞不进上面那种一行多列的格式 */
export function commitMessageArgs(git: string, dir: string, sha: string): string[] {
  return at(git, dir, "show", "-s", "--format=%B", sha, "--");
}

/**
 * 改动文件：一条命令同时要 --raw 与 --numstat。
 *
 * 两段都要是因为各缺一半：--raw 给状态字母与**未压缩**的新旧路径（重命名是
 * `R100\told\tnew` 两列），--numstat 给增删行数但重命名路径会被压成
 * `dir/{old => new}/f` 这种紧凑形式。git 对两段用的是同一个 diff queue，
 * 文件顺序一致，所以按下标配对——解析侧对不上时退化成"数字未知"，见 parseCommitFiles。
 */
export function commitFilesArgs(git: string, dir: string, sha: string): string[] {
  return at(git, dir, "show", "--format=", FIRST_PARENT, "--raw", "--numstat", sha, "--");
}

/** 某条提交里单个文件的 diff */
export function commitFileDiffArgs(
  git: string,
  dir: string,
  sha: string,
  path: string,
  origPath?: string
): string[] {
  const paths = origPath ? [origPath, path] : [path];
  return at(git, dir, "show", "--format=", FIRST_PARENT, sha, "--", ...paths);
}

/**
 * 工作区已跟踪文件的增删行数，基准 HEAD（暂存 + 未暂存一起看）。
 *
 * 与 statusArgs 是同一个视角，两者的结果按路径配对——不能按下标配，
 * status 会列出未跟踪文件而 numstat 不会，两边条数本来就不一样。
 */
export function workingNumstatArgs(git: string, dir: string, base: string): string[] {
  return at(git, dir, "diff", "--numstat", base);
}

/**
 * 未跟踪文件的行数：与空文件比。
 *
 * 只能一个文件一条命令（--no-index 恰好收两个路径），所以调用方要把它们
 * 批成一次 exec，并且限个数——见 UNTRACKED_NUMSTAT_CAP。
 *
 * 不用 `git add -N` 那个更省事的办法：那会写 index，而这个面板是只读的，
 * 用户的暂存区不该因为看了一眼就被动过。
 */
export function untrackedNumstatArgs(git: string, dir: string, path: string): string[] {
  return at(git, dir, "diff", "--numstat", "--no-index", "--", "/dev/null", path);
}

/**
 * 最多为多少个未跟踪文件算行数。
 *
 * 新建一个装满文件的目录就能有几百个未跟踪文件，每个都要一条命令，
 * 命令行会被撑爆（Windows 上尤其紧）。超出的显示成"未知"，不是 0。
 */
export const UNTRACKED_NUMSTAT_CAP = 80;

/** 「修改」面板最多列多少个文件 */
export const WORKING_FILE_CAP = 500;

// ---------------- 提交 ----------------

/** 把选中的未跟踪文件放进 index。已跟踪的不用——pathspec commit 直接取工作区 */
export function addPathsArgs(git: string, dir: string, paths: string[]): string[] {
  return at(git, dir, "add", "--", ...paths);
}

/** 提交全部改动前的那一步。-A 含未跟踪文件与删除 */
export function addAllArgs(git: string, dir: string): string[] {
  return at(git, dir, "add", "-A");
}

/**
 * 提交。
 *
 * 带 pathspec 时 git **忽略 index**，直接拿这些路径的工作区内容成提交——
 * 实测过：index 里别人暂存的东西不会被顺带提交走，未选中的文件也不动。
 * 这正是面板要的语义（它显示的就是暂存+未暂存的合并视图）。
 *
 * 不加 --no-verify：pre-commit 钩子是用户自己配的，面板没有资格跳过它。
 * 钩子可能很慢，所以调用方要给足超时（TIMEOUT_SYNC）。
 */
export function commitArgs(
  git: string,
  dir: string,
  message: string,
  paths?: string[]
): string[] {
  const args = ["commit", "-m", message];
  if (paths && paths.length > 0) args.push("--", ...paths);
  return at(git, dir, ...args);
}

/**
 * pathspec 拼进命令行的字符预算。
 *
 * 卡得这么死是因为 Windows 远端：命令走 `powershell -EncodedCommand`，载荷先
 * 转 UTF-16LE 再 base64（长度 ×8/3），而 cmd.exe 的命令行上限是 8191。
 * 留出 git 参数与 env 前缀后，路径部分能用的也就两千出头。
 *
 * 超了不是截断——截断会**悄悄少提交几个文件**，那是最糟的一种失败。
 * 服务端直接拒绝，让用户改用"全选"（走 add -A，不拼路径）或者分两次提交。
 */
export const COMMIT_PATHSPEC_BUDGET = 2000;

export function pathspecTooLong(paths: string[]): boolean {
  // +3 给引号与分隔符留的余量，宁可保守
  return paths.reduce((n, p) => n + p.length + 3, 0) > COMMIT_PATHSPEC_BUDGET;
}

/**
 * Pull。--ff-only 是刻意的：能快进就快进，不能就停下报错。
 *
 * 面板上一个按钮不该在用户看不见的地方造出合并提交，更不该把工作区搅成冲突
 * 状态——那之后所有会话里的 shell 都在一个半挂的仓库里干活。真要合并/变基，
 * 用户在终端里做，那是他清楚自己在做什么的地方。
 */
export function pullArgs(git: string, dir: string): string[] {
  return at(git, dir, "pull", "--ff-only");
}

/**
 * Push 当前分支到它的 upstream。
 *
 * 不传 refspec 也不加 --set-upstream：没有 upstream 时 git 会直接报错并把
 * 该敲的命令印在 stderr 里，那比我们替他猜一个远程分支名要好。
 * 绝不加 --force——按钮点下去要么是安全的，要么就失败。
 */
export function pushArgs(git: string, dir: string): string[] {
  return at(git, dir, "push");
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

// ---------------- 批量 git ----------------

/**
 * 哨兵行前缀。git 的输出不可能撞上它：porcelain / rev-parse / log 的每种
 * 格式都不会产出这种行；就算路径里被人恶意塞进这个串，status 行有 "XY " 前缀、
 * diff 行有 +/- 前缀，都不会整行等于哨兵。
 */
const BATCH_MARK = "__FALCON_GIT_";

/**
 * 把多条 git 命令拼成**一次** exec：每条命令后打一行 `__FALCON_GIT_<i>_<code>__`
 * 哨兵，带序号与真实退出码。SSH 上一条 exec 就是一次 channel open/close 往返，
 * Git 面板一轮快照要跑十来条命令，逐条发就是十来个往返——与 existsManyCommand
 * 是同一笔账。
 *
 * 退出码的取法两边不同但语义一致：POSIX 直接 `$?`；PowerShell 沿用
 * powerShellScript 的预置哨兵 127（命令没跑起来时 $LASTEXITCODE 不会被赋值，
 * 见那边的注释），跑起来了就被真实退出码覆盖。
 */
export function batchGitCommandLine(
  kind: HostKind,
  argvs: string[][],
  env: Record<string, string> = GIT_ENV
): string {
  if (kind === "windows") {
    const lines = [
      ...GIT_UNSET.map((k) => `$env:${k} = $null`),
      ...Object.entries(env).map(([k, v]) => `$env:${k} = ${quotePowerShell(v)}`),
    ];
    argvs.forEach((argv, i) => {
      const [exe, ...rest] = argv;
      lines.push("$LASTEXITCODE = 127");
      lines.push([`& ${quotePowerShell(exe!)}`, ...rest.map(quotePowerShell)].join(" "));
      // [char]10 前导换行：命令输出不带结尾换行时，哨兵不能黏在同一行上
      lines.push(`Write-Output ([char]10 + '${BATCH_MARK}${i}_' + $LASTEXITCODE + '__')`);
    });
    return encodePowerShell(lines.join("; "));
  }
  const parts = [
    `unset ${GIT_UNSET.join(" ")}`,
    ...Object.entries(env).map(([k, v]) => `export ${k}=${quotePosix(v)}`),
  ];
  argvs.forEach((argv, i) => {
    parts.push(argv.map(quotePosix).join(" "));
    // 前导 \n 同 PowerShell 侧；$? 在 printf 求值时仍指向上一条命令
    parts.push(`printf '\\n${BATCH_MARK}${i}_%s__\\n' "$?"`);
  });
  return parts.join("; ");
}

export interface BatchGitResult {
  /** null = 没找到这条命令的哨兵（整批被掐断 / 环境异常） */
  code: number | null;
  stdout: string;
}

/** 按哨兵切分整批输出。哨兵缺失的命令按 code:null 处理，调用方视同失败。 */
export function parseGitBatch(stdout: string, count: number): BatchGitResult[] {
  const out: BatchGitResult[] = Array.from({ length: count }, () => ({
    code: null,
    stdout: "",
  }));
  const re = new RegExp(`^${BATCH_MARK}(\\d+)_(\\d+)__$`);
  let cur: string[] = [];
  for (const line of lines(stdout)) {
    const m = re.exec(line.trim());
    if (!m) {
      cur.push(line);
      continue;
    }
    const i = Number(m[1]);
    if (i >= 0 && i < count) out[i] = { code: Number(m[2]), stdout: cur.join("\n") };
    cur = [];
  }
  return out;
}

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
    const [name, upstream, head, symref] = line.split("\t");
    if (!name) continue;
    // 指向默认分支的 symref（origin/HEAD）不是可检出的目标，列出来只会误导。
    // 它跟自己指向的那条分支永远同时出现，丢掉不会少任何一个选项
    if (symref) continue;
    const remote = remotes.some((r) => name === r || name.startsWith(`${r}/`));
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

// ---------------- History 面板的解析 ----------------

/**
 * 解析 %D（decoration）。
 *
 * 形如 `HEAD -> main, origin/main, tag: v1.0, origin/HEAD`。
 * - `HEAD -> x` 里的 x 是当前分支，标 head:true；
 * - 光杆 `HEAD`（detached）不是 ref，丢掉；
 * - `origin/HEAD` 是指向默认分支的 symref，与 parseBranchList 同一个理由丢掉——
 *   它跟 origin/main 永远同时出现在同一条提交上，列出来就是重复一格。
 */
export function parseRefLabels(
  decoration: string,
  remotes: string[]
): { name: string; kind: "local" | "remote" | "tag"; head?: boolean }[] {
  const out: { name: string; kind: "local" | "remote" | "tag"; head?: boolean }[] = [];
  for (const raw of decoration.split(",")) {
    let name = raw.trim();
    if (!name) continue;
    if (name.startsWith("tag: ")) {
      out.push({ name: name.slice(5).trim(), kind: "tag" });
      continue;
    }
    let head = false;
    const arrow = name.indexOf(" -> ");
    if (arrow >= 0) {
      // 左边必然是字面量 HEAD，右边才是分支名
      name = name.slice(arrow + 4).trim();
      head = true;
    }
    if (!name || name === "HEAD") continue;
    const remote = remotes.some((r) => name === r || name.startsWith(`${r}/`));
    if (remote && name.endsWith("/HEAD")) continue;
    out.push({ name, kind: remote ? "remote" : "local", ...(head ? { head } : {}) });
  }
  return out;
}

export interface ParsedLogCommit {
  sha: string;
  short: string;
  author: string;
  authorEmail: string;
  authoredAt: number;
  subject: string;
  parents: string[];
  decoration: string;
}

/** 解析 logPageArgs 的输出。列不齐的行直接丢——宁可少一条也不要画错的图 */
export function parseLogPage(stdout: string): ParsedLogCommit[] {
  const out: ParsedLogCommit[] = [];
  for (const line of lines(stdout)) {
    if (!line.trim()) continue;
    const cols = line.split("\t");
    if (cols.length < 8) continue;
    const [sha, short, author, authorEmail, at, parents, decoration, ...rest] = cols;
    if (!sha || !short) continue;
    const sec = Number(at);
    out.push({
      sha,
      short,
      author: author ?? "",
      authorEmail: authorEmail ?? "",
      authoredAt: Number.isFinite(sec) ? sec * 1000 : 0,
      parents: (parents ?? "").split(" ").filter(Boolean),
      decoration: decoration ?? "",
      // subject 是最后一列，里面的 TAB 要原样拼回去
      subject: rest.join("\t"),
    });
  }
  return out;
}

/** 作者采样：按出现次数降序，同次数按首次出现的顺序（log 是新→旧，即最近活跃优先） */
export function rankAuthors(stdout: string): string[] {
  const counts = new Map<string, number>();
  for (const line of lines(stdout)) {
    const name = line.trim();
    if (!name) continue;
    counts.set(name, (counts.get(name) ?? 0) + 1);
  }
  return [...counts.keys()].sort((a, b) => (counts.get(b) ?? 0) - (counts.get(a) ?? 0));
}

export interface ParsedCommitMeta {
  sha: string;
  short: string;
  author: string;
  authorEmail: string;
  authoredAt: number;
  committer: string;
  committedAt: number;
  parents: string[];
  decoration: string;
}

/** 解析 commitMetaArgs 的一行输出。列不齐返回 null，调用方当"读不到这条提交" */
export function parseCommitMeta(stdout: string): ParsedCommitMeta | null {
  const line = lines(stdout).find((l) => l.trim());
  if (!line) return null;
  const cols = line.split("\t");
  if (cols.length < 8) return null;
  const [sha, short, author, authorEmail, aAt, committer, cAt, parents, decoration] = cols;
  if (!sha || !short) return null;
  const aSec = Number(aAt);
  const cSec = Number(cAt);
  return {
    sha,
    short,
    author: author ?? "",
    authorEmail: authorEmail ?? "",
    authoredAt: Number.isFinite(aSec) ? aSec * 1000 : 0,
    committer: committer ?? "",
    committedAt: Number.isFinite(cSec) ? cSec * 1000 : 0,
    parents: (parents ?? "").split(" ").filter(Boolean),
    decoration: decoration ?? "",
  };
}

export interface ParsedCommitFile {
  path: string;
  origPath?: string;
  status: string;
  added: number | null;
  deleted: number | null;
}

/**
 * 解析 commitFilesArgs 的两段输出。
 *
 * raw 段每行以 `:` 开头：`:<旧模式> <新模式> <旧sha> <新sha> <状态>\t<路径>[\t<新路径>]`。
 * 状态可能带相似度数字（R100 / C85），只取首字母。
 *
 * numstat 段是 `<增>\t<删>\t<路径>`，二进制文件两列都是 `-`（记 null，不是 0——
 * "改了但不知道多少行"和"一行没改"在界面上是两回事）。
 *
 * 两段的文件顺序由 git 的同一个 diff queue 决定，一致，所以按下标配对。
 * 条数对不上（不该发生，但输出被 profile 之类污染过就会）时宁可丢掉全部数字，
 * 也不要把 A 文件的行数记到 B 文件头上。
 */
export function parseCommitFiles(stdout: string): ParsedCommitFile[] {
  const raw: { path: string; origPath?: string; status: string }[] = [];
  const nums: { added: number | null; deleted: number | null }[] = [];
  for (const line of lines(stdout)) {
    if (!line.trim()) continue;
    if (line.startsWith(":")) {
      // 状态与路径之间是 TAB，前面那截模式/sha 用空格分隔
      const tab = line.indexOf("\t");
      if (tab < 0) continue;
      const head = line.slice(0, tab).trim().split(/\s+/);
      const status = (head[head.length - 1] ?? "").charAt(0);
      const paths = line
        .slice(tab + 1)
        .split("\t")
        .map((p) => unquoteCPath(p.trim()))
        .filter(Boolean);
      if (!status || paths.length === 0) continue;
      // R / C 有两个路径：先旧后新
      if (paths.length > 1) raw.push({ status, origPath: paths[0], path: paths[1]! });
      else raw.push({ status, path: paths[0]! });
      continue;
    }
    const cols = line.split("\t");
    if (cols.length < 3) continue;
    const [a, d] = cols;
    const num = (s: string | undefined): number | null => {
      if (!s || s === "-") return null;
      const n = Number(s);
      return Number.isFinite(n) ? n : null;
    };
    nums.push({ added: num(a), deleted: num(d) });
  }
  const aligned = nums.length === raw.length;
  return raw.map((f, i) => ({
    ...f,
    added: aligned ? (nums[i]?.added ?? null) : null,
    deleted: aligned ? (nums[i]?.deleted ?? null) : null,
  }));
}

/** 提交详情里最多列多少个文件。改了几千个文件的提交不该把面板撑爆 */
export const COMMIT_FILE_CAP = 300;

/**
 * 解析 `diff --numstat`：`<增>\t<删>\t<路径>`，按路径索引。
 *
 * 重命名在这里是紧凑形式（`dir/{old => new}/f`），解析回两个路径太脆，
 * 而调用方手上已经有 porcelain 给的准确新旧路径——所以这里只认能直接
 * 用的那些，配不上的按"未知行数"处理，见 parseNumstatMap 的调用点。
 */
export function parseNumstatMap(stdout: string): Map<string, { added: number | null; deleted: number | null }> {
  const out = new Map<string, { added: number | null; deleted: number | null }>();
  for (const line of lines(stdout)) {
    if (!line.trim()) continue;
    const cols = line.split("\t");
    if (cols.length < 3) continue;
    const num = (s: string | undefined): number | null => {
      if (!s || s === "-") return null; // 二进制文件两列都是 -
      const n = Number(s);
      return Number.isFinite(n) ? n : null;
    };
    // 路径可能含 TAB，后面的列拼回去
    const path = unquoteCPath(cols.slice(2).join("\t").trim());
    if (path) out.set(path, { added: num(cols[0]), deleted: num(cols[1]) });
  }
  return out;
}

/** `status --porcelain --ignored=matching` 里 `!!` 开头的那些 */
export function countIgnored(stdout: string): number {
  return lines(stdout).filter((l) => l.startsWith("!!")).length;
}

/** diff 文本的上限。锁文件之类的 diff 可以到几十 MB，浏览器不该收这么多 */
export const DIFF_CAP = 500_000;

/** 超限时在行边界截断，别把最后一行剪成半句 */
export function truncateDiff(
  text: string,
  cap = DIFF_CAP
): { text: string; truncated: boolean } {
  if (text.length <= cap) return { text, truncated: false };
  const cut = text.slice(0, cap);
  const nl = cut.lastIndexOf("\n");
  return { text: nl > 0 ? cut.slice(0, nl + 1) : cut, truncated: true };
}
