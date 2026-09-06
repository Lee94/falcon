import type { TermAppearance } from "./termEnv.js";

export type { OscColorHint, TermAppearance } from "./termEnv.js";
export {
  appearanceFromHex,
  applyTermPtyEnv,
  hexLuminance,
  hexToOscRgb,
  isTermAppearance,
  OscColorGate,
  oscColorReplies,
  parseHexRgb,
  sanitizeColorHint,
  termPtyEnv,
} from "./termEnv.js";
export { TermModeTracker } from "./termModes.js";

// ============ Project ============

export type ProjectType = "local" | "ssh";

export type SshAuthMethod = "key" | "password" | "agent";

export interface SshConfig {
  host: string;
  port: number;
  username: string;
  authMethod: SshAuthMethod;
  /** 后端所在机器上的私钥文件路径（authMethod = key 时） */
  keyPath?: string;
  /** 密码 / passphrase 是否已保存（内容绝不出现在 API 响应中） */
  hasSecret: boolean;
}

/**
 * 预先保存的远端 SSH 主机。
 *
 * 项目创建时从这里选一台，连接配置复制到项目上——会话链路仍然读项目自己的
 * ssh 字段，不按 hostId 解引用。改主机时再把连接配置刷回引用它的项目。
 */
export interface SshHost {
  id: string;
  name: string;
  host: string;
  port: number;
  username: string;
  authMethod: SshAuthMethod;
  keyPath?: string;
  hasSecret: boolean;
  /** 当前引用此主机的项目数（含附属项目） */
  projectCount: number;
  createdAt: number;
}

/** 创建 / 更新远端主机。secret 仅在写入方向出现。 */
export interface SshHostInput {
  name: string;
  host: string;
  port: number;
  username: string;
  authMethod: SshAuthMethod;
  keyPath?: string;
  secret?: string;
}

/**
 * SSH 连通性探测。形状与 RepoInfo 同源：环境事实写在 ok/error 里，不抛 4xx。
 * 测的是「现在这组凭据能不能登上」，不是 Zellij / git 好不好用。
 */
export type SshProbeResult =
  | { ok: true; kind: "posix" | "windows"; home: string }
  | { ok: false; error: string };

// ============ Port Forward（SSH 端口转发） ============

/**
 * 挂在 SSH 项目上的 TCP 隧道，走该项目的 SshLink。
 *
 * local：在 falcon 后端监听，经 SSH 打到远端能到达的地址（ssh -L）。
 * remote：在远端监听，打回后端能到达的地址（ssh -R）。
 */
export type ForwardKind = "local" | "remote";

/** 运行时状态。规则本身用 enabled 表示「该不该跑」，state 是此刻的事实。 */
export type ForwardState = "stopped" | "starting" | "active" | "error";

export interface PortForward {
  id: string;
  projectId: string;
  /** 可选备注，如 vite / postgres */
  name?: string;
  kind: ForwardKind;
  /** 监听地址。local = 后端本机，remote = 远端 */
  bindHost: string;
  bindPort: number;
  destHost: string;
  destPort: number;
  enabled: boolean;
  state: ForwardState;
  error?: string;
  createdAt: number;
}

export interface PortForwardInput {
  name?: string;
  kind: ForwardKind;
  bindHost?: string;
  bindPort: number;
  destHost?: string;
  destPort: number;
  enabled?: boolean;
}

export interface Project {
  id: string;
  name: string;
  type: ProjectType;
  /** 终端初始 cwd：本地为后端机器路径，SSH 为远端路径（可选） */
  workingDir?: string;
  /** 覆盖默认 shell，可选 */
  shell?: string;
  ssh?: SshConfig;
  /** 创建时选中的已保存主机；存量项目或手写 ssh 字段的请求没有这项 */
  hostId?: string;
  /** 存在 ⇔ 这是附属项目（工作目录是某个 git 仓库的 worktree） */
  worktree?: WorktreeInfo;
  /** 存在 ⇔ 这是多仓库项目（容器或批量派生的产物），见 MultiRepoInfo */
  multi?: MultiRepoInfo;
  createdAt: number;
}

