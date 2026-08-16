import { create } from "zustand";
import type {
  AuthStatus,
  NonDurableReason,
  Project,
  SessionState,
  SessionWithProject,
  SystemInfo,
} from "@mojito/shared";
import { toast as sonner } from "sonner";
import { api, ApiRequestError } from "./api.js";
import i18n from "./i18n.js";

export type ActiveView = { kind: "overview" } | { kind: "terminal"; sessionId: string };

export type OverviewFilter = "all" | SessionState;

/** 还没拿到后端 id 的会话：tab 立刻出现并显示"正在建立会话…"，而不是等 REST 返回 */
export interface PendingSession {
  id: string;
  projectId: string;
  error?: string;
}

export type ToastKind = "info" | "success" | "warning" | "danger";

export interface ToastSpec {
  kind: ToastKind;
  title: string;
  body?: string;
  actionLabel?: string;
  onAction?: () => void;
  dismissLabel?: string;
  onDismiss?: () => void;
  /** 需要用户读完的消息（如"旧会话仍非持久"）停留更久 */
  sticky?: boolean;
}

export interface MenuItemSpec {
  label: string;
  kbd?: string;
  danger?: boolean;
  /** 上方画一条分隔线，危险项永远单独分组置底 */
  separated?: boolean;
  onSelect: () => void;
}

export interface MenuSpec {
  x: number;
  y: number;
  items: MenuItemSpec[];
}

export interface ConfirmListItem {
  name: string;
  meta?: string;
  /** 省略 = 这条不是会话（例如 worktree 里未提交的文件），不画状态记号 */
  state?: SessionState;
}

export interface ConfirmSpec {
  title: string;
  body: string;
  list?: ConfirmListItem[];
  footnote?: string;
  confirmLabel: string;
  onConfirm: () => void | Promise<void>;
}

export interface InstallSpec {
  projectId: string;
  /** 安装流程结束后是否继续创建会话（从「新建终端」进来时为 true） */
  thenCreate: boolean;
}

export interface InstallFailureRecord {
  reason: NonDurableReason;
  detail?: string;
  attempts: number;
}

const WORKSPACE_KEY = "mojito.workspace";
const DETACH_KEY = "mojito.detachEducated";
const PENDING_PREFIX = "pending:";

export function isPendingId(id: string): boolean {
  return id.startsWith(PENDING_PREFIX);
}

/** 侧栏此刻是否真的显示：用户偏好与窄屏临时隐藏的合成结果 */
export const selectSidebarVisible = (s: {
  sidebarOpen: boolean;
  sidebarAutoHidden: boolean;
}) => s.sidebarOpen && !s.sidebarAutoHidden;

interface PersistedWorkspace {
  tabs: string[];
  active: ActiveView;
  sidebarOpen: boolean;
  collapsed: Record<string, boolean>;
}

function loadWorkspace(): PersistedWorkspace {
  const fallback: PersistedWorkspace = {
    tabs: [],
    active: { kind: "overview" },
    sidebarOpen: true,
    collapsed: {},
  };
  try {
    const raw = localStorage.getItem(WORKSPACE_KEY);
    if (!raw) return fallback;
    const parsed = JSON.parse(raw) as Partial<PersistedWorkspace>;
    return {
      // 持久化的 tab 里绝不该混进上一次的 pending id
      tabs: (parsed.tabs ?? []).filter((t) => typeof t === "string" && !isPendingId(t)),
      active:
        parsed.active?.kind === "terminal" && typeof parsed.active.sessionId === "string"
          ? parsed.active
          : { kind: "overview" },
      sidebarOpen: parsed.sidebarOpen !== false,
      collapsed: parsed.collapsed ?? {},
    };
  } catch {
    return fallback;
  }
}

const initialWorkspace = loadWorkspace();

let pendingSeq = 0;
/** 本次页面加载内只提示一次 Detach，"不再提示"才写 localStorage */
let detachToastShown = false;

interface AppState {
  authChecked: boolean;
  auth: AuthStatus | null;
  system: SystemInfo | null;
  projects: Project[];
  sessions: SessionWithProject[];

  /** 打开的终端 tab（Detach 语义：关 tab 不杀会话），可能含 pending id */
  tabs: string[];
  active: ActiveView;
  pending: PendingSession[];

  /** 用户的侧栏偏好（持久化） */
  sidebarOpen: boolean;
  /** 窄屏临时隐藏，不写回偏好——不然开一次窄窗口就把用户的设置改了 */
  sidebarAutoHidden: boolean;
  collapsed: Record<string, boolean>;

