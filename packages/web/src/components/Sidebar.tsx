import {
  useEffect,
  useMemo,
  useState,
  type DragEvent,
  type MouseEvent,
  type ReactNode,
} from "react";
import { useTranslation } from "react-i18next";
import {
  Archive,
  Bot,
  ChevronDown,
  ChevronRight,
  Ellipsis,
  Folder,
  Folders,
  GitBranch,
  Monitor,
  PanelLeft,
  Plus,
  Server,
  Settings,
  SquareTerminal,
} from "lucide-react";
import { toast } from "sonner";
import type { Project, SessionWithProject } from "@falcon/shared";
import { WORKTREE_ARCHIVE_TTL_MS } from "@falcon/shared";
import { api } from "../api.js";
import {
  useApp,
  type PendingSession,
  type ProjectChanges,
  type ProjectHead,
} from "../store.js";
import {
  checkoutLabel,
  folderKey,
  groupServers,
  type FolderGroup,
  type ServerGroup,
} from "../lib/projectTree.js";
import { memberBasename } from "../lib/multiDerive.js";
import { StatusMark, type MarkState } from "./common/StatusMark.js";
import { useSessionLabel } from "../lib/useSessionLabel.js";
import { useActions } from "../lib/useActions.js";
import { chord } from "../lib/shortcuts.js";
import {
  hasMeegleWorkItemType,
  parseMeegleWorkItemDrag,
  type MeegleWorkItemDragPayload,
} from "../lib/meegleDrag.js";
import { meegleDisplayKey } from "../lib/meegleKey.js";
import { cn, pollWhileVisible } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { menuAnchor, openContextMenu } from "./common/Menu.js";

const CHANGES_POLL_MS = 8000;

