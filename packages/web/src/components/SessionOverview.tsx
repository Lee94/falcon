import { useTranslation } from "react-i18next";
import { Ellipsis, ShieldCheck, TerminalIcon, TriangleAlert } from "lucide-react";
import type { SessionState, SessionWithProject } from "@mojito/shared";
import { api } from "../api.js";
import { useApp, type OverviewFilter } from "../store.js";
import { hostLabel } from "../lib/hostColor.js";
import { durabilityHint } from "../lib/reason.js";
import { absoluteTime, idleText, useActions } from "../lib/useActions.js";
import { cn } from "@/lib/utils";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { StatusMark } from "./common/StatusMark.js";
import { menuAnchor } from "./common/Menu.js";

const CARD_STATES: SessionState[] = ["active", "unverified", "dead"];

/** 舰队视图：先看健康度，再按需下钻到某一台机器上的某一个会话。 */
export function SessionOverview() {
  const { t } = useTranslation();
  const sessions = useApp((s) => s.sessions);
  const projects = useApp((s) => s.projects);
  const auth = useApp((s) => s.auth);
  const system = useApp((s) => s.system);
  const filter = useApp((s) => s.overviewFilter);
  const projectFilter = useApp((s) => s.overviewProject);
  const selected = useApp((s) => s.selected);
  const setFilter = useApp((s) => s.setFilter);
  const setProjectFilter = useApp((s) => s.setProjectFilter);
  const toggleSelected = useApp((s) => s.toggleSelected);
  const setSelected = useApp((s) => s.setSelected);
  const openSession = useApp((s) => s.openSession);
  const openMenu = useApp((s) => s.openMenu);
  const openDrawer = useApp((s) => s.openDrawer);
  const openProjectForm = useApp((s) => s.openProjectForm);
  const setPasswordOpen = useApp((s) => s.setPasswordOpen);
  const refreshAuth = useApp((s) => s.refreshAuth);
  const actions = useActions();

  const rows = sessions.filter(
    (s) =>
      (filter === "all" || s.state === filter) &&
      (projectFilter === null || s.projectId === projectFilter)
  );
  const count = (state: SessionState) => sessions.filter((s) => s.state === state).length;
  const filteredProject = projects.find((p) => p.id === projectFilter);
  const allSelected = rows.length > 0 && rows.every((r) => selected.includes(r.id));

  const logout = async () => {
    await api.logout();
    await refreshAuth();
  };

  const resetFilter = () => {
    setFilter("all");
    setProjectFilter(null);
  };

  return (
    <section className="min-h-0 flex-1 overflow-y-auto px-6 pt-6 pb-8">
      <h1 className="mb-4 text-xl font-semibold tracking-tight">{t("overview.title")}</h1>

      {/* 摘要卡把"当前舰队健康度"提到最前，兼作筛选器 */}
      <div className="mb-5 flex gap-3">
        {CARD_STATES.map((state) => (
          <button
            key={state}
            className={cn(
              "w-44 rounded-lg border bg-card px-3.5 py-3 text-left outline-none transition-colors hover:border-ring/60 focus-visible:ring-[3px] focus-visible:ring-ring/50",
              filter === state && "border-ring/60 bg-accent"
            )}
            onClick={() => setFilter(filter === state ? "all" : (state as OverviewFilter))}
          >
            <div className="mb-1.5 flex items-center gap-1.5 text-xs text-muted-foreground">
              <StatusMark state={state} />
              {t(`session.state_${state}`)}
            </div>
            <div className="font-mono text-[22px] leading-tight font-semibold">
              {count(state)}
            </div>
          </button>
        ))}
      </div>

      <div className="mb-2.5 flex items-center gap-2">
        <span className="text-xs text-muted-foreground">{t("overview.filter")}</span>
        <Badge variant="secondary" className="font-normal">
          {filter === "all" ? t("overview.filterAll") : t(`session.state_${filter}`)}
          {filteredProject ? ` · ${filteredProject.name}` : ""}
        </Badge>
        <Button variant="ghost" size="sm" onClick={resetFilter}>
          {t("overview.all")}
        </Button>
        <span className="flex-1" />
        {selected.length > 0 && (
          <Button
            variant="destructive"
            size="sm"
            onClick={() => actions.terminateMany(selected)}
          >
            {t("overview.terminateSelected", { n: selected.length })}
          </Button>
        )}
        <Button variant="secondary" size="sm" onClick={() => void actions.clearAllDead()}>
          {t("overview.clearAllDead")}
        </Button>
      </div>

      {rows.length === 0 ? (
        <EmptyState
          hasProjects={projects.length > 0}
          hasSessions={sessions.length > 0}
          onNewProject={() => openProjectForm(null)}
          onClearFilter={resetFilter}
        />
      ) : (
        <div className="overflow-x-auto rounded-lg border bg-card">
          <Table className="min-w-[800px]">
            <TableHeader>
              <TableRow className="hover:bg-transparent">
                <TableHead className="w-9">
                  <Checkbox
                    checked={allSelected}
                    aria-label={t("overview.selectAll")}
                    onCheckedChange={() =>
                      setSelected(allSelected ? [] : rows.map((r) => r.id))
                    }
                  />
                </TableHead>
                <TableHead className="w-23">{t("overview.state")}</TableHead>
                <TableHead>{t("overview.session")}</TableHead>
                <TableHead>{t("overview.where")}</TableHead>
                <TableHead className="w-30">{t("overview.durability")}</TableHead>
                <TableHead className="w-22">{t("overview.idle")}</TableHead>
                <TableHead className="w-33" />
              </TableRow>
            </TableHeader>
            <TableBody>
              {rows.map((session) => (
                <Row
                  key={session.id}
                  session={session}
                  selected={selected.includes(session.id)}
                  hostName={hostLabel(
                    projects.find((p) => p.id === session.projectId),
                    t("project.typeLocalShort")
                  )}
                  onToggle={() => toggleSelected(session.id)}
                  onOpen={() => openSession(session.id)}
                  onReattach={() => void actions.reattach(session)}
                  onClear={() => void actions.clearDead(session)}
                  onDurability={() => openDrawer(session.projectId)}
                  onMenu={(e) =>
                    openMenu({
                      ...menuAnchor(e),
                      items: actions.sessionMenuItems(session),
                    })
                  }
                />
              ))}
            </TableBody>
          </Table>
        </div>
      )}

      <h2 className="mt-8 mb-3 text-sm font-semibold text-muted-foreground">
        {t("overview.settings")}
      </h2>
      <div className="flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
        <Button variant="outline" size="sm" onClick={() => setPasswordOpen(true)}>
          {t("password.title")}
        </Button>
        {auth?.required && auth.authenticated && (
          <Button variant="ghost" size="sm" onClick={() => void logout()}>
            {t("common.logout")}
          </Button>
        )}
        {system && (
          <span>
            {t("overview.version")} v{system.version} · {t("overview.platform")}{" "}
            {system.platform}
          </span>
        )}
      </div>
    </section>
  );
}

