import type {
  AuthStatus,
  DeleteProjectResult,
  HostZellijStatus,
  Project,
  ProjectInput,
  RepoInfo,
  Session,
  SessionWithProject,
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

  listProjects: () => request<Project[]>("GET", "/api/projects"),
  createProject: (input: ProjectInput) =>
    request<Project>("POST", "/api/projects", input),
  updateProject: (id: string, input: ProjectInput) =>
    request<Project>("PUT", `/api/projects/${id}`, input),
  deleteProject: (id: string, force: boolean) =>
    request<DeleteProjectResult>("DELETE", `/api/projects/${id}?force=${force}`),

  /** 源项目的仓库信息。环境事实写在 derivable/reason 里，不会抛 */
  repoInfo: (projectId: string) => request<RepoInfo>("GET", `/api/projects/${projectId}/repo`),
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
  createSession: (projectId: string, name?: string) =>
    request<Session>("POST", `/api/projects/${projectId}/sessions`, { name }),
  reattachSession: (id: string) => request<Session>("POST", `/api/sessions/${id}/reattach`),
  terminateSession: (id: string) => request("POST", `/api/sessions/${id}/terminate`),
  clearSession: (id: string) => request("DELETE", `/api/sessions/${id}`),
  renameSession: (id: string, name: string) =>
    request("PATCH", `/api/sessions/${id}`, { name }),
};
