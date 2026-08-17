import { create } from "zustand";
import type {
  AuthStatus,
  NonDurableReason,
  Project,
  ProjectType,
  SessionState,
  SessionWithProject,
  SshHost,
  SystemInfo,
} from "@mojito/shared";
import { toast as sonner } from "sonner";
import { api, ApiRequestError } from "./api.js";
import i18n from "./i18n.js";
import {
  applyTheme,
  loadThemePref,
  resolveTheme,
  saveThemePref,
  watchSystemTheme,
  type ThemeMode,
  type ThemePref,
} from "./lib/theme.js";
import {
  DEFAULT_TERM_PREF,
  loadTermPref,
  saveTermPref,
  sanitizeTermPref,
  type TermPref,
} from "./lib/term.js";

export type ActiveView =
  | { kind: "overview" }
  | { kind: "terminal"; sessionId: string }
  /** 选中了项目但还没有可显示的终端 */
  | { kind: "project" };

export function tabProjectId(
  tabId: string,
  sessions: SessionWithProject[],
  pending: PendingSession[]
): string | undefined {
  if (isPendingId(tabId)) return pending.find((p) => p.id === tabId)?.projectId;
  return sessions.find((s) => s.id === tabId)?.projectId;
}

/** 右侧栏目前只有 Git 这一格；id 留下是为了以后加面板不用改持久化形状 */
export type RightPanelId = "git";

export const selectRightVisible = (s: { rightOpen: boolean }) => s.rightOpen;

/** 侧栏选中项目时，主区 tab 只显示这个项目下的会话 */
export function visibleTabs(s: {
  tabs: string[];
  sessions: SessionWithProject[];
  pending: PendingSession[];
  selectedProjectId: string | null;
}): string[] {
  if (!s.selectedProjectId) return s.tabs;
  return s.tabs.filter(
    (id) => tabProjectId(id, s.sessions, s.pending) === s.selectedProjectId
  );
}

export type OverviewFilter = "all" | SessionState;

/** 设置弹窗左侧模块。打开时记住上次停在哪一格 */
export type SettingsTab = "appearance" | "account" | "hosts" | "about";

/** 还没拿到后端 id 的会话：tab 立刻出现并显示"正在建立会话…"，而不是等 REST 返回 */
export interface PendingSession {
  id: string;
  projectId: string;
  error?: string;
}

/**
 * Git 面板跟谁走：侧栏选中的项目优先，否则当前会话所属项目。
 * 总览且没选项目时为 null——不要退回第一个项目，免得打开面板看到别人的仓库。
 */
export function selectFocusProjectId(s: {
  selectedProjectId: string | null;
  active: ActiveView;
  sessions: SessionWithProject[];
  pending: PendingSession[];
}): string | null {
  if (s.selectedProjectId) return s.selectedProjectId;
  if (s.active.kind === "terminal") {
    const id = s.active.sessionId;
    return (
      s.pending.find((p) => p.id === id)?.projectId ??
      s.sessions.find((x) => x.id === id)?.projectId ??
      null
    );
  }
  return null;
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
  /** 单选组里当前生效的那一项，画一个勾（如主题） */
  checked?: boolean;
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

/** 从某台服务器新建项目时预填类型 / 主机，编辑已有项目时不用 */
export interface ProjectFormPreset {
  type?: ProjectType;
  hostId?: string;
}

/** 源项目工作区当前 HEAD，侧栏在没有 worktree 时用分支名代表默认仓库 */
export interface ProjectHead {
  branch?: string;
  sha?: string;
}

const WORKSPACE_KEY = "mojito.workspace";
const CLOSE_KILLS_KEY = "mojito.closeKillsEducated";
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
  rightOpen: boolean;
  rightPanel: RightPanelId;
  collapsed: Record<string, boolean>;
  selectedProjectId: string | null;
}