export function Sidebar() {
  const { t } = useTranslation();
  const projects = useApp((s) => s.projects);
  const hosts = useApp((s) => s.hosts);
  const heads = useApp((s) => s.heads);
  const changes = useApp((s) => s.changes);
  const collapsed = useApp((s) => s.collapsed);
  const toggleSidebar = useApp((s) => s.toggleSidebar);
  const toggleCollapsed = useApp((s) => s.toggleCollapsed);
  const openProjectForm = useApp((s) => s.openProjectForm);
  const selectProject = useApp((s) => s.selectProject);
  const selectedProjectId = useApp((s) => s.selectedProjectId);
  const openSettings = useApp((s) => s.openSettings);
  const settingsOpen = useApp((s) => s.settingsOpen);
  const openMenu = useApp((s) => s.openMenu);
  const refreshChanges = useApp((s) => s.refreshChanges);
  const showArchived = useApp((s) => s.showArchived);
  const [meegleDragging, setMeegleDragging] = useState(false);
  const actions = useActions();
  const servers = groupServers(
    projects,
    hosts,
    t("project.typeLocalShort"),
    showArchived
  ).filter((s) => s.kind !== "local" || s.folders.length > 0 || hosts.length === 0);
  const empty = projects.length === 0 && hosts.length === 0;

  useEffect(() => {
    void refreshChanges();
    return pollWhileVisible(() => void refreshChanges(), CHANGES_POLL_MS);
  }, [refreshChanges]);

  useEffect(() => {
    const clear = () => setMeegleDragging(false);
    window.addEventListener("dragend", clear);
    window.addEventListener("drop", clear);
    return () => {
      window.removeEventListener("dragend", clear);
      window.removeEventListener("drop", clear);
    };
  }, []);

  return (
    <aside
      className="island flex min-h-0 flex-1 flex-col overflow-hidden text-sidebar-foreground"
      onDragEnter={(e) => {
        if (hasMeegleWorkItemType(e.dataTransfer)) setMeegleDragging(true);
      }}
      onDragLeave={(e) => {
        if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setMeegleDragging(false);
      }}
      onDrop={() => setMeegleDragging(false)}
    >
      {/* 顶上不设标题栏：树本身就是内容，新建项目走各服务器行 hover 的 + / 右键 / 命令面板 */}
      {/* 左右也留出内边距：行的 hover / 选中是内缩的圆角块，贴着岛边会切掉那个圆角 */}
      <div className="min-h-0 flex-1 overflow-y-auto p-1.5">
        {empty && (
          <p className="px-2 pt-1.5 pb-2.5 text-xs leading-relaxed text-muted-foreground">
            {t("sidebar.empty")}
          </p>
        )}
        {servers.map((server) => (
          <ServerNode
            key={server.key}
            server={server}
            heads={heads}
            changes={changes}
            expanded={!collapsed[server.key]}
            collapsed={collapsed}
            onToggle={() => toggleCollapsed(server.key)}
            onToggleKey={toggleCollapsed}
            selectedProjectId={selectedProjectId}
            onSelectProject={selectProject}
            onNewProject={() =>
              openProjectForm(
                null,
                server.kind === "host" && server.host
                  ? { type: "ssh", hostId: server.host.id }
                  : server.kind === "local"
                    ? { type: "local" }
                    : { type: "ssh" }
              )
            }
            onMenu={
              server.kind === "host" && server.host
                ? (e) =>
                    openMenu({
                      ...menuAnchor(e),
                      items: actions.hostMenuItems(server.host!),
                    })
                : server.kind === "local"
                  ? (e) =>
                      openMenu({
                        ...menuAnchor(e),
                        items: actions.serverMenuItems("local"),
                      })
                  : undefined
            }
            onContextMenu={(e) =>
              openContextMenu(
                e,
                server.kind === "host" && server.host
                  ? actions.hostMenuItems(server.host)
                  : actions.serverMenuItems(server.kind === "local" ? "local" : "legacy")
              )
            }
            onProjectMenu={(project, e) =>
              openMenu({ ...menuAnchor(e), items: actions.projectMenuItems(project) })
            }
            onProjectContext={(project, e) => {
              openContextMenu(e, actions.projectMenuItems(project));
            }}
            meegleDragging={meegleDragging}
          />
        ))}
      </div>

      <div className="flex h-9 shrink-0 items-center gap-2 px-1.5">
        <Button
          variant="ghost"
          size="sm"
          className={cn(
            "h-6.5 gap-2 px-2 text-xs font-normal text-muted-foreground",
            settingsOpen && "bg-accent text-accent-foreground"
          )}
          aria-label={t("sidebar.settingsTitle")}
          title={t("sidebar.settingsTitle")}
          onClick={() => openSettings()}
        >
          <Settings />
          {t("sidebar.settings")}
        </Button>
        <Button
          variant="ghost"
          size="icon-sm"
          className="ml-auto text-muted-foreground"
          aria-label={t("sidebar.collapse")}
          title={`${t("sidebar.collapse")} · ${chord("toggleSidebar")}`}
          onClick={toggleSidebar}
        >
          <PanelLeft />
        </Button>
      </div>
    </aside>
  );
}