/** 创建 / 更新项目的请求体。secret 为明文密码或 passphrase，仅在写入方向出现。 */
export interface ProjectInput {
  name: string;
  type: ProjectType;
  workingDir?: string;
  shell?: string;
  /**
   * 已保存主机。有值时连接配置从该主机复制，忽略 ssh 字段。
   * 新建 SSH 项目走这条；存量项目仍可用下面的 ssh 手写。
   */
  hostId?: string;
  ssh?: {
    host: string;
    port: number;
    username: string;
    authMethod: SshAuthMethod;
    keyPath?: string;
    secret?: string;
  };
  /**
   * 多仓库容器的成员仓库路径。创建时带上 ⇒ 建的是容器；编辑容器时带上 ⇒ 替换成员清单。
   * 判别式在创建时定死：普通项目带 repos、或附属项目改 repos，一律 400——
   * 派生产物的成员清单是删除目标，写入路径必须不可达（与 worktree 四列同罪）。
   */
  repos?: string[];
}

// ============ Worktree（附属项目） ============

/**
 * 附属项目独有的属性。存在与否即判别式：worktree != null ⇔ 这是附属项目。
 *
 * 不给 ProjectType 加第三个值：「附属」与「local/ssh」是正交的两个维度——
 * 附属项目同时也是 local 或 ssh 项目，用同一条链路、同一台宿主机。塞进同一个
 * 枚举会让每一处 `type === "ssh"` 都要改成两条判断。
 */
export interface WorktreeInfo {
  /** 派生自哪个 Project */
  sourceProjectId: string;
  /** 检出的分支短名（不带 refs/heads/） */
  branch: string;
  /**
   * 派生时记录的仓库根。删除时绝不去读源项目的 workingDir——那一列用户可改、
   * 源项目行还可能已被删，而护栏必须拿创建那一刻记下的值去比对。
   */
  repoDir: string;
  /** 目录是不是 falcon 建的。false 时删除项目绝不删目录 */
  createdByFalcon: boolean;
  /**
   * 存档时间（unix 毫秒）。存在 ⇔ 已存档：侧栏默认隐藏，worktree 目录原样保留，
   * 到期（WORKTREE_ARCHIVE_TTL_MS）由后台清扫自动删除；此前随时可恢复。
   */
  archivedAt?: number;
}

/**
 * 存档的附属项目多久后自动删除并清理 worktree 目录。
 * 后端清扫与前端"还剩几天"的倒计时用同一个数，两边永远对得上。
 */
export const WORKTREE_ARCHIVE_TTL_DAYS = 7;
export const WORKTREE_ARCHIVE_TTL_MS = WORKTREE_ARCHIVE_TTL_DAYS * 24 * 60 * 60 * 1000;

/**
 * worktree 操作的失败原因。与 ZellijInstallFailure 同构：闭集字符串联合，
 * 前后端共用，前端据此渲染具体说明而不是笼统的"操作失败"。
 *
 * 不做自动重试（与 Zellij 安装刻意不同）：那边一轮是 14 MiB 下载，重试期望收益极高；
 * 这边每条 git 命令都是宿主机上百毫秒级的本地操作，用户重按一次按钮的成本约等于零，
 * 加一套重试循环只会多一条没人走的代码路径。
 */
export type WorktreeFailure =
  /** 宿主机上没有 git，或 git 不在非登录 shell 的 PATH 里 */
  | "git-missing"
  /** 工作目录不在任何 git 仓库里（裸仓库也归到这里） */
  | "not-a-repo"
  /** SSH 项目没指定工作目录，无从派生同级路径 */
  | "no-working-dir"
  /** 分支已在另一个 worktree 中检出——git 不允许同一分支检出两次 */
  | "branch-in-use"
  /** 新建模式下同名分支已存在 */
  | "branch-exists"
  /** 分支 / 基点不存在（浅克隆上检出远程分支时常见） */
  | "branch-unknown"
  /** 目标目录已存在 */
  | "path-occupied"
  /** 目标目录落在仓库内部——会被源仓库当成一堆未跟踪文件 */
  | "path-inside-repo"
  /** 路径过长，Windows 上建得出来却删不掉 */
  | "path-too-long"
  /** git worktree add 以其他理由失败，detail 带原始 stderr */
  | "worktree-add-failed"
  /** 命令根本没跑起来：SSH 抖动 / spawn 失败 */
  | "link-failed";