function loadWorkspace(): PersistedWorkspace {
  const fallback: PersistedWorkspace = {
    tabs: [],
    active: { kind: "overview" },
    sidebarOpen: true,
    rightOpen: false,
    rightPanel: "git",
    collapsed: {},
    selectedProjectId: null,
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
          : parsed.active?.kind === "project"
            ? { kind: "project" }
            : { kind: "overview" },
      sidebarOpen: parsed.sidebarOpen !== false,
      rightOpen: parsed.rightOpen === true,
      rightPanel: parsed.rightPanel === "git" ? parsed.rightPanel : "git",
      collapsed: parsed.collapsed ?? {},
      selectedProjectId:
        typeof parsed.selectedProjectId === "string" ? parsed.selectedProjectId : null,
    };
  } catch {
    return fallback;
  }
}

const initialWorkspace = loadWorkspace();
const initialThemePref = loadThemePref();
const initialTermPref = loadTermPref();

let pendingSeq = 0;
/** 本次页面加载内只解释一次"关 tab 会结束会话"，"不再提示"才写 localStorage */
let closeKillsToastShown = false;

/**
 * 侧栏用分支名代表默认仓库，但 list projects 不跑 git。
 * 只探源项目：附属项目的分支写在 worktree.branch 里，不必再问一次。
 */
async function refreshHeads(
  set: (partial: { heads: Record<string, ProjectHead> }) => void,
  get: () => { heads: Record<string, ProjectHead> },
  projects: Project[]
) {
  const sources = projects.filter((p) => !p.worktree && p.workingDir);
  const next: Record<string, ProjectHead> = { ...get().heads };
  await Promise.all(
    sources.map(async (p) => {
      try {
        const info = await api.repoInfo(p.id);
        if (info.headBranch) next[p.id] = { branch: info.headBranch };
        else if (info.headSha) next[p.id] = { sha: info.headSha };
        else delete next[p.id];
      } catch {
        // 探不到就继续用项目名，侧栏不能因为一台 SSH 抖动整棵树空白
      }
    })
  );
  const alive = new Set(projects.map((p) => p.id));
  for (const id of Object.keys(next)) {
    if (!alive.has(id)) delete next[id];
  }
  set({ heads: next });
}

/**
 * 关 tab 会杀掉会话——这是最容易让人措手不及的一步，头一次得说清楚，
 * 顺带告诉用户想留后台跑该怎么关。说过一次就闭嘴，每次都念是噪音。
 */
function closeKillsHint(): Partial<
  Pick<ToastSpec, "sticky" | "body" | "actionLabel" | "dismissLabel" | "onDismiss">
> {
  let educated = true;
  try {
    educated = localStorage.getItem(CLOSE_KILLS_KEY) === "1";
  } catch {
    educated = false;
  }
  if (educated || closeKillsToastShown) return {};
  closeKillsToastShown = true;
  return {
    sticky: true,
    body: i18n.t("toast.closeKillsBody"),
    actionLabel: i18n.t("toast.gotIt"),
    dismissLabel: i18n.t("toast.dontShowAgain"),
    onDismiss: () => {
      try {
        localStorage.setItem(CLOSE_KILLS_KEY, "1");
      } catch {
        // 写不进去就下次再提示一遍，无害
      }
    },
  };
}

interface AppState {
  authChecked: boolean;
  auth: AuthStatus | null;
  system: SystemInfo | null;
  projects: Project[];
  hosts: SshHost[];
  sessions: SessionWithProject[];

  /** 打开的终端 tab（手动关掉 = 结束会话，见 closeTab），可能含 pending id */
  tabs: string[];
  active: ActiveView;
  pending: PendingSession[];

  /** 用户的主题偏好（持久化）与它此刻实际解析成的明暗 */
  themePref: ThemePref;
  theme: ThemeMode;
  /** 终端画面偏好（字体 / 字号 / 主题），与界面主题分开存 */
  term: TermPref;