function ServerNode({
  server,
  heads,
  changes,
  expanded,
  collapsed,
  onToggle,
  onToggleKey,
  selectedProjectId,
  onSelectProject,
  onNewProject,
  onMenu,
  onContextMenu,
  onProjectMenu,
  onProjectContext,
  meegleDragging,
}: {
  server: ServerGroup;
  heads: Record<string, ProjectHead>;
  changes: Record<string, ProjectChanges>;
  expanded: boolean;
  collapsed: Record<string, boolean>;
  onToggle: () => void;
  onToggleKey: (key: string) => void;
  selectedProjectId: string | null;
  onSelectProject: (id: string) => void;
  onNewProject: () => void;
  onMenu?: (e: { currentTarget: HTMLElement }) => void;
  onContextMenu: (e: MouseEvent) => void;
  onProjectMenu: (project: Project, e: { currentTarget: HTMLElement }) => void;
  onProjectContext: (project: Project, e: MouseEvent) => void;
  meegleDragging: boolean;
}) {
  const { t } = useTranslation();
  const Icon = server.kind === "local" ? Monitor : Server;

  return (
    <div className="mb-0.5">
      <div
        className="group/row flex h-7.5 items-center gap-1 rounded-lg pr-1.5 pl-1 hover:bg-sidebar-accent"
        title={`${server.conn ?? server.name} · ${t("sidebar.projectContext")}`}
        onContextMenu={onContextMenu}
      >
        <button
          className="grid size-4.5 shrink-0 place-items-center rounded-sm text-muted-foreground outline-none focus-visible:ring-1 focus-visible:ring-ring"
          aria-label={t("sidebar.toggleServer")}
          aria-expanded={expanded}
          onClick={onToggle}
        >
          {expanded ? <ChevronDown className="size-3" /> : <ChevronRight className="size-3" />}
        </button>
        <span
          className="h-4 w-[3px] shrink-0 rounded-full"
          style={{ background: server.bar ?? "transparent" }}
        />
        <Icon className="size-3.5 shrink-0 text-muted-foreground" />
        <span className="min-w-0 flex-1 truncate pl-1 font-medium" title={server.conn}>
          {server.name}
        </span>
        <Button
          variant="ghost"
          size="icon-xs"
          className="text-muted-foreground opacity-0 group-hover/row:opacity-100 focus-visible:opacity-100"
          aria-label={t("sidebar.newProject")}
          title={t("sidebar.newProject")}
          onClick={onNewProject}
        >
          <Plus />
        </Button>
        {onMenu && (
          <Button
            variant="ghost"
            size="icon-xs"
            className="text-muted-foreground"
            aria-label={t("sidebar.serverMenu")}
            title={t("common.more")}
            onClick={onMenu}
          >
            <Ellipsis />
          </Button>
        )}
      </div>

      {expanded &&
        (server.folders.length === 0 ? (
          <p className="py-1 pr-2 pl-8.5 text-[11px] text-muted-foreground">
            {t("sidebar.emptyServer")}
          </p>
        ) : (
          server.folders.map((folder) => (
            <FolderNode
              key={folder.project.id}
              folder={folder}
              heads={heads}
              changes={changes}
              collapsed={collapsed}
              selectedProjectId={selectedProjectId}
              onSelectProject={onSelectProject}
              onToggleKey={onToggleKey}
              onProjectMenu={onProjectMenu}
              onProjectContext={onProjectContext}
              meegleDragging={meegleDragging}
            />
          ))
        ))}
    </div>
  );
}