export interface RepoBranch {
  /** 短名。本地为 main，远程为 origin/main */
  name: string;
  remote: boolean;
  /** 远程分支对应的本地分支名（origin/feat/x → feat/x）；本地分支为 undefined */
  localName?: string;
  /** 已在某个 worktree 中检出的路径。有值 ⇒ 不能再检出一次 */
  checkedOutAt?: string;
  /** 是不是源项目当前的 HEAD */
  head: boolean;
  /** 按该分支派生时的建议目录 */
  suggestedDir: string;
  /** 建议目录是否已被占用 */
  dirOccupied: boolean;
}

/**
 * 源项目的仓库信息。
 *
 * 探测端点永不因环境事实报错——形状与 HostZellijStatus 同源：返回一份
 * "能不能干、为什么不能"的报告，而不是 4xx。创建端点则把同样的事实当状态冲突。
 */
export interface RepoInfo {
  derivable: boolean;
  /** derivable=false 时必定有值 */
  reason?: WorktreeFailure;
  detail?: string;
  repoDir?: string;
  /** 当前 HEAD 分支；detached 时为 undefined */
  headBranch?: string;
  /** detached 时的短 sha，作为新建分支的默认基点显示 */
  headSha?: string;
  branches: RepoBranch[];
}

export interface WorktreeInput {
  /** 留空则用分支名 */
  name?: string;
  mode: "new-branch" | "existing-branch";
  /**
   * 目标分支的**本地**名。检出远程分支走 new-branch + startPoint=origin/x——
   * 直接把 origin/x 当 commit-ish 会得到 detached HEAD，而 detached worktree 里的
   * 提交在 remove 之后立刻不可达、可被 gc 回收，那是真数据丢失。
   */
  branch: string;
  /** mode=new-branch 的基点，缺省 HEAD */
  startPoint?: string;
  /** 目标目录；缺省用服务端派生的同级平铺路径 */
  dir?: string;
}

// ============ 多仓库项目 ============

/**
 * 多仓库项目的一个成员。存在于两种行上，dir 的语义不同：
 * - 容器：用户选的路径（可以是仓库子目录，派生时才 rev-parse 出仓库根）。
 * - 派生产物：该成员 worktree 的绝对路径——git 自己报的，删除护栏拿它比对。
 */
export interface MultiRepoMember {
  dir: string;
  /**
   * 仅派生产物有值：成员所属仓库根（主 worktree 路径），删除取证用。
   * 容器成员没有——与 WorktreeInfo.repoDir 同理，判据必须是创建那一刻记下的值。
   */
  repoDir?: string;
}

/**
 * 存在 ⇔ 多仓库项目。与 worktree != null ⇔ 附属项目 是同一个判别式模式：
 * 「多仓库」与「local/ssh」正交（成员全在容器自己的宿主机上），与「容器/附属」
 * 也正交——multi 与 worktree 同时存在 = 批量派生出的多仓库附属项目。
 * 不给 ProjectType 加第三个值的理由见 WorktreeInfo 的注释。
 */
export interface MultiRepoInfo {
  repos: MultiRepoMember[];
}

/** 容器成员数上限。批量派生逐仓库串行，每条 add 最长 2 分钟，上限同时是耗时上限 */
export const MULTI_REPO_MAX = 16;

/**
 * 批量派生请求。与 WorktreeInput 刻意分开：没有 startPoint——每个成员的基点
 * 恒为**各自的** HEAD，跨成员挑一个共同基点没有意义；多一个 auto 模式，
 * 因为统一分支名跨 N 个仓库时"有的仓库已有这条分支、有的没有"是常态，
 * 没有 auto，全有或全无的语义会让混合状态永远派生不出来。
 */
export interface MultiWorktreeInput {
  /** 留空则用分支名 */
  name?: string;
  /** auto：分支存在则检出、不存在则从各自 HEAD 新建 */
  mode: "new-branch" | "existing-branch" | "auto";
  /** 目标分支的**本地**名，对全部成员统一 */
  branch: string;
  /** 集中目录；缺省 = 第一个成员仓库根的父目录 + <容器名slug>-<分支slug> */
  dir?: string;
}

/**
 * 多仓库容器的派生前探测（GET /api/projects/:id/repos）。
 * 与 RepoInfo 同哲学：环境事实不报错，逐成员写在 derivable / reason 里。
 */
export interface MultiRepoProbe {
  /** 与容器成员同序；dir 是容器里配置的成员路径 */
  members: { dir: string; info: RepoInfo }[];
  /** 集中目录会建在哪个父目录下（第一个成员仓库根的父目录），供前端预览 */
  baseDir?: string;
}

