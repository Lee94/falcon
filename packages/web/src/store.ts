import { create } from "zustand";
import type {
  AuthStatus,
  DeadReason,
  GitFileChange,
  Project,
  ProjectType,
  SessionForeground,
  SessionState,
  SessionWithProject,
  SshHost,
  SystemInfo,
} from "@falcon/shared";
import { toast as sonner } from "sonner";
import { api, ApiRequestError } from "./api.js";
import i18n from "./i18n.js";
import { applyThemeToDom, systemPrefersDark, watchSystemTheme } from "./lib/theme/apply.js";
import { findTheme, loadCatalog } from "./lib/theme/catalog.js";
import { deriveTheme, type ResolvedTheme } from "./lib/theme/derive.js";
import {
  DEFAULT_THEME_SETTINGS,
  choiceOf,
  loadThemeSettings,
  resolveThemeMode,
  saveThemeSettings,
  type ThemeChoice,
  type ThemeMode,
  type ThemePref,
  type ThemeSettings,
} from "./lib/theme/pref.js";
import {
  DEFAULT_TERM_PREF,
  loadTermPref,
  saveTermPref,
  sanitizeTermPref,
  type TermPref,
} from "./lib/term.js";
import { PANEL_WIDTH_DEFAULT, clampPanelWidth, parsePanelWidth } from "./lib/panelWidth.js";
import {
  DIFF_KEY,
  applyVisibleOrder,
  defaultStripKeys,
  fileKey,
  insertAfter,
  orderedStripKeys,
  parseStripKey,
  replaceKey,
  termKey,
  weaveOrder,
  type StripFile,
} from "./lib/tabStrip.js";

export type ActiveView =
  | { kind: "overview" }
  | { kind: "terminal"; sessionId: string }
  /** 选中了项目但还没有可显示的终端 */
  | { kind: "project" }
  /** Git 面板点开的文件差异（见 diffTab） */
  | { kind: "diff" }
  /** 文件查看 tab（见 fileTabs）；path 用来区分同时打开的多个文件 */
  | { kind: "file"; projectId: string; path: string };

/**
 * 差异查看 tab 的目标。单例：再点别的文件就地替换内容，像编辑器的预览 tab——
 * 每个文件各开一个 tab 只会让人在一排 tab 里找不到终端。不持久化：刷新后
 * 工作区的 diff 早就变了，恢复一个过期视图没有意义。
 */
export interface DiffTabTarget {
  projectId: string;
  file: GitFileChange;
  /**
   * 从 History 的提交详情点进来时带上这条提交，diff 就取那一次改动；
   * 不带就是「修改」面板点进来的，看工作区现状。
   */
  commit?: { sha: string; short: string; subject: string };
  /** 多仓库项目：diff 属于哪个成员仓库（成员 dir 原文），随面板的成员选择带过来 */
  repo?: string;
}

export function tabProjectId(
  tabId: string,
  sessions: SessionWithProject[],
  pending: PendingSession[]
): string | undefined {
  if (isPendingId(tabId)) return pending.find((p) => p.id === tabId)?.projectId;
  return sessions.find((s) => s.id === tabId)?.projectId;
}

/**
 * 文件查看 tab 的目标。不持久化：刷新后工作区的文件可能已经变了。
 *
 * 与 diffTab 不同——差异仍是单例预览，文件可以同时开多个（⌘P / 文件面板点开
 * 都是新增 tab；同一文件再点一次只是聚焦）。关掉只是收起视图，不碰会话。
 */
export interface FileTabTarget {
  projectId: string;
  /** 工作目录相对路径，一律 `/` 分隔（见 WorkspaceEntry） */
  path: string;
}

export function sameFile(a: FileTabTarget, b: FileTabTarget): boolean {
  return a.projectId === b.projectId && a.path === b.path;
}

export function visibleFileTabs(s: {
  fileTabs: FileTabTarget[];
  selectedProjectId: string | null;
}): FileTabTarget[] {
  if (!s.selectedProjectId) return s.fileTabs;
  return s.fileTabs.filter((f) => f.projectId === s.selectedProjectId);
}

/** 右侧栏打开的是哪一格。加面板时在这里加一个 id，持久化形状不用改。 */
export type RightPanelId = "git" | "changes" | "forward" | "files";

/** 持久化的布局可能来自旧版本，认不出的面板名一律退回默认 */
function isRightPanelId(v: unknown): v is RightPanelId {
  return v === "git" || v === "changes" || v === "forward" || v === "files";
}

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

/**
 * 关掉一个不代表会话的 tab（diff / file）之后落到哪。
 * 顺序：最近的可见终端 → 还开着的文件 tab → 差异 tab → 项目空页 → 总览。
 */
function fallbackActive(s: {
  tabs: string[];
  sessions: SessionWithProject[];
  pending: PendingSession[];
  selectedProjectId: string | null;
  fileTabs: FileTabTarget[];
  diffTab: DiffTabTarget | null;
}): ActiveView {
  const rest = visibleTabs(s);
  if (rest.length) return { kind: "terminal", sessionId: rest[rest.length - 1]! };
  const files = visibleFileTabs(s);
  if (files.length) {
    const f = files[files.length - 1]!;
    return { kind: "file", projectId: f.projectId, path: f.path };
  }
  if (s.diffTab && (!s.selectedProjectId || s.diffTab.projectId === s.selectedProjectId)) {
    return { kind: "diff" };
  }
  return s.selectedProjectId ? { kind: "project" } : { kind: "overview" };
}

function sameActive(a: ActiveView, b: ActiveView): boolean {
  if (a.kind !== b.kind) return false;
  if (a.kind === "terminal" && b.kind === "terminal") return a.sessionId === b.sessionId;
  if (a.kind === "file" && b.kind === "file") return a.projectId === b.projectId && a.path === b.path;
  return true;
}