function Row({
  session,
  selected,
  hostName,
  onToggle,
  onOpen,
  onReattach,
  onClear,
  onDurability,
  onMenu,
}: {
  session: SessionWithProject;
  selected: boolean;
  hostName: string;
  onToggle: () => void;
  onOpen: () => void;
  onReattach: () => void;
  onClear: () => void;
  onDurability: () => void;
  onMenu: (e: { currentTarget: HTMLElement }) => void;
}) {
  const { t } = useTranslation();
  const dead = session.state === "dead";
  // 按状态给一个主操作，其余进 ⋯ ——一行铺四个按钮时危险操作也被平铺了
  const primary =
    session.state === "unverified"
      ? { label: t("session.reattach"), variant: "warning" as const, run: onReattach }
      : dead
        ? { label: t("session.clear"), variant: "ghost" as const, run: onClear }
        : { label: t("session.open"), variant: "outline" as const, run: onOpen };

  return (
    <TableRow>
      <TableCell>
        <Checkbox
          checked={selected}
          aria-label={t("overview.selectOne")}
          onCheckedChange={onToggle}
        />
      </TableCell>
      <TableCell>
        <span className="flex items-center gap-1.5 text-xs text-muted-foreground">
          <StatusMark state={session.state} />
          {t(`session.state_${session.state}`)}
        </span>
      </TableCell>
      <TableCell
        className={cn("max-w-0 truncate", dead && "text-muted-foreground line-through")}
        title={session.name}
      >
        {session.name}
      </TableCell>
      <TableCell className="max-w-0 truncate text-muted-foreground">
        {session.projectName} <span className="text-border">·</span>{" "}
        <span className="font-mono text-xs">{hostName}</span>
      </TableCell>
      <TableCell>
        {/* 看到问题的地方就是解决问题的地方：徽标直达主机的持久会话设置 */}
        <Button
          variant="ghost"
          size="xs"
          className={cn(
            "font-normal",
            session.durable ? "text-muted-foreground" : "bg-warning/10 text-warning"
          )}
          title={durabilityHint(t, session.durable, session.nonDurableReason)}
          onClick={onDurability}
        >
          {session.durable ? <ShieldCheck /> : <TriangleAlert />}
          {session.durable ? t("session.durable") : t("session.nonDurable")}
        </Button>
      </TableCell>
      <TableCell
        className="text-xs text-muted-foreground"
        title={absoluteTime(session.lastActiveAt)}
      >
        {idleText(t, session.lastActiveAt)}
      </TableCell>
      <TableCell>
        <span className="flex justify-end gap-1.5">
          <Button variant={primary.variant} size="sm" onClick={primary.run}>
            {primary.label}
          </Button>
          <Button
            variant="ghost"
            size="icon-sm"
            aria-label={t("session.moreActions")}
            onClick={onMenu}
          >
            <Ellipsis />
          </Button>
        </span>
      </TableCell>
    </TableRow>
  );
}

/** 空状态不是"没有会话"这四个字：一句说明 + 一个主操作 */
function EmptyState({
  hasProjects,
  hasSessions,
  onNewProject,
  onClearFilter,
}: {
  hasProjects: boolean;
  hasSessions: boolean;
  onNewProject: () => void;
  onClearFilter: () => void;
}) {
  const { t } = useTranslation();

  return (
    <div className="flex flex-col items-center gap-2.5 py-12 text-center text-muted-foreground">
      <TerminalIcon className="size-7 stroke-[1.5] text-muted-foreground/50" />
      <span className="text-[15px] text-foreground">
        {!hasProjects
          ? t("overview.emptyNoProjectTitle")
          : hasSessions
            ? t("overview.emptyFilteredTitle")
            : t("overview.emptyNoSessionTitle")}
      </span>
      <span className="max-w-115 text-[12.5px] leading-relaxed">
        {!hasProjects
          ? t("overview.emptyNoProjectBody")
          : t("overview.emptyNoSessionBody")}
      </span>
      {!hasProjects ? (
        <Button className="mt-1.5" onClick={onNewProject}>
          {t("sidebar.newProject")}
        </Button>
      ) : (
        hasSessions && (
          <Button variant="outline" className="mt-1.5" onClick={onClearFilter}>
            {t("overview.all")}
          </Button>
        )
      )}
    </div>
  );
}