  /** 用户的侧栏偏好（持久化） */
  sidebarOpen: boolean;
  /** 窄屏临时隐藏，不写回偏好——不然开一次窄窗口就把用户的设置改了 */
  sidebarAutoHidden: boolean;
  /** 用户的右侧栏偏好（持久化）。默认关：第一次打开不该把终端挤窄 */
  rightOpen: boolean;
  /** 右侧打开的是哪一格。现在只有 git */
  rightPanel: RightPanelId;
  collapsed: Record<string, boolean>;
  /** 源项目 HEAD，按 projectId；附属项目用自己的 worktree.branch */
  heads: Record<string, ProjectHead>;
  /** 侧栏当前选中的项目；右侧只显示它下面的终端。null = 在总览 */
  selectedProjectId: string | null;

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
  projectForm: { edit: Project | null; preset?: ProjectFormPreset } | null;
  /**
   * 远端主机表单。onSaved 给「从新建项目里顺手加一台」用：
   * 存完之后把新主机选进项目表单，不用用户再点一次下拉框。
   */
  hostForm: { edit: SshHost | null; onSaved?: (host: SshHost) => void } | null;
  /** 正在从哪个源项目派生附属项目；null = 关闭 */
  worktreeFor: string | null;
  settingsOpen: boolean;
  settingsTab: SettingsTab;

  init(): Promise<void>;
  refreshAuth(): Promise<void>;
  refreshProjects(): Promise<void>;
  refreshHosts(): Promise<void>;
  refreshSessions(): Promise<void>;

  openSession(sessionId: string): void;
  /** 手动关 tab：Terminate，顺手结束会话，首次会解释这件事 */
  closeTab(id: string): Promise<void>;
  /** Detach：只收起 tab，会话留在后台继续跑（Shift+关闭） */
  detachTab(id: string): void;
  /** 只把 tab 摘掉，不碰会话、不做任何引导——清除已丢失记录这类场景用它 */
  dropTab(id: string): void;
  selectProject(projectId: string): void;
  showOverview(): void;
  focusTabAt(index: number): void;
  cycleTab(delta: number): void;