/**
 * 批量派生的失败响应体。error 仍是一句可以直接展示的话，
 * member / leftover 供前端标明"哪个仓库、什么原因、回滚剩了什么"。
 */
export interface MultiDeriveError {
  error: string;
  /** 第一个失败的成员（全有或全无 ⇒ 至多一个）。dir 是容器里配置的成员路径 */
  member?: { dir: string; reason: WorktreeFailure; detail?: string };
  /** 回滚后仍残留在磁盘上的绝对路径；缺省/空 = 回滚干净。非空 ⇒ HTTP 502 */
  leftover?: string[];
}

/**
 * 右侧 Git 面板的仓库快照。源项目和附属项目都能问。
 *
 * 与 RepoInfo 分开：那边是派生用的分支清单（建议目录、占用），这边是当前
 * 工作区状态。环境事实同样不抛 4xx，写在 available / reason 里。
 */
export type GitUnavailableReason = Extract<
  WorktreeFailure,
  "git-missing" | "not-a-repo" | "no-working-dir" | "link-failed"
>;

export interface GitFileChange {
  path: string;
  /** 重命名 / 复制前的路径 */
  origPath?: string;
  /** porcelain X：暂存区状态，空格表示无 */
  index: string;
  /** porcelain Y：工作区状态，空格表示无 */
  work: string;
}

export interface GitRemote {
  name: string;
  url: string;
}

export interface GitCommit {
  sha: string;
  author: string;
  /** unix 毫秒 */
  authoredAt: number;
  subject: string;
}

export interface GitWorktreeRef {
  path: string;
  branch?: string;
  head: string;
  current: boolean;
}

export interface GitSnapshot {
  available: boolean;
  reason?: GitUnavailableReason;
  detail?: string;
  repoDir?: string;
  /** 项目工作目录（可能是仓库里的子目录） */
  workDir?: string;
  headBranch?: string;
  headSha?: string;
  detached?: boolean;
  upstream?: string;
  /** 没有 upstream 时为 null */
  ahead?: number | null;
  behind?: number | null;
  remotes: GitRemote[];
  files: GitFileChange[];
  /** 工作区改动总数；files 可能被截断 */
  fileCount: number;
  worktrees: GitWorktreeRef[];
  commits: GitCommit[];
}

/**
 * Git 面板里单个文件的 diff。
 *
 * 基准一律是 HEAD（暂存 + 未暂存合在一起），未跟踪文件与空文件比对。
 * 环境事实与命令失败同样不抛 4xx，写在 available / reason / detail 里。
 */
export interface GitFileDiff {
  available: boolean;
  reason?: GitUnavailableReason;
  detail?: string;
  /** unified diff 文本；没有差异（如空的未跟踪文件）时为空串 */
  diff: string;
  /** 超过上限时按行边界截断 */
  truncated: boolean;
}

// ---------------- History 面板 ----------------

/** 提交上挂的 ref 标签。同一条提交可能同时有本地分支、远程分支、标签 */
export interface GitRefLabel {
  /** 去掉 refs/heads/ refs/remotes/ refs/tags/ 前缀后的短名 */
  name: string;
  kind: "local" | "remote" | "tag";
  /** HEAD 指向的那条本地分支（%D 里的 `HEAD -> x`） */
  head?: boolean;
}

/**
 * History 列表里的一条提交。
 *
 * sha 是**完整**的 40 位：提交图要靠它与 parents 对应，短 sha 在大仓库里
 * 有撞的可能，而撞一次画出来的线就整个错位。展示用短 sha 走 short 字段。
 */
export interface GitLogCommit {
  sha: string;
  short: string;
  author: string;
  authorEmail: string;
  /** unix 毫秒 */
  authoredAt: number;
  subject: string;
  /** 完整 sha，按 git 的顺序（第一个是 first parent） */
  parents: string[];
  refs: GitRefLabel[];
}

/** History 列表的一页。环境事实同样不抛 4xx，写在 available / reason 里 */
export interface GitLogPage {
  available: boolean;
  reason?: GitUnavailableReason;
  detail?: string;
  commits: GitLogCommit[];
  /** 还有下一页（服务端多取一条探出来的，不做 count） */
  hasMore: boolean;
}