function stripSource(s: {
  tabs: string[];
  sessions: SessionWithProject[];
  pending: PendingSession[];
  selectedProjectId: string | null;
  fileTabs: FileTabTarget[];
  diffTab: DiffTabTarget | null;
}) {
  return {
    terminalIds: visibleTabs(s),
    files: visibleFileTabs(s) as StripFile[],
    hasDiff: !!(
      s.diffTab &&
      (!s.selectedProjectId || s.diffTab.projectId === s.selectedProjectId)
    ),
  };
}

/** 主区 tab 栏从左到右的可见 key。记住的拖拽顺序优先，新开的接到末尾。 */
export function visibleStripKeys(s: {
  tabs: string[];
  sessions: SessionWithProject[];
  pending: PendingSession[];
  selectedProjectId: string | null;
  fileTabs: FileTabTarget[];
  diffTab: DiffTabTarget | null;
  tabOrder: string[];
}): string[] {
  return orderedStripKeys(s.tabOrder, defaultStripKeys(stripSource(s)));
}

/** 切 tab 快捷键按 strip 顺序走，跟拖拽之后看到的一样 */
function viewTabs(s: {
  tabs: string[];
  sessions: SessionWithProject[];
  pending: PendingSession[];
  selectedProjectId: string | null;
  fileTabs: FileTabTarget[];
  diffTab: DiffTabTarget | null;
  tabOrder: string[];
}): ActiveView[] {
  const views: ActiveView[] = [];
  for (const key of visibleStripKeys(s)) {
    const item = parseStripKey(key);
    if (!item) continue;
    if (item.kind === "terminal") views.push({ kind: "terminal", sessionId: item.id });
    else if (item.kind === "file") {
      views.push({ kind: "file", projectId: item.projectId, path: item.path });
    } else views.push({ kind: "diff" });
  }
  return views;
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
 * 右侧各面板跟谁走：侧栏选中的项目优先，否则当前 tab 所属项目。
 * 总览且没选项目时为 null——不要退回第一个项目，免得打开面板看到别人的仓库。
 *
 * 两个查看 tab（diff / file）也要认：从文件面板点开一个文件，active 就从
 * terminal 变成 file，这时若不看 file tab 的 projectId，面板会当场塌回
 * "选一个项目"——用户刚从那棵树里点的文件。
 */
export function selectFocusProjectId(s: {
  selectedProjectId: string | null;
  active: ActiveView;
  sessions: SessionWithProject[];
  pending: PendingSession[];
  diffTab: DiffTabTarget | null;
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
  if (s.active.kind === "file") return s.active.projectId;
  if (s.active.kind === "diff") return s.diffTab?.projectId ?? null;
  return null;
}

/**
 * 多仓库项目在 Git / 修改面板里当前看的成员仓库；没选过（或选的成员已被
 * 编辑掉）就退回第一个成员。非多仓库项目返回 undefined——调用方原样把它
 * 传给 api 的 repo 参数即可，单仓库路径不受影响。
 */
export function selectMultiRepoDir(
  s: { multiRepo: Record<string, string> },
  project: Project | null | undefined
): string | undefined {
  if (!project?.multi) return undefined;
  const picked = s.multiRepo[project.id];
  if (picked && project.multi.repos.some((m) => m.dir === picked)) return picked;
  return project.multi.repos[0]?.dir;
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
  /** 无选区时的「复制」这类：看得见，点不了 */
  disabled?: boolean;
  onSelect: () => void;
}

export interface MenuSpec {
  x: number;
  y: number;
  /** 右键菜单从指针展开（start）；按钮菜单贴着触发器右缘（end，默认） */
  align?: "start" | "end";
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

export interface AskpassSpec {
  id: string;
  prompt: string;
}

export interface InstallSpec {
  projectId: string;
  /** 安装流程结束后是否继续创建会话（从「新建终端」进来时为 true） */
  thenCreate: boolean;
}

/** 从某台服务器新建项目时预填类型 / 主机，编辑已有项目时不用 */
export interface ProjectFormPreset {
  type?: ProjectType;
  hostId?: string;
  /** 直接进多仓库表单（位置随 type/hostId 定死），侧栏主机菜单的「新建多仓库项目」用 */
  multi?: boolean;
}

/** 源项目工作区当前 HEAD，侧栏在没有 worktree 时用分支名代表默认仓库 */
export interface ProjectHead {
  branch?: string;
  sha?: string;
}

/** 侧栏最后一层的 +N −M。只给脏工作区留条目，干净的不占位置 */
export interface ProjectChanges {
  added: number;
  deleted: number;
}

const WORKSPACE_KEY = "falcon.workspace";
const WORKSPACE_KEY_LEGACY = "mojito.workspace";
const CLOSE_KILLS_KEY = "falcon.closeKillsEducated";
const CLOSE_KILLS_KEY_LEGACY = "mojito.closeKillsEducated";
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
  /** 侧栏是否显示已存档的附属项目。默认藏起来，存档就是为了少占地方 */
  showArchived: boolean;
  sidebarWidth: number;
  rightWidth: number;
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
    showArchived: false,
    sidebarWidth: PANEL_WIDTH_DEFAULT,
    rightWidth: PANEL_WIDTH_DEFAULT,
  };
  try {
    const raw = localStorage.getItem(WORKSPACE_KEY) ?? localStorage.getItem(WORKSPACE_KEY_LEGACY);
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
      rightPanel: isRightPanelId(parsed.rightPanel) ? parsed.rightPanel : "git",
      collapsed: parsed.collapsed ?? {},
      selectedProjectId:
        typeof parsed.selectedProjectId === "string" ? parsed.selectedProjectId : null,
      showArchived: parsed.showArchived === true,
      sidebarWidth: parsePanelWidth(parsed.sidebarWidth),
      rightWidth: parsePanelWidth(parsed.rightWidth),
    };
  } catch {
    return fallback;
  }
}