  setTheme(pref: ThemePref): void;
  setTerm(patch: Partial<TermPref>): void;
  resetTerm(): void;
  toggleSidebar(): void;
  setSidebarAutoHidden(hidden: boolean): void;
  /** 点同一格再关；点另一格则切过去。现在只有 git */
  toggleRightPanel(id?: RightPanelId): void;
  toggleCollapsed(key: string): void;
  setFilter(filter: OverviewFilter): void;
  setProjectFilter(projectId: string | null): void;
  toggleSelected(id: string): void;
  setSelected(ids: string[]): void;
  openProjectForm(edit: Project | null, preset?: ProjectFormPreset): void;
  closeProjectForm(): void;
  openHostForm(edit: SshHost | null, onSaved?: (host: SshHost) => void): void;
  closeHostForm(): void;
  openWorktreeForm(sourceProjectId: string): void;
  closeWorktreeForm(): void;
  openSettings(tab?: SettingsTab): void;
  closeSettings(): void;
  setSettingsTab(tab: SettingsTab): void;

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
    const { tabs, active, sidebarOpen, rightOpen, rightPanel, collapsed, selectedProjectId } =
      get();
    const payload: PersistedWorkspace = {
      tabs: tabs.filter((t) => !isPendingId(t)),
      active:
        active.kind === "terminal" && isPendingId(active.sessionId)
          ? selectedProjectId
            ? { kind: "project" }
            : { kind: "overview" }
          : active,
      sidebarOpen,
      rightOpen,
      rightPanel,
      collapsed,
      selectedProjectId,
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
    hosts: [],
    sessions: [],

    tabs: initialWorkspace.tabs,
    active: initialWorkspace.active,
    pending: [],

    themePref: initialThemePref,
    theme: resolveTheme(initialThemePref),
    term: initialTermPref,

    sidebarOpen: initialWorkspace.sidebarOpen,
    sidebarAutoHidden: false,
    rightOpen: initialWorkspace.rightOpen,
    rightPanel: initialWorkspace.rightPanel,
    collapsed: initialWorkspace.collapsed,
    heads: {},
    selectedProjectId: initialWorkspace.selectedProjectId,

    overviewFilter: "all",
    overviewProject: null,
    selected: [],
    projectForm: null,
    hostForm: null,
    worktreeFor: null,
    settingsOpen: false,
    settingsTab: "appearance",

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
          get().refreshHosts(),
          get().refreshSessions(),
        ]);
        set({ system });
        const selected = get().selectedProjectId;
        if (selected && get().projects.some((p) => p.id === selected)) {
          get().selectProject(selected);
        } else if (selected) {
          set({ selectedProjectId: null });
        }
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
        const projects = await api.listProjects();
        const selected = get().selectedProjectId;
        const still = selected != null && projects.some((p) => p.id === selected);
        set({
          projects,
          selectedProjectId: still ? selected : null,
          active:
            !still && get().active.kind === "project"
              ? { kind: "overview" }
              : get().active,
        });
        void refreshHeads(set, get, projects);
      } catch (err) {
        get().handleApiError(err);
      }
    },

    async refreshHosts() {
      try {
        set({ hosts: await api.listHosts() });
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
              ? state.selectedProjectId
                ? { kind: "project" }
                : { kind: "overview" }
              : state.active,
          selected: state.selected.filter((id) => alive.has(id)),
        }));
        persist();
      } catch (err) {
        get().handleApiError(err);
      }
    },

    openSession(sessionId) {
      set((state) => {
        const projectId =
          tabProjectId(sessionId, state.sessions, state.pending) ?? state.selectedProjectId;
        return {
          tabs: state.tabs.includes(sessionId) ? state.tabs : [...state.tabs, sessionId],
          active: { kind: "terminal" as const, sessionId },
          selectedProjectId: projectId ?? state.selectedProjectId,
          paletteOpen: false,
          menu: null,
        };
      });
      persist();
    },

    selectProject(projectId) {
      const state = get();
      if (!state.projects.some((p) => p.id === projectId)) return;
      const extra = state.sessions
        .filter((s) => s.projectId === projectId && !state.tabs.includes(s.id))
        .sort((a, b) => a.lastActiveAt - b.lastActiveAt)
        .map((s) => s.id);
      const tabs = [...state.tabs, ...extra];
      const mine = tabs.filter(
        (id) => tabProjectId(id, state.sessions, state.pending) === projectId
      );
      let active: ActiveView;
      if (state.active.kind === "terminal" && mine.includes(state.active.sessionId)) {
        active = state.active;
      } else {
        const pendingMine = state.pending.filter((p) => p.projectId === projectId);
        const newest = state.sessions
          .filter((s) => s.projectId === projectId)
          .sort((a, b) => b.lastActiveAt - a.lastActiveAt)[0];
        const pendingLast = pendingMine[pendingMine.length - 1];
        active = pendingLast
          ? { kind: "terminal", sessionId: pendingLast.id }
          : newest
            ? { kind: "terminal", sessionId: newest.id }
            : { kind: "project" };
      }
      set({
        selectedProjectId: projectId,
        tabs,
        active,
        menu: null,
        paletteOpen: false,
      });
      persist();
    },

    dropTab(id) {
      set((state) => {
        const tabs = state.tabs.filter((t) => t !== id);
        const pending = state.pending.filter((p) => p.id !== id);
        let active = state.active;
        if (active.kind === "terminal" && active.sessionId === id) {
          const rest = visibleTabs({
            ...state,
            tabs,
            pending,
          });
          active = rest.length
            ? { kind: "terminal", sessionId: rest[rest.length - 1]! }
            : state.selectedProjectId
              ? { kind: "project" }
              : { kind: "overview" };
        }
        return { tabs, active, pending };
      });
      persist();
    },

    /**
     * 手动关 tab 直接结束会话——tab 就是会话，收起它等于不要它了。
     * 想留着后台跑的用 detachTab（Shift+关闭）。
     *
     * 不弹确认：确认框挡在每一次关 tab 前面就成了噪音，用户会闭眼点。
     * 代价是用一条事后 toast + 一次性说明来兜住"我不知道会杀掉"。
     */
    async closeTab(id) {
      const session = isPendingId(id) ? undefined : get().sessions.find((s) => s.id === id);
      get().dropTab(id);

      // pending 还没有后端 id；dead 会话没什么可杀的，记录留着等用户自己清
      if (!session || session.state === "dead") return;

      try {
        await api.terminateSession(session.id);
        get().toast({
          kind: "danger",
          title: i18n.t("toast.terminated", { name: session.name }),
          ...closeKillsHint(),
        });
      } catch (err) {
        get().handleApiError(err);
        get().toast({
          kind: "danger",
          title: i18n.t("toast.failed"),
          body: (err as Error).message,
        });
      }
      await get().refreshSessions();
    },

    detachTab(id) {
      const session = isPendingId(id) ? undefined : get().sessions.find((s) => s.id === id);
      get().dropTab(id);
      if (!session || session.state === "dead") return;
      get().toast({
        kind: "info",
        title: i18n.t("toast.detachTitle", { name: session.name }),
        body: i18n.t("toast.detachBody"),
      });
    },

    showOverview() {
      set({
        active: { kind: "overview" },
        selectedProjectId: null,
        paletteOpen: false,
        menu: null,
      });
      persist();
    },

    focusTabAt(index) {
      const tabs = visibleTabs(get());
      const id = tabs[index];
      if (id) {
        set({ active: { kind: "terminal", sessionId: id } });
        persist();
      }
    },

    cycleTab(delta) {
      const state = get();
      const tabs = visibleTabs(state);
      if (tabs.length === 0) return;
      const current =
        state.active.kind === "terminal" ? tabs.indexOf(state.active.sessionId) : -1;
      const next = (current + delta + tabs.length * 2) % tabs.length;
      set({ active: { kind: "terminal", sessionId: tabs[next]! } });
      persist();
    },

    /** 主题偏好单独存一个 key：换主题不该把工作区布局也写回去一遍 */
    setTheme(pref) {
      const mode = resolveTheme(pref);
      saveThemePref(pref);
      applyTheme(mode);
      set({ themePref: pref, theme: mode, menu: null, paletteOpen: false });
    },

    setTerm(patch) {
      const term = sanitizeTermPref({ ...get().term, ...patch });
      saveTermPref(term);
      set({ term });
    },

    resetTerm() {
      saveTermPref(DEFAULT_TERM_PREF);
      set({ term: { ...DEFAULT_TERM_PREF } });
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

    toggleRightPanel(id = "git") {
      const { rightOpen, rightPanel } = get();
      if (rightOpen && rightPanel === id) {
        set({ rightOpen: false });
      } else {
        set({ rightOpen: true, rightPanel: id });
      }
      persist();
    },

    toggleCollapsed(key) {
      set((s) => ({
        collapsed: { ...s.collapsed, [key]: !s.collapsed[key] },
      }));
      persist();
    },

    setFilter(filter) {
      set({ overviewFilter: filter, selected: [] });
    },

    setProjectFilter(projectId) {
      set({ overviewProject: projectId, selected: [] });
    },

    openProjectForm(edit, preset) {
      set({ projectForm: { edit, preset }, menu: null, paletteOpen: false });
    },
    closeProjectForm() {
      set({ projectForm: null });
    },
    openHostForm(edit, onSaved) {
      set({ hostForm: { edit, onSaved }, menu: null, paletteOpen: false });
    },
    closeHostForm() {
      set({ hostForm: null });
    },
    openWorktreeForm(sourceProjectId) {
      set({ worktreeFor: sourceProjectId, menu: null, paletteOpen: false });
    },
    closeWorktreeForm() {
      set({ worktreeFor: null });
    },
    openSettings(tab) {
      set({
        settingsOpen: true,
        settingsTab: tab ?? get().settingsTab,
        menu: null,
        paletteOpen: false,
      });
    },
    closeSettings() {
      set({ settingsOpen: false });
    },
    setSettingsTab(tab) {
      set({ settingsTab: tab });
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
        selectedProjectId: projectId,
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

// index.html 的内联脚本已经按同一份偏好写过 class 了，这里再落一次是为了兜住
// 那段脚本读不到 localStorage 的情况——两边算出来的结果必须一致。
applyTheme(useApp.getState().theme);

// 跟随系统时才响应；选定了浅色/深色的用户不该因为系统入夜就被换掉主题
watchSystemTheme((mode) => {
  const { themePref, theme } = useApp.getState();
  if (themePref !== "system" || theme === mode) return;
  applyTheme(mode);
  useApp.setState({ theme: mode });
});
