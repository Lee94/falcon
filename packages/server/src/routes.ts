import fs from "node:fs";
import type { FastifyInstance } from "fastify";
import type {
  AuthStatus,
  DeleteProjectResult,
  FsListing,
  GitChangeCounts,
  GitFileDiff,
  GitSnapshot,
  GitUnavailableReason,
  HostZellijStatus,
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
} from "@mojito/shared";
import { PASTE_IMAGE_MAX_BYTES, sanitizeColorHint } from "@mojito/shared";
import { Db, type ProjectRow, type SshHostRow } from "./db.js";
import { listDirectories, listRemoteDirectories } from "./fs.js";
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
import { gitErrorLine, WorktreeError, worktreeFailureText } from "./git/error.js";
import { gitHostFor } from "./git/host.js";
import { withRepoLock } from "./git/lock.js";
import {
  canonKey,
  isAbsolute,
  isAncestor,
  isUnc,
  joinPath,
  pathDepth,
  siblingWorktreePath,
  vetoTargetDir,
} from "./git/path.js";
import { remoteRoot } from "./zellij/host.js";
import { cleanupWorktree } from "./git/remove.js";
import {
  addWorktree,
  describeGit,
  describeGitChanges,
  describeGitDiff,
  describeRepo,
  pathExists,
  repoRoot,
  unavailableChanges,
  unavailableDiff,
  unavailableSnapshot,
  worktreeStatus,
} from "./git/repo.js";
import { DEFAULT_BASE_URL, ZELLIJ_VERSION } from "./zellij/version.js";

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
  /** mojito 数据目录，本地会话的粘贴图片落在 <dataDir>/paste */
  dataDir: string;
}