const initialWorkspace = loadWorkspace();
const initialThemes = loadThemeSettings(safeStorage());
const initialTermPref = loadTermPref();

/** 沙箱 iframe 里连 `localStorage` 这个属性读取都会抛 SecurityError */
function safeStorage(): Storage | undefined {
  try {
    return globalThis.localStorage;
  } catch {
    return undefined;
  }
}

/**
 * 主题槽位 → 派生结果，按槽位对象引用缓存：TerminalView 把 activeTheme.xterm 交给
 * 适配器，rio 那边按引用比对判断要不要重建渲染器，同一套主题必须给同一个对象。
 */
const resolvedThemes = new WeakMap<ThemeChoice, ResolvedTheme>();
function resolveChoice(choice: ThemeChoice): ResolvedTheme {
  let resolved = resolvedThemes.get(choice);
  if (!resolved) {
    resolved = deriveTheme(choice.colors);
    resolvedThemes.set(choice, resolved);
  }
  return resolved;
}

let pendingSeq = 0;
/** 本次页面加载内只解释一次"关 tab 会结束会话"，"不再提示"才写 localStorage */
let closeKillsToastShown = false;

let changesInFlight = false;

/**
 * 轮询刷新的常态是"没变"。无条件 set 新引用会让所有订阅该字段的组件
 * （每个 TerminalView、TabBar、整棵侧栏）每个周期白白重渲一遍，
 * 这里比对内容，真有变化才换引用。只适用于平坦对象（API 返回的 JSON 行）。
 */
function shallowEqualFlat(a: object, b: object): boolean {
  if (a === b) return true;
  const ka = Object.keys(a) as (keyof typeof a)[];
  const kb = Object.keys(b);
  if (ka.length !== kb.length) return false;
  for (const k of ka) {
    if (!Object.is(a[k], (b as typeof a)[k])) return false;
  }
  return true;
}

function sameFlatArray<T extends object>(a: readonly T[], b: readonly T[]): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    if (!shallowEqualFlat(a[i], b[i])) return false;
  }
  return true;
}

function sameFlatRecord<T extends object>(
  a: Record<string, T>,
  b: Record<string, T>
): boolean {
  const ka = Object.keys(a);
  if (ka.length !== Object.keys(b).length) return false;
  for (const k of ka) {
    if (!(k in b) || !shallowEqualFlat(a[k], b[k])) return false;
  }
  return true;
}

/**
 * 侧栏最后一层的文件计数。附属项目也要问——它们各有自己的工作区。
 * 探不到就沿用上次的数：SSH 抖一下不该让徽标闪没。
 */
async function refreshChanges(
  set: (partial: { changes: Record<string, ProjectChanges> }) => void,
  get: () => { changes: Record<string, ProjectChanges> },
  projects: Project[]
) {
  if (changesInFlight) return;
  changesInFlight = true;
  try {
    // 存档的附属项目不轮询：默认看不见，也不该为它跑 git status（SSH 上还是往返）。
    // 多仓库项目（容器与派生行）也不轮询——刻意降级：一个 id 对 N 个仓库，
    // 求和徽标说不清是哪个仓库脏，误导大于信息；按 (id, repo) 键控留给将来
    const targets = projects.filter((p) => p.workingDir && !p.worktree?.archivedAt && !p.multi);
    const next: Record<string, ProjectChanges> = {};
    try {
      // 一个批量请求代替 N 个并发 GET：服务端按宿主机分组，同主机一次 exec 拿全
      const counts = await api.gitChangesBatch(targets.map((p) => p.id));
      for (const p of targets) {
        const c = counts[p.id];
        if (c?.available && (c.added > 0 || c.deleted > 0)) {
          next[p.id] = { added: c.added, deleted: c.deleted };
        }
      }
    } catch {
      // 整个请求失败（网络抖动）沿用上次的数：徽标不该因为一次超时闪没
      for (const p of targets) {
        const prev = get().changes[p.id];
        if (prev) next[p.id] = prev;
      }
    }
    const alive = new Set(projects.map((p) => p.id));
    for (const id of Object.keys(next)) {
      if (!alive.has(id)) delete next[id];
    }
    if (!sameFlatRecord(get().changes, next)) set({ changes: next });
  } finally {
    changesInFlight = false;
  }
}

/**
 * 侧栏用分支名代表默认仓库，但 list projects 不跑 git。
 * 只探源项目：附属项目的分支写在 worktree.branch 里，不必再问一次。
 */
