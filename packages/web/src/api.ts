import type {
  AuthStatus,
  DeleteProjectResult,
  FsListing,
  GitChangeCounts,
  GitSnapshot,
  HostZellijStatus,
  PasteImageResult,
  PortForward,
  PortForwardInput,
  Project,
  ProjectInput,
  RepoInfo,
  CreateSessionRequest,
  Session,
  SessionWithProject,
  ShellsInfo,
  SshHost,
  SshHostInput,
  SshProbeResult,
  SystemInfo,
  WorktreeInput,
  WorktreeStatus,
} from "@mojito/shared";

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
      res.status
    );
  }
  return data as T;
}

export class ApiRequestError extends Error {
  constructor(
    message: string,
    public status: number
  ) {
    super(message);
  }
}

export const api = {
  authStatus: () => request<AuthStatus>("GET", "/api/auth/status"),
  login: (password: string) => request("POST", "/api/auth/login", { password }),
  logout: () => request("POST", "/api/auth/logout"),
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
  /** 右侧 Git 面板。源项目和附属项目都能问，环境事实写在 available/reason 里 */
  gitSnapshot: (projectId: string) => request<GitSnapshot>("GET", `/api/projects/${projectId}/git`),
  /** 侧栏最后一层的 +N −M。读不到时 available=false，不抛 */
  gitChanges: (projectId: string) =>
    request<GitChangeCounts>("GET", `/api/projects/${projectId}/git/changes`),
  listForwards: (projectId: string) =>
    request<PortForward[]>("GET", `/api/projects/${projectId}/forwards`),
  createForward: (projectId: string, input: PortForwardInput) =>
    request<PortForward>("POST", `/api/projects/${projectId}/forwards`, input),
  updateForward: (projectId: string, id: string, patch: Partial<PortForwardInput>) =>
    request<PortForward>("PATCH", `/api/projects/${projectId}/forwards/${id}`, patch),
  deleteForward: (projectId: string, id: string) =>
    request<{ ok: true }>("DELETE", `/api/projects/${projectId}/forwards/${id}`),
  createWorktree: (projectId: string, input: WorktreeInput) =>
    request<Project>("POST", `/api/projects/${projectId}/worktrees`, input),
  worktreeStatus: (projectId: string) =>
    request<WorktreeStatus>("GET", `/api/projects/${projectId}/worktree`),

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