/**
 * History 的 Branch 筛选项。刻意不复用 RepoBranch——那边的 suggestedDir /
 * dirOccupied 是派生用的，每次都要多跑一轮路径存在性探测，一个筛选下拉不值这个价。
 */
export interface GitBranchRef {
  /** 短名。本地为 main，远程为 origin/main */
  name: string;
  remote: boolean;
  head: boolean;
  /** 本地分支跟踪的远程分支（origin/main）；没有跟踪关系时 undefined */
  upstream?: string;
}

/** Branch / User 两个筛选下拉的候选值 */
export interface GitRefsInfo {
  available: boolean;
  reason?: GitUnavailableReason;
  detail?: string;
  branches: GitBranchRef[];
  /** 近若干条提交里出现过的作者名，按出现次数降序 */
  authors: string[];
  /** 这台机器上 git 配的 user.name，作者下拉里的「我」。没配则 undefined */
  me?: string;
}

/**
 * 「修改」面板里的一个工作区改动文件。
 *
 * 比 GitFileChange 多了增删行数：porcelain 只说"改了"，不说改了多少。
 */
export interface GitWorkingFile extends GitFileChange {
  /** 二进制文件、或算不出来时为 null（与 GitCommitFile 同一个约定） */
  added: number | null;
  deleted: number | null;
}

/**
 * 工作区里全部未提交的改动。基准是 HEAD，暂存与未暂存合在一起看——
 * 面板不区分暂存区，它回答的是"我这次动了什么"。
 */
export interface GitWorkingChanges {
  available: boolean;
  reason?: GitUnavailableReason;
  detail?: string;
  /** 仓库根目录名，目录树视图拿它当根节点 */
  repoName?: string;
  files: GitWorkingFile[];
  /** 改动总数；files 可能被截断 */
  fileCount: number;
}

/** 提交详情里的一个改动文件 */
export interface GitCommitFile {
  path: string;
  /** 重命名 / 复制前的路径 */
  origPath?: string;
  /** raw diff 的状态字母：A M D R C T */
  status: string;
  /** 二进制文件为 null（numstat 那两列是 `-`） */
  added: number | null;
  deleted: number | null;
}

/**
 * 选中提交的详情：完整提交信息 + 改动文件。
 *
 * 合并提交按 first-parent 取 diff——不加 --diff-merges 的话 `git show` 对合并
 * 提交一个文件都不输出，面板上看着就像"这次合并什么都没改"。
 */
export interface GitCommitDetail {
  available: boolean;
  reason?: GitUnavailableReason;
  detail?: string;
  sha: string;
  short: string;
  author: string;
  authorEmail: string;
  authoredAt: number;
  committer: string;
  committedAt: number;
  parents: string[];
  refs: GitRefLabel[];
  /** 完整提交信息（含标题行） */
  message: string;
  files: GitCommitFile[];
  /** 改动文件总数；files 可能被截断 */
  fileCount: number;
}

/**
 * 提交请求。
 *
 * all 与 paths 是两条不同的实现路径，不是同一件事的两种写法：
 * - all=true 走 `git add -A` + 无 pathspec 的 commit，命令行长度恒定，
 *   所以"提交全部改动"不受文件数限制；
 * - 否则按 pathspec 提交选中的那些，路径要拼进命令行，有长度上限
 *   （Windows 远端尤其紧，见服务端的 COMMIT_PATHSPEC_BUDGET）。
 */
export interface GitCommitInput {
  message: string;
  all: boolean;
  /** all=false 时必填。重命名要同时给新旧两个路径 */
  paths?: string[];
}

/**
 * pull / push 的结果。
 *
 * 失败**不抛 4xx**：凭据不对、有冲突、远端拒绝都是仓库的正常状态，
 * 前端要原样把 git 说的话给用户看，而不是一句"操作失败"。
 */
export interface GitSyncResult {
  ok: boolean;
  reason?: GitUnavailableReason;
  /** git 的输出（成功时是 stdout，失败时优先 stderr），已截断到可读长度 */
  detail: string;
}

/**
 * 侧栏最后一层用的工作区文件计数。只跑 `status --porcelain`，不跑 numstat。
 *
 * +added = 新增 / 未跟踪 / 已修改 / 重命名（文件还在的改动）；
 * -deleted = 删除。环境事实写在 available 里，不抛 4xx。
 */
export interface GitChangeCounts {
  available: boolean;
  added: number;
  deleted: number;
}

