import { useEffect, type MouseEvent, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import {
  ChevronDown,
  ChevronRight,
  Ellipsis,
  Folder,
  GitBranch,
  Monitor,
  PanelLeft,
  Plus,
  Server,
  Settings,
} from "lucide-react";
import type { Project, SshHost } from "@mojito/shared";
import { useApp, type ProjectChanges, type ProjectHead } from "../store.js";
import { hostBarFromSsh, sshBar, sshConn } from "../lib/hostColor.js";
import { useActions } from "../lib/useActions.js";
import { chord } from "../lib/shortcuts.js";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { menuAnchor, openContextMenu } from "./common/Menu.js";
import { ThemeButton } from "./common/ThemeToggle.js";

const CHANGES_POLL_MS = 8000;

/** 侧栏第一层：本机、已保存主机、以及没有绑定主机的存量 SSH */
interface ServerGroup {
  key: string;
  kind: "local" | "host" | "legacy";
  name: string;
  conn?: string;
  bar?: string;
  host?: SshHost;
  folders: FolderGroup[];
}

/** 侧栏第二层：一个源项目（文件夹）。第三层永远是当前检出 + 附属 worktree */
interface FolderGroup {
  project: Project;
  worktrees: Project[];
}

function folderKey(projectId: string): string {
  return `p:${projectId}`;
}

function checkoutLabel(project: Project, head?: ProjectHead): string {
  if (project.worktree) return project.worktree.branch;
  return head?.branch ?? head?.sha ?? project.name;
}

function groupServers(
  projects: Project[],
  hosts: SshHost[],
  localName: string
): ServerGroup[] {
  const sources = projects.filter((p) => !p.worktree);
  const sourceIds = new Set(sources.map((p) => p.id));
  const kidsBySource = new Map<string, Project[]>();
  const orphans: Project[] = [];
  for (const p of projects) {
    const src = p.worktree?.sourceProjectId;
    if (!src) continue;
    if (sourceIds.has(src)) {
      const list = kidsBySource.get(src) ?? [];
      list.push(p);
      kidsBySource.set(src, list);
    } else {
      orphans.push(p);
    }
  }

  const foldersOf = (match: (p: Project) => boolean): FolderGroup[] => [
    ...sources.filter(match).map((project) => ({
      project,
      worktrees: kidsBySource.get(project.id) ?? [],
    })),
    ...orphans.filter(match).map((project) => ({ project, worktrees: [] })),
  ];

  const servers: ServerGroup[] = [
    {
      key: "s:local",
      kind: "local",
      name: localName,
      folders: foldersOf((p) => p.type === "local"),
    },
  ];

  for (const host of hosts) {
    servers.push({
      key: `s:host:${host.id}`,
      kind: "host",
      name: host.name,
      conn: sshConn(host),
      bar: hostBarFromSsh(host),
      host,
      folders: foldersOf((p) => p.type === "ssh" && p.hostId === host.id),
    });
  }

  const seen = new Set<string>();
  for (const p of projects) {
    if (p.type !== "ssh" || p.hostId) continue;
    const conn = p.ssh ? sshConn(p.ssh) : "ssh";
    if (seen.has(conn)) continue;
    seen.add(conn);
    servers.push({
      key: `s:legacy:${conn}`,
      kind: "legacy",
      name: p.ssh?.host ?? conn,
      conn,
      bar: sshBar(p),
      folders: foldersOf(
        (x) => x.type === "ssh" && !x.hostId && (x.ssh ? sshConn(x.ssh) : "ssh") === conn
      ),
    });
  }

  return servers;
}

export function Sidebar() {
  const { t } = useTranslation();
  const projects = useApp((s) => s.projects);
  const hosts = useApp((s) => s.hosts);
  const heads = useApp((s) => s.heads);
  const changes = useApp((s) => s.changes);
  const collapsed = useApp((s) => s.collapsed);
  const system = useApp((s) => s.system);
  const toggleSidebar = useApp((s) => s.toggleSidebar);
  const toggleCollapsed = useApp((s) => s.toggleCollapsed);
  const openProjectForm = useApp((s) => s.openProjectForm);
  const selectProject = useApp((s) => s.selectProject);
  const selectedProjectId = useApp((s) => s.selectedProjectId);
  const showOverview = useApp((s) => s.showOverview);
  const openSettings = useApp((s) => s.openSettings);
  const settingsOpen = useApp((s) => s.settingsOpen);
  const openMenu = useApp((s) => s.openMenu);
  const refreshChanges = useApp((s) => s.refreshChanges);
  const actions = useActions();
  const servers = groupServers(projects, hosts, t("project.typeLocalShort")).filter(
    (s) => s.kind !== "local" || s.folders.length > 0 || hosts.length === 0
  );
  const empty = projects.length === 0 && hosts.length === 0;

  useEffect(() => {
    void refreshChanges();
    const timer = setInterval(() => void refreshChanges(), CHANGES_POLL_MS);
    return () => clearInterval(timer);
  }, [refreshChanges]);

  return (
    <aside className="flex w-65 shrink-0 flex-col border-r bg-sidebar text-sidebar-foreground">
      <div className="flex h-11 shrink-0 items-center gap-2 border-b pr-2 pl-3">
        <button
          type="button"
          className="text-base font-medium tracking-tight outline-none focus-visible:ring-[3px] focus-visible:ring-ring/50"
          title={t("palette.overview")}
          onClick={showOverview}
        >
          {t("appName")}
        </button>
        <span className="flex-1" />
        <Button
          variant="ghost"
          size="icon-sm"
          aria-label={t("sidebar.collapse")}
          title={`${t("sidebar.collapse")} · ${chord("toggleSidebar")}`}
          onClick={toggleSidebar}
        >
          <PanelLeft />
        </Button>
      </div>

      <div className="mt-1 flex h-8 items-center pr-1.5 pl-3">
        <span className="text-[11px] tracking-wide text-muted-foreground">
          {t("sidebar.servers")}
        </span>
        <span className="flex-1" />
        <Button
          variant="ghost"
          size="icon-sm"
          aria-label={t("sidebar.newProject")}
          title={t("sidebar.newProject")}
          onClick={() => openProjectForm(null)}
        >
          <Plus />
        </Button>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto pb-2">
        {empty && (
          <p className="px-3 pt-1.5 pb-2.5 text-xs leading-relaxed text-muted-foreground">
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
          />
        ))}
      </div>

      <div className="flex h-9 shrink-0 items-center gap-2 border-t px-1.5">
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
        <span className="ml-auto flex items-center gap-1">
          {system?.version && (
            <span className="font-mono text-[11px] text-muted-foreground/70">
              v{system.version}
            </span>
          )}
          <ThemeButton />
        </span>
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
}) {
  const { t } = useTranslation();
  const Icon = server.kind === "local" ? Monitor : Server;

  return (
    <div className="mb-0.5">
      <div
        className="group/row flex h-7.5 items-center gap-1 pr-1.5 hover:bg-sidebar-accent"
        title={`${server.conn ?? server.name} · ${t("sidebar.projectContext")}`}
        onContextMenu={onContextMenu}
      >
        <button
          className="grid size-4.5 shrink-0 place-items-center rounded-sm text-muted-foreground outline-none focus-visible:ring-[3px] focus-visible:ring-ring/50"
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
          <p className="px-3 py-1 pl-8.5 text-[11px] text-muted-foreground">
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
}) {
  const { project, worktrees } = folder;
  const head = heads[project.id];
  // 未写过偏好 = 展开。第三层是分支 / worktree，默认要看见，不能让人再点一次。
  const open = collapsed[folderKey(project.id)] !== true;

  return (
    <div>
      <TreeRow
        depth={1}
        expanded={open}
        icon={<Folder className="size-3.5 shrink-0 text-muted-foreground" />}
        label={project.name}
        title={project.workingDir ?? project.name}
        toggleLabel="sidebar.toggleProject"
        onToggle={() => onToggleKey(folderKey(project.id))}
        onSelect={() => onSelectProject(project.id)}
        onMenu={(e) => onProjectMenu(project, e)}
        onContextMenu={(e) => onProjectContext(project, e)}
      />
      {open && (
        <>
          <CheckoutNode
            project={project}
            label={checkoutLabel(project, head)}
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
  const sourceName = useApp((s) =>
    project.worktree
      ? (s.projects.find((p) => p.id === project.worktree?.sourceProjectId)?.name ?? "")
      : ""
  );
  const title = project.worktree
    ? `${t("worktree.derivedFrom", { name: sourceName })} · ${project.workingDir ?? ""}`
    : (project.workingDir ?? project.name);

  return (
    <TreeRow
      depth={depth}
      icon={
        branched ? (
          <GitBranch className="size-3.5 shrink-0 text-muted-foreground" />
        ) : (
          <Folder className="size-3.5 shrink-0 text-muted-foreground" />
        )
      }
      label={label}
      meta={meta}
      changes={changes}
      title={title}
      selected={selected}
      onSelect={onSelect}
      onContextMenu={onContextMenu}
    />
  );
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
  selected,
  onSelect,
  onMenu,
  onContextMenu,
}: {
  depth: number;
  expanded?: boolean;
  icon: ReactNode;
  label: string;
  meta?: string;
  changes?: ProjectChanges;
  title?: string;
  toggleLabel?: "sidebar.toggleServer" | "sidebar.toggleProject" | "sidebar.toggleWorktree";
  onToggle?: () => void;
  selected?: boolean;
  onSelect?: () => void;
  onMenu?: (e: { currentTarget: HTMLElement }) => void;
  onContextMenu?: (e: MouseEvent) => void;
}) {
  const { t } = useTranslation();
  const leaf = !onToggle;
  return (
    <div
      role={onSelect ? "button" : undefined}
      tabIndex={onSelect ? 0 : undefined}
      aria-current={selected ? "true" : undefined}
      title={
        onContextMenu
          ? `${title ?? label} · ${t("sidebar.projectContext")}`
          : title
      }
      className={cn(
        "group/row flex h-7.5 items-center gap-1 pr-1.5 hover:bg-sidebar-accent",
        onSelect && "cursor-pointer",
        selected && "bg-accent"
      )}
      style={{ paddingLeft: 4 + depth * 12 }}
      onClick={onSelect}
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
          className="grid size-4.5 shrink-0 place-items-center rounded-sm text-muted-foreground outline-none focus-visible:ring-[3px] focus-visible:ring-ring/50"
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
      <span className="min-w-0 flex-1 truncate pl-1 font-medium" title={title}>
        {label}
      </span>
      {meta && (
        <span className="max-w-24 truncate font-mono text-[11px] text-muted-foreground" title={meta}>
          {meta}
        </span>
      )}
      {leaf ? (
        <GitChangeBadge changes={changes} />
      ) : (
        onMenu && (
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
        )
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