async function refreshHeads(
  set: (partial: { heads: Record<string, ProjectHead> }) => void,
  get: () => { heads: Record<string, ProjectHead> },
  projects: Project[]
) {
  // 多仓库容器没有单一 HEAD（/repo 对它 400），侧栏用「N 个仓库」代替分支名
  const sources = projects.filter((p) => !p.worktree && !p.multi && p.workingDir);
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
  if (!sameFlatRecord(get().heads, next)) set({ heads: next });
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
    educated =
      localStorage.getItem(CLOSE_KILLS_KEY) === "1" ||
      localStorage.getItem(CLOSE_KILLS_KEY_LEGACY) === "1";
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
  /**
   * 整条 tab 栏的混排顺序（终端 / 文件 / 差异的 strip key）。
   * 不持久化：刷新后面板 tab 本来就会消失，终端顺序已经在 tabs 里。
   */
  tabOrder: string[];
  active: ActiveView;
  pending: PendingSession[];
  /** 差异查看 tab；null = 没开 */
  diffTab: DiffTabTarget | null;
  /** 已打开的文件查看 tab，从左到右 */
  fileTabs: FileTabTarget[];

  /** 明暗模式偏好（持久化）与它此刻实际解析成的槽位 */
  themePref: ThemePref;
  themeMode: ThemeMode;
  /** 浅色 / 深色槽位各放一套主题（持久化，含颜色副本） */
  themes: Pick<ThemeSettings, "light" | "dark">;
  /**
   * 此刻整个应用（界面 + 终端）用的主题：正常是 themes[themeMode] 的派生结果；
   * 主题选择器里高亮某一项时临时换成预览的那套，关掉选择器就复原
   */
  activeTheme: ResolvedTheme;
  /** 终端画面偏好（字体 / 字号 / 光标 / 引擎），配色不在这里 */
  term: TermPref;

  /** 用户的侧栏偏好（持久化） */
  sidebarOpen: boolean;
  /** 窄屏临时隐藏，不写回偏好——不然开一次窄窗口就把用户的设置改了 */
  sidebarAutoHidden: boolean;
  /** 左右栏宽度（持久化）。拖的时候只改内存，松手才写回 */
  sidebarWidth: number;
  rightWidth: number;
  /** 用户的右侧栏偏好（持久化）。默认关：第一次打开不该把终端挤窄 */
  rightOpen: boolean;
  /** 右侧打开的是哪一格 */
  rightPanel: RightPanelId;
  collapsed: Record<string, boolean>;
  /** 侧栏是否显示已存档的附属项目（持久化） */
  showArchived: boolean;
  /** 源项目 HEAD，按 projectId；附属项目用自己的 worktree.branch */
  heads: Record<string, ProjectHead>;
  /** 工作区文件计数，按 projectId；干净的项目不在里面 */
  changes: Record<string, ProjectChanges>;
  /**
   * 多仓库项目在 Git / 修改面板里当前看的成员（projectId → 成员 dir）。
   * 内存态不持久化；放 store 而不放组件本地，是因为两个面板 + diff tab
   * 必须看同一个成员，各存一份必然漂移。
   */
  multiRepo: Record<string, string>;
  /** 侧栏当前选中的项目；右侧只显示它下面的终端。null = 在总览 */
  selectedProjectId: string | null;

  overviewFilter: OverviewFilter;
  /** 总览按项目筛选；null = 全部项目 */
  overviewProject: string | null;
  selected: string[];

  menu: MenuSpec | null;
  confirm: ConfirmSpec | null;
  /** sudo askpass 排队；对话框只展示队头 */
  askpass: AskpassSpec[];
  /** 就地重命名的会话 id，替代 prompt() */
  renameFor: string | null;
  paletteOpen: boolean;
  /** ⌘P 文件搜索，与命令面板互斥 */
  quickOpen: boolean;
  install: InstallSpec | null;
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
  /** 侧栏打开时轮询工作区 +N −M */
  refreshChanges(): Promise<void>;
  /** WS 推过来的状态立刻写进列表，不等 5s 轮询——否则接回后仍显示「待接回」 */
  applySessionState(id: string, state: SessionState, deadReason?: DeadReason): void;

  openSession(sessionId: string): void;
  /** 在差异 tab 里打开一个文件（就地替换上一个）。带 commit 则看那次提交的改动 */
  openDiff(
    projectId: string,
    file: GitFileChange,
    commit?: DiffTabTarget["commit"],
    repo?: string
  ): void;
  /** 多仓库项目：切换 Git / 修改面板正在看的成员仓库 */
  setMultiRepo(projectId: string, dir: string): void;
  /** 切回已开的差异 tab */
  showDiff(): void;
  closeDiff(): void;
  /** 打开工作目录里的一个文件：已开则聚焦，否则新增 tab */
  openFile(projectId: string, path: string): void;
  /** 关掉一个文件 tab；不传则关当前正在看的那个 */
  closeFile(target?: FileTabTarget): void;
  /** 手动关 tab：Terminate，顺手结束会话，首次会解释这件事 */
  closeTab(id: string): Promise<void>;
  /** Detach：只收起 tab，会话留在后台继续跑（Shift+关闭） */
  detachTab(id: string): void;
  /** 只把 tab 摘掉，不碰会话、不做任何引导——清除已丢失记录这类场景用它 */
  dropTab(id: string): void;
  /** 拖拽松手：把可见 strip 的 from 挪到 to，隐藏项目的 tab 原地不动 */
  moveStrip(from: number, to: number): void;
  /**
   * 关掉一组 strip key（关闭其他 / 左 / 右）。文件和差异立刻收起；
   * 终端走 Terminate。有前台程序在跑时合成一次确认，避免连弹。
   */
  closeStripKeys(keys: string[]): Promise<void>;
  selectProject(projectId: string): void;
  showOverview(): void;
  focusTabAt(index: number): void;
  cycleTab(delta: number): void;

  setTheme(pref: ThemePref): void;
  /** 给某个槽位换主题，立刻生效并持久化 */
  setThemeChoice(slot: ThemeMode, choice: ThemeChoice): void;
  /** 临时把整个应用换成某套主题看效果；null 复原。不持久化 */
  previewTheme(choice: ThemeChoice | null): void;
  resetThemes(): void;
  setTerm(patch: Partial<TermPref>): void;
  resetTerm(): void;
  toggleSidebar(): void;
  setSidebarAutoHidden(hidden: boolean): void;
  setSidebarWidth(width: number): void;
  setRightWidth(width: number): void;
  /** 松手 / 键盘调完宽度之后才落盘，拖的途中不要同步写 localStorage */
  persistLayout(): void;
  /** 点同一格再关；点另一格则切过去 */
  toggleRightPanel(id?: RightPanelId): void;
  toggleCollapsed(key: string): void;
  toggleShowArchived(): void;
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
  pushAskpass(spec: AskpassSpec): void;
  shiftAskpass(): void;
  openRename(sessionId: string): void;
  closeRename(): void;
  setPalette(open: boolean): void;
  setQuickOpen(open: boolean): void;
  openInstall(spec: InstallSpec): void;
  closeInstall(): void;

  newTerminal(projectId: string, afterKey?: string): Promise<void>;
  createSessionNow(projectId: string, afterKey?: string): Promise<void>;
  retryPending(pendingId: string): Promise<void>;

  handleApiError(err: unknown): void;
}