/** 附属项目的工作区状态，删除前的预检 */
export interface WorktreeStatus {
  /** 目录还在不在（用户可能手工删了） */
  present: boolean;
  /** 已跟踪改动 + 未跟踪文件 */
  dirtyCount: number;
  /** 前若干条样例路径，直接进确认框的 list */
  dirtySample: string[];
  /**
   * 被 .gitignore 忽略的文件数。必须单列：git status --porcelain 默认不含它们，
   * 但 .env、本地 sqlite、上传目录会跟着一起被删——.env 通常是全世界唯一一份。
   */
  ignoredCount: number;
  /** 未推送提交数；null = 该分支没有 upstream */
  ahead: number | null;
  /** 读不到状态时的原因，确认框据此改口说"无法确认" */
  error?: string;
}

// ============ Session ============

export type SessionState = "active" | "unverified" | "dead";

export type DeadReason = "exited" | "backend-restart" | "link-lost" | "session-gone";

export interface Session {
  id: string;
  projectId: string;
  name: string;
  state: SessionState;
  /** 持久会话 = Zellij 包装，可在断链 / 后端重启后接回 */
  durable: boolean;
  /** durable=false 时的具体原因，UI 据此标注"为什么不持久"而非笼统的"非持久" */
  nonDurableReason?: NonDurableReason;
  deadReason?: DeadReason;
  createdAt: number;
  lastActiveAt: number;
}

export interface SessionWithProject extends Session {
  projectName: string;
  projectType: ProjectType;
}

/**
 * GET /api/sessions/:id/foreground 的返回：关 tab 前问"有没有程序在跑"。
 * 侦测不到（非持久 SSH、链路不通、探测失败）时 busy 恒为 false——
 * 它是道保险，自己坏了不能把关 tab 拦下来。
 */
export interface SessionForeground {
  busy: boolean;
  /** busy=true 时正在跑的命令行，供确认框展示 */
  command: string | null;
}

/**
 * 粘贴图片的大小上限。Retina 全屏截图的 PNG 能到 10 MB 上下，取 20 MB；
 * 前端超限时不发请求直接提示，后端的 bodyLimit 用同一个数兜底。
 */
export const PASTE_IMAGE_MAX_BYTES = 20 * 1024 * 1024;

/** POST /api/sessions/:id/paste-image 的返回：图片在会话宿主机上的绝对路径 */
export interface PasteImageResult {
  path: string;
}

/**
 * 创建会话。appearance 是当前 xterm 配色的深浅，不是界面主题——
 * 用户可以浅色 UI + Solarized Dark。后端据此写 COLORFGBG / GROK_APPEARANCE。
 */
export interface CreateSessionRequest {
  name?: string;
  appearance?: TermAppearance;
  /** #rrggbb，给 OSC 11 用；缺省按 appearance 给黑 / 白 */
  background?: string;
  foreground?: string;
}

// ============ Zellij 安装 ============

/** 安装阶段。宿主机自己下载，后端拿不到字节数，因此只有阶段没有百分比。 */
export type ZellijInstallStage =
  | "probing"
  | "downloading"
  | "extracting"
  | "verifying";

export type ZellijInstallFailure =
  | "arch-unsupported"
  | "no-downloader"
  | "no-tar"
  | "dir-not-writable"
  /** 探测宿主机失败：命令根本没跑通（SSH 抖动），或输出认不出是哪种系统 */
  | "probe-failed"
  | "download-failed"
  | "extract-failed"
  /** 跑不起来：noexec 挂载、或 Windows ARM 的 x64 模拟不可用 */
  | "verify-failed"
  | "cancelled";

/**
 * 后端会自动重试的失败：都属于"再来一次可能就好了"的瞬时故障——
 * 网络抖动、镜像 5xx、SSH 通道半路断开。
 *
 * verify-failed 不在其中：能走到验证说明包已完整解压（tar/zip 自带校验，
 * 截断的包在解压阶段就报错了），此时跑不起来的原因是 noexec 挂载或架构不兼容，
 * 重试只是把同一个错误再犯两遍，白白多下 14 MB。缺 curl / 缺 tar /
 * 目录不可写 / 架构无构建同理，都是稳定的环境事实，得先改环境。
 */
export const AUTO_RETRY_FAILURES: readonly ZellijInstallFailure[] = [
  "probe-failed",
  "download-failed",
  "extract-failed",
];