function FolderNode({
  folder,
  heads,
  changes,
  collapsed,
  selectedProjectId,
  onSelectProject,
  onToggleKey,
  onProjectMenu,
  onProjectContext,
  meegleDragging,
}: {
  folder: FolderGroup;
  heads: Record<string, ProjectHead>;
  changes: Record<string, ProjectChanges>;
  collapsed: Record<string, boolean>;
  selectedProjectId: string | null;
  onSelectProject: (id: string) => void;
  onToggleKey: (key: string) => void;
  onProjectMenu: (project: Project, e: { currentTarget: HTMLElement }) => void;
  onProjectContext: (project: Project, e: MouseEvent) => void;
  meegleDragging: boolean;
}) {
  const { t } = useTranslation();
  const { project, worktrees } = folder;
  const head = heads[project.id];
  // 未写过偏好 = 展开。第三层是分支 / worktree，默认要看见，不能让人再点一次。
  const open = collapsed[folderKey(project.id)] !== true;
  // 多仓库容器：文件夹行换 Folders 图标，tooltip 列成员名（成员不占独立行，
  // 它们不是项目，点了没有去处）
  const memberNames = project.multi?.repos.map((r) => memberBasename(r.dir)) ?? [];

  return (
    <div>
      <TreeRow
        depth={1}
        expanded={open}
        icon={
          project.multi ? (
            <Folders className="size-3.5 shrink-0 text-muted-foreground" />
          ) : (
            <Folder className="size-3.5 shrink-0 text-muted-foreground" />
          )
        }
        label={project.name}
        title={project.multi ? memberNames.join(" · ") : (project.workingDir ?? project.name)}
        toggleLabel="sidebar.toggleProject"
        onToggle={() => onToggleKey(folderKey(project.id))}
        onSelect={() => onSelectProject(project.id)}
        onMenu={(e) => onProjectMenu(project, e)}
        onContextMenu={(e) => onProjectContext(project, e)}
        meegleDrop={
          meegleDragging && !project.worktree ? { sourceId: project.id } : undefined
        }
      />
      {open && (
        <>
          <CheckoutNode
            project={project}
            label={
              project.multi
                ? t("multi.repoCount", { n: memberNames.length })
                : checkoutLabel(project, head)
            }
            meta={project.multi ? memberNames.slice(0, 3).join(" · ") : undefined}
            changes={changes[project.id]}
            branched
            depth={2}
            selected={selectedProjectId === project.id}
            onSelect={() => onSelectProject(project.id)}
            onContextMenu={(e) => onProjectContext(project, e)}
          />
          {worktrees.map((wt) => (
            <CheckoutNode
              key={wt.id}
              project={wt}
              label={checkoutLabel(wt)}
              changes={changes[wt.id]}
              branched
              depth={2}
              selected={selectedProjectId === wt.id}
              onSelect={() => onSelectProject(wt.id)}
              onContextMenu={(e) => onProjectContext(wt, e)}
            />
          ))}
        </>
      )}
    </div>
  );
}

function CheckoutNode({
  project,
  label,
  meta,
  changes,
  branched,
  depth,
  selected,
  onSelect,
  onContextMenu,
}: {
  project: Project;
  label: string;
  meta?: string;
  changes?: ProjectChanges;
  branched?: boolean;
  depth: number;
  selected?: boolean;
  onSelect?: () => void;
  onContextMenu: (e: MouseEvent) => void;
}) {
  const { t } = useTranslation();
  const openMenu = useApp((s) => s.openMenu);
  const actions = useActions();
  const { mine, minePending } = useProjectSessions(project.id);
  // 会话行默认收着（见 store 的 sessionsOpen）：一个检出能挂五六个终端，
  // 全摊开就把侧栏占满了，平时看这一行尾巴上的计数就够
  const sessionsOpen = useApp((s) => s.sessionsOpen[project.id] === true);
  const toggleSessions = useApp((s) => s.toggleSessions);
  const sourceName = useApp((s) =>
    project.worktree
      ? (s.projects.find((p) => p.id === project.worktree?.sourceProjectId)?.name ?? "")
      : ""
  );
  // 存档的附属项目：置灰、显示删除倒计时、点击不再选中（要恢复走右键菜单）
  const archivedAt = project.worktree?.archivedAt;
  const deadline = archivedAt ? archivedAt + WORKTREE_ARCHIVE_TTL_MS : 0;
  const daysLeft = archivedAt ? Math.ceil((deadline - Date.now()) / 86_400_000) : 0;
  const archivedNote = archivedAt
    ? daysLeft >= 1
      ? t("worktree.archivedMetaDays", { n: daysLeft })
      : t("worktree.archivedMetaSoon")
    : undefined;
  const title = project.worktree
    ? [
        t("worktree.derivedFrom", { name: sourceName }),
        archivedAt
          ? t("worktree.archivedTitle", { date: new Date(deadline).toLocaleString() })
          : null,
        project.workingDir ?? "",
      ]
        .filter(Boolean)
        .join(" · ")
    : (project.workingDir ?? project.name);

  const count = mine.length + minePending.length;
  // 存档的检出开不了会话，也就没有会话行可摊（下面 SessionRows 同样跳过）
  const expandable = !archivedAt && count > 0;

  return (
    <>
      <TreeRow
        depth={depth}
        expanded={expandable ? sessionsOpen : undefined}
        toggleLabel={expandable ? "sidebar.toggleSessions" : undefined}
        onToggle={expandable ? () => toggleSessions(project.id) : undefined}
        trailing={
          expandable ? (
            <SessionCountBadge
              count={count}
              worst={worstState(mine, minePending)}
              expanded={sessionsOpen}
              onToggle={() => toggleSessions(project.id)}
            />
          ) : undefined
        }
        icon={
          archivedAt ? (
            <Archive className="size-3.5 shrink-0 text-muted-foreground" />
          ) : project.multi && !project.worktree ? (
            <Folders className="size-3.5 shrink-0 text-muted-foreground" />
          ) : branched ? (
            <GitBranch className="size-3.5 shrink-0 text-muted-foreground" />
          ) : (
            <Folder className="size-3.5 shrink-0 text-muted-foreground" />
          )
        }
        label={label}
        meta={archivedNote ?? meta}
        changes={archivedAt ? undefined : changes}
        title={title}
        muted={!!archivedAt}
        selected={selected}
        onSelect={archivedAt ? undefined : onSelect}
        onContextMenu={onContextMenu}
        // 存档的项目不能再开会话（目录到期要连着删）
        onNew={
          archivedAt
            ? undefined
            : (e) => openMenu({ ...menuAnchor(e), items: actions.newSessionItems(project.id) })
        }
      />
      {expandable && sessionsOpen && (
        <SessionRows
          sessions={mine}
          pending={minePending}
          projectId={project.id}
          depth={depth + 1}
        />
      )}
    </>
  );
}

