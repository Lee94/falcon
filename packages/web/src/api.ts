import type {
  AuthStatus,
  DeleteProjectResult,
  FsListing,
  GitChangeCounts,
  GitCommitDetail,
  GitCommitInput,
  GitFileDiff,
  GitLogPage,
  GitOpInput,
  GitRefsInfo,
  GitSnapshot,
  GitSyncResult,
  GitWorkingChanges,
  DockerLogs,
  DockerOpInput,
  DockerOpResult,
  DockerSnapshot,
  HostZellijStatus,
  MultiRepoProbe,
  MultiWorktreeInput,
  PasteImageResult,
  PortForward,
  PortForwardInput,
  Project,
  ProjectInput,
  RepoInfo,
  CreateSessionRequest,
  Session,
  SessionForeground,
  SessionWithProject,
  ShellsInfo,
  SshHost,
  SshHostInput,
  SshProbeResult,
  SystemInfo,
  UploadResult,
  FileOpResult,
  FileRemoveResult,
  WorktreeInput,
  WorktreeStatus,
  WorkspaceFile,
  WorkspaceIndex,
  WorkspaceListing,
} from "@falcon/shared";

async function request<T>(method: string, url: string, body?: unknown): Promise<T> {
  const res = await fetch(url, {
    method,
    headers: body != null ? { "Content-Type": "application/json" } : undefined,
    body: body != null ? JSON.stringify(body) : undefined,
    credentials: "same-origin",
  });
  const data = await res.json().catch(() => ({}));
  if (!res.ok) {
    throw new ApiRequestError(
      (data as { error?: string }).error ?? `HTTP ${res.status}`,
      res.status,
      data
    );
  }
  return data as T;
}

export class ApiRequestError extends Error {
  constructor(
    message: string,
    public status: number,
    /** 服务端返回的完整错误体（如批量派生的 MultiDeriveError），调用方按需收窄 */
    public body?: unknown
  ) {
    super(message);
  }
}

/** 多仓库项目的 git 端点统一带 ?repo=<成员dir>（取自 project.multi.repos，原样带回） */
function repoQuery(repo?: string): string {
  return repo ? `?repo=${encodeURIComponent(repo)}` : "";
}

