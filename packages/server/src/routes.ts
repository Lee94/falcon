import fs from "node:fs";
import { Readable } from "node:stream";
import type { FastifyInstance, FastifyReply } from "fastify";
import type {
  AuthStatus,
  DeleteProjectResult,
  FsListing,
  GitChangeCounts,
  GitCommitDetail,
  GitCommitInput,
  GitFileDiff,
  GitOpInput,
  GitLogPage,
  GitRefsInfo,
  GitSnapshot,
  GitSyncResult,
  GitUnavailableReason,
  GitWorkingChanges,
  HostZellijStatus,
  MultiDeriveError,
  MultiRepoProbe,
  MultiWorktreeInput,
  PortForward,
  PortForwardInput,
  ProjectInput,
  RepoInfo,
  SessionWithProject,
  ShellsInfo,
  SshHost,
  SshHostInput,
  SshProbeResult,
  SystemInfo,
  WorktreeFailure,
  WorktreeInput,
  WorktreeStatus,
  WorkspaceFile,
  WorkspaceIndex,
  UploadResult,
  FileOpResult,
  FileRemoveResult,
  WorkspaceListing,
} from "@falcon/shared";
import {
  PASTE_IMAGE_MAX_BYTES,
  WORKSPACE_RAW_CAP,
  isSessionAgent,
  sanitizeColorHint,
} from "@falcon/shared";
import { AskpassCancelled, AskpassTimeout, type AskpassHub } from "./askpass/hub.js";
import { Db, type ProjectRow, type SshHostRow } from "./db.js";
import { listDirectories, listRemoteDirectories } from "./fs.js";
import {
  capFileIndex,
  indexWorkspace,
  listWorkspace,
  mkdirWorkspace,
  mimeOf,
  readWorkspaceBytes,
  readWorkspaceFile,
  removeWorkspace,
  renameWorkspace,
  type FileHost,
} from "./files.js";
import { contentDisposition, openDownload, receiveUpload, type DownloadSource } from "./transfer.js";
import { detectShells } from "./shells.js";
import { defaultLocalShell } from "./sessions/local.js";
import { localExec, localKind } from "./zellij/exec.js";
import {
  imageExt,
  pasteDir,
  pasteFileName,
  posixWriteCommand,
  windowsWriteCommand,
  writeLocalPasteFile,
} from "./paste.js";
import type { SecretBox } from "./crypto.js";
import type { Auth } from "./auth.js";
import { ForwardConflictError } from "./sessions/forward.js";
import { hostAsProject, type SessionManager } from "./sessions/manager.js";
import { parseDefaultWorktreeBranch, parseGitOpInput } from "./git/command.js";
import { gitErrorLine, WorktreeError, worktreeFailureText } from "./git/error.js";
import { gitHostFor, hostKeyOf } from "./git/host.js";
import { repoLockKey, withRepoLock } from "./git/lock.js";
import {
  deriveMultiWorktrees,
  MultiVetoError,
  MultiWorktreeError,
  validateMemberList,
} from "./git/multi.js";
import {
  basenameOf,
  dirnameOf,
  isAbsolute,
  isAncestor,
  isUnc,
  joinPath,
  pathDepth,
  siblingWorktreePath,
  vetoTargetDir,
} from "./git/path.js";

import { cleanupWorktree } from "./git/remove.js";
import {
  centralManifestFiles,
  posixWriteManifestCommand,
  windowsWriteManifestCommands,
  writeLocalManifest,
} from "./virtualdir.js";
import {
  addWorktree,
  commitWorking,
  describeCommit,
  describeCommitDiff,
  describeGit,
  describeGitChanges,
  describeGitChangesMany,
  describeGitDiff,
  describeGitLog,
  describeGitRefs,
  describeRepo,
  describeWorkingChanges,
  listRepoFiles,
  pathExists,
  repoRoot,
  runGitOp,
  syncGit,
  unavailableChanges,
  unavailableCommit,
  unavailableDiff,
  unavailableLog,
  unavailableRefs,
  unavailableSnapshot,
  unavailableWorking,
  worktreeStatus,
} from "./git/repo.js";
import { DEFAULT_BASE_URL, ZELLIJ_VERSION } from "./zellij/version.js";
import type { MeegleClient } from "./meegle/client.js";
import { registerMeegleRoutes } from "./meegle/routes.js";

/**
 * WorktreeFailure → HTTP 码。
 *
 * 环境事实与状态冲突一律 409（用户去改环境 / 换个分支就能过），
 * 只有"命令没跑起来"和"git 自己失败了"才是 502。
 */
const GIT_UNAVAILABLE = new Set<WorktreeFailure>([
  "git-missing",
  "not-a-repo",
  "no-working-dir",
  "link-failed",
]);

/**
 * 探测类端点共用：把 WorktreeError 的 reason 收敛到面板认识的那四种。
 * 派生专属的失败（branch-in-use 之类）在这些端点上根本不该出现，出现了
 * 也只能当"命令没跑起来"报出去。
 */
function gitReasonOf(err: WorktreeError): GitUnavailableReason {
  return GIT_UNAVAILABLE.has(err.reason)
    ? (err.reason as GitUnavailableReason)
    : "link-failed";
}

const WORKTREE_STATUS: Record<WorktreeFailure, number> = {
  "git-missing": 409,
  "not-a-repo": 409,
  "no-working-dir": 409,
  "branch-in-use": 409,
  "branch-exists": 409,
  "branch-unknown": 409,
  "path-occupied": 409,
  "path-inside-repo": 409,
  "path-too-long": 409,
  "worktree-add-failed": 502,
  "link-failed": 502,
};


export interface RouteDeps {
  db: Db;
  auth: Auth;
  manager: SessionManager;
  secrets: SecretBox;
  version: string;
  /** falcon 数据目录，本地会话的粘贴图片落在 <dataDir>/paste */
  dataDir: string;
  askpass: AskpassHub;
  meegle: MeegleClient;
}