/** 某个检出下的会话与在建会话。计数徽标与会话行共用一份筛选，不能各筛各的。 */
function useProjectSessions(projectId: string) {
  const sessions = useApp((s) => s.sessions);
  const pending = useApp((s) => s.pending);
  // sessions 内容没变时引用是稳的（store 的 sameFlatArray），filter 放渲染里就够
  const mine = useMemo(
    () => sessions.filter((s) => s.projectId === projectId),
    [sessions, projectId]
  );
  const minePending = useMemo(
    () => pending.filter((p) => p.projectId === projectId),
    [pending, projectId]
  );
  return { mine, minePending };
}

/** 折叠着也得看得出出没出事：取最重的那个状态，全是 active 就不摆记号。 */
function worstState(
  sessions: SessionWithProject[],
  pending: { error?: string }[]
): MarkState | null {
  if (sessions.some((s) => s.state === "dead") || pending.some((p) => p.error)) return "dead";
  if (sessions.some((s) => s.state === "unverified")) return "unverified";
  if (pending.length > 0) return "creating";
  return null;
}

/**
 * 检出行尾巴上的会话计数。点它 = 摊开 / 收起会话行，和行首的箭头同一个动作——
 * 那个箭头只有 12px 见方，这里给个够大的落点。
 */
function SessionCountBadge({
  count,
  worst,
  expanded,
  onToggle,
}: {
  count: number;
  worst: MarkState | null;
  expanded: boolean;
  onToggle: () => void;
}) {
  const { t } = useTranslation();
  const label = t("sidebar.sessionCount", { n: count });
  return (
    <button
      className={cn(
        "flex h-5 shrink-0 items-center gap-0.5 rounded-sm px-0.5 text-[11px] tabular-nums outline-none focus-visible:ring-1 focus-visible:ring-ring",
        expanded ? "text-foreground" : "text-muted-foreground hover:text-foreground"
      )}
      aria-label={label}
      aria-expanded={expanded}
      title={label}
      onClick={(e) => {
        e.stopPropagation();
        onToggle();
      }}
    >
      {worst ? (
        <StatusMark state={worst} />
      ) : (
        <SquareTerminal className="size-3" strokeWidth={2} />
      )}
      {count}
    </button>
  );
}