  overviewFilter: OverviewFilter;
  /** 总览按项目筛选；null = 全部项目 */
  overviewProject: string | null;
  selected: string[];

  menu: MenuSpec | null;
  confirm: ConfirmSpec | null;
  /** 就地重命名的会话 id，替代 prompt() */
  renameFor: string | null;
  paletteOpen: boolean;
  drawerProjectId: string | null;
  install: InstallSpec | null;
  installFailures: Record<string, InstallFailureRecord>;
  /** null = 关闭；{ edit: null } = 新建 */
  projectForm: { edit: Project | null } | null;
  /** 正在从哪个源项目派生附属项目；null = 关闭 */
  worktreeFor: string | null;
  passwordOpen: boolean;

  init(): Promise<void>;
  refreshAuth(): Promise<void>;
  refreshProjects(): Promise<void>;
  refreshSessions(): Promise<void>;

  openSession(sessionId: string): void;
  /** Detach：关 tab 不杀会话，首次会解释这件事 */
  closeTab(id: string): void;
  /** 只把 tab 摘掉，不做 Detach 引导——清除已丢失记录这类场景用它 */
  dropTab(id: string): void;
  showOverview(): void;
  focusTabAt(index: number): void;
  cycleTab(delta: number): void;

  toggleSidebar(): void;
  setSidebarAutoHidden(hidden: boolean): void;
  toggleProject(projectId: string): void;
  setFilter(filter: OverviewFilter): void;
  setProjectFilter(projectId: string | null): void;
  toggleSelected(id: string): void;
  setSelected(ids: string[]): void;
  openProjectForm(edit: Project | null): void;
  closeProjectForm(): void;
  openWorktreeForm(sourceProjectId: string): void;
  closeWorktreeForm(): void;
  setPasswordOpen(open: boolean): void;

  toast(spec: ToastSpec): string;
  openMenu(spec: MenuSpec): void;
  closeMenu(): void;
  askConfirm(spec: ConfirmSpec): void;
  closeConfirm(): void;
  openRename(sessionId: string): void;
  closeRename(): void;
  setPalette(open: boolean): void;
  openDrawer(projectId: string): void;
  closeDrawer(): void;
  openInstall(spec: InstallSpec): void;
  closeInstall(): void;
  noteInstallFailure(projectId: string, record: InstallFailureRecord): void;

  newTerminal(projectId: string): Promise<void>;
  createSessionNow(projectId: string): Promise<void>;
  retryPending(pendingId: string): Promise<void>;

  handleApiError(err: unknown): void;
}