export const api = {
  authStatus: () => request<AuthStatus>("GET", "/api/auth/status"),
  login: (password: string) => request("POST", "/api/auth/login", { password }),
  logout: () => request("POST", "/api/auth/logout"),
  pendingAskpass: () =>
    request<{ id: string; prompt: string }[]>("GET", "/api/askpass/pending"),
  answerAskpass: (id: string, password: string) =>
    request<{ ok: true }>("POST", `/api/askpass/${id}/answer`, { password }),
  cancelAskpass: (id: string) =>
    request<{ ok: true }>("POST", `/api/askpass/${id}/answer`, { cancel: true }),
  setPassword: (next: string, current?: string) =>
    request("POST", "/api/auth/password", { next, current }),

  system: () => request<SystemInfo>("GET", "/api/system"),
  validatePath: (path: string) =>
    request<{ ok: boolean; error?: string }>("POST", "/api/fs/validate", { path }),
  /**
   * `dir` 缺省为家目录；空字符串是 Windows 盘符列表。
   * 传 hostId / projectId 则列远端，否则列后端本机。
   */
  listDir: (dir?: string, opts?: { hostId?: string; projectId?: string }) => {
    const q = new URLSearchParams();
    if (dir !== undefined) q.set("path", dir);
    if (opts?.hostId) q.set("hostId", opts.hostId);
    if (opts?.projectId) q.set("projectId", opts.projectId);
    const qs = q.toString();
    return request<FsListing>("GET", `/api/fs/list${qs ? `?${qs}` : ""}`);
  },
  /** 侦测宿主机可用 shell。不带参数侦测后端本机，带 hostId/projectId 侦测远端 */
  listShells: (opts?: { hostId?: string; projectId?: string }) => {
    const q = new URLSearchParams();
    if (opts?.hostId) q.set("hostId", opts.hostId);
    if (opts?.projectId) q.set("projectId", opts.projectId);
    const qs = q.toString();
    return request<ShellsInfo>("GET", `/api/shells${qs ? `?${qs}` : ""}`);
  },

  listHosts: () => request<SshHost[]>("GET", "/api/hosts"),
  createHost: (input: SshHostInput) => request<SshHost>("POST", "/api/hosts", input),
  updateHost: (id: string, input: SshHostInput) =>
    request<SshHost>("PUT", `/api/hosts/${id}`, input),
  deleteHost: (id: string) => request<{ ok: true }>("DELETE", `/api/hosts/${id}`),
  /** 已保存主机 */
  testHost: (id: string) => request<SshProbeResult>("POST", `/api/hosts/${id}/test`),
  /** 表单草稿。编辑已有主机时带 hostId，空 secret 沿用已保存的 */
  testHostDraft: (input: SshHostInput & { hostId?: string }) =>
    request<SshProbeResult>("POST", "/api/hosts/test", input),

  listProjects: () => request<Project[]>("GET", "/api/projects"),
  createProject: (input: ProjectInput) =>
    request<Project>("POST", "/api/projects", input),
  updateProject: (id: string, input: ProjectInput) =>
    request<Project>("PUT", `/api/projects/${id}`, input),
  deleteProject: (id: string, force: boolean) =>
    request<DeleteProjectResult>("DELETE", `/api/projects/${id}?force=${force}`),

  /** 源项目的仓库信息。环境事实写在 derivable/reason 里，不会抛 */
  repoInfo: (projectId: string) => request<RepoInfo>("GET", `/api/projects/${projectId}/repo`),
  /** 多仓库容器的派生前探测：逐成员 RepoInfo，环境事实同样不抛 */
  repoInfoMulti: (projectId: string) =>
    request<MultiRepoProbe>("GET", `/api/projects/${projectId}/repos`),
  /** 右侧 Git 面板。源项目和附属项目都能问，环境事实写在 available/reason 里 */
  gitSnapshot: (projectId: string, opts?: { repo?: string }) =>
    request<GitSnapshot>("GET", `/api/projects/${projectId}/git${repoQuery(opts?.repo)}`),
  /** 侧栏最后一层的 +N −M。读不到时 available=false，不抛 */
  gitChanges: (projectId: string) =>
    request<GitChangeCounts>("GET", `/api/projects/${projectId}/git/changes`),
  /** 侧栏轮询的批量版：服务端按宿主机分组，同主机的所有检出一次 exec 拿全 */
  gitChangesBatch: (projectIds: string[]) =>
    request<Record<string, GitChangeCounts>>("POST", "/api/git/changes", { ids: projectIds }),
  /** Git 面板里单个文件的 diff。环境事实与命令失败写在 available/reason 里，不抛 */
  gitFileDiff: (
    projectId: string,
    file: { path: string; origPath?: string; untracked?: boolean },
    opts?: { repo?: string }
  ) => {
    const q = new URLSearchParams({ path: file.path });
    if (file.origPath) q.set("origPath", file.origPath);
    if (file.untracked) q.set("untracked", "1");
    if (opts?.repo) q.set("repo", opts.repo);
    return request<GitFileDiff>("GET", `/api/projects/${projectId}/git/diff?${q}`);
  },

  /**
   * 「修改」面板：工作区全部未提交改动，带每个文件的 +N −M。
   * 比 gitChanges 重（多一条 numstat），只在面板打开时问。
   */
  gitWorking: (projectId: string, opts?: { repo?: string }) =>
    request<GitWorkingChanges>(
      "GET",
      `/api/projects/${projectId}/git/working${repoQuery(opts?.repo)}`
    ),
  /** History 列表的一页。筛选与分页都在 query 里，环境事实写在 available 里 */
  gitLog: (
    projectId: string,
    opts: { branch?: string; author?: string; q?: string; skip?: number; repo?: string } = {}
  ) => {
    const q = new URLSearchParams();
    if (opts.branch) q.set("branch", opts.branch);
    if (opts.author) q.set("author", opts.author);
    if (opts.q) q.set("q", opts.q);
    if (opts.skip) q.set("skip", String(opts.skip));
    if (opts.repo) q.set("repo", opts.repo);
    return request<GitLogPage>("GET", `/api/projects/${projectId}/git/log?${q}`);
  },
  /** Branch / User 两个筛选下拉的候选值 */
  gitRefs: (projectId: string, opts?: { repo?: string }) =>
    request<GitRefsInfo>("GET", `/api/projects/${projectId}/git/refs${repoQuery(opts?.repo)}`),
  /** 选中提交的详情：完整提交信息 + 改动文件 */
  gitCommit: (projectId: string, sha: string, opts?: { repo?: string }) => {
    const q = new URLSearchParams({ sha });
    if (opts?.repo) q.set("repo", opts.repo);
    return request<GitCommitDetail>("GET", `/api/projects/${projectId}/git/commit?${q}`);
  },
  /** 某条提交里单个文件的 diff */
  gitCommitDiff: (
    projectId: string,
    sha: string,
    file: { path: string; origPath?: string },
    opts?: { repo?: string }
  ) => {
    const q = new URLSearchParams({ sha, path: file.path });
    if (file.origPath) q.set("origPath", file.origPath);
    if (opts?.repo) q.set("repo", opts.repo);
    return request<GitFileDiff>("GET", `/api/projects/${projectId}/git/commit/diff?${q}`);
  },
  /**
   * 提交工作区改动。失败不抛——没配 user.name、pre-commit 钩子拒绝、
   * 没有可提交的改动都写在 ok/detail 里。多仓库项目必须带 repo（写操作服务端不猜）。
   */
  gitCommitChanges: (projectId: string, input: GitCommitInput, opts?: { repo?: string }) =>
    request<GitSyncResult>(
      "POST",
      `/api/projects/${projectId}/git/commit${repoQuery(opts?.repo)}`,
      input
    ),
  /**
   * Pull / Push。失败不抛——凭据不对、非快进、远端拒绝都写在 ok/detail 里，
   * 面板要把 git 的原话给用户看。多仓库项目必须带 repo。
   */
  gitSync: (projectId: string, action: "pull" | "push", opts?: { repo?: string }) =>
    request<GitSyncResult>(
      "POST",
      `/api/projects/${projectId}/git/${action}${repoQuery(opts?.repo)}`
    ),
  /**
   * 历史面板写操作（fetch / checkout / cherry-pick / revert / 建分支）。
   * 失败不抛，git 的原话写在 ok/detail 里。多仓库项目必须带 repo。
   */
  gitOp: (projectId: string, input: GitOpInput, opts?: { repo?: string }) =>
    request<GitSyncResult>(
      "POST",
      `/api/projects/${projectId}/git/op${repoQuery(opts?.repo)}`,
      input
    ),
  /**
   * 文件面板：工作目录里的一层。`path` 缺省为工作目录本身。
   * 与 git 无关——未跟踪、被 ignore 的文件同样在里面。
   */
  listFiles: (projectId: string, path?: string) => {
    const q = path ? `?path=${encodeURIComponent(path)}` : "";
    return request<WorkspaceListing>("GET", `/api/projects/${projectId}/files${q}`);
  },
  /** Quick Open：工作目录里的文件路径清单（git 仓库走 ls-files） */
  indexFiles: (projectId: string) =>
    request<WorkspaceIndex>("GET", `/api/projects/${projectId}/files/index`),
  /**
   * 查看 tab：读一个文件。二进制与超大文件也是 200，形状里写清了是什么。
   * 响应里的 rawBase 拼上路径就是图片 / HTML 预览用的原始字节地址（lib/rawUrl.ts）
   */
  readFile: (projectId: string, path: string) =>
    request<WorkspaceFile>(
      "GET",
      `/api/projects/${projectId}/file?path=${encodeURIComponent(path)}`
    ),
  /**
   * 下载地址（lib/fileTransfer.ts 的 triggerDownload 用）。同源导航自带登录
   * cookie，不需要原始字节路由那种放在 URL 里的作用域令牌；服务端按流回整个
   * 文件，没有预览的 16MB 上限。
   */
  downloadUrl: (projectId: string, path: string) =>
    `/api/projects/${projectId}/download?path=${encodeURIComponent(path)}`,
  /**
   * 上传一个文件到工作目录里的 dir。用 XHR 而不是 fetch：只有它给上传进度。
   * 请求体就是 File 本身，浏览器按流发、自动带 Content-Length（服务端靠它核对
   * 收满了没有）。同名文件已存在且没带 overwrite 时服务端回 409，调用方问过
   * 用户再带 overwrite 重发。
   */
  uploadFile: (
    projectId: string,
    dir: string,
    file: File,
    opts: { overwrite?: boolean; onProgress?: (sent: number, total: number) => void } = {}
  ) =>
    new Promise<UploadResult>((resolve, reject) => {
      const q = new URLSearchParams({ path: dir, name: file.name });
      if (opts.overwrite) q.set("overwrite", "1");
      const xhr = new XMLHttpRequest();
      xhr.open("PUT", `/api/projects/${projectId}/upload?${q}`);
      xhr.setRequestHeader("Content-Type", "application/octet-stream");
      xhr.responseType = "json";
      xhr.withCredentials = true;
      xhr.upload.onprogress = (e) => {
        if (e.lengthComputable) opts.onProgress?.(e.loaded, e.total);
      };
      xhr.onerror = () => reject(new ApiRequestError("网络错误", 0));
      xhr.onabort = () => reject(new ApiRequestError("上传已取消", 0));
      xhr.onload = () => {
        const data = (xhr.response ?? {}) as { error?: string };
        if (xhr.status >= 200 && xhr.status < 300) resolve(data as UploadResult);
        else reject(new ApiRequestError(data.error ?? `HTTP ${xhr.status}`, xhr.status, data));
      };
      xhr.send(file);
    }),
  mkdir: (projectId: string, path: string, recursive = false) =>
    request<FileOpResult>("POST", `/api/projects/${projectId}/mkdir`, { path, recursive }),
  renameFile: (projectId: string, path: string, name: string) =>
    request<FileOpResult>("POST", `/api/projects/${projectId}/rename`, { path, name }),
  removeFiles: (projectId: string, paths: string[]) =>
    request<FileRemoveResult>("POST", `/api/projects/${projectId}/remove`, { paths }),

  /**
   * 右侧 Docker 面板。命令跑在当前项目的宿主机上。
   * 环境事实写在 available/reason 里，不会抛。`file` 是要看的 compose 相对路径。
   */
  dockerSnapshot: (projectId: string, file?: string) => {
    const q = file ? `?file=${encodeURIComponent(file)}` : "";
    return request<DockerSnapshot>("GET", `/api/projects/${projectId}/docker${q}`);
  },
  dockerOp: (projectId: string, input: DockerOpInput) =>
    request<DockerOpResult>("POST", `/api/projects/${projectId}/docker/op`, input),
  dockerLogs: (
    projectId: string,
    opts: { target: "container"; ref: string; tail?: number } | { target: "compose"; file: string; tail?: number }
  ) => {
    const q = new URLSearchParams({ target: opts.target });
    if (opts.target === "container") q.set("ref", opts.ref);
    else q.set("file", opts.file);
    if (opts.tail != null) q.set("tail", String(opts.tail));
    return request<DockerLogs>("GET", `/api/projects/${projectId}/docker/logs?${q}`);
  },

  listForwards: (projectId: string) =>
    request<PortForward[]>("GET", `/api/projects/${projectId}/forwards`),
  createForward: (projectId: string, input: PortForwardInput) =>
    request<PortForward>("POST", `/api/projects/${projectId}/forwards`, input),
  updateForward: (projectId: string, id: string, patch: Partial<PortForwardInput>) =>
    request<PortForward>("PATCH", `/api/projects/${projectId}/forwards/${id}`, patch),
  deleteForward: (projectId: string, id: string) =>
    request<{ ok: true }>("DELETE", `/api/projects/${projectId}/forwards/${id}`),
  /** 派生：单仓库项目吃 WorktreeInput，多仓库容器吃 MultiWorktreeInput（服务端按项目分流） */
  createWorktree: (projectId: string, input: WorktreeInput | MultiWorktreeInput) =>
    request<Project>("POST", `/api/projects/${projectId}/worktrees`, input),
  worktreeStatus: (projectId: string) =>
    request<WorktreeStatus>("GET", `/api/projects/${projectId}/worktree`),
  /** 存档附属项目：隐藏并终止其会话，目录保留，到期由后端自动删除 */
  archiveProject: (id: string) => request<Project>("POST", `/api/projects/${id}/archive`),
  restoreProject: (id: string) => request<Project>("POST", `/api/projects/${id}/restore`),

  hostStatus: (projectId: string) =>
    request<HostZellijStatus>("GET", `/api/projects/${projectId}/host`),
  setHostAuthorization: (
    projectId: string,
    patch: { authorized?: boolean; baseUrl?: string }
  ) => request("POST", `/api/projects/${projectId}/host`, patch),

  listSessions: () => request<SessionWithProject[]>("GET", "/api/sessions"),
  createSession: (projectId: string, body?: CreateSessionRequest) =>
    request<Session>("POST", `/api/projects/${projectId}/sessions`, body ?? {}),
  reattachSession: (id: string) => request<Session>("POST", `/api/sessions/${id}/reattach`),
  /** 关 tab 前问一嘴前台有没有程序在跑；侦测不到的场景 busy 恒为 false */
  sessionForeground: (id: string) =>
    request<SessionForeground>("GET", `/api/sessions/${id}/foreground`),
  terminateSession: (id: string) => request("POST", `/api/sessions/${id}/terminate`),
  clearSession: (id: string) => request("DELETE", `/api/sessions/${id}`),
  renameSession: (id: string, name: string) =>
    request("PATCH", `/api/sessions/${id}`, { name }),
  /** 图片按原始字节直传，Content-Type 就是图片类型，不走 JSON 包装 */
  pasteImage: async (id: string, blob: Blob): Promise<PasteImageResult> => {
    const res = await fetch(`/api/sessions/${id}/paste-image`, {
      method: "POST",
      headers: { "Content-Type": blob.type },
      body: blob,
      credentials: "same-origin",
    });
    const data = await res.json().catch(() => ({}));
    if (!res.ok) {
      throw new ApiRequestError(
        (data as { error?: string }).error ?? `HTTP ${res.status}`,
        res.status
      );
    }
    return data as PasteImageResult;
  },
};