/**
 * 项目下的会话行。没有顶部 tab 栏之后，"现在开着哪些终端"就摆在侧栏这棵树里：
 * 点一行把焦点交给画布上那扇窗口，还没在画布上的（别的项目、之前 Detach 掉的）
 * 点了会重新摆一列出来（openSession 负责）。
 */
function SessionRows({
  sessions: mine,
  pending: minePending,
  projectId,
  depth,
}: {
  sessions: SessionWithProject[];
  pending: PendingSession[];
  projectId: string;
  depth: number;
}) {
  const { t } = useTranslation();
  const active = useApp((s) => s.active);
  const openSession = useApp((s) => s.openSession);
  const openMenu = useApp((s) => s.openMenu);
  const actions = useActions();
  const activeId = active.kind === "terminal" ? active.sessionId : null;

  return (
    <>
      {mine.map((session) => (
        <SessionRow
          key={session.id}
          session={session}
          depth={depth}
          selected={activeId === session.id}
          onSelect={() => openSession(session.id)}
          onMenu={(e) =>
            openMenu({ ...menuAnchor(e), items: actions.sessionMenuItems(session) })
          }
          onContextMenu={(e) => openContextMenu(e, actions.sessionMenuItems(session))}
        />
      ))}
      {minePending.map((p) => (
        <div
          key={p.id}
          className="flex h-7 items-center gap-1.5 rounded-lg pr-1.5 text-muted-foreground"
          style={{ paddingLeft: 4 + depth * 12 }}
        >
          <span className="size-4.5 shrink-0" />
          <StatusMark state={p.error ? "dead" : "creating"} />
          <span className="min-w-0 flex-1 truncate pl-1">{t("tab.creating")}</span>
        </div>
      ))}
    </>
  );
}

function SessionRow({
  session,
  depth,
  selected,
  onSelect,
  onMenu,
  onContextMenu,
}: {
  session: SessionWithProject;
  depth: number;
  selected: boolean;
  onSelect: () => void;
  onMenu: (e: { currentTarget: HTMLElement }) => void;
  onContextMenu: (e: MouseEvent) => void;
}) {
  const { t } = useTranslation();
  const Icon = session.agent ? Bot : SquareTerminal;
  const label = useSessionLabel()(session);
  return (
    <div
      role="button"
      tabIndex={0}
      aria-current={selected ? "true" : undefined}
      title={`${label} · ${t("sidebar.projectContext")}`}
      className={cn(
        "group/row flex h-7 cursor-pointer items-center gap-1 rounded-lg pr-1.5 hover:bg-sidebar-accent",
        selected && "bg-tint text-tint-foreground hover:bg-tint"
      )}
      style={{ paddingLeft: 4 + depth * 12 }}
      onClick={onSelect}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          onSelect();
        }
      }}
      onContextMenu={(e) => {
        e.preventDefault();
        e.stopPropagation();
        onContextMenu(e);
      }}
    >
      <span className="size-4.5 shrink-0" />
      <Icon className="size-3.5 shrink-0 text-muted-foreground" />
      <span className="min-w-0 flex-1 truncate pl-1">{label}</span>
      {/* 运行中不摆状态记号，异常才值得占位置 */}
      {session.state !== "active" && <StatusMark state={session.state} />}
      <Button
        variant="ghost"
        size="icon-xs"
        className="text-muted-foreground opacity-0 group-hover/row:opacity-100 focus-visible:opacity-100"
        aria-label={t("session.moreActions")}
        title={t("common.more")}
        onClick={(e) => {
          e.stopPropagation();
          onMenu(e);
        }}
      >
        <Ellipsis />
      </Button>
    </div>
  );
}