export function isAutoRetryable(reason: ZellijInstallFailure): boolean {
  return AUTO_RETRY_FAILURES.includes(reason);
}

/**
 * 手动重试是否有意义。
 *
 * 比自动重试宽得多：自动重试问的是"同样的环境再来一次能不能成"，手动重试问的是
 * "用户去远端装了 curl / 改了 noexec 挂载 / 连上了 VPN 之后能不能成"——
 * 除了架构没有官方构建和 Windows Job Object 这两个改不掉的事实，其余都值得给按钮。
 */
export function canRetryInstall(reason?: NonDurableReason): boolean {
  return reason !== "arch-unsupported" && reason !== "windows-job-object";
}

/**
 * 会话为什么不持久。UI 据此给出具体说明，而不是笼统的"非持久"——
 * 用户需要知道该去查什么。
 */
export type NonDurableReason =
  | ZellijInstallFailure
  /** 用户拒绝在该主机安装 Zellij */
  | "not-authorized"
  /**
   * 后端进程处于 Windows Job Object 中，Zellij server 会被连坐杀掉。
   * 与其静默失效，不如诚实标注非持久。
   */
  | "windows-job-object";

/** 某台宿主机上的 Zellij 状态。授权按主机记（host+port+username），不按项目。 */
export interface HostZellijStatus {
  /** null = 还没问过用户 */
  authorized: boolean | null;
  installedVersion?: string;
  /** 该主机的下载源；未设置时用官方地址 */
  baseUrl?: string;
  /** Windows 远端的真实断线验证结果；null = 未验证 */
  verifiedDurable: boolean | null;
  /** 官方默认下载源，供 UI 显示占位 */
  defaultBaseUrl: string;
  /** falcon 锁定的 Zellij 版本 */
  requiredVersion: string;
}

// ============ WebSocket 协议 ============

export type ClientMessage =
  | { type: "input"; data: string }
  | { type: "resize"; cols: number; rows: number }
  /** 当前 Viewer 的终端深浅；主题切换时再推一次，供 OSC 10/11/12 答复 */
  | {
      type: "appearance";
      appearance: TermAppearance;
      background?: string;
      foreground?: string;
    };

/**
 * 终端数据走二进制帧，不走 JSON：1 字节类型头 + UTF-8 载荷。
 * JSON 文本帧对 ANSI 密集数据的转义（\x1b → ）会把线上字节膨胀
 * 1.3~1.7 倍，且每帧多一次全量转义扫描；二进制帧还让前端能把 Uint8Array
 * 直接喂给 xterm.write，省一轮 UTF-8 → UTF-16 → UTF-8。
 */
export const TERM_FRAME_OUTPUT = 0x01;
/** 回放帧：前端先 reset 再写入 */
export const TERM_FRAME_REPLAY = 0x02;

export type ServerMessage =
  /** 控制类消息仍走 JSON 文本帧；output / replay 见 TERM_FRAME_* 二进制帧 */
  | { type: "state"; state: SessionState; deadReason?: DeadReason }
  /** SSH 断线自动重连中 */
  | { type: "reconnecting"; attempt: number }
  | { type: "error"; message: string };

/**
 * Zellij 安装通道（/ws/install/:projectId）。
 * 安装是主机级操作、可能耗时数十秒，不适合塞进创建会话的 REST 请求里。
 */
export type InstallClientMessage = { type: "cancel" };

export type InstallServerMessage =
  /** attempt 从 1 起；>1 表示后端在自动重试瞬时故障，UI 据此说明"为什么还在转" */
  | { type: "stage"; stage: ZellijInstallStage; attempt: number; command?: string }
  | { type: "done" }
  /** 失败原因用 NonDurableReason：除了安装本身失败，也可能是尚未授权 */
  | { type: "failed"; reason: NonDurableReason; detail?: string; attempts?: number };

// ============ REST API ============

export interface AuthStatus {
  /** 当前绑定是否要求认证 */
  required: boolean;
  authenticated: boolean;
  passwordSet: boolean;
}

export interface SystemInfo {
  platform: string;
  /** 本地会话能否持久；null = 尚未探测（首次创建本地会话时才探测） */
  localDurable: boolean | null;
  /** localDurable=false 时的原因 */
  localDurableReason?: NonDurableReason;
  version: string;
}

