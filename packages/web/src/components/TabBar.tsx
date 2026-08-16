import { useTranslation } from "react-i18next";
import { PanelLeft, Plus, X } from "lucide-react";
import { useApp, isPendingId, selectSidebarVisible } from "../store.js";
import { connLabel, sshBar } from "../lib/hostColor.js";
import { chord } from "../lib/shortcuts.js";
import { cn } from "@/lib/utils";
import { StatusMark } from "./common/StatusMark.js";

/**
 * 常驻渲染——从 0 个 tab 到 1 个 tab 主区不再上下跳。
 * 关闭按钮的 tooltip 必须说清 Detach 语义：关标签页 ≠ 终止会话。
 */
export function TabBar() {
  const { t } = useTranslation();
  const tabs = useApp((s) => s.tabs);
  const sessions = useApp((s) => s.sessions);
  const projects = useApp((s) => s.projects);
  const pending = useApp((s) => s.pending);
  const active = useApp((s) => s.active);
  const system = useApp((s) => s.system);
  const sidebarVisible = useApp(selectSidebarVisible);
  const toggleSidebar = useApp((s) => s.toggleSidebar);
  const openSession = useApp((s) => s.openSession);
  const closeTab = useApp((s) => s.closeTab);
  const newTerminal = useApp((s) => s.newTerminal);

  const activeId = active.kind === "terminal" ? active.sessionId : null;

  /** ＋ 建在当前会话所属的项目里；没有当前会话就用第一个项目 */
  const targetProjectId = (() => {
    if (activeId) {
      const pendingEntry = pending.find((p) => p.id === activeId);
      if (pendingEntry) return pendingEntry.projectId;
      const session = sessions.find((s) => s.id === activeId);
      if (session) return session.projectId;
    }
    return projects[0]?.id ?? null;
  })();

  const plain =
    "grid w-8.5 shrink-0 place-items-center text-muted-foreground outline-none hover:bg-accent/50 hover:text-foreground focus-visible:ring-[3px] focus-visible:ring-ring/50 disabled:pointer-events-none disabled:opacity-40 [&_svg]:size-3.5";

  return (
    <div
      role="tablist"
      className="flex h-8.5 shrink-0 items-stretch overflow-x-auto border-b bg-sidebar [scrollbar-width:none] [&::-webkit-scrollbar]:h-0"
    >
      {!sidebarVisible && (
        <button
          className={cn(plain, "w-7.5 border-r")}
          aria-label={t("sidebar.expand")}
          title={`${t("sidebar.expand")} · ${chord("toggleSidebar")}`}
          onClick={toggleSidebar}
        >
          <PanelLeft />
        </button>
      )}

      {tabs.map((id) => {
        const pendingEntry = pending.find((p) => p.id === id);
        const session = isPendingId(id) ? undefined : sessions.find((s) => s.id === id);
        const project = projects.find(
          (p) => p.id === (session?.projectId ?? pendingEntry?.projectId)
        );
        const bar = sshBar(project);
        const on = activeId === id;
        const label = session?.name ?? t("tab.creating");
        const conn = connLabel(project, system, t("project.typeLocalShort"));
        const state = pendingEntry
          ? pendingEntry.error
            ? ("dead" as const)
            : ("creating" as const)
          : (session?.state ?? "creating");
        // 运行中的会话不摆状态记号——满屏绿点是噪音，异常才值得占位置
        const showMark = state !== "active";

        return (
          <div
            key={id}
            role="tab"
            aria-selected={on}
            tabIndex={0}
            className={cn(
              "flex max-w-55 min-w-0 cursor-pointer items-center gap-2 border-t-2 border-r border-t-transparent px-2.5 text-xs outline-none focus-visible:ring-[3px] focus-visible:ring-ring/50",
              on
                ? "bg-background text-foreground"
                : "text-muted-foreground hover:bg-accent/50"
            )}
            title={conn ? `${label} · ${conn}` : label}
            style={{ borderTopColor: on ? (bar ?? "var(--primary)") : undefined }}
            onClick={() => openSession(id)}
            onKeyDown={(e) => {
              if (e.key === "Enter" || e.key === " ") {
                e.preventDefault();
                openSession(id);
              }
            }}
            onAuxClick={(e) => {
              if (e.button === 1) {
                e.preventDefault();
                closeTab(id);
              }
            }}
          >
            {bar && (
              <span
                className="h-3.5 w-[3px] shrink-0 rounded-full"
                style={{ background: bar }}
              />
            )}
            {showMark && (
              <StatusMark
                state={state}
                label={pendingEntry?.error ? t("session.createFailedTitle") : undefined}
              />
            )}
            <span className="min-w-0 flex-1 truncate">{label}</span>
            <button
              className="grid size-4 shrink-0 place-items-center rounded-sm text-muted-foreground outline-none hover:bg-accent hover:text-foreground focus-visible:ring-[3px] focus-visible:ring-ring/50"
              aria-label={t("tab.closeHint")}
              title={t("tab.closeHint")}
              onClick={(e) => {
                e.stopPropagation();
                closeTab(id);
              }}
            >
              <X className="size-3" />
            </button>
          </div>
        );
      })}

      <button
        className={plain}
        aria-label={t("sidebar.newTerminal")}
        title={t("tab.newHint", { kbd: chord("newTerminal") })}
        disabled={!targetProjectId}
        onClick={() => targetProjectId && void newTerminal(targetProjectId)}
      >
        <Plus />
      </button>
      <span className="flex-1" />
    </div>
  );
}