/**
 * 拖过来的 payload 里只有原始工作项 id，带前缀的 Key 藏在详情的模板名里（见 meegleKey.ts）。
 * 先按 id 把表单开出来，用户多半直接回车就把分支和目录名建错了，所以宁可等这一下详情
 * ——服务端对详情有 5 分钟 TTL 缓存，通常是秒回。详情读不到就退回原始 id：
 * 拖拽不能因为 Key 前缀查不到就整个失效，但要明说预填的是哪一种。
 */
async function openWorktreeFromMeegle(
  sourceId: string,
  payload: MeegleWorkItemDragPayload,
  t: (key: string) => string
) {
  // 命中服务端缓存时详情几十毫秒就回来，立刻弹 loading 只是闪一下；慢了才值得交代在等什么
  let notification: string | number | undefined;
  const hint = setTimeout(() => {
    notification = toast.loading(t("meegle.dropKeyLoading"));
  }, 300);
  const detail = await api.meegleWorkItem(payload.spaceKey, payload.id).catch((err: unknown) => {
    useApp.getState().handleApiError(err);
    return null;
  });
  clearTimeout(hint);
  if (!detail) toast.warning(t("meegle.dropKeyFallback"), { id: notification });
  else if (notification !== undefined) toast.dismiss(notification);
  const key = detail ? meegleDisplayKey(detail) : payload.id;
  const source = useApp.getState().projects.find((p) => p.id === sourceId);
  useApp.getState().openWorktreeForm(sourceId, {
    name: key,
    branch: key,
    mode: "new-branch",
    startPoint: source?.defaultWorktreeBranch || "HEAD",
  });
}

