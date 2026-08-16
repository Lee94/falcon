import { useTranslation } from "react-i18next";
import {
  ChevronDown,
  ChevronRight,
  Ellipsis,
  GitBranch,
  PanelLeft,
  Plus,
  Settings,
  X,
} from "lucide-react";
import type { Project, SessionWithProject } from "@mojito/shared";
import { useApp } from "../store.js";
import { hostLabel, sshBar } from "../lib/hostColor.js";
import { reasonText } from "../lib/reason.js";
import { useActions } from "../lib/useActions.js";
import { chord } from "../lib/shortcuts.js";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { StatusMark } from "./common/StatusMark.js";
import { menuAnchor } from "./common/Menu.js";
import { ThemeButton } from "./common/ThemeToggle.js";

export function Sidebar() {
  const { t } = useTranslation();
  const projects = useApp((s) => s.projects);
  const sessions = useApp((s) => s.sessions);
  const active = useApp((s) => s.active);
  const collapsed = useApp((s) => s.collapsed);
  const system = useApp((s) => s.system);
  const toggleSidebar = useApp((s) => s.toggleSidebar);
  const toggleProject = useApp((s) => s.toggleProject);
  const openProjectForm = useApp((s) => s.openProjectForm);
  const openSession = useApp((s) => s.openSession);
  const showOverview = useApp((s) => s.showOverview);
  const openMenu = useApp((s) => s.openMenu);
  const actions = useActions();

  const isOverview = active.kind === "overview";

  /**
   * 源项目 → 紧跟它的附属项目。
   *
   * 附属项目仍是**顶层节点**、不嵌进源项目的展开区：这样 collapsed[projectId]、
   * 会话列表、⋯ 菜单全部零改动，父子关系靠缩进 + 分支图标表达。
   * 源项目折叠时它的附属项目一并隐藏——与隐藏其会话是同一个心智。
   */
  const ordered = projects
    .filter((p) => !p.worktree)
    .flatMap((p) =>
      collapsed[p.id]
        ? [{ project: p, nested: false }]
        : [
            { project: p, nested: false },
            ...projects
              .filter((c) => c.worktree?.sourceProjectId === p.id)
              .map((c) => ({ project: c, nested: true })),
          ]
    );

  return (
    <aside className="flex w-65 shrink-0 flex-col border-r bg-sidebar text-sidebar-foreground">
      <div className="flex h-11 shrink-0 items-center gap-2 border-b pr-2 pl-3">
        <span className="text-base font-medium tracking-tight">{t("appName")}</span>
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
          {t("sidebar.projects")}
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
        {projects.length === 0 && (
          <p className="px-3 pt-1.5 pb-2.5 text-xs leading-relaxed text-muted-foreground">
            {t("sidebar.empty")}
          </p>
        )}
        {ordered.map(({ project, nested }) => (
          <ProjectNode
            key={project.id}
            project={project}
            nested={nested}
            sessions={sessions.filter((s) => s.projectId === project.id)}
            expanded={!collapsed[project.id]}
            activeSessionId={active.kind === "terminal" ? active.sessionId : null}
            onToggle={() => toggleProject(project.id)}
            onOpenSession={openSession}
            onMenu={(e) =>
              openMenu({ ...menuAnchor(e), items: actions.projectMenuItems(project) })
            }
            onReattach={actions.reattach}
            onClear={actions.clearDead}
          />
        ))}
      </div>

      <div className="flex h-9 shrink-0 items-center gap-2 border-t px-1.5">
        <Button
          variant="ghost"
          size="sm"
          className={cn(
            "h-6.5 gap-2 px-2 text-xs font-normal text-muted-foreground",
            isOverview && "bg-accent text-accent-foreground"
          )}
          aria-label={t("sidebar.settingsTitle")}
          title={t("sidebar.settingsTitle")}
          onClick={showOverview}
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

function ProjectNode({
  project,
  nested,
  sessions,
  expanded,
  activeSessionId,
  onToggle,
  onOpenSession,
  onMenu,
  onReattach,
  onClear,
}: {
  project: Project;
  nested: boolean;
  sessions: SessionWithProject[];
  expanded: boolean;
  activeSessionId: string | null;
  onToggle: () => void;
  onOpenSession: (id: string) => void;
  onMenu: (e: { currentTarget: HTMLElement }) => void;
  onReattach: (s: SessionWithProject) => void;
  onClear: (s: SessionWithProject) => void;
}) {
  const { t } = useTranslation();
  const bar = sshBar(project);
  const wt = project.worktree;
  const sourceName = useApp((s) =>
    wt ? (s.projects.find((p) => p.id === wt.sourceProjectId)?.name ?? "") : ""
  );

  return (
    /* 附属项目：缩进一级，表达"依附于上面那个源项目"。
       刻意不做成真正的嵌套 DOM——那样 collapsed / 会话列表 / 菜单全要改 */
    <div className={cn("mb-0.5", nested && "pl-3")}>
      <div className="group/row flex h-7.5 items-center gap-1 pr-1.5 hover:bg-sidebar-accent">
        <button
          className="grid size-4.5 shrink-0 place-items-center rounded-sm text-muted-foreground outline-none focus-visible:ring-[3px] focus-visible:ring-ring/50"
          aria-label={t("sidebar.toggleProject")}
          aria-expanded={expanded}
          onClick={onToggle}
        >
          {expanded ? <ChevronDown className="size-3" /> : <ChevronRight className="size-3" />}
        </button>
        {/* 有颜色 = 在别人的机器上；本地项目故意不给色条 */}
        <span
          className="h-4 w-[3px] shrink-0 rounded-full"
          style={{ background: bar ?? "transparent" }}
        />
        {wt && <GitBranch className="size-3.5 shrink-0 text-muted-foreground" />}
        <span
          className="min-w-0 flex-1 truncate pl-1 font-medium"
          title={
            wt
              ? `${t("worktree.derivedFrom", { name: sourceName })} · ${project.workingDir ?? ""}`
              : (project.workingDir ?? project.name)
          }
        >
          {project.name}
        </span>
        {/* 附属项目显示分支：宿主机与源项目相同，分支信息价值高得多 */}
        <span
          className="max-w-24 truncate font-mono text-[11px] text-muted-foreground"
          title={wt ? wt.branch : undefined}
        >
          {wt ? wt.branch : hostLabel(project, t("project.typeLocalShort"))}
        </span>
        <Button
          variant="ghost"
          size="icon-xs"
          className="text-muted-foreground"
          aria-label={t("sidebar.projectMenu")}
          title={t("common.more")}
          onClick={onMenu}
        >
          <Ellipsis />
        </Button>
      </div>

      {expanded &&
        sessions.map((session) => {
          const dead = session.state === "dead";
          const deadReason = session.deadReason
            ? t(`session.deadReason_${session.deadReason.replace(/-/g, "_")}`)
            : "";
          return (
            <div
              key={session.id}
              role="button"
              tabIndex={0}
              className={cn(
                "flex h-6.5 w-full cursor-pointer items-center gap-2 pr-1.5 pl-7.5 text-left outline-none hover:bg-sidebar-accent focus-visible:ring-[3px] focus-visible:ring-ring/50",
                activeSessionId === session.id && "bg-accent"
              )}
              title={
                dead
                  ? `${session.name} · ${deadReason}`
                  : session.durable
                    ? session.name
                    : `${session.name} · ${reasonText(t, session.nonDurableReason)}`
              }
              onClick={() => onOpenSession(session.id)}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  onOpenSession(session.id);
                }
              }}
            >
              <StatusMark state={session.state} />
              <span
                className={cn(
                  "min-w-0 flex-1 truncate",
                  dead && "text-muted-foreground line-through"
                )}
              >
                {session.name}
              </span>
              {/* 状态需要动作时，动作就在行内——不用先打开 tab 再找 banner */}
              {session.state === "unverified" && (
                <Button
                  variant="warning"
                  size="xs"
                  className="h-5"
                  onClick={(e) => {
                    e.stopPropagation();
                    onReattach(session);
                  }}
                >
                  {t("session.reattach")}
                </Button>
              )}
              {dead && (
                <Button
                  variant="ghost"
                  size="icon-xs"
                  className="size-4.5 text-muted-foreground"
                  aria-label={t("session.clearRecord")}
                  title={t("session.clearRecordHint")}
                  onClick={(e) => {
                    e.stopPropagation();
                    onClear(session);
                  }}
                >
                  <X />
                </Button>
              )}
            </div>
          );
        })}
    </div>
  );
}