export function registerRoutes(app: FastifyInstance, deps: RouteDeps) {
  const { db, auth, manager, secrets, askpass, meegle } = deps;

  // 粘贴图片的请求体是原始图片字节。fastify 默认只认 JSON，这里按原样收成 Buffer
  app.addContentTypeParser(
    /^image\//,
    { parseAs: "buffer", bodyLimit: PASTE_IMAGE_MAX_BYTES },
    (_req, body, done) => done(null, body)
  );
  // 上传的请求体原样以流的形式交给路由（PUT /upload），不攒、不设上限：
  // 字节直接接到宿主机的写入端，几百 MB 的文件也不经过内存
  app.addContentTypeParser("application/octet-stream", (_req, payload, done) =>
    done(null, payload)
  );

  app.addHook("onRequest", async (req, reply) => {
    const url = req.url;
    if (!url.startsWith("/api/")) return;
    if (url.startsWith("/api/auth/")) return;
    // 原始字节路由自己验作用域令牌（见下方 /raw/），cookie 在那里只是可选的加分项
    if ((req.routeOptions.config as { rawToken?: boolean } | undefined)?.rawToken) return;
    if ((req.routeOptions.config as { askpassHelper?: boolean } | undefined)?.askpassHelper) {
      return;
    }
    if (!auth.isAuthenticated(req)) {
      reply.code(401).send({ error: "未认证" });
    }
  });

  // ---- auth ----

  app.get("/api/auth/status", async (req): Promise<AuthStatus> => {
    return {
      required: auth.required(),
      authenticated: auth.isAuthenticated(req),
      passwordSet: auth.passwordSet(),
    };
  });

  app.post("/api/auth/login", async (req, reply) => {
    const { password } = (req.body ?? {}) as { password?: string };
    const token = password ? auth.login(password) : null;
    if (!token) return reply.code(401).send({ error: "密码错误" });
    auth.setCookie(reply, token);
    return { ok: true };
  });

  app.post("/api/auth/logout", async (req, reply) => {
    const token = (req.cookies as Record<string, string | undefined>)?.[
      auth.cookieName()
    ];
    auth.logout(token);
    auth.clearCookie(reply);
    return { ok: true };
  });

  app.post("/api/auth/password", async (req, reply) => {
    const { current, next } = (req.body ?? {}) as { current?: string; next?: string };
    if (!next || next.length < 6) {
      return reply.code(400).send({ error: "密码至少 6 位" });
    }
    if (auth.passwordSet() && !auth.isAuthenticated(req)) {
      return reply.code(401).send({ error: "未认证" });
    }
    if (!auth.setPassword(next, current)) {
      return reply.code(400).send({ error: "当前密码不正确" });
    }
    return { ok: true };
  });

  // ---- system / fs ----

  /**
   * sudo askpass helper 长轮询。Bearer 是写进宿主机 conf 的专用令牌，
   * 不能走登录 cookie——helper 不是浏览器。
   */
  app.post("/api/askpass", { config: { askpassHelper: true } }, async (req, reply) => {
    const header = req.headers.authorization;
    if (!askpass.tokenMatches(typeof header === "string" ? header : undefined)) {
      return reply.code(401).send({ error: "未认证" });
    }
    const body = (req.body ?? {}) as { prompt?: unknown; sessionId?: unknown };
    const prompt = typeof body.prompt === "string" && body.prompt ? body.prompt : "Password:";
    const sessionId = typeof body.sessionId === "string" ? body.sessionId : undefined;
    try {
      const password = await askpass.request({ prompt, sessionId });
      return { password };
    } catch (err) {
      if (err instanceof AskpassCancelled) return reply.code(409).send({ error: "cancelled" });
      if (err instanceof AskpassTimeout) return reply.code(504).send({ error: "timeout" });
      throw err;
    }
  });

  app.get("/api/askpass/pending", async () => {
    return askpass.pendingPrompts().map((p) => ({ id: p.id, prompt: p.prompt }));
  });

  app.post("/api/askpass/:id/answer", async (req, reply) => {
    const { id } = req.params as { id: string };
    const body = (req.body ?? {}) as { password?: unknown; cancel?: unknown };
    if (body.cancel === true) {
      if (!askpass.cancel(id)) return reply.code(404).send({ error: "不存在" });
      return { ok: true };
    }
    if (typeof body.password !== "string") {
      return reply.code(400).send({ error: "missing password" });
    }
    if (!askpass.answer(id, body.password)) return reply.code(404).send({ error: "不存在" });
    return { ok: true };
  });

  app.get("/api/system", async (): Promise<SystemInfo> => {
    // 只报已知状态，不触发探测/下载——那是首次创建本地会话时才做的事
    const local = manager.localDurableState();
    return {
      platform: process.platform,
      localDurable: local ? local.durable : null,
      localDurableReason: local?.reason,
      version: deps.version,
    };
  });

  app.post("/api/fs/validate", async (req) => {
    const { path: p } = (req.body ?? {}) as { path?: string };
    if (!p) return { ok: false, error: "路径为空" };
    try {
      const stat = fs.statSync(p);
      if (!stat.isDirectory()) return { ok: false, error: "不是文件夹" };
      return { ok: true };
    } catch {
      return { ok: false, error: "路径不存在或不可访问" };
    }
  });

  /**
   * 列子目录。不带 hostId/projectId 时列后端本机；带了就走 SSH 列远端。
   * query 缺省是家目录；`path=` 空字符串是 Windows 盘符列表。
   * 读失败回 400，不回 500——路径不存在或 SSH 连不上都是调用方能处理的。
   */
  /** hostId / projectId → 该远端的 SshLink。两个只读探查路由共用这段解析。 */
  const resolveLink = (hostId?: string, projectId?: string) => {
    if (projectId) {
      const row = db.getProject(projectId);
      if (!row) throw new Error("项目不存在");
      if (row.type !== "ssh") throw new Error("只有 SSH 项目能访问远端");
      return manager.getLink(row);
    }
    const host = db.getHost(hostId!);
    if (!host) throw new Error("主机不存在");
    return manager.getHostLink(host);
  };

  app.get("/api/fs/list", async (req, reply): Promise<FsListing | void> => {
    const { path: p, hostId, projectId } = req.query as {
      path?: string;
      hostId?: string;
      projectId?: string;
    };
    try {
      if (!hostId && !projectId) return await listDirectories(p);
      const link = resolveLink(hostId, projectId);
      const facts = await link.hostFacts();
      return await listRemoteDirectories(link.exec, facts.kind, facts.home, p);
    } catch (err) {
      return reply.code(400).send({ error: (err as Error).message });
    }
  });

  /**
   * 侦测宿主机上可用的 shell，供项目表单的 shell 选择。
   * 不带 hostId/projectId 时侦测后端本机；带了就经 SSH 侦测远端。
   * 探测命令本身失败不算错（至少有默认项），连不上远端才回 400。
   */
  app.get("/api/shells", async (req, reply): Promise<ShellsInfo | void> => {
    const { hostId, projectId } = req.query as { hostId?: string; projectId?: string };
    try {
      if (!hostId && !projectId) {
        return await detectShells(localExec, localKind(), defaultLocalShell());
      }
      const link = resolveLink(hostId, projectId);
      const facts = await link.hostFacts();
      return await detectShells(link.exec, facts.kind, facts.shell);
    } catch (err) {
      return reply.code(400).send({ error: (err as Error).message });
    }
  });

  // ---- projects ----

  function validateSshFields(ssh: NonNullable<ProjectInput["ssh"]> | SshHostInput): string | null {
    if (!ssh.host?.trim()) return "SSH 主机不能为空";
    if (!ssh.username?.trim()) return "SSH 用户名不能为空";
    if (ssh.port != null && (!Number.isInteger(ssh.port) || ssh.port < 1 || ssh.port > 65535)) {
      return "端口无效";
    }
    if (ssh.authMethod === "key" && !ssh.keyPath?.trim()) return "密钥认证必须指定私钥路径";
    if (ssh.authMethod !== "key" && ssh.authMethod !== "password" && ssh.authMethod !== "agent") {
      return "未知认证方式";
    }
    return null;
  }

  function validateHostInput(input: SshHostInput): string | null {
    if (!input.name?.trim()) return "主机名称不能为空";
    return validateSshFields(input);
  }

  /**
   * 成员仓库清单的校验 + 归一化。repos: null 表示请求里没带（不是多仓库容器 /
   * 编辑时成员不变）。local 逐成员 stat；ssh 成员刻意不预检——与 ssh 项目的
   * workingDir 零校验同理，连通性与"是不是仓库"都是派生时的事。
   */
  function resolveRepos(
    input: ProjectInput
  ): { ok: true; repos: string[] | null } | { ok: false; error: string } {
    if (input.repos == null) return { ok: true, repos: null };
    const parsed = validateMemberList(input.repos);
    if (!parsed.ok) return { ok: false, error: parsed.error };
    if (input.type === "local") {
      for (const dir of parsed.repos) {
        // statSync 会把相对路径解析到后端进程的 cwd 上，必须先拦绝对性
        if (!isAbsolute(localKind(), dir)) {
          return { ok: false, error: `成员仓库必须是绝对路径：${dir}` };
        }
        try {
          if (!fs.statSync(dir).isDirectory()) {
            return { ok: false, error: `成员路径不是文件夹：${dir}` };
          }
        } catch {
          return { ok: false, error: `成员文件夹不存在或不可访问：${dir}` };
        }
      }
    }
    return { ok: true, repos: parsed.repos };
  }

  /** container = 多仓库容器：workingDir 只是可选的会话 cwd（留空 = 家目录，ssh 先例） */
  function validateProjectInput(input: ProjectInput, container = false): string | null {
    if (!input.name?.trim()) return "项目名称不能为空";
    if (input.type === "local") {
      if (!input.workingDir?.trim()) {
        if (!container) return "本地项目必须指定文件夹路径";
      } else {
        try {
          if (!fs.statSync(input.workingDir).isDirectory()) return "路径不是文件夹";
        } catch {
          return "文件夹路径不存在或不可访问";
        }
      }
    } else if (input.type === "ssh") {
      // 选了已保存主机时连接配置从主机复制，ssh 字段忽略
      if (!input.hostId && !input.ssh) return "请选择一台已保存的远端主机";
      if (!input.hostId && input.ssh) {
        const sshErr = validateSshFields(input.ssh);
        if (sshErr) return sshErr;
      }
    } else {
      return "未知项目类型";
    }
    const base = parseDefaultWorktreeBranch(input.defaultWorktreeBranch);
    if (!base.ok) return base.error;
    return null;
  }

  type ProjectSshCols = Pick<
    ProjectRow,
    | "host_id"
    | "ssh_host"
    | "ssh_port"
    | "ssh_username"
    | "ssh_auth_method"
    | "ssh_key_path"
    | "ssh_secret_enc"
  >;

  function sshFromHost(host: SshHostRow): ProjectSshCols {
    return {
      host_id: host.id,
      ssh_host: host.host,
      ssh_port: host.port,
      ssh_username: host.username,
      ssh_auth_method: host.auth_method,
      ssh_key_path: host.key_path,
      ssh_secret_enc: host.secret_enc,
    };
  }

  function resolveProjectSsh(
    input: ProjectInput,
    existing?: ProjectRow
  ): { ok: true; ssh: ProjectSshCols } | { ok: false; error: string } {
    if (input.type !== "ssh") {
      return {
        ok: true,
        ssh: {
          host_id: null,
          ssh_host: null,
          ssh_port: null,
          ssh_username: null,
          ssh_auth_method: null,
          ssh_key_path: null,
          ssh_secret_enc: null,
        },
      };
    }
    if (input.hostId) {
      const host = db.getHost(input.hostId);
      if (!host) return { ok: false, error: "所选主机不存在" };
      return { ok: true, ssh: sshFromHost(host) };
    }
    return {
      ok: true,
      ssh: {
        host_id: null,
        ssh_host: input.ssh?.host?.trim() ?? existing?.ssh_host ?? null,
        ssh_port: input.ssh?.port ?? existing?.ssh_port ?? 22,
        ssh_username: input.ssh?.username?.trim() ?? existing?.ssh_username ?? null,
        ssh_auth_method: input.ssh?.authMethod ?? existing?.ssh_auth_method ?? null,
        ssh_key_path:
          input.ssh?.authMethod === "key"
            ? (input.ssh?.keyPath?.trim() ?? existing?.ssh_key_path ?? null)
            : input.ssh
              ? null
              : (existing?.ssh_key_path ?? null),
        ssh_secret_enc: input.ssh?.secret
          ? secrets.encrypt(input.ssh.secret)
          : (existing?.ssh_secret_enc ?? null),
      },
    };
  }

  app.get("/api/projects", async () => {
    return db.listProjects().map(Db.toProject);
  });

  app.post("/api/projects", async (req, reply) => {
    const input = req.body as ProjectInput;
    const repos = resolveRepos(input);
    if (!repos.ok) return reply.code(400).send({ error: repos.error });
    const err = validateProjectInput(input, repos.repos != null);
    if (err) return reply.code(400).send({ error: err });

    const ssh = resolveProjectSsh(input);
    if (!ssh.ok) return reply.code(400).send({ error: ssh.error });
    const defaultBranch = parseDefaultWorktreeBranch(input.defaultWorktreeBranch);
    const defaultWorktreeBranch = defaultBranch.ok ? defaultBranch.value : null;

    const row: ProjectRow = {
      id: crypto.randomUUID(),
      name: input.name.trim(),
      type: input.type,
      working_dir: input.workingDir?.trim() || null,
      shell: input.shell?.trim() || null,
      ...ssh.ssh,
      created_at: Date.now(),
      // 普通项目：worktree 四列一律 null。附属项目只能经
      // POST /api/projects/:id/worktrees 创建，绝不从这个端点进来
      source_project_id: null,
      worktree_branch: null,
      worktree_repo_dir: null,
      worktree_created_by_mojito: null,
      worktree_archived_at: null,
      // repos 有值 ⇒ 多仓库容器。派生产物的 multi_repos（带 repoDir 的那种）
      // 同样只能经 worktrees 端点进来
      multi_repos: repos.repos ? JSON.stringify(repos.repos.map((dir) => ({ dir }))) : null,
      default_worktree_branch: defaultWorktreeBranch,
    };
    db.insertProject(row);
    return Db.toProject(row);
  });

  app.put("/api/projects/:id", async (req, reply) => {
    const { id } = req.params as { id: string };
    const existing = db.getProject(id);
    if (!existing) return reply.code(404).send({ error: "项目不存在" });
    const input = req.body as ProjectInput;
    if (input.type !== existing.type)
      return reply.code(400).send({ error: "项目类型不可更改" });
    // 附属项目的工作目录就是删除目标。它一旦能从这里改写，"删项目会删目录"
    // 就变成了"删项目会删你填的任意路径"——这是本功能唯一的灾难性风险。
    // 护栏还有深度 / 仓库根 / home / worktree 注册这几层，但第一层必须在这儿。
    if (
      existing.source_project_id &&
      (input.workingDir?.trim() || null) !== existing.working_dir
    ) {
      return reply.code(400).send({ error: "附属项目的工作目录由 worktree 决定，不可修改" });
    }
    // 成员清单的写入护栏，与 working_dir 同构：派生行的成员是删除目标，显式拒绝
    // 是第一道防线（updateMultiRepos 的 SQL 条件是第二道）；普通项目也不能凭空
    // 变成容器——判别式在创建时定死，与 worktree 同一条规矩。
    if (input.repos !== undefined) {
      if (existing.source_project_id) {
        return reply.code(400).send({ error: "附属项目的成员由派生决定，不可修改" });
      }
      if (existing.multi_repos == null) {
        return reply.code(400).send({ error: "普通项目不能改成多仓库项目" });
      }
    }
    const repos = resolveRepos(input);
    if (!repos.ok) return reply.code(400).send({ error: repos.error });
    const container = existing.multi_repos != null && !existing.source_project_id;
    const err = validateProjectInput(input, container);
    if (err) return reply.code(400).send({ error: err });

    const ssh = resolveProjectSsh(input, existing);
    if (!ssh.ok) return reply.code(400).send({ error: ssh.error });
    const defaultBranch = parseDefaultWorktreeBranch(input.defaultWorktreeBranch);
    const defaultWorktreeBranch = defaultBranch.ok ? defaultBranch.value : null;

    const row: ProjectRow = {
      ...existing,
      name: input.name.trim(),
      working_dir: input.workingDir?.trim() || null,
      shell: input.shell?.trim() || null,
      ...ssh.ssh,
      // 附属项目不能再派生，这项对它们没有意义；仍照单全收，避免 PUT 形状因行而异
      default_worktree_branch: defaultWorktreeBranch,
    };
    db.updateProject(row);
    if (container && repos.repos) {
      db.updateMultiRepos(row.id, repos.repos.map((dir) => ({ dir })));
      row.multi_repos = JSON.stringify(repos.repos.map((dir) => ({ dir })));
    }
    if (container) {
      // 名字与成员都会进虚拟目录的清单（见 virtualdir.ts），编辑后作废缓存。
      // 刷新是 fire-and-forget：宿主离线不能挡编辑；正在跑的会话尽快看到新清单
      // 即可，失败也无妨——attach 路径不走缓存，下次开会话/文件面板会重写
      manager.invalidateVirtualDir(row.id);
      if (!row.working_dir) {
        void manager
          .ensureVirtualDir(row, { refresh: true })
          .catch((err) => req.log.warn({ err }, "虚拟项目目录刷新失败（下次开会话时会重写）"));
      }
    }
    // 附属项目的 ssh_* 是从源项目复制来的（让 SshLink / getLink / GET host 全都
    // 不用改），代价就是这条手动传播。本地项目没有可传播的东西。
    if (row.type === "ssh") db.updateChildrenSsh(row.id, row);
    // 连接配置可能已变化，废弃旧链路（不影响已附着的会话，直到下次断链）
    return Db.toProject(row);
  });

  /**
   * 删除项目，连坐它的附属项目。
   *
   * 顺序上有三个硬约束：
   * 1. 会话必须先死透，再动目录（Zellij pane 的 cwd 就在里面，见 terminate 的注释）。
   * 2. disposeLink 必须排在 git 命令之后——那些命令要经这条 SSH 链路跑。
   * 3. DB 行无条件删，文件系统清理 best-effort。留一条删不掉的项目行，用户唯一的
   *    出路是去改 SQLite；残留目录他自己删得掉，路径已经写进 warnings 了。
   */
  app.delete("/api/projects/:id", async (req, reply): Promise<DeleteProjectResult | void> => {
    const { id } = req.params as { id: string };
    const { force } = req.query as { force?: string };
    const project = db.getProject(id);
    if (!project) return reply.code(404).send({ error: "项目不存在" });

    // 附属项目连坐。会话数要把它们的也算进去，否则确认框上的数字对不上
    const children = project.source_project_id ? [] : db.listWorktreeChildren(id);
    const targets = [...children, project]; // 先删附属，最后删自己
    const alive = targets.flatMap((p) =>
      db.listSessionsByProject(p.id).filter((s) => s.state !== "dead")
    );
    if (alive.length > 0 && force !== "true") {
      return reply.code(409).send({ error: `项目仍有 ${alive.length} 个未终止的会话` });
    }

    // 先把所有会话终止掉再动任何目录。这一步失败就整体放弃——带着活进程删目录
    // 是整条流程里唯一真会毁数据的操作，不值得为它赌一把
    try {
      for (const s of alive) {
        await manager.terminate(s.id, { waitGone: true });
      }
    } catch (err) {
      return reply.code(502).send({ error: `终止会话失败，未做任何清理：${(err as Error).message}` });
    }

    const doomed = new Set(targets.map((p) => p.id));
    // guardDirsOf 除 working_dir 外还收多仓库项目的成员路径——容器的成员是
    // 用户的真仓库，别的删除不许踩上去
    const otherDirs = db
      .listProjects()
      .filter((p) => !doomed.has(p.id))
      .flatMap((p) => Db.guardDirsOf(p));

    const warnings: string[] = [];
    for (const row of targets) {
      // 只有附属项目才碰文件系统。源项目的目录不是 falcon 建的，永远不动
      if (row.source_project_id) {
        try {
          warnings.push(...(await cleanupWorktree(row, await gitHostFor(row, manager), otherDirs)));
        } catch (err) {
          const e = err as WorktreeError;
          warnings.push(
            `没能在宿主机上执行清理，目录未删除：${row.working_dir}（${e.detail ?? e.message}）`
          );
        }
      } else if (row.multi_repos != null) {
        // 容器：清 falcon 根下的虚拟项目目录（克制删除，非空保留 + warning，
        // 见 manager.removeVirtualDir）。它在 falcon 自己的数据根里，链路都
        // 连不上时连目录在不在都不知道，只记日志不打扰用户
        try {
          const w = await manager.removeVirtualDir(row);
          if (w) warnings.push(w);
        } catch (err) {
          req.log.warn({ err }, "虚拟项目目录清理失败");
        }
      }
      manager.disposeLink(row.id);
      db.deleteProject(row.id);
    }
    return warnings.length > 0 ? { ok: true, warnings } : { ok: true };
  });

  // ---- saved SSH hosts ----

  app.get("/api/hosts", async (): Promise<SshHost[]> => {
    return db.listHosts();
  });

  app.post("/api/hosts", async (req, reply) => {
    const input = (req.body ?? {}) as SshHostInput;
    const err = validateHostInput(input);
    if (err) return reply.code(400).send({ error: err });
    const name = input.name.trim();
    if (db.findHostByName(name)) {
      return reply.code(409).send({ error: "已有同名主机" });
    }
    const row: SshHostRow = {
      id: crypto.randomUUID(),
      name,
      host: input.host.trim(),
      port: input.port || 22,
      username: input.username.trim(),
      auth_method: input.authMethod,
      key_path: input.authMethod === "key" ? input.keyPath?.trim() || null : null,
      secret_enc: input.secret ? secrets.encrypt(input.secret) : null,
      created_at: Date.now(),
    };
    db.insertHost(row);
    return Db.toSshHost(row, 0);
  });

  app.put("/api/hosts/:id", async (req, reply) => {
    const { id } = req.params as { id: string };
    const existing = db.getHost(id);
    if (!existing) return reply.code(404).send({ error: "主机不存在" });
    const input = (req.body ?? {}) as SshHostInput;
    const err = validateHostInput(input);
    if (err) return reply.code(400).send({ error: err });
    const name = input.name.trim();
    if (db.findHostByName(name, id)) {
      return reply.code(409).send({ error: "已有同名主机" });
    }
    const row: SshHostRow = {
      ...existing,
      name,
      host: input.host.trim(),
      port: input.port || 22,
      username: input.username.trim(),
      auth_method: input.authMethod,
      key_path:
        input.authMethod === "key"
          ? (input.keyPath?.trim() || existing.key_path)
          : null,
      secret_enc: input.secret ? secrets.encrypt(input.secret) : existing.secret_enc,
    };
    db.updateHost(row);
    db.updateProjectsFromHost(row);
    // 凭据可能已经变了，丢掉浏览用的缓存连接
    manager.disposeHostLink(id);
    return Db.toSshHost(row, db.countProjectsByHost(id));
  });

  app.delete("/api/hosts/:id", async (req, reply) => {
    const { id } = req.params as { id: string };
    const existing = db.getHost(id);
    if (!existing) return reply.code(404).send({ error: "主机不存在" });
    const n = db.countProjectsByHost(id);
    if (n > 0) {
      return reply.code(409).send({ error: `有 ${n} 个项目正在使用该主机，请先改绑或删除这些项目` });
    }
    db.deleteHost(id);
    manager.disposeHostLink(id);
    return { ok: true };
  });

  /**
   * 试连一台已保存主机。连不上是环境事实，200 + ok:false，不 4xx。
   */
  app.post("/api/hosts/:id/test", async (req, reply): Promise<SshProbeResult | void> => {
    const { id } = req.params as { id: string };
    const host = db.getHost(id);
    if (!host) return reply.code(404).send({ error: "主机不存在" });
    return probeHost(host);
  });

  /**
   * 试连表单里这组还没保存（或正在改）的凭据。
   * 编辑已有主机时带 hostId：secret / keyPath 留空则沿用已保存的。
   */
  app.post("/api/hosts/test", async (req, reply): Promise<SshProbeResult | void> => {
    const input = (req.body ?? {}) as SshHostInput & { hostId?: string };
    const existing = input.hostId ? db.getHost(input.hostId) : null;
    if (input.hostId && !existing) return reply.code(404).send({ error: "主机不存在" });
    const err = validateSshFields(input);
    if (err) return { ok: false, error: err };
    const row: SshHostRow = {
      id: existing?.id ?? `draft:${crypto.randomUUID()}`,
      name: input.name?.trim() || existing?.name || "test",
      host: input.host.trim(),
      port: input.port || 22,
      username: input.username.trim(),
      auth_method: input.authMethod,
      key_path:
        input.authMethod === "key"
          ? (input.keyPath?.trim() || existing?.key_path || null)
          : null,
      secret_enc: input.secret ? secrets.encrypt(input.secret) : (existing?.secret_enc ?? null),
      created_at: existing?.created_at ?? Date.now(),
    };
    return probeHost(row);
  });

  async function probeHost(host: SshHostRow): Promise<SshProbeResult> {
    try {
      const facts = await manager.probeSsh(hostAsProject(host));
      return { ok: true, kind: facts.kind, home: facts.home };
    } catch (err) {
      return { ok: false, error: (err as Error).message };
    }
  }

  // ---- worktree（附属项目） ----

  /**
   * 源项目的仓库信息。
   *
   * **探测端点永不因环境事实报错**：没装 git、不是仓库、没填工作目录，一律 200 +
   * derivable:false + reason，让前端渲染一句具体说明。形状与
   * GET /api/projects/:id/host 返回 HostZellijStatus 同源（那边的 authorized:null
   * 也是"还没问过"而不是错误）。同样的事实在 POST 那边才当状态冲突（409）。
   */
  app.get("/api/projects/:id/repo", async (req, reply): Promise<RepoInfo | void> => {
    const { id } = req.params as { id: string };
    const row = db.getProject(id);
    if (!row) return reply.code(404).send({ error: "项目不存在" });
    if (row.source_project_id) {
      return reply.code(400).send({ error: "附属项目不能再派生" });
    }
    if (row.multi_repos != null) {
      return reply.code(400).send({ error: "多仓库项目请用批量派生（GET /repos）" });
    }
    if (!row.working_dir) {
      return {
        derivable: false,
        reason: "no-working-dir",
        detail: worktreeFailureText("no-working-dir"),
        branches: [],
      };
    }
    try {
      return await describeRepo(await gitHostFor(row, manager), row.working_dir);
    } catch (err) {
      const e = err as WorktreeError;
      return {
        derivable: false,
        reason: e.reason ?? "link-failed",
        detail: e.detail ?? e.message,
        branches: [],
      };
    }
  });

  /**
   * 多仓库容器的派生前探测：逐成员的 RepoInfo。与 GET /repo 同一条规矩——
   * 环境事实（没装 git、不是仓库、连不上）永不 4xx，写在每个成员的
   * derivable/reason 里，前端据此逐行渲染、任一成员不可派生就禁用提交。
   */
  app.get("/api/projects/:id/repos", async (req, reply): Promise<MultiRepoProbe | void> => {
    const { id } = req.params as { id: string };
    const row = db.getProject(id);
    if (!row) return reply.code(404).send({ error: "项目不存在" });
    const members = Db.parseMultiRepos(row);
    if (!members || row.source_project_id) {
      return reply.code(400).send({ error: "不是多仓库容器" });
    }

    const unavailable = (reason: WorktreeFailure, detail?: string): RepoInfo => ({
      derivable: false,
      reason,
      detail,
      branches: [],
    });

    try {
      const host = await gitHostFor(row, manager);
      const out: MultiRepoProbe = { members: [] };
      // 成员间串行：describeRepo 内部已是批量往返，一个探测端点不值得并发压宿主机
      for (const m of members) {
        try {
          const info = await describeRepo(host, m.dir);
          out.members.push({ dir: m.dir, info });
          // 集中目录默认建在第一个成员仓库根的父目录下，给前端预览用
          if (out.baseDir == null && info.repoDir) {
            out.baseDir = dirnameOf(host.kind, info.repoDir);
          }
        } catch (err) {
          const e = err as WorktreeError;
          out.members.push({
            dir: m.dir,
            info: unavailable(e.reason ?? "link-failed", e.detail ?? e.message),
          });
        }
      }
      return out;
    } catch (err) {
      // 链路 / git 探测失败：所有成员同一个答案，不用逐个再试
      const e = err as WorktreeError;
      return {
        members: members.map((m) => ({
          dir: m.dir,
          info: unavailable(e.reason ?? "link-failed", e.detail ?? e.message),
        })),
      };
    }
  });

  /**
   * git 面板端点的目标目录解析。
   *
   * 非多仓库行忽略 repo 参数，照旧用 working_dir。多仓库行的 working_dir 不是
   * 仓库（容器 = 可选会话 cwd，派生行 = 集中目录），git 目标是某个成员：
   * repo 给了必须**精确等于**某个成员 dir——前端从 project.multi.repos 原样带回，
   * 不需要归一化比较，不匹配就是请求造错了（400）；repo 缺省时读端点回退第一个
   * 成员（面板至少有东西看），写端点（commit / pull / push / op）拒绝——写操作不猜。
   */
  function gitTargetOf(
    row: ProjectRow,
    repo: string | undefined,
    write = false
  ): { dir: string } | { badRequest: string } | { noWorkingDir: true } {
    const members = Db.parseMultiRepos(row);
    if (!members) {
      return row.working_dir ? { dir: row.working_dir } : { noWorkingDir: true };
    }
    if (repo) {
      const hit = members.find((m) => m.dir === repo);
      return hit ? { dir: hit.dir } : { badRequest: "repo 不是该项目的成员仓库" };
    }
    if (write) return { badRequest: "多仓库项目必须指定 repo 参数" };
    return members.length > 0 ? { dir: members[0]!.dir } : { noWorkingDir: true };
  }

  /**
   * 右侧 Git 面板的仓库快照。
   *
   * 源项目和附属项目都能问（跟 GET /repo 不同，那边拒绝附属项目）。
   * 环境事实写在 available/reason 里，不抛 4xx。多仓库项目带 ?repo=<成员dir>。
   */
  app.get("/api/projects/:id/git", async (req, reply): Promise<GitSnapshot | void> => {
    const { id } = req.params as { id: string };
    const q = req.query as { repo?: string };
    const row = db.getProject(id);
    if (!row) return reply.code(404).send({ error: "项目不存在" });
    const t = gitTargetOf(row, q.repo);
    if ("badRequest" in t) return reply.code(400).send({ error: t.badRequest });
    if ("noWorkingDir" in t) {
      return unavailableSnapshot("no-working-dir", worktreeFailureText("no-working-dir"));
    }
    try {
      return await describeGit(await gitHostFor(row, manager), t.dir);
    } catch (err) {
      const e = err as WorktreeError;
      return unavailableSnapshot(gitReasonOf(e), e.detail ?? e.message);
    }
  });

  /**
   * 侧栏最后一层的 +N −M。源项目和附属项目都能问。
   * 比 GET /git 轻一个数量级（一条 status），环境事实同样不抛 4xx。
   */
  app.get("/api/projects/:id/git/changes", async (req, reply): Promise<GitChangeCounts | void> => {
    const { id } = req.params as { id: string };
    const row = db.getProject(id);
    if (!row) return reply.code(404).send({ error: "项目不存在" });
    if (!row.working_dir) return unavailableChanges();
    try {
      return await describeGitChanges(await gitHostFor(row, manager), row.working_dir);
    } catch {
      return unavailableChanges();
    }
  });

  /**
   * 侧栏轮询的批量版：一次拿全部项目的 +N −M。
   *
   * 按宿主机分组，同主机的所有检出批成一次 exec——N 个项目逐个 GET 就是
   * N 条 SSH channel，每 8 秒一轮，弱网上会持续排队。主机之间并行互不拖累；
   * 任何一台失败只让它自己的项目 unavailable，形状与单个端点一致。
   */
  app.post("/api/git/changes", async (req, reply): Promise<Record<string, GitChangeCounts> | void> => {
    const ids = (req.body as { ids?: unknown } | null)?.ids;
    if (!Array.isArray(ids) || ids.length > 500 || ids.some((x) => typeof x !== "string")) {
      return reply.code(400).send({ error: "ids 必须是字符串数组" });
    }
    const out: Record<string, GitChangeCounts> = {};
    const groups = new Map<string, ProjectRow[]>();
    for (const id of ids as string[]) {
      const row = db.getProject(id);
      if (!row) continue; // 轮询窗口里刚被删掉的项目，跳过即可
      if (!row.working_dir) {
        out[id] = unavailableChanges();
        continue;
      }
      const key = hostKeyOf(row);
      groups.get(key)?.push(row) ?? groups.set(key, [row]);
    }
    await Promise.all(
      [...groups.values()].map(async (rows) => {
        try {
          const host = await gitHostFor(rows[0]!, manager);
          const counts = await describeGitChangesMany(
            host,
            rows.map((r) => r.working_dir!)
          );
          rows.forEach((r, i) => {
            out[r.id] = counts[i] ?? unavailableChanges();
          });
        } catch {
          for (const r of rows) out[r.id] = unavailableChanges();
        }
      })
    );
    return out;
  });

  /**
   * Git 面板里单个文件的 diff。path / origPath 由前端从快照原样带回
   * （仓库根相对路径），untracked=1 表示走 --no-index 伪 diff。
   * 环境事实与命令失败同样不抛 4xx，写在 available/reason/detail 里。
   */
  app.get("/api/projects/:id/git/diff", async (req, reply): Promise<GitFileDiff | void> => {
    const { id } = req.params as { id: string };
    const q = req.query as { path?: string; origPath?: string; untracked?: string; repo?: string };
    const row = db.getProject(id);
    if (!row) return reply.code(404).send({ error: "项目不存在" });
    if (!q.path) return reply.code(400).send({ error: "缺少 path 参数" });
    const t = gitTargetOf(row, q.repo);
    if ("badRequest" in t) return reply.code(400).send({ error: t.badRequest });
    if ("noWorkingDir" in t) {
      return unavailableDiff("no-working-dir", worktreeFailureText("no-working-dir"));
    }
    try {
      return await describeGitDiff(await gitHostFor(row, manager), t.dir, {
        path: q.path,
        origPath: q.origPath || undefined,
        untracked: q.untracked === "1",
      });
    } catch (err) {
      const e = err as WorktreeError;
      return unavailableDiff(gitReasonOf(e), e.detail ?? e.message);
    }
  });

  /**
   * 「修改」面板：工作区里全部未提交的改动，带每个文件的 +N −M。
   *
   * 比 GET /git/changes 重（多一条 numstat，未跟踪文件还要各来一条），
   * 所以那个轻量端点留给侧栏轮询，这个只在面板打开时问。
   */
  app.get(
    "/api/projects/:id/git/working",
    async (req, reply): Promise<GitWorkingChanges | void> => {
      const { id } = req.params as { id: string };
      const q = req.query as { repo?: string };
      const row = db.getProject(id);
      if (!row) return reply.code(404).send({ error: "项目不存在" });
      const t = gitTargetOf(row, q.repo);
      if ("badRequest" in t) return reply.code(400).send({ error: t.badRequest });
      if ("noWorkingDir" in t) {
        return unavailableWorking("no-working-dir", worktreeFailureText("no-working-dir"));
      }
      try {
        return await describeWorkingChanges(await gitHostFor(row, manager), t.dir);
      } catch (err) {
        const e = err as WorktreeError;
        return unavailableWorking(gitReasonOf(e), e.detail ?? e.message);
      }
    }
  );

  /**
   * History 列表的一页。
   *
   * 筛选与分页全在 query 里：branch / author / q / skip。与 GET /git 同一条
   * 规矩，环境事实不抛 4xx。
   */
  app.get("/api/projects/:id/git/log", async (req, reply): Promise<GitLogPage | void> => {
    const { id } = req.params as { id: string };
    const q = req.query as {
      branch?: string;
      author?: string;
      q?: string;
      skip?: string;
      repo?: string;
    };
    const row = db.getProject(id);
    if (!row) return reply.code(404).send({ error: "项目不存在" });
    const t = gitTargetOf(row, q.repo);
    if ("badRequest" in t) return reply.code(400).send({ error: t.badRequest });
    if ("noWorkingDir" in t) {
      return unavailableLog("no-working-dir", worktreeFailureText("no-working-dir"));
    }
    const skip = Number(q.skip);
    try {
      return await describeGitLog(await gitHostFor(row, manager), t.dir, {
        rev: q.branch || undefined,
        author: q.author || undefined,
        grep: q.q || undefined,
        skip: Number.isFinite(skip) && skip > 0 ? Math.floor(skip) : 0,
      });
    } catch (err) {
      const e = err as WorktreeError;
      return unavailableLog(gitReasonOf(e), e.detail ?? e.message);
    }
  });

  /** Branch / User 两个筛选下拉的候选值。面板挂载时取一次，不参与轮询 */
  app.get("/api/projects/:id/git/refs", async (req, reply): Promise<GitRefsInfo | void> => {
    const { id } = req.params as { id: string };
    const q = req.query as { repo?: string };
    const row = db.getProject(id);
    if (!row) return reply.code(404).send({ error: "项目不存在" });
    const t = gitTargetOf(row, q.repo);
    if ("badRequest" in t) return reply.code(400).send({ error: t.badRequest });
    if ("noWorkingDir" in t) {
      return unavailableRefs("no-working-dir", worktreeFailureText("no-working-dir"));
    }
    try {
      return await describeGitRefs(await gitHostFor(row, manager), t.dir);
    } catch (err) {
      const e = err as WorktreeError;
      return unavailableRefs(gitReasonOf(e), e.detail ?? e.message);
    }
  });

  /** 选中提交的详情：完整提交信息 + 改动文件 */
  app.get(
    "/api/projects/:id/git/commit",
    async (req, reply): Promise<GitCommitDetail | void> => {
      const { id } = req.params as { id: string };
      const { sha, repo } = req.query as { sha?: string; repo?: string };
      const row = db.getProject(id);
      if (!row) return reply.code(404).send({ error: "项目不存在" });
      // 只认十六进制：sha 要作为 rev 传给 git，形状先钉死，别指望下游转义
      if (!sha || !/^[0-9a-f]{4,40}$/i.test(sha)) {
        return reply.code(400).send({ error: "sha 参数不合法" });
      }
      const t = gitTargetOf(row, repo);
      if ("badRequest" in t) return reply.code(400).send({ error: t.badRequest });
      if ("noWorkingDir" in t) {
        return unavailableCommit("no-working-dir", worktreeFailureText("no-working-dir"));
      }
      try {
        return await describeCommit(await gitHostFor(row, manager), t.dir, sha);
      } catch (err) {
        const e = err as WorktreeError;
        return unavailableCommit(gitReasonOf(e), e.detail ?? e.message);
      }
    }
  );

  /** 某条提交里单个文件的 diff。path / origPath 由前端从详情原样带回 */
  app.get(
    "/api/projects/:id/git/commit/diff",
    async (req, reply): Promise<GitFileDiff | void> => {
      const { id } = req.params as { id: string };
      const q = req.query as { sha?: string; path?: string; origPath?: string; repo?: string };
      const row = db.getProject(id);
      if (!row) return reply.code(404).send({ error: "项目不存在" });
      if (!q.sha || !/^[0-9a-f]{4,40}$/i.test(q.sha)) {
        return reply.code(400).send({ error: "sha 参数不合法" });
      }
      if (!q.path) return reply.code(400).send({ error: "缺少 path 参数" });
      const t = gitTargetOf(row, q.repo);
      if ("badRequest" in t) return reply.code(400).send({ error: t.badRequest });
      if ("noWorkingDir" in t) {
        return unavailableDiff("no-working-dir", worktreeFailureText("no-working-dir"));
      }
      try {
        return await describeCommitDiff(
          await gitHostFor(row, manager),
          t.dir,
          q.sha,
          { path: q.path, origPath: q.origPath || undefined }
        );
      } catch (err) {
        const e = err as WorktreeError;
        return unavailableDiff(gitReasonOf(e), e.detail ?? e.message);
      }
    }
  );

  /**
   * 提交工作区改动。
   *
   * 路径写在 `:action` 那条之前只是为了读起来顺——Fastify 静态段本来就优先于
   * 参数段，`POST .../git/commit` 不会掉进 `:action` 里。
   *
   * 与 pull/push 同样两条规矩：走 withRepoLock（键是宿主机 + 仓库根），
   * 失败不是 4xx（没配 user.name、pre-commit 钩子拒绝、没有可提交的改动，
   * 都是仓库的正常状态，git 的原话比"操作失败"有用得多）。
   */
  app.post("/api/projects/:id/git/commit", async (req, reply): Promise<GitSyncResult | void> => {
    const { id } = req.params as { id: string };
    const body = (req.body ?? {}) as Partial<GitCommitInput>;
    const row = db.getProject(id);
    if (!row) return reply.code(404).send({ error: "项目不存在" });
    const amend = body.amend === true;
    const message = typeof body.message === "string" ? body.message : "";
    if (!amend && !message.trim()) {
      return reply.code(400).send({ error: "缺少提交信息" });
    }
    const all = body.all === true;
    const paths = Array.isArray(body.paths) ? body.paths : [];
    if (!all && (paths.length === 0 || paths.some((p) => typeof p !== "string" || !p))) {
      return reply.code(400).send({ error: "paths 必须是非空字符串数组" });
    }
    // 写操作对多仓库项目必须显式指定成员，不猜
    const t = gitTargetOf(row, (req.query as { repo?: string }).repo, true);
    if ("badRequest" in t) return reply.code(400).send({ error: t.badRequest });
    if ("noWorkingDir" in t) {
      return {
        ok: false,
        reason: "no-working-dir",
        detail: worktreeFailureText("no-working-dir"),
      };
    }
    try {
      const host = await gitHostFor(row, manager);
      const root = await repoRoot(host, t.dir);
      return await withRepoLock(repoLockKey(host, root), () =>
        commitWorking(host, root, {
          message,
          all,
          paths,
          amend,
          push: body.push === true,
        })
      );
    } catch (err) {
      const e = err as WorktreeError;
      return { ok: false, reason: gitReasonOf(e), detail: e.detail ?? e.message };
    }
  });

  /**
   * 历史面板写操作：fetch / checkout / cherry-pick / revert / 建分支。
   *
   * 必须写在 `:action` 之前——Fastify 静态段优先，但把 `op` 漏进 :action
   * 会变成 404「未知操作」。规矩与 commit / pull / push 相同：withRepoLock、
   * 多仓库强制 repo、git 失败回 GitSyncResult 而不是 4xx。
   */
  app.post("/api/projects/:id/git/op", async (req, reply): Promise<GitSyncResult | void> => {
    const { id } = req.params as { id: string };
    const row = db.getProject(id);
    if (!row) return reply.code(404).send({ error: "项目不存在" });
    const parsed = parseGitOpInput(req.body);
    if ("error" in parsed) return reply.code(400).send({ error: parsed.error });
    const input: GitOpInput = parsed;
    const t = gitTargetOf(row, (req.query as { repo?: string }).repo, true);
    if ("badRequest" in t) return reply.code(400).send({ error: t.badRequest });
    if ("noWorkingDir" in t) {
      return {
        ok: false,
        reason: "no-working-dir",
        detail: worktreeFailureText("no-working-dir"),
      };
    }
    try {
      const host = await gitHostFor(row, manager);
      const root = await repoRoot(host, t.dir);
      return await withRepoLock(repoLockKey(host, root), () => runGitOp(host, root, input));
    } catch (err) {
      const e = err as WorktreeError;
      return { ok: false, reason: gitReasonOf(e), detail: e.detail ?? e.message };
    }
  });

  /**
   * Pull / Push。
   *
   * 走 withRepoLock，键是「宿主机 + 仓库根」（不是 projectId——同一个仓库
   * 完全可能挂着好几个 Project，按 id 加锁等于没加）。同一棵检出上并发
   * pull 会争 index.lock，报出来的错对用户毫无意义。
   *
   * 失败**不是 4xx**：凭据不对、非快进、远端拒绝都是仓库的正常状态，
   * 把 git 的原话回给前端展示，那比一句"操作失败"有用得多。
   */
  app.post("/api/projects/:id/git/:action", async (req, reply): Promise<GitSyncResult | void> => {
    const { id, action } = req.params as { id: string; action: string };
    if (action !== "pull" && action !== "push") {
      return reply.code(404).send({ error: "未知操作" });
    }
    const row = db.getProject(id);
    if (!row) return reply.code(404).send({ error: "项目不存在" });
    // 写操作对多仓库项目必须显式指定成员，不猜
    const t = gitTargetOf(row, (req.query as { repo?: string }).repo, true);
    if ("badRequest" in t) return reply.code(400).send({ error: t.badRequest });
    if ("noWorkingDir" in t) {
      return {
        ok: false,
        reason: "no-working-dir",
        detail: worktreeFailureText("no-working-dir"),
      };
    }
    try {
      const host = await gitHostFor(row, manager);
      const root = await repoRoot(host, t.dir);
      return await withRepoLock(repoLockKey(host, root), () =>
        syncGit(host, root, action)
      );
    } catch (err) {
      const e = err as WorktreeError;
      return { ok: false, reason: gitReasonOf(e), detail: e.detail ?? e.message };
    }
  });

  // ---- 项目文件 ----

  /**
   * 文件面板的执行环境。
   *
   * 不经 gitHostFor：文件面板与 git 无关，为了列个目录去探测一遍 git 的绝对路径
   * 既慢又会让"没装 git 的机器"平白打不开文件树。SSH 侧复用 SessionManager 已有
   * 的链路，与 /api/fs/list 同一条路子。
   *
   * SSH 项目的工作目录可以留空（表单里就是可选的），那时以远端家目录为根——
   * 和会话启动时的行为一致。多仓库容器留空则以虚拟项目目录为根（会话 cwd
   * 同款语义，见 manager.ensureVirtualDir）：首次打开文件面板会顺手把目录建出来。
   */
  const fileHostFor = async (row: ProjectRow): Promise<{ host: FileHost; root: string }> => {
    const container = row.multi_repos != null && !row.source_project_id;
    if (row.type === "local") {
      if (!row.working_dir) {
        if (container) {
          return { host: { local: true, kind: localKind() }, root: await manager.ensureVirtualDir(row) };
        }
        throw new Error(worktreeFailureText("no-working-dir"));
      }
      return { host: { local: true, kind: localKind() }, root: row.working_dir };
    }
    const link = manager.getLink(row);
    const facts = await link.hostFacts();
    return {
      host: {
        local: false,
        kind: facts.kind,
        exec: link.exec,
        execStream: (cmd) => link.execStream(cmd),
      },
      root: row.working_dir || (container ? await manager.ensureVirtualDir(row) : facts.home),
    };
  };

  /**
   * 列工作目录里的一层。`path` 是工作目录相对路径，缺省为工作目录本身。
   *
   * 读失败回 400 而不是 500：路径不存在、没权限、SSH 连不上，都是调用方能处理
   * 并且该向用户如实转述的事实。
   */
  app.get("/api/projects/:id/files", async (req, reply): Promise<WorkspaceListing | void> => {
    const { id } = req.params as { id: string };
    const { path: p } = req.query as { path?: string };
    const row = db.getProject(id);
    if (!row) return reply.code(404).send({ error: "项目不存在" });
    try {
      const { host, root } = await fileHostFor(row);
      return await listWorkspace(host, root, p);
    } catch (err) {
      return reply.code(400).send({ error: (err as Error).message });
    }
  });

  /**
   * Quick Open 的文件路径清单。git 仓库走 ls-files（尊重 gitignore），
   * 否则遍历工作目录并跳过 node_modules 之类。失败回 400，与列目录同一套。
   */
  app.get("/api/projects/:id/files/index", async (req, reply): Promise<WorkspaceIndex | void> => {
    const { id } = req.params as { id: string };
    const row = db.getProject(id);
    if (!row) return reply.code(404).send({ error: "项目不存在" });
    try {
      const { host, root } = await fileHostFor(row);
      try {
        const gitHost = await gitHostFor(row, manager);
        const listed = await listRepoFiles(gitHost, root);
        if (listed) return capFileIndex(listed);
      } catch (err) {
        // git 缺失 / 不是仓库：改走遍历。链路挂了就别假装能列文件
        if (err instanceof WorktreeError && err.reason === "link-failed") throw err;
      }
      return await indexWorkspace(host, root);
    } catch (err) {
      return reply.code(400).send({ error: (err as Error).message });
    }
  });

  const rawBaseFor = (projectId: string) =>
    `/api/projects/${encodeURIComponent(projectId)}/raw/${auth.rawToken(projectId)}/`;

  /**
   * 读一个文件供查看 tab 渲染。二进制 / 超大文件也回 200，形状里写清是什么。
   * 顺带给出这个项目的原始字节前缀（含新鲜令牌），图片与 HTML 预览拼它用。
   */
  app.get("/api/projects/:id/file", async (req, reply): Promise<WorkspaceFile | void> => {
    const { id } = req.params as { id: string };
    const { path: p } = req.query as { path?: string };
    const row = db.getProject(id);
    if (!row) return reply.code(404).send({ error: "项目不存在" });
    if (!p) return reply.code(400).send({ error: "缺少文件路径" });
    try {
      const { host, root } = await fileHostFor(row);
      const preview = await readWorkspaceFile(host, root, p);
      return { preview, rawBase: rawBaseFor(id) };
    } catch (err) {
      return reply.code(400).send({ error: (err as Error).message });
    }
  });

  /**
   * 原始字节：把工作目录里的一个文件按它本来的 Content-Type 原样吐给浏览器。
   * 图片预览的 `<img src>`、HTML 预览的 `<iframe src>` 以及页面里相对路径引用的
   * CSS / JS / 图片 / 字体都走这里。
   *
   * 路由是**路径形状**而不是 `?path=`：HTML 里的 `./style.css` 要能按 URL 规则
   * 相对当前文档解析到 `.../raw/<token>/docs/style.css`，query 形状做不到。
   *
   * 鉴权与别的 /api 不同（onRequest 钩子对它放行）：HTML 预览跑在没有
   * allow-same-origin 的沙箱 iframe 里，origin 是 opaque 的，浏览器不给它的子资源
   * 请求带 SameSite=Lax 的登录 cookie，所以凭据只能放在 URL 里——一枚只能读这个
   * 项目文件的作用域令牌（Auth.rawToken）。登录 cookie 若在（用户直接在新标签页
   * 打开原始地址）也认。
   *
   * 响应头是另一半护栏：`Content-Security-Policy: sandbox` 让这个 HTML 即使被
   * 当成顶层页面打开也跑在 opaque origin 里，碰不到本站的 cookie / storage /
   * 其它接口；nosniff 防止把 octet-stream 猜成脚本。no-store 是因为用户改完文件
   * 按刷新就想看到新的，这里不发 ETag。
   */
  app.get(
    "/api/projects/:id/raw/:token/*",
    { config: { rawToken: true } },
    async (req, reply) => {
      const { id, token, "*": rel } = req.params as { id: string; token: string; "*": string };
      if (!auth.isAuthenticated(req) && !auth.rawTokenValid(token, id)) {
        return reply.code(401).send({ error: "未认证" });
      }
      const row = db.getProject(id);
      if (!row) return reply.code(404).send({ error: "项目不存在" });
      if (!rel) return reply.code(400).send({ error: "缺少文件路径" });
      let name: string;
      let size: number;
      let bytes: Buffer;
      try {
        const { host, root } = await fileHostFor(row);
        ({ name, size, bytes } = await readWorkspaceBytes(host, root, rel, WORKSPACE_RAW_CAP));
      } catch (err) {
        const message = (err as Error).message;
        return reply.code(message === "路径不存在或不可访问" ? 404 : 400).send({ error: message });
      }
      if (size > bytes.length) {
        // 截断的字节对浏览器没有意义（半张图、半个脚本），如实拒绝
        return reply.code(413).send({ error: `文件超过 ${WORKSPACE_RAW_CAP / 1024 / 1024}MB` });
      }
      return reply
        .header("Content-Type", mimeOf(name))
        .header("Content-Length", bytes.length)
        .header("Cache-Control", "no-store")
        .header("X-Content-Type-Options", "nosniff")
        .header("Content-Security-Policy", "sandbox allow-scripts allow-forms allow-popups allow-modals")
        .header("Referrer-Policy", "no-referrer")
        .send(bytes);
    }
  );

  /**
   * 下载工作目录里的一个文件：整个文件按流接到响应上，没有原始字节路由那个
   * 16MB 上限（ADR 0008）。
   *
   * 鉴权就是普通的登录 cookie：下载由主页面发起的同源导航触发（`<a download>`），
   * 浏览器会带 cookie，用不着原始字节路由那种放在 URL 里的作用域令牌。
   * Content-Type 一律 octet-stream + attachment：这是"存到本地"，不是"在浏览器里
   * 看"，浏览器按文件名后缀自己认类型。
   */
  app.get("/api/projects/:id/download", async (req, reply) => {
    const { id } = req.params as { id: string };
    const { path: p } = req.query as { path?: string };
    const row = db.getProject(id);
    if (!row) return reply.code(404).send({ error: "项目不存在" });
    if (!p) return reply.code(400).send({ error: "缺少文件路径" });
    let src: DownloadSource;
    try {
      const { host, root } = await fileHostFor(row);
      src = await openDownload(host, root, p);
    } catch (err) {
      const message = (err as Error).message;
      return reply.code(message === "路径不存在或不可访问" ? 404 : 400).send({ error: message });
    }
    return reply
      .header("Content-Type", "application/octet-stream")
      .header("Content-Length", src.size)
      .header("Content-Disposition", contentDisposition(src.name))
      .header("Cache-Control", "no-store")
      .header("X-Content-Type-Options", "nosniff")
      .send(src.stream);
  });

  /**
   * 上传一个文件到工作目录里的 `path` 目录下，文件名是 `name`，请求体是原始字节
   * （application/octet-stream，见上面的解析器），按流写到宿主机。
   *
   * Content-Length 是必需的：宿主机那头收满这个数才把文件改名到位，中途断开的
   * 上传不会留下截断的文件。浏览器给 File 请求体一定带这个头。
   *
   * 同名文件已存在且没带 overwrite=1 时回 409，前端问过用户再重发；此时请求体
   * 可能还没收完，Node 会把剩下的读掉丢弃，连接不会被掐断。
   */
  app.put("/api/projects/:id/upload", async (req, reply): Promise<UploadResult | void> => {
    const { id } = req.params as { id: string };
    const { path: dir, name, overwrite } = req.query as {
      path?: string;
      name?: string;
      overwrite?: string;
    };
    const row = db.getProject(id);
    if (!row) return reply.code(404).send({ error: "项目不存在" });
    if (!name) return reply.code(400).send({ error: "缺少文件名" });
    const size = Number(req.headers["content-length"]);
    if (!Number.isInteger(size) || size < 0) {
      return reply.code(411).send({ error: "缺少 Content-Length" });
    }
    // 空文件时 fastify 可能跳过解析器，body 为 undefined；别的 Content-Type 会解成
    // JSON / 字符串，那不是这条路由要的
    const body =
      req.body instanceof Readable ? req.body : size === 0 ? Readable.from([]) : null;
    if (!body) return reply.code(415).send({ error: "请求体必须是 application/octet-stream" });
    try {
      const { host, root } = await fileHostFor(row);
      return await receiveUpload(host, root, dir, name, size, overwrite === "1", body);
    } catch (err) {
      const message = (err as Error).message;
      return reply.code(message === "同名文件已存在" ? 409 : 400).send({ error: message });
    }
  });

  /**
   * 在工作目录里建一个文件夹。`path` 是工作目录相对路径（含要建的那一段）。
   * recursive 给文件夹上传用：中间层已存在当成功；已存在一个同名文件仍是 409。
   */
  app.post("/api/projects/:id/mkdir", async (req, reply): Promise<FileOpResult | void> => {
    const { id } = req.params as { id: string };
    const { path: p, recursive } = (req.body ?? {}) as { path?: string; recursive?: boolean };
    const row = db.getProject(id);
    if (!row) return reply.code(404).send({ error: "项目不存在" });
    if (!p) return reply.code(400).send({ error: "缺少路径" });
    try {
      const { host, root } = await fileHostFor(row);
      return await mkdirWorkspace(host, root, p, recursive === true);
    } catch (err) {
      const message = (err as Error).message;
      return reply.code(message === "同名文件已存在" ? 409 : 400).send({ error: message });
    }
  });

  /**
   * 重命名工作目录里的一项。只改最后一段名字，不移动到别的目录。
   */
  app.post("/api/projects/:id/rename", async (req, reply): Promise<FileOpResult | void> => {
    const { id } = req.params as { id: string };
    const { path: p, name } = (req.body ?? {}) as { path?: string; name?: string };
    const row = db.getProject(id);
    if (!row) return reply.code(404).send({ error: "项目不存在" });
    if (!p) return reply.code(400).send({ error: "缺少路径" });
    if (!name) return reply.code(400).send({ error: "缺少文件名" });
    try {
      const { host, root } = await fileHostFor(row);
      return await renameWorkspace(host, root, p, name);
    } catch (err) {
      const message = (err as Error).message;
      return reply.code(message === "同名文件已存在" ? 409 : 400).send({ error: message });
    }
  });

  /**
   * 删除工作目录里的若干项。每条单独试，部分失败仍 200，细节在 errors 里。
   * 空路径（工作目录本身）会被丢掉；前端不该把它送来。
   */
  app.post("/api/projects/:id/remove", async (req, reply): Promise<FileRemoveResult | void> => {
    const { id } = req.params as { id: string };
    const { paths } = (req.body ?? {}) as { paths?: unknown };
    const row = db.getProject(id);
    if (!row) return reply.code(404).send({ error: "项目不存在" });
    if (!Array.isArray(paths) || paths.length === 0) {
      return reply.code(400).send({ error: "缺少路径" });
    }
    if (!paths.every((x) => typeof x === "string")) {
      return reply.code(400).send({ error: "路径不合法" });
    }
    try {
      const { host, root } = await fileHostFor(row);
      return await removeWorkspace(host, root, paths);
    } catch (err) {
      return reply.code(400).send({ error: (err as Error).message });
    }
  });

  /**
   * 派生一个附属项目。
   *
   * 与 GET /repo 的分工：那边是探测，环境事实如实报告；这边是操作，同样的事实
   * 一律当状态冲突（409）。
   */
  /**
   * 多仓库容器的批量派生分支：统一分支名、逐成员建树、**全有或全无**。
   * 成功产出**一个**多仓库附属项目（multi 与 worktree 同时存在）；任一成员失败
   * 由 deriveMultiWorktrees 回滚，这里只负责翻译三类错误：
   * 容器级校验 → 409 纯文本；成员级失败 → 按 reason 映射 + member 归因
   * （回滚有残留 ⇒ 一律 502 + leftover 路径原样列出）；其余 → 502。
   */
  async function handleMultiDerive(
    reply: FastifyReply,
    src: ProjectRow,
    members: NonNullable<ReturnType<typeof Db.parseMultiRepos>>,
    body: unknown
  ) {
    if (members.length === 0) return reply.code(409).send({ error: "容器没有成员仓库" });
    const input = (body ?? {}) as MultiWorktreeInput;
    if (input.mode !== "new-branch" && input.mode !== "existing-branch" && input.mode !== "auto") {
      return reply.code(400).send({ error: "未知的派生方式" });
    }
    if ((input as { startPoint?: unknown }).startPoint != null) {
      return reply
        .code(400)
        .send({ error: "批量派生不支持在请求里指定基点，新建分支用源项目的默认 worktree 基点" });
    }
    const branch = input.branch?.trim();
    if (!branch) return reply.code(400).send({ error: "分支名不能为空" });

    try {
      const host = await gitHostFor(src, manager);
      const outcome = await deriveMultiWorktrees(
        host,
        { name: src.name, members },
        { ...input, branch },
        { startPoint: src.default_worktree_branch || undefined }
      );
      const row: ProjectRow = {
        id: crypto.randomUUID(),
        name: input.name?.trim() || branch,
        type: src.type,
        working_dir: outcome.centralDir,
        shell: src.shell,
        // ssh_* 与 host_id 从容器整行复制，理由与单派生完全相同（见下面那段注释）
        ssh_host: src.ssh_host,
        ssh_port: src.ssh_port,
        ssh_username: src.ssh_username,
        ssh_auth_method: src.ssh_auth_method,
        ssh_key_path: src.ssh_key_path,
        ssh_secret_enc: src.ssh_secret_enc,
        host_id: src.host_id,
        created_at: Date.now(),
        source_project_id: src.id,
        worktree_branch: branch,
        // 单值列对多仓库无意义：逐成员的仓库根在 multi_repos 里
        worktree_repo_dir: null,
        worktree_created_by_mojito: 1,
        worktree_archived_at: null,
        multi_repos: JSON.stringify(outcome.members),
        default_worktree_branch: null,
      };

      // 集中目录清单：给 coding agent 的结构说明（见 virtualdir.ts）。写失败**不回滚**：
      // 全有或全无护的是 worktree——建错了要付回滚代价的东西；清单是纯引导文件、
      // 可再生，为它逆序 remove N 棵刚建好的树得不偿失。集中目录必然是本请求
      // 新建的（原先不存在才允许派生），首写无覆盖风险。
      try {
        const files = centralManifestFiles(
          row.name,
          branch,
          outcome.members.map((m) => ({ base: basenameOf(host.kind, m.dir), repoDir: m.repoDir }))
        );
        if (src.type === "local") {
          await writeLocalManifest(outcome.centralDir, files);
        } else if (host.kind === "windows") {
          const link = manager.getLink(src);
          for (const { cmd, stdinBase64 } of windowsWriteManifestCommands(outcome.centralDir, files)) {
            const res = await link.execWithInput(cmd, stdinBase64);
            if (res.code !== 0) throw new Error(res.stderr.trim() || `exit ${res.code}`);
          }
        } else {
          const res = await host.exec(posixWriteManifestCommand(outcome.centralDir, files));
          if (res.code !== 0) throw new Error(res.stderr.trim() || `exit ${res.code}`);
        }
      } catch (err) {
        reply.log.warn({ err }, `派生成功但集中目录清单写入失败：${outcome.centralDir}`);
      }

      db.insertProject(row);
      return Db.toProject(row);
    } catch (err) {
      if (err instanceof MultiVetoError) {
        return reply.code(409).send({ error: err.message });
      }
      if (err instanceof MultiWorktreeError) {
        // 回滚有残留 ⇒ 一律 502：不管起因是什么，宿主机上已经躺着要人收拾的目录
        const status = err.leftover.length > 0 ? 502 : WORKTREE_STATUS[err.reason];
        const out: MultiDeriveError = {
          error: `成员 ${err.memberDir} 派生失败：${
            err.detail ? `${err.message}：${gitErrorLine(err.detail)}` : err.message
          }`,
          member: { dir: err.memberDir, reason: err.reason, detail: err.detail },
        };
        if (err.leftover.length > 0) out.leftover = err.leftover;
        return reply.code(status).send(out);
      }
      if (err instanceof WorktreeError) {
        return reply
          .code(WORKTREE_STATUS[err.reason])
          .send({ error: err.detail ? `${err.message}：${gitErrorLine(err.detail)}` : err.message });
      }
      return reply.code(502).send({ error: `派生失败：${(err as Error).message}` });
    }
  }

  app.post("/api/projects/:id/worktrees", async (req, reply) => {
    const { id } = req.params as { id: string };
    const src = db.getProject(id);
    if (!src) return reply.code(404).send({ error: "项目不存在" });

    // 禁止二级派生。git 本身允许（从 linked worktree 建的 worktree 仍属同一仓库），
    // 但侧栏的两级树会变成任意深度、删除级联要递归，而收益是零——从源项目派生
    // 完全等价。将来若要放开：把 source 取成 `row.source_project_id ?? row.id`
    // 重新挂到根即可，树仍是两级。多仓库派生行也带 source_project_id，天然被挡。
    if (src.source_project_id) {
      return reply.code(409).send({ error: "附属项目不能再派生，请从它的源项目派生" });
    }
    // 多仓库容器在 working_dir 检查之前分叉：容器允许没有工作目录
    const srcMembers = Db.parseMultiRepos(src);
    if (srcMembers) return handleMultiDerive(reply, src, srcMembers, req.body);
    if (!src.working_dir) {
      return reply.code(409).send({ error: worktreeFailureText("no-working-dir") });
    }

    const input = (req.body ?? {}) as WorktreeInput;
    if (input.mode !== "new-branch" && input.mode !== "existing-branch") {
      return reply.code(400).send({ error: "未知的派生方式" });
    }
    const branch = input.branch?.trim();
    if (!branch) return reply.code(400).send({ error: "分支名不能为空" });

    try {
      const host = await gitHostFor(src, manager);
      const main = await repoRoot(host, src.working_dir);

      const dir = input.dir?.trim() || siblingWorktreePath(host.kind, main, branch);
      if (!isAbsolute(host.kind, dir)) {
        return reply.code(400).send({ error: "目标目录必须是绝对路径" });
      }
      if (isUnc(host.kind, dir)) {
        return reply.code(400).send({ error: "暂不支持在 UNC 网络路径上派生" });
      }
      // 与删除护栏同一个门槛：真实的 worktree 都是"某个仓库目录的同级"，
      // 而仓库不会直接躺在盘符根上。这里挡住，删除那边就永远不会遇到
      if (pathDepth(host.kind, dir) < 2) {
        return reply.code(400).send({ error: "目标目录过于靠近文件系统根，请换一个位置" });
      }
      if (isAncestor(host.kind, dir, main)) {
        return reply.code(400).send({ error: "目标目录包含仓库根，拒绝创建" });
      }
      const veto = vetoTargetDir(host.kind, main, dir);
      if (veto) {
        return reply.code(409).send({
          error:
            veto === "path-inside-repo"
              ? `${worktreeFailureText(veto)}：源仓库会把它当成一堆未跟踪文件`
              : `${worktreeFailureText(veto)}：${dir.length} 字符，Windows 上建得出来却删不掉`,
        });
      }

      const created = await withRepoLock(repoLockKey(host, main), async () => {
        // 锁内再查一次占用：预检与创建之间用户可能刚建了同名目录。
        // 空目录也拒绝——git 的 add 只在非空时 die，但"接管一个已存在的空目录"
        // 不是用户要的语义，也不该继承"删项目会删这个目录"的承诺
        if (await pathExists(host, dir)) {
          throw new WorktreeError(
            "path-occupied",
            worktreeFailureText("path-occupied"),
            dir
          );
        }
        return addWorktree(host, main, {
          mode: input.mode,
          branch,
          startPoint: input.startPoint?.trim() || undefined,
          dir,
        });
      });

      const row: ProjectRow = {
        id: crypto.randomUUID(),
        name: input.name?.trim() || branch,
        type: src.type,
        working_dir: created,
        shell: src.shell,
        // ssh_* 与 host_id 从源项目整行复制（含密文，同一个 SecretBox 能解，全程不碰明文）：
        // SshLink 由 ProjectRow 构造、按 project.id 缓存，getLink / prepareZellij /
        // GET host 全部直接读 project.ssh_host。复制让这些点一处都不用改，也让
        // 删除清理不依赖源项目行还在不在。代价是源项目改配置时要手动传播
        // （见 PUT 里的 updateChildrenSsh）；已保存主机改配置走 updateProjectsFromHost。
        //
        // 顺带一个白捡的正确行为：Zellij 授权按 host+port+username 记，
        // 所以附属项目第一次开会话不会再弹一次安装授权。
        ssh_host: src.ssh_host,
        ssh_port: src.ssh_port,
        ssh_username: src.ssh_username,
        ssh_auth_method: src.ssh_auth_method,
        ssh_key_path: src.ssh_key_path,
        ssh_secret_enc: src.ssh_secret_enc,
        host_id: src.host_id,
        created_at: Date.now(),
        source_project_id: src.id,
        worktree_branch: branch,
        worktree_repo_dir: main,
        worktree_created_by_mojito: 1,
        worktree_archived_at: null,
        multi_repos: null,
        default_worktree_branch: null,
      };
      db.insertProject(row);
      return Db.toProject(row);
    } catch (err) {
      if (err instanceof WorktreeError) {
        return reply
          .code(WORKTREE_STATUS[err.reason])
          .send({ error: err.detail ? `${err.message}：${gitErrorLine(err.detail)}` : err.message });
      }
      return reply.code(502).send({ error: `派生失败：${(err as Error).message}` });
    }
  });

  /**
   * 附属项目的工作区状态：删除前的预检。
   *
   * 读不到状态不阻断删除，只是让确认框改口说"无法确认里面有没有未保存的东西"——
   * 把"读不到"和"是干净的"混成一个答案，才是真会让用户丢东西的做法。
   */
  app.get("/api/projects/:id/worktree", async (req, reply): Promise<WorktreeStatus | void> => {
    const { id } = req.params as { id: string };
    const row = db.getProject(id);
    if (!row) return reply.code(404).send({ error: "项目不存在" });
    if (!row.source_project_id || !row.working_dir) {
      return reply.code(400).send({ error: "不是附属项目" });
    }
    const members = Db.parseMultiRepos(row);
    try {
      const host = await gitHostFor(row, manager);
      if (!members) return await worktreeStatus(host, row.working_dir);

      // 多仓库派生行：逐成员汇总成同一个 WorktreeStatus 形状，确认框零改动。
      // 样例路径带 <成员目录名>/ 前缀，用户一眼能看出脏文件在哪棵 worktree 里
      const merged: WorktreeStatus = {
        present: false,
        dirtyCount: 0,
        dirtySample: [],
        ignoredCount: 0,
        ahead: null,
      };
      for (const m of members) {
        const s = await worktreeStatus(host, m.dir);
        const base = basenameOf(host.kind, m.dir);
        merged.present ||= s.present;
        merged.dirtyCount += s.dirtyCount;
        merged.ignoredCount += s.ignoredCount;
        if (s.ahead != null) merged.ahead = (merged.ahead ?? 0) + s.ahead;
        for (const f of s.dirtySample) merged.dirtySample.push(`${base}/${f}`);
        if (s.error) merged.error = merged.error ? `${merged.error}；${s.error}` : s.error;
      }
      merged.dirtySample = merged.dirtySample.slice(0, 8);
      return merged;
    } catch (err) {
      const e = err as WorktreeError;
      return {
        present: true,
        dirtyCount: 0,
        dirtySample: [],
        ignoredCount: 0,
        ahead: null,
        error: e.detail ?? e.message,
      };
    }
  });

  /**
   * 存档附属项目：从侧栏隐藏，worktree 目录与分支原样保留，到期由后台清扫
   * 自动删除（见 archive.ts）；此前随时可恢复。
   *
   * 会话与删除一样连坐——项目都收起来了，留着挂在上面的终端只会变成幽灵 tab。
   * 终止失败就整体放弃，不存档：存档隐含"到期会删目录"的承诺，而带着活进程
   * 删目录是唯一真会毁数据的操作。
   */
  app.post("/api/projects/:id/archive", async (req, reply) => {
    const { id } = req.params as { id: string };
    const row = db.getProject(id);
    if (!row) return reply.code(404).send({ error: "项目不存在" });
    if (!row.source_project_id) {
      return reply.code(400).send({ error: "只有附属项目能存档" });
    }
    if (row.worktree_archived_at) return Db.toProject(row); // 幂等
    try {
      for (const s of db.listSessionsByProject(id)) {
        if (s.state === "dead") manager.deleteDead(s.id);
        else await manager.terminate(s.id);
      }
    } catch (err) {
      return reply
        .code(502)
        .send({ error: `终止会话失败，未存档：${(err as Error).message}` });
    }
    db.setWorktreeArchived(id, Date.now());
    return Db.toProject(db.getProject(id)!);
  });

  /** 恢复已存档的附属项目。会话在存档时已经终止，恢复后按需新建。 */
  app.post("/api/projects/:id/restore", async (req, reply) => {
    const { id } = req.params as { id: string };
    const row = db.getProject(id);
    if (!row) return reply.code(404).send({ error: "项目不存在" });
    if (!row.worktree_archived_at) return Db.toProject(row); // 幂等
    db.setWorktreeArchived(id, null);
    return Db.toProject(db.getProject(id)!);
  });

  // ---- SSH 端口转发 ----

  /**
   * 规则挂在项目上，隧道走该项目的 SshLink。
   * 本地项目 400；环境事实（连不上、端口占用）写在每条规则的 state/error 里，
   * 列表本身不因某条隧道失败而 5xx。
   */
  app.get("/api/projects/:id/forwards", async (req, reply): Promise<PortForward[] | void> => {
    const { id } = req.params as { id: string };
    const project = db.getProject(id);
    if (!project) return reply.code(404).send({ error: "项目不存在" });
    if (project.type !== "ssh") return reply.code(400).send({ error: "只有 SSH 项目能做端口转发" });
    return manager.forwards.list(id);
  });

  app.post("/api/projects/:id/forwards", async (req, reply): Promise<PortForward | void> => {
    const { id } = req.params as { id: string };
    const project = db.getProject(id);
    if (!project) return reply.code(404).send({ error: "项目不存在" });
    try {
      return await manager.forwards.create(project, (req.body ?? {}) as PortForwardInput);
    } catch (err) {
      if (err instanceof ForwardConflictError) return reply.code(409).send({ error: err.message });
      return reply.code(400).send({ error: (err as Error).message });
    }
  });

  app.patch("/api/projects/:id/forwards/:fwdId", async (req, reply): Promise<PortForward | void> => {
    const { id, fwdId } = req.params as { id: string; fwdId: string };
    const project = db.getProject(id);
    if (!project) return reply.code(404).send({ error: "项目不存在" });
    try {
      return await manager.forwards.update(project, fwdId, (req.body ?? {}) as Partial<PortForwardInput>);
    } catch (err) {
      if (err instanceof ForwardConflictError) return reply.code(409).send({ error: err.message });
      const msg = (err as Error).message;
      if (msg === "转发规则不存在") return reply.code(404).send({ error: msg });
      return reply.code(400).send({ error: msg });
    }
  });

  app.delete("/api/projects/:id/forwards/:fwdId", async (req, reply) => {
    const { id, fwdId } = req.params as { id: string; fwdId: string };
    const project = db.getProject(id);
    if (!project) return reply.code(404).send({ error: "项目不存在" });
    try {
      await manager.forwards.remove(project, fwdId);
      return { ok: true };
    } catch (err) {
      const msg = (err as Error).message;
      if (msg === "转发规则不存在") return reply.code(404).send({ error: msg });
      return reply.code(400).send({ error: msg });
    }
  });

  // ---- 宿主机 Zellij 状态 ----

  /**
   * 安装授权按主机记（host+port+username），不按项目：
   * 同一台机器上的第二个项目不该再问一遍，二进制本来就已经装好了。
   */
  app.get("/api/projects/:id/host", async (req, reply): Promise<HostZellijStatus | void> => {
    const { id } = req.params as { id: string };
    const project = db.getProject(id);
    if (!project) return reply.code(404).send({ error: "项目不存在" });
    if (project.type !== "ssh") return reply.code(400).send({ error: "仅 SSH 项目有宿主机状态" });

    const row = db.getZellijHost(
      project.ssh_host!,
      project.ssh_port ?? 22,
      project.ssh_username!
    );
    return {
      authorized: row?.authorized == null ? null : row.authorized === 1,
      installedVersion: row?.installed_version ?? undefined,
      baseUrl: row?.base_url ?? undefined,
      verifiedDurable: row?.verified_durable == null ? null : row.verified_durable === 1,
      defaultBaseUrl: DEFAULT_BASE_URL,
      requiredVersion: ZELLIJ_VERSION,
    };
  });

  app.post("/api/projects/:id/host", async (req, reply) => {
    const { id } = req.params as { id: string };
    const project = db.getProject(id);
    if (!project) return reply.code(404).send({ error: "项目不存在" });
    if (project.type !== "ssh") return reply.code(400).send({ error: "仅 SSH 项目有宿主机状态" });

    const { authorized, baseUrl } = (req.body ?? {}) as {
      authorized?: boolean;
      baseUrl?: string;
    };
    db.upsertZellijHost(
      project.ssh_host!,
      project.ssh_port ?? 22,
      project.ssh_username!,
      {
        ...(authorized == null ? {} : { authorized: authorized ? 1 : 0 }),
        // 换了下载源就作废之前的安装记录，下次重新走一遍安装流程
        ...(baseUrl === undefined ? {} : { base_url: baseUrl.trim() || null, installed_version: null }),
      }
    );
    manager.resetPrepare(id);
    return { ok: true };
  });

  // ---- sessions ----

  app.get("/api/sessions", async (): Promise<SessionWithProject[]> => {
    const projects = new Map(db.listProjects().map((p) => [p.id, p]));
    return db.listSessions().map((row) => {
      const p = projects.get(row.project_id);
      return {
        ...Db.toSession(row),
        // 自动标题不入库，只有还活着的 entry 手上有（可能是陈旧值，见 titleOf）
        title: manager.titleOf(row.id),
        projectName: p?.name ?? "?",
        projectType: p?.type ?? "local",
      };
    });
  });

  app.post("/api/projects/:id/sessions", async (req, reply) => {
    const { id } = req.params as { id: string };
    const project = db.getProject(id);
    if (!project) return reply.code(404).send({ error: "项目不存在" });
    // 存档的项目到期会连目录一起删掉，不能再往里开终端
    if (project.worktree_archived_at) {
      return reply.code(409).send({ error: "项目已存档，请先恢复再新建终端" });
    }
    const body = (req.body ?? {}) as {
      name?: string;
      appearance?: unknown;
      background?: unknown;
      foreground?: unknown;
      agent?: unknown;
    };
    // 认不出的 agent 一律当普通终端：宁可开出一个 shell，也不要 400 一个新会话
    const agent = isSessionAgent(body.agent) ? body.agent : undefined;
    try {
      // 不起名（空串）是常态：UI 显示的是前台命令 / agent / 工作目录，
      // 编号名（"Terminal 3"）既没信息量，序号还会随删除重号
      return await manager.createSession(
        project,
        body.name?.trim() ?? "",
        sanitizeColorHint(body),
        agent
      );
    } catch (err) {
      return reply.code(502).send({ error: `创建会话失败：${(err as Error).message}` });
    }
  });

  app.post("/api/sessions/:id/reattach", async (req, reply) => {
    const { id } = req.params as { id: string };
    try {
      const row = await manager.ensureAttached(id, { force: true });
      return Db.toSession(row);
    } catch (err) {
      const row = db.getSession(id);
      if (row?.state === "dead") return Db.toSession(row);
      return reply.code(502).send({ error: `接回失败：${(err as Error).message}` });
    }
  });

  // 关 tab 前问一嘴前台有没有程序在跑。会话不存在也答"空闲"——
  // 这条路径上前端接下来就是终止，404 只会让它多走一个错误分支
  app.get("/api/sessions/:id/foreground", async (req) => {
    const { id } = req.params as { id: string };
    return manager.foreground(id);
  });

  app.post("/api/sessions/:id/terminate", async (req) => {
    const { id } = req.params as { id: string };
    await manager.terminate(id);
    return { ok: true };
  });

  app.delete("/api/sessions/:id", async (req, reply) => {
    const { id } = req.params as { id: string };
    if (!manager.deleteDead(id)) {
      return reply.code(409).send({ error: "只能清除已丢失（dead）的会话" });
    }
    return { ok: true };
  });

  app.patch("/api/sessions/:id", async (req, reply) => {
    const { id } = req.params as { id: string };
    const { name } = (req.body ?? {}) as { name?: string };
    if (typeof name !== "string") return reply.code(400).send({ error: "缺少名称" });
    if (!db.getSession(id)) return reply.code(404).send({ error: "会话不存在" });
    // 空串是合法的：清掉名字就回到自动标题，这是"取消重命名"的唯一出口
    db.renameSession(id, name.trim());
    return { ok: true };
  });

  /**
   * 粘贴图片：写进会话宿主机的 <falcon 根>/paste，返回绝对路径。
   * 前端把路径粘进终端输入——Claude Code 认输入框里的图片路径，
   * 拖拽文件进原生终端就是同一个机制。
   */
  app.post("/api/sessions/:id/paste-image", async (req, reply) => {
    const { id } = req.params as { id: string };
    const row = db.getSession(id);
    if (!row) return reply.code(404).send({ error: "会话不存在" });
    const project = db.getProject(row.project_id);
    if (!project) return reply.code(404).send({ error: "项目不存在" });

    const ext = imageExt(req.headers["content-type"]);
    if (!ext) return reply.code(415).send({ error: "不支持的图片类型" });
    const data = req.body as Buffer;
    if (!Buffer.isBuffer(data) || data.length === 0) {
      return reply.code(400).send({ error: "图片内容为空" });
    }

    try {
      if (project.type === "local") {
        return { path: await writeLocalPasteFile(deps.dataDir, ext, data) };
      }
      const link = manager.getLink(project);
      const facts = await link.hostFacts();
      const dir = pasteDir(facts.kind, facts.root);
      const file = joinPath(facts.kind, dir, pasteFileName(ext));
      const res =
        facts.kind === "windows"
          ? await link.execWithInput(windowsWriteCommand(dir, file), data.toString("base64"))
          : await link.execWithInput(posixWriteCommand(dir, file), data);
      if (res.code !== 0) {
        throw new Error(res.stderr.trim() || `远端写入失败（exit ${res.code}）`);
      }
      return { path: file };
    } catch (err) {
      return reply.code(502).send({ error: `图片上传失败：${(err as Error).message}` });
    }
  });

  // ---- 飞书项目（Meegle）----
  // 放在最后、鉴权钩子之后注册：这些路由同样只认登录 cookie
  registerMeegleRoutes(app, meegle, db);
}