let lastPersistedWorkspace = "";

export const useApp = create<AppState>((set, get) => {
  /** tabs / active / 侧栏状态写回 localStorage —— 刷新页面后工作台原样恢复 */
  const persist = () => {
    const {
      tabs,
      active,
      sidebarOpen,
      rightOpen,
      rightPanel,
      collapsed,
      selectedProjectId,
      showArchived,
      sidebarWidth,
      rightWidth,
    } = get();
    const payload: PersistedWorkspace = {
      tabs: tabs.filter((t) => !isPendingId(t)),
      // pending id 与两个查看 tab（diff / file）都活不过刷新，落成项目 / 总览视图
      active:
        active.kind === "diff" ||
        active.kind === "file" ||
        (active.kind === "terminal" && isPendingId(active.sessionId))
          ? selectedProjectId
            ? { kind: "project" }
            : { kind: "overview" }
          : active,
      sidebarOpen,
      rightOpen,
      rightPanel,
      collapsed,
      selectedProjectId,
      showArchived,
      sidebarWidth,
      rightWidth,
    };
    const json = JSON.stringify(payload);
    // localStorage.setItem 是同步阻塞 API，轮询周期里内容多半没变，别白写
    if (json === lastPersistedWorkspace) return;
    lastPersistedWorkspace = json;
    try {
      localStorage.setItem(WORKSPACE_KEY, json);
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
    tabOrder: initialWorkspace.tabs.map(termKey),
    active: initialWorkspace.active,
    pending: [],
    diffTab: null,
    fileTabs: [],

    themePref: initialThemes.settings.mode,
    themeMode: resolveThemeMode(initialThemes.settings.mode, systemPrefersDark()),
    themes: { light: initialThemes.settings.light, dark: initialThemes.settings.dark },
    activeTheme: resolveChoice(
      initialThemes.settings[resolveThemeMode(initialThemes.settings.mode, systemPrefersDark())]
    ),
    term: initialTermPref,

    sidebarOpen: initialWorkspace.sidebarOpen,
    sidebarAutoHidden: false,
    sidebarWidth: initialWorkspace.sidebarWidth,
    rightWidth: initialWorkspace.rightWidth,
    rightOpen: initialWorkspace.rightOpen,
    rightPanel: initialWorkspace.rightPanel,
    collapsed: initialWorkspace.collapsed,
    showArchived: initialWorkspace.showArchived,
    heads: {},
    changes: {},
    multiRepo: {},
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
    askpass: [],
    renameFor: null,
    paletteOpen: false, quickOpen: false,
    install: null,

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
        // 被存档的项目不能保持选中：它已从侧栏消失，主区不能停在一个看不见的项目上
        const still =
          selected != null &&
          projects.some((p) => p.id === selected && !p.worktree?.archivedAt);
        set({
          projects,
          selectedProjectId: still ? selected : null,
          active:
            !still && get().active.kind === "project"
              ? { kind: "overview" }
              : get().active,
        });
        void refreshHeads(set, get, projects);
        void refreshChanges(set, get, projects);
      } catch (err) {
        get().handleApiError(err);
      }
    },

    async refreshChanges() {
      await refreshChanges(set, get, get().projects);
    },

    async refreshHosts() {
      try {
        set({ hosts: await api.listHosts() });
      } catch (err) {
        get().handleApiError(err);
      }
    },

    applySessionState(id, state, deadReason) {
      set((s) => ({
        sessions: s.sessions.map((x) =>
          x.id === id
            ? {
                ...x,
                state,
                deadReason: state === "dead" ? (deadReason ?? x.deadReason) : undefined,
              }
            : x
        ),
      }));
    },

    async refreshSessions() {
      try {
        const sessions = await api.listSessions();
        // 内容没变就整个跳过：sessions 没变则 alive 集合没变，tabs/active/selected
        // 在上一轮已经收敛，不会有需要清理的残留
        if (sameFlatArray(get().sessions, sessions)) return;
        const alive = new Set(sessions.map((s) => s.id));
        // dead 会话仍在列表里，因此恢复出来的 tab 不会被静默丢弃，只是显示为已丢失
        const keep = (id: string) => isPendingId(id) || alive.has(id);
        set((state) => ({
          sessions,
          tabs: state.tabs.filter(keep),
          tabOrder: state.tabOrder.filter((k) => {
            const item = parseStripKey(k);
            return item?.kind !== "terminal" || keep(item.id);
          }),
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
        const already = state.tabs.includes(sessionId);
        const key = termKey(sessionId);
        return {
          tabs: already ? state.tabs : [...state.tabs, sessionId],
          tabOrder:
            already || !state.tabOrder.length || state.tabOrder.includes(key)
              ? state.tabOrder
              : [...state.tabOrder, key],
          active: { kind: "terminal" as const, sessionId },
          selectedProjectId: projectId ?? state.selectedProjectId,
          paletteOpen: false, quickOpen: false,
          menu: null,
        };
      });
      persist();
    },

    openDiff(projectId, file, commit, repo) {
      set((state) => ({
        diffTab: { projectId, file, commit, repo },
        tabOrder:
          !state.tabOrder.length || state.tabOrder.includes(DIFF_KEY)
            ? state.tabOrder
            : [...state.tabOrder, DIFF_KEY],
        active: { kind: "diff" as const },
      }));
    },

    setMultiRepo(projectId, dir) {
      set((state) => ({ multiRepo: { ...state.multiRepo, [projectId]: dir } }));
    },

    showDiff() {
      if (get().diffTab) set({ active: { kind: "diff" } });
    },

    closeDiff() {
      set((state) => ({
        diffTab: null,
        tabOrder: state.tabOrder.filter((k) => k !== DIFF_KEY),
        ...(state.active.kind === "diff"
          ? { active: fallbackActive({ ...state, diffTab: null }) }
          : null),
      }));
    },

    openFile(projectId, path) {
      set((state) => {
        const next = { projectId, path };
        const exists = state.fileTabs.some((f) => sameFile(f, next));
        const key = fileKey(next);
        return {
          fileTabs: exists ? state.fileTabs : [...state.fileTabs, next],
          tabOrder:
            exists || !state.tabOrder.length || state.tabOrder.includes(key)
              ? state.tabOrder
              : [...state.tabOrder, key],
          active: { kind: "file" as const, projectId, path },
          paletteOpen: false,
          quickOpen: false,
          menu: null,
        };
      });
    },

    closeFile(target) {
      set((state) => {
        const closing =
          target ??
          (state.active.kind === "file"
            ? { projectId: state.active.projectId, path: state.active.path }
            : null);
        if (!closing) return state;
        const fileTabs = state.fileTabs.filter((f) => !sameFile(f, closing));
        const wasActive =
          state.active.kind === "file" && sameFile(state.active, closing);
        return {
          fileTabs,
          tabOrder: state.tabOrder.filter((k) => k !== fileKey(closing)),
          ...(wasActive ? { active: fallbackActive({ ...state, fileTabs }) } : null),
        };
      });
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
        paletteOpen: false, quickOpen: false,
      });
      persist();
    },

    dropTab(id) {
      set((state) => {
        const tabs = state.tabs.filter((t) => t !== id);
        const pending = state.pending.filter((p) => p.id !== id);
        const tabOrder = state.tabOrder.filter((k) => k !== termKey(id));
        let active = state.active;
        if (active.kind === "terminal" && active.sessionId === id) {
          active = fallbackActive({ ...state, tabs, pending });
        }
        return { tabs, tabOrder, active, pending };
      });
      persist();
    },

    moveStrip(from, to) {
      const state = get();
      const visible = visibleStripKeys(state);
      if (
        from === to ||
        from < 0 ||
        to < 0 ||
        from >= visible.length ||
        to >= visible.length
      ) {
        return;
      }
      const nextVisible = visible.slice();
      const [moved] = nextVisible.splice(from, 1);
      nextVisible.splice(to, 0, moved!);

      const visTerm = new Set(
        visible.flatMap((k) => {
          const item = parseStripKey(k);
          return item?.kind === "terminal" ? [item.id] : [];
        })
      );
      const newTerm = nextVisible.flatMap((k) => {
        const item = parseStripKey(k);
        return item?.kind === "terminal" ? [item.id] : [];
      });
      const visFile = new Set(
        visible.flatMap((k) => {
          const item = parseStripKey(k);
          return item?.kind === "file" ? [`${item.projectId}:${item.path}`] : [];
        })
      );
      const newFiles: FileTabTarget[] = nextVisible.flatMap((k) => {
        const item = parseStripKey(k);
        return item?.kind === "file" ? [{ projectId: item.projectId, path: item.path }] : [];
      });

      set({
        tabOrder: weaveOrder(
          state.tabOrder.length ? state.tabOrder : visible,
          visible,
          nextVisible
        ),
        tabs: applyVisibleOrder(state.tabs, newTerm, (id) => visTerm.has(id)),
        fileTabs: applyVisibleOrder(state.fileTabs, newFiles, (f) =>
          visFile.has(`${f.projectId}:${f.path}`)
        ),
      });
      persist();
    },

    async closeStripKeys(keys) {
      if (keys.length === 0) return;
      const state = get();
      const files: FileTabTarget[] = [];
      const termIds: string[] = [];
      let closeDiff = false;
      for (const key of keys) {
        const item = parseStripKey(key);
        if (!item) continue;
        if (item.kind === "file") files.push({ projectId: item.projectId, path: item.path });
        else if (item.kind === "diff") closeDiff = true;
        else termIds.push(item.id);
      }

      const dropOrIdle: string[] = [];
      const live: SessionWithProject[] = [];
      for (const id of termIds) {
        if (isPendingId(id)) {
          dropOrIdle.push(id);
          continue;
        }
        const session = state.sessions.find((s) => s.id === id);
        if (!session || session.state === "dead") dropOrIdle.push(id);
        else live.push(session);
      }

      const busy: { session: SessionWithProject; command: string }[] = [];
      const idleLive: SessionWithProject[] = [];
      await Promise.all(
        live.map(async (session) => {
          let fg: SessionForeground | null = null;
          try {
            fg = await Promise.race([
              api.sessionForeground(session.id),
              new Promise<null>((resolve) => setTimeout(() => resolve(null), 2000)),
            ]);
          } catch {
            // 侦测失败按空闲，跟单独关 tab 同一条保底
          }
          if (fg?.busy) busy.push({ session, command: fg.command ?? "" });
          else idleLive.push(session);
        })
      );

      const run = async (sessions: SessionWithProject[]) => {
        for (const file of files) get().closeFile(file);
        if (closeDiff) get().closeDiff();
        for (const id of dropOrIdle) get().dropTab(id);
        const names: string[] = [];
        await Promise.all(
          sessions.map(async (session) => {
            get().dropTab(session.id);
            try {
              await api.terminateSession(session.id);
              names.push(session.name);
            } catch (err) {
              get().handleApiError(err);
              get().toast({
                kind: "danger",
                title: i18n.t("toast.failed"),
                body: (err as Error).message,
              });
            }
          })
        );
        if (names.length === 1) {
          get().toast({
            kind: "danger",
            title: i18n.t("toast.terminated", { name: names[0] }),
            ...closeKillsHint(),
          });
        } else if (names.length > 1) {
          get().toast({
            kind: "danger",
            title: i18n.t("toast.terminatedMany", { n: names.length }),
            ...closeKillsHint(),
          });
        }
        await get().refreshSessions();
      };

      if (busy.length === 0) {
        await run(idleLive);
        return;
      }

      get().askConfirm({
        title:
          busy.length === 1
            ? i18n.t("tab.busyTitle", { name: busy[0]!.session.name })
            : i18n.t("tab.closeBusyManyTitle", { n: busy.length }),
        body:
          busy.length === 1
            ? i18n.t("tab.busyBody", { command: busy[0]!.command })
            : i18n.t("tab.closeBusyManyBody"),
        list:
          busy.length > 1
            ? busy.map((b) => ({
                name: b.session.name,
                state: b.session.state,
                meta: b.command,
              }))
            : undefined,
        footnote: i18n.t("tab.busyFootnote"),
        confirmLabel: i18n.t("tab.closeBusyManyConfirm"),
        onConfirm: () => run([...idleLive, ...busy.map((b) => b.session)]),
      });
    },

    /**
     * 手动关 tab 直接结束会话——tab 就是会话，收起它等于不要它了。
     * 想留着后台跑的用 detachTab（Shift+关闭）。
     *
     * 空闲时不弹确认：确认框挡在每一次关 tab 前面就成了噪音，用户会闭眼点。
     * 只在侦测到前台真有程序在跑时拦一道——这时杀掉的不再是一个空 shell。
     * 侦测失败或超时按空闲处理（保底就是旧的直接关），
     * "我不知道会杀掉"仍由事后 toast + 一次性说明兜住。
     */
    async closeTab(id) {
      const session = isPendingId(id) ? undefined : get().sessions.find((s) => s.id === id);

      // pending 还没有后端 id；dead 会话没什么可杀的，记录留着等用户自己清
      if (!session || session.state === "dead") {
        get().dropTab(id);
        return;
      }

      const terminate = async () => {
        get().dropTab(id);
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
      };

      // SSH 上探测要过一次网络往返；它卡住不能连累关 tab，超时按空闲
      let fg: SessionForeground | null = null;
      try {
        fg = await Promise.race([
          api.sessionForeground(session.id),
          new Promise<null>((resolve) => setTimeout(() => resolve(null), 2000)),
        ]);
      } catch {
        // 侦测是道保险，它自己坏了不挡关闭
      }

      if (!fg?.busy) {
        await terminate();
        return;
      }
      get().askConfirm({
        title: i18n.t("tab.busyTitle", { name: session.name }),
        body: i18n.t("tab.busyBody", { command: fg.command }),
        footnote: i18n.t("tab.busyFootnote"),
        confirmLabel: i18n.t("session.terminateConfirm"),
        onConfirm: terminate,
      });
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
        paletteOpen: false, quickOpen: false,
        menu: null,
      });
      persist();
    },

    focusTabAt(index) {
      const next = viewTabs(get())[index];
      if (next) {
        set({ active: next });
        persist();
      }
    },

    cycleTab(delta) {
      const state = get();
      const items = viewTabs(state);
      if (items.length === 0) return;
      const current = items.findIndex((v) => sameActive(v, state.active));
      const from = current < 0 ? (delta > 0 ? -1 : 0) : current;
      const next = items[(from + delta + items.length) % items.length]!;
      set({ active: next });
      persist();
    },

    /** 主题偏好单独存一个 key：换主题不该把工作区布局也写回去一遍 */
    setTheme(pref) {
      const mode = resolveThemeMode(pref, systemPrefersDark());
      const { themes } = get();
      saveThemeSettings(safeStorage(), { mode: pref, ...themes });
      const activeTheme = resolveChoice(themes[mode]);
      applyThemeToDom(activeTheme);
      set({ themePref: pref, themeMode: mode, activeTheme, menu: null, paletteOpen: false, quickOpen: false });
    },

    setThemeChoice(slot, choice) {
      const themes = { ...get().themes, [slot]: choice };
      saveThemeSettings(safeStorage(), { mode: get().themePref, ...themes });
      const activeTheme = resolveChoice(themes[get().themeMode]);
      applyThemeToDom(activeTheme);
      set({ themes, activeTheme });
    },

    previewTheme(choice) {
      const activeTheme = resolveChoice(choice ?? get().themes[get().themeMode]);
      if (activeTheme === get().activeTheme) return;
      applyThemeToDom(activeTheme);
      set({ activeTheme });
    },

    resetThemes() {
      const themes = { light: DEFAULT_THEME_SETTINGS.light, dark: DEFAULT_THEME_SETTINGS.dark };
      saveThemeSettings(safeStorage(), { mode: get().themePref, ...themes });
      const activeTheme = resolveChoice(themes[get().themeMode]);
      applyThemeToDom(activeTheme);
      set({ themes, activeTheme });
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

    setSidebarWidth(width) {
      const next = clampPanelWidth(width);
      if (get().sidebarWidth === next) return;
      set({ sidebarWidth: next });
    },

    setRightWidth(width) {
      const next = clampPanelWidth(width);
      if (get().rightWidth === next) return;
      set({ rightWidth: next });
    },

    persistLayout() {
      persist();
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

    toggleShowArchived() {
      set((s) => ({ showArchived: !s.showArchived, menu: null }));
      persist();
    },

    setFilter(filter) {
      set({ overviewFilter: filter, selected: [] });
    },

    setProjectFilter(projectId) {
      set({ overviewProject: projectId, selected: [] });
    },

    openProjectForm(edit, preset) {
      set({ projectForm: { edit, preset }, menu: null, paletteOpen: false, quickOpen: false });
    },
    closeProjectForm() {
      set({ projectForm: null });
    },
    openHostForm(edit, onSaved) {
      set({ hostForm: { edit, onSaved }, menu: null, paletteOpen: false, quickOpen: false });
    },
    closeHostForm() {
      set({ hostForm: null });
    },
    openWorktreeForm(sourceProjectId) {
      set({ worktreeFor: sourceProjectId, menu: null, paletteOpen: false, quickOpen: false });
    },
    closeWorktreeForm() {
      set({ worktreeFor: null });
    },
    openSettings(tab) {
      set({
        settingsOpen: true,
        settingsTab: tab ?? get().settingsTab,
        menu: null,
        paletteOpen: false, quickOpen: false,
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
      set({ confirm: spec, menu: null, paletteOpen: false, quickOpen: false });
    },
    closeConfirm() {
      set({ confirm: null });
    },
    pushAskpass(spec) {
      set((s) => {
        if (s.askpass.some((p) => p.id === spec.id)) return s;
        return { askpass: [...s.askpass, spec] };
      });
    },
    shiftAskpass() {
      set((s) => ({ askpass: s.askpass.slice(1) }));
    },
    openRename(sessionId) {
      set({ renameFor: sessionId, menu: null, paletteOpen: false, quickOpen: false });
    },
    closeRename() {
      set({ renameFor: null });
    },
    setPalette(open) {
      set({ paletteOpen: open, quickOpen: false, menu: null });
    },
    setQuickOpen(open) {
      set({ quickOpen: open, paletteOpen: false, menu: null });
    },
    openInstall(spec) {
      set({ install: spec, menu: null, paletteOpen: false, quickOpen: false });
    },
    closeInstall() {
      set({ install: null });
    },

    /**
     * 新建终端。SSH 项目首次用时先走授权 + 安装；
     * 已拒绝过的主机不再打扰，直接建非持久会话。重新启用走命令面板。
     */
    async newTerminal(projectId, afterKey) {
      const project = get().projects.find((p) => p.id === projectId);
      if (!project) return;
      set({ menu: null, paletteOpen: false, quickOpen: false });
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
      await get().createSessionNow(projectId, afterKey);
    },

    async createSessionNow(projectId, afterKey) {
      const pendingId = `${PENDING_PREFIX}${++pendingSeq}`;
      const key = termKey(pendingId);
      set((s) => {
        const pending = [...s.pending, { id: pendingId, projectId }];
        const tabs = [...s.tabs, pendingId];
        const current = visibleStripKeys({ ...s, pending, tabs });
        const without = current.filter((k) => k !== key);
        const tabOrder = insertAfter(without, afterKey, key);
        const visIds = new Set(visibleTabs({ ...s, pending, tabs }));
        const orderedVis = tabOrder.flatMap((k) => {
          const item = parseStripKey(k);
          return item?.kind === "terminal" ? [item.id] : [];
        });
        return {
          pending,
          tabs: applyVisibleOrder(tabs, orderedVis, (id) => visIds.has(id)),
          tabOrder,
          active: { kind: "terminal" as const, sessionId: pendingId },
          selectedProjectId: projectId,
          menu: null,
          paletteOpen: false,
          quickOpen: false,
        };
      });
      try {
        const session = await api.createSession(projectId, get().activeTheme.hint);
        await get().refreshSessions();
        set((s) => ({
          pending: s.pending.filter((p) => p.id !== pendingId),
          tabs: s.tabs.map((t) => (t === pendingId ? session.id : t)),
          tabOrder: replaceKey(s.tabOrder, key, termKey(session.id)),
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
        const session = await api.createSession(entry.projectId, get().activeTheme.hint);
        await get().refreshSessions();
        set((s) => ({
          pending: s.pending.filter((p) => p.id !== pendingId),
          tabs: s.tabs.map((t) => (t === pendingId ? session.id : t)),
          tabOrder: replaceKey(s.tabOrder, termKey(pendingId), termKey(session.id)),
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

// index.html 的内联脚本只写了底色 / 字色与 .dark，整套 token 在这里落；
// 两边对深浅的判断必须一致（都按底色亮度）。
applyThemeToDom(useApp.getState().activeTheme);
// 旧格式第一次升级：把迁移出来的新格式写回去，之后内联脚本就能直接读到
if (!safeStorage()?.getItem("falcon.themes")) {
  const { themePref, themes } = useApp.getState();
  saveThemeSettings(safeStorage(), { mode: themePref, ...themes });
}
// 旧版终端单独配色能对上 Ghostty 内置主题的，拉目录后补进对应槽位（一次性）
if (initialThemes.legacyTerm) {
  const { slot, name } = initialThemes.legacyTerm;
  void loadCatalog()
    .then((entries) => {
      const entry = findTheme(entries, name);
      if (entry) useApp.getState().setThemeChoice(slot, choiceOf(entry));
    })
    .catch(() => undefined);
}

// 跟随系统时才响应；选定了浅色/深色的用户不该因为系统入夜就被换掉主题
watchSystemTheme((dark) => {
  const { themePref, themeMode, themes } = useApp.getState();
  const mode: ThemeMode = dark ? "dark" : "light";
  if (themePref !== "system" || themeMode === mode) return;
  const activeTheme = resolveChoice(themes[mode]);
  applyThemeToDom(activeTheme);
  useApp.setState({ themeMode: mode, activeTheme });
});