export const useApp = create<AppState>((set, get) => {
  /** tabs / active / 侧栏状态写回 localStorage —— 刷新页面后工作台原样恢复 */
  const persist = () => {
    const { tabs, active, sidebarOpen, collapsed } = get();
    const payload: PersistedWorkspace = {
      tabs: tabs.filter((t) => !isPendingId(t)),
      active: active.kind === "terminal" && isPendingId(active.sessionId)
        ? { kind: "overview" }
        : active,
      sidebarOpen,
      collapsed,
    };
    try {
      localStorage.setItem(WORKSPACE_KEY, JSON.stringify(payload));
    } catch {
      // 隐私模式下写不进去，不影响本次会话
    }
  };

  return {
    authChecked: false,
    auth: null,
    system: null,
    projects: [],
    sessions: [],

    tabs: initialWorkspace.tabs,
    active: initialWorkspace.active,
    pending: [],

    sidebarOpen: initialWorkspace.sidebarOpen,
    sidebarAutoHidden: false,
    collapsed: initialWorkspace.collapsed,

    overviewFilter: "all",
    overviewProject: null,
    selected: [],
    projectForm: null,
    worktreeFor: null,
    passwordOpen: false,

    menu: null,
    confirm: null,
    renameFor: null,
    paletteOpen: false,
    drawerProjectId: null,
    install: null,
    installFailures: {},

    async init() {
      await get().refreshAuth();
      const { auth } = get();
      if (auth && (!auth.required || auth.authenticated)) {
        const [system] = await Promise.all([
          api.system(),
          get().refreshProjects(),
          get().refreshSessions(),
        ]);
        set({ system });
      }
    },

    async refreshAuth() {
      try {
        const auth = await api.authStatus();
        set({ auth, authChecked: true });
      } catch {
        set({ authChecked: true });
      }
    },

    async refreshProjects() {
      try {
        set({ projects: await api.listProjects() });
      } catch (err) {
        get().handleApiError(err);
      }
    },

    async refreshSessions() {
      try {
        const sessions = await api.listSessions();
        const alive = new Set(sessions.map((s) => s.id));
        // dead 会话仍在列表里，因此恢复出来的 tab 不会被静默丢弃，只是显示为已丢失
        const keep = (id: string) => isPendingId(id) || alive.has(id);
        set((state) => ({
          sessions,
          tabs: state.tabs.filter(keep),
          active:
            state.active.kind === "terminal" && !keep(state.active.sessionId)
              ? { kind: "overview" }
              : state.active,
          selected: state.selected.filter((id) => alive.has(id)),
        }));
        persist();
      } catch (err) {
        get().handleApiError(err);
      }
    },

    openSession(sessionId) {
      set((state) => ({
        tabs: state.tabs.includes(sessionId) ? state.tabs : [...state.tabs, sessionId],
        active: { kind: "terminal", sessionId },
        paletteOpen: false,
        menu: null,
      }));
      persist();
    },

    dropTab(id) {
      set((state) => {
        const tabs = state.tabs.filter((t) => t !== id);
        let active = state.active;
        if (active.kind === "terminal" && active.sessionId === id) {
          active =
            tabs.length > 0
              ? { kind: "terminal", sessionId: tabs[tabs.length - 1]! }
              : { kind: "overview" };
        }
        return { tabs, active, pending: state.pending.filter((p) => p.id !== id) };
      });
      persist();
    },

    closeTab(id) {
      const wasReal = !isPendingId(id);
      const session = get().sessions.find((s) => s.id === id);
      get().dropTab(id);

      // 关 tab ≠ 杀会话，这是产品最容易被误解的语义，值得一次性引导
      if (!wasReal || !session) return;
      let educated = true;
      try {
        educated = localStorage.getItem(DETACH_KEY) === "1";
      } catch {
        educated = false;
      }
      if (educated || detachToastShown) return;
      detachToastShown = true;
      get().toast({
        kind: "info",
        sticky: true,
        title: i18n.t("toast.detachTitle", { name: session.name }),
        body: i18n.t("toast.detachBody"),
        actionLabel: i18n.t("toast.gotIt"),
        dismissLabel: i18n.t("toast.dontShowAgain"),
        onDismiss: () => {
          try {
            localStorage.setItem(DETACH_KEY, "1");
          } catch {
            // 写不进去就下次再提示一遍，无害
          }
        },
      });
    },

    showOverview() {
      set({ active: { kind: "overview" }, paletteOpen: false, menu: null });
      persist();
    },

    focusTabAt(index) {
      const { tabs } = get();
      const id = tabs[index];
      if (id) {
        set({ active: { kind: "terminal", sessionId: id } });
        persist();
      }
    },

    cycleTab(delta) {
      const { tabs, active } = get();
      if (tabs.length === 0) return;
      const current =
        active.kind === "terminal" ? tabs.indexOf(active.sessionId) : -1;
      const next = (current + delta + tabs.length * 2) % tabs.length;
      set({ active: { kind: "terminal", sessionId: tabs[next]! } });
      persist();
    },

    /** 显式开合永远以"现在看到的样子"为准，并解除窄屏的临时隐藏 */
    toggleSidebar() {
      const visible = get().sidebarOpen && !get().sidebarAutoHidden;
      set({ sidebarOpen: !visible, sidebarAutoHidden: false });
      persist();
    },

    setSidebarAutoHidden(hidden) {
      set({ sidebarAutoHidden: hidden });
    },

    toggleProject(projectId) {
      set((s) => ({
        collapsed: { ...s.collapsed, [projectId]: !s.collapsed[projectId] },
      }));
      persist();
    },

    setFilter(filter) {
      set({ overviewFilter: filter, selected: [] });
    },

    setProjectFilter(projectId) {
      set({ overviewProject: projectId, selected: [] });
    },

    openProjectForm(edit) {
      set({ projectForm: { edit }, menu: null, paletteOpen: false });
    },
    closeProjectForm() {
      set({ projectForm: null });
    },
    openWorktreeForm(sourceProjectId) {
      set({ worktreeFor: sourceProjectId, menu: null, paletteOpen: false });
    },
    closeWorktreeForm() {
      set({ worktreeFor: null });
    },
    setPasswordOpen(open) {
      set({ passwordOpen: open, menu: null, paletteOpen: false });
    },

    toggleSelected(id) {
      set((s) => ({
        selected: s.selected.includes(id)
          ? s.selected.filter((x) => x !== id)
          : [...s.selected, id],
      }));
    },

    setSelected(ids) {
      set({ selected: ids });
    },

    /**
     * 转交给 sonner。保留这个 store 方法而不是让各处直接 import sonner：
     * 调用点只描述"发生了什么"，"长什么样、停多久"是这一处的事。
     */
    toast(spec) {
      const emit =
        spec.kind === "success"
          ? sonner.success
          : spec.kind === "warning"
            ? sonner.warning
            : spec.kind === "danger"
              ? sonner.error
              : sonner.info;
      const id = emit(spec.title, {
        description: spec.body,
        // 需要用户读完的消息（如"旧会话仍非持久"）停留更久
        duration: spec.sticky ? 12000 : 5000,
        action: spec.actionLabel
          ? { label: spec.actionLabel, onClick: () => spec.onAction?.() }
          : undefined,
        cancel: spec.dismissLabel
          ? { label: spec.dismissLabel, onClick: () => spec.onDismiss?.() }
          : undefined,
      });
      return String(id);
    },

    openMenu(spec) {
      set({ menu: spec });
    },
    closeMenu() {
      set({ menu: null });
    },
    askConfirm(spec) {
      set({ confirm: spec, menu: null, paletteOpen: false });
    },
    closeConfirm() {
      set({ confirm: null });
    },
    openRename(sessionId) {
      set({ renameFor: sessionId, menu: null, paletteOpen: false });
    },
    closeRename() {
      set({ renameFor: null });
    },
    setPalette(open) {
      set({ paletteOpen: open, menu: null });
    },
    openDrawer(projectId) {
      set({ drawerProjectId: projectId, menu: null, paletteOpen: false });
    },
    closeDrawer() {
      set({ drawerProjectId: null });
    },
    openInstall(spec) {
      set({ install: spec, menu: null, paletteOpen: false, drawerProjectId: null });
    },
    closeInstall() {
      set({ install: null });
    },
    noteInstallFailure(projectId, record) {
      set((s) => ({ installFailures: { ...s.installFailures, [projectId]: record } }));
    },

    /**
     * 新建终端。SSH 项目首次用时先走授权 + 安装；
     * 已拒绝过的主机不再打扰（撤销/重新启用走「持久会话设置」），直接建非持久会话。
     */
    async newTerminal(projectId) {
      const project = get().projects.find((p) => p.id === projectId);
      if (!project) return;
      set({ menu: null, paletteOpen: false });
      if (project.type === "ssh") {
        try {
          const status = await api.hostStatus(projectId);
          const needsSetup =
            status.authorized === null ||
            (status.authorized === true && !status.installedVersion);
          if (needsSetup) {
            get().openInstall({ projectId, thenCreate: true });
            return;
          }
        } catch {
          // 查不到主机状态就照常建会话，由后端判定持久性
        }
      }
      await get().createSessionNow(projectId);
    },

    async createSessionNow(projectId) {
      const pendingId = `${PENDING_PREFIX}${++pendingSeq}`;
      set((s) => ({
        pending: [...s.pending, { id: pendingId, projectId }],
        tabs: [...s.tabs, pendingId],
        active: { kind: "terminal", sessionId: pendingId },
        menu: null,
        paletteOpen: false,
      }));
      try {
        const session = await api.createSession(projectId);
        await get().refreshSessions();
        set((s) => ({
          pending: s.pending.filter((p) => p.id !== pendingId),
          tabs: s.tabs.map((t) => (t === pendingId ? session.id : t)),
          active:
            s.active.kind === "terminal" && s.active.sessionId === pendingId
              ? { kind: "terminal", sessionId: session.id }
              : s.active,
        }));
        persist();
      } catch (err) {
        get().handleApiError(err);
        set((s) => ({
          pending: s.pending.map((p) =>
            p.id === pendingId ? { ...p, error: (err as Error).message } : p
          ),
        }));
      }
    },

    async retryPending(pendingId) {
      const entry = get().pending.find((p) => p.id === pendingId);
      if (!entry) return;
      set((s) => ({
        pending: s.pending.map((p) =>
          p.id === pendingId ? { ...p, error: undefined } : p
        ),
      }));
      try {
        const session = await api.createSession(entry.projectId);
        await get().refreshSessions();
        set((s) => ({
          pending: s.pending.filter((p) => p.id !== pendingId),
          tabs: s.tabs.map((t) => (t === pendingId ? session.id : t)),
          active:
            s.active.kind === "terminal" && s.active.sessionId === pendingId
              ? { kind: "terminal", sessionId: session.id }
              : s.active,
        }));
        persist();
      } catch (err) {
        get().handleApiError(err);
        set((s) => ({
          pending: s.pending.map((p) =>
            p.id === pendingId ? { ...p, error: (err as Error).message } : p
          ),
        }));
      }
    },

    handleApiError(err) {
      if (err instanceof ApiRequestError && err.status === 401) {
        void get().refreshAuth();
      }
    },
  };
});