export function registerRoutes(app: FastifyInstance, deps: RouteDeps) {
  const { db, auth, manager, secrets } = deps;

  // 粘贴图片的请求体是原始图片字节。fastify 默认只认 JSON，这里按原样收成 Buffer
  app.addContentTypeParser(
    /^image\//,
    { parseAs: "buffer", bodyLimit: PASTE_IMAGE_MAX_BYTES },
    (_req, body, done) => done(null, body)
  );

  app.addHook("onRequest", async (req, reply) => {
    const url = req.url;
    if (!url.startsWith("/api/")) return;
    if (url.startsWith("/api/auth/")) return;
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

  function validateProjectInput(input: ProjectInput): string | null {
    if (!input.name?.trim()) return "项目名称不能为空";
    if (input.type === "local") {
      if (!input.workingDir?.trim()) return "本地项目必须指定文件夹路径";
      try {
        if (!fs.statSync(input.workingDir).isDirectory()) return "路径不是文件夹";
      } catch {
        return "文件夹路径不存在或不可访问";
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
    const err = validateProjectInput(input);
    if (err) return reply.code(400).send({ error: err });

    const ssh = resolveProjectSsh(input);
    if (!ssh.ok) return reply.code(400).send({ error: ssh.error });

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
    const err = validateProjectInput(input);
    if (err) return reply.code(400).send({ error: err });

    const ssh = resolveProjectSsh(input, existing);
    if (!ssh.ok) return reply.code(400).send({ error: ssh.error });

    const row: ProjectRow = {
      ...existing,
      name: input.name.trim(),
      working_dir: input.workingDir?.trim() || null,
      shell: input.shell?.trim() || null,
      ...ssh.ssh,
    };
    db.updateProject(row);
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
    const otherDirs = db
      .listProjects()
      .filter((p) => !doomed.has(p.id))
      .map((p) => p.working_dir)
      .filter((d): d is string => !!d);

    const warnings: string[] = [];
    for (const row of targets) {
      // 只有附属项目才碰文件系统。源项目的目录不是 mojito 建的，永远不动
      if (row.source_project_id) {
        try {
          warnings.push(...(await cleanupWorktree(row, await gitHostFor(row, manager), otherDirs)));
        } catch (err) {
          const e = err as WorktreeError;
          warnings.push(
            `没能在宿主机上执行清理，目录未删除：${row.working_dir}（${e.detail ?? e.message}）`
          );
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
   * 右侧 Git 面板的仓库快照。
   *
   * 源项目和附属项目都能问（跟 GET /repo 不同，那边拒绝附属项目）。
   * 环境事实写在 available/reason 里，不抛 4xx。
   */
  app.get("/api/projects/:id/git", async (req, reply): Promise<GitSnapshot | void> => {
    const { id } = req.params as { id: string };
    const row = db.getProject(id);
    if (!row) return reply.code(404).send({ error: "项目不存在" });
    if (!row.working_dir) {
      return unavailableSnapshot("no-working-dir", worktreeFailureText("no-working-dir"));
    }
    try {
      return await describeGit(await gitHostFor(row, manager), row.working_dir);
    } catch (err) {
      const e = err as WorktreeError;
      const reason: GitUnavailableReason = GIT_UNAVAILABLE.has(e.reason)
        ? (e.reason as GitUnavailableReason)
        : "link-failed";
      return unavailableSnapshot(reason, e.detail ?? e.message);
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
   * Git 面板里单个文件的 diff。path / origPath 由前端从快照原样带回
   * （仓库根相对路径），untracked=1 表示走 --no-index 伪 diff。
   * 环境事实与命令失败同样不抛 4xx，写在 available/reason/detail 里。
   */
  app.get("/api/projects/:id/git/diff", async (req, reply): Promise<GitFileDiff | void> => {
    const { id } = req.params as { id: string };
    const q = req.query as { path?: string; origPath?: string; untracked?: string };
    const row = db.getProject(id);
    if (!row) return reply.code(404).send({ error: "项目不存在" });
    if (!q.path) return reply.code(400).send({ error: "缺少 path 参数" });
    if (!row.working_dir) {
      return unavailableDiff("no-working-dir", worktreeFailureText("no-working-dir"));
    }
    try {
      return await describeGitDiff(await gitHostFor(row, manager), row.working_dir, {
        path: q.path,
        origPath: q.origPath || undefined,
        untracked: q.untracked === "1",
      });
    } catch (err) {
      const e = err as WorktreeError;
      const reason: GitUnavailableReason = GIT_UNAVAILABLE.has(e.reason)
        ? (e.reason as GitUnavailableReason)
        : "link-failed";
      return unavailableDiff(reason, e.detail ?? e.message);
    }
  });

  /**
   * 派生一个附属项目。
   *
   * 与 GET /repo 的分工：那边是探测，环境事实如实报告；这边是操作，同样的事实
   * 一律当状态冲突（409）。
   */
  app.post("/api/projects/:id/worktrees", async (req, reply) => {
    const { id } = req.params as { id: string };
    const src = db.getProject(id);
    if (!src) return reply.code(404).send({ error: "项目不存在" });

    // 禁止二级派生。git 本身允许（从 linked worktree 建的 worktree 仍属同一仓库），
    // 但侧栏的两级树会变成任意深度、删除级联要递归，而收益是零——从源项目派生
    // 完全等价。将来若要放开：把 source 取成 `row.source_project_id ?? row.id`
    // 重新挂到根即可，树仍是两级。
    if (src.source_project_id) {
      return reply.code(409).send({ error: "附属项目不能再派生，请从它的源项目派生" });
    }
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

      const created = await withRepoLock(`${host.key}::${canonKey(host.kind, main)}`, async () => {
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
    try {
      return await worktreeStatus(await gitHostFor(row, manager), row.working_dir);
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
    };
    const count = db.listSessionsByProject(id).length;
    try {
      return await manager.createSession(
        project,
        body.name?.trim() || `Terminal ${count + 1}`,
        sanitizeColorHint(body)
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
    if (!name?.trim()) return reply.code(400).send({ error: "名称不能为空" });
    if (!db.getSession(id)) return reply.code(404).send({ error: "会话不存在" });
    db.renameSession(id, name.trim());
    return { ok: true };
  });

  /**
   * 粘贴图片：写进会话宿主机的 <mojito 根>/paste，返回绝对路径。
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
      const dir = pasteDir(facts.kind, remoteRoot(facts.kind, facts.home));
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
}