function TreeRow({
  depth,
  expanded,
  icon,
  label,
  meta,
  changes,
  title,
  toggleLabel,
  onToggle,
  muted,
  selected,
  onSelect,
  onMenu,
  onNew,
  onContextMenu,
  trailing,
  meegleDrop,
}: {
  depth: number;
  expanded?: boolean;
  icon: ReactNode;
  label: string;
  meta?: string;
  changes?: ProjectChanges;
  title?: string;
  toggleLabel?:
    | "sidebar.toggleServer"
    | "sidebar.toggleProject"
    | "sidebar.toggleWorktree"
    | "sidebar.toggleSessions";
  onToggle?: () => void;
  /** 存档行的置灰态 */
  muted?: boolean;
  selected?: boolean;
  onSelect?: () => void;
  onMenu?: (e: { currentTarget: HTMLElement }) => void;
  /** 行尾的 ＋：新建会话的菜单（普通终端 / 各家 CLI） */
  onNew?: (e: { currentTarget: HTMLElement }) => void;
  onContextMenu?: (e: MouseEvent) => void;
  /** 摆在 +N −M 左边的附加徽标（检出行的会话计数） */
  trailing?: ReactNode;
  /** 这一行能接飞书工作项：拖进来就以它为源开派生项目 */
  meegleDrop?: { sourceId: string };
}) {
  const { t } = useTranslation();
  const [dropOver, setDropOver] = useState(false);
  const onDragOver = meegleDrop
    ? (e: DragEvent<HTMLDivElement>) => {
        if (!hasMeegleWorkItemType(e.dataTransfer)) return;
        e.preventDefault();
        e.stopPropagation();
        e.dataTransfer.dropEffect = "copy";
        setDropOver(true);
      }
    : undefined;
  return (
    <div
      role={onSelect ? "button" : undefined}
      tabIndex={onSelect ? 0 : undefined}
      aria-current={selected ? "true" : undefined}
      title={
        onContextMenu
          ? `${title ?? label} · ${
              meegleDrop ? t("sidebar.meegleDropTarget") : t("sidebar.projectContext")
            }`
          : title
      }
      className={cn(
        // 圆角块而不是铺满整行的条：选中 / hover 都内缩在岛里
        "group/row flex h-7.5 items-center gap-1 rounded-lg pr-1.5 hover:bg-sidebar-accent",
        onSelect && "cursor-pointer",
        selected && "bg-tint text-tint-foreground hover:bg-tint",
        meegleDrop && "bg-primary/5 ring-1 ring-inset ring-primary/40",
        dropOver && "bg-accent ring-primary"
      )}
      style={{ paddingLeft: 4 + depth * 12 }}
      onClick={onSelect}
      onDragOver={onDragOver}
      onDragLeave={
        meegleDrop
          ? (e) => {
              if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setDropOver(false);
            }
          : undefined
      }
      onDrop={
        meegleDrop
          ? (e) => {
              e.preventDefault();
              e.stopPropagation();
              setDropOver(false);
              const payload = parseMeegleWorkItemDrag(e.dataTransfer);
              if (!payload) return;
              void openWorktreeFromMeegle(meegleDrop.sourceId, payload, (key) => t(key));
            }
          : undefined
      }
      onContextMenu={
        onContextMenu
          ? (e) => {
              e.preventDefault();
              e.stopPropagation();
              onContextMenu(e);
            }
          : undefined
      }
      onKeyDown={
        onSelect
          ? (e) => {
              if (e.key === "Enter" || e.key === " ") {
                e.preventDefault();
                onSelect();
              }
            }
          : undefined
      }
    >
      {onToggle && toggleLabel ? (
        <button
          className="grid size-4.5 shrink-0 place-items-center rounded-sm text-muted-foreground outline-none focus-visible:ring-1 focus-visible:ring-ring"
          aria-label={t(toggleLabel)}
          aria-expanded={expanded}
          onClick={(e) => {
            e.stopPropagation();
            onToggle();
          }}
        >
          {expanded ? <ChevronDown className="size-3" /> : <ChevronRight className="size-3" />}
        </button>
      ) : (
        <span className="size-4.5 shrink-0" />
      )}
      {icon}
      <span
        className={cn(
          "min-w-0 flex-1 truncate pl-1 font-medium",
          muted && "text-muted-foreground"
        )}
        title={title}
      >
        {label}
      </span>
      {meta && (
        <span className="max-w-24 truncate font-mono text-[11px] text-muted-foreground" title={meta}>
          {meta}
        </span>
      )}
      {onNew && (
        <Button
          variant="ghost"
          size="icon-xs"
          className="text-muted-foreground opacity-0 group-hover/row:opacity-100 focus-visible:opacity-100"
          aria-label={t("sidebar.newTerminal")}
          title={`${t("sidebar.newTerminal")} · ${chord("newTerminal")}`}
          onClick={(e) => {
            e.stopPropagation();
            onNew(e);
          }}
        >
          <Plus />
        </Button>
      )}
      {trailing}
      <GitChangeBadge changes={changes} />
      {onMenu && (
        <Button
          variant="ghost"
          size="icon-xs"
          className="text-muted-foreground"
          aria-label={t("sidebar.projectMenu")}
          title={t("common.more")}
          onClick={(e) => {
            e.stopPropagation();
            onMenu(e);
          }}
        >
          <Ellipsis />
        </Button>
      )}
    </div>
  );
}

function GitChangeBadge({ changes }: { changes?: ProjectChanges }) {
  const { t } = useTranslation();
  if (!changes || (changes.added <= 0 && changes.deleted <= 0)) return null;
  const title =
    changes.added > 0 && changes.deleted > 0
      ? t("sidebar.changesTitle", { added: changes.added, deleted: changes.deleted })
      : changes.added > 0
        ? t("sidebar.changesAdded", { n: changes.added })
        : t("sidebar.changesDeleted", { n: changes.deleted });
  return (
    <span
      className="shrink-0 font-mono text-[11px] leading-none tabular-nums"
      title={title}
      aria-label={title}
    >
      {changes.added > 0 && <span className="text-success">+{changes.added}</span>}
      {changes.added > 0 && changes.deleted > 0 && " "}
      {changes.deleted > 0 && <span className="text-destructive">-{changes.deleted}</span>}
    </span>
  );
}