/** 目录浏览里的一项，只含文件夹（含指向目录的符号链接） */
export interface FsDirEntry {
  name: string;
  path: string;
}

/**
 * 后端机器上某一层目录的列表。
 *
 * 给本地项目选工作目录用：浏览器拿不到后端的真实路径，
 * `showDirectoryPicker` 也给不出服务端路径，只能后端自己列。
 *
 * `path === ""` 是 Windows 的盘符列表（虚拟层，不是真实目录）；
 * POSIX 没有这一层，根就是 `/`。
 */
export interface FsListing {
  path: string;
  /** 上一级。POSIX 根为 null；Windows 盘符根的上一级是 ""（盘符列表） */
  parent: string | null;
  home: string;
  roots: string[];
  entries: FsDirEntry[];
}

// ---------------- 项目文件浏览 ----------------

/**
 * 项目工作目录里的一项。
 *
 * `path` 是**工作目录相对**、一律以 `/` 分隔的路径——Windows 远端也是这样，
 * 平台分隔符只在后端拼绝对路径时才出现。前端拿它当 React key 与展开状态的键，
 * 不需要知道宿主机是什么平台。
 */
export interface WorkspaceEntry {
  name: string;
  path: string;
  /** 指向目录的符号链接算 dir——用户点它期待的是进去 */
  kind: "file" | "dir";
}

/** 工作目录下某一层的列表。目录在前、文件在后，各自按名字排 */
export interface WorkspaceListing {
  /** 工作目录相对路径；`""` 是工作目录本身 */
  path: string;
  entries: WorkspaceEntry[];
  /** 条目数超过 WORKSPACE_LIST_CAP，只给了前面一批（node_modules 这种） */
  truncated: boolean;
}

/** 一层目录最多列多少条。超过就截断——几万条目的列表画出来也没人看 */
export const WORKSPACE_LIST_CAP = 2000;

/**
 * ⌘P 文件索引的上限。git ls-files 对中型仓库很快，但把整份 node_modules
 * 塞进前端没有意义——Quick Open 也只会显示前几十条匹配。
 */
export const WORKSPACE_INDEX_CAP = 8000;

/** 工作目录下的文件路径清单，给 Quick Open 用。路径一律 `/` 分隔 */
export interface WorkspaceIndex {
  paths: string[];
  truncated: boolean;
}

/**
 * 查看单个文件的上限。base64 传输会膨胀 1/3，2MB 的文件已经是 2.7MB 的响应，
 * 再大就不是"查看"而是"下载"了。
 */
export const WORKSPACE_FILE_CAP = 2 * 1024 * 1024;

/**
 * 查看一个文件的结果。
 *
 * 类型判定在后端做（要看字节，前端只有 base64 会更啰嗦）：
 * text 直接给解好的 UTF-8 文本，image 给 base64 让前端拼 data URL，
 * 剩下的一律 binary，前端只报"这是二进制文件"。
 */
export type FilePreview =
  | {
      kind: "text";
      /** UTF-8 解码后的文本；超过上限时按字节截断（末尾可能缺半行） */
      text: string;
      size: number;
      truncated: boolean;
    }
  | { kind: "image"; mime: string; base64: string; size: number }
  | { kind: "binary"; size: number }
  /** 超过 WORKSPACE_FILE_CAP 且不是文本（文本会截断显示），只报大小 */
  | { kind: "too-large"; size: number };

/**
 * 宿主机上可用 shell 的侦测结果（项目表单的 shell 选择用）。
 *
 * `default` 是不设覆盖时后端实际会用的 shell：POSIX 为探测到的登录 shell，
 * Windows 一律 PowerShell；恒等于 shells[0]。
 */
export interface ShellsInfo {
  kind: "posix" | "windows";
  default: string;
  /** 侦测到的可用 shell 绝对路径，去重后默认项排最前 */
  shells: string[];
}

export interface ApiError {
  error: string;
}

/**
 * 删除项目的结果。
 *
 * warnings 是文件系统清理的**非致命**失败，含残留路径。删除一律返回 200：
 * DB 行无条件删掉，文件系统清理 best-effort——留一条删不掉的项目行，用户唯一的
 * 出路是去改 SQLite；残留目录他自己删得掉，前提是我们把路径原样告诉他。
 */
export interface DeleteProjectResult {
  ok: true;
  warnings?: string[];
}
