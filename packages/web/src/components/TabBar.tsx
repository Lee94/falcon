import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { PanelLeft, Plus, X } from "lucide-react";
import { api } from "../api.js";
import { useApp, isPendingId, selectSidebarVisible, visibleTabs } from "../store.js";
import { connLabel, sshBar } from "../lib/hostColor.js";
import { chord } from "../lib/shortcuts.js";
import { cn } from "@/lib/utils";
import { StatusMark } from "./common/StatusMark.js";

/**
 * Tab 标题就地重命名。Enter / 失焦提交，Esc 取消；空名或未改动直接退出。
 * finished 挡住提交成功后 input 卸载触发的 blur，避免把取消当成保存。
 */
function TabNameEditor({
  id,
  initial,
  onStop,
}: {
  id: string;
  initial: string;
  onStop: () => void;
}) {
  const { t } = useTranslation();
  const refreshSessions = useApp((s) => s.refreshSessions);
  const toast = useApp((s) => s.toast);
  const [draft, setDraft] = useState(initial);
  const [busy, setBusy] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);
  const finished = useRef(false);

  useEffect(() => {
    const el = inputRef.current;
    if (!el) return;
    el.focus();
    el.select();
  }, []);

  const stop = () => {
    finished.current = true;
    onStop();
  };

  const commit = async () => {
    if (finished.current) return;
    const name = draft.trim();
    if (!name || name === initial) {
      stop();
      return;
    }
    setBusy(true);
    try {
      await api.renameSession(id, name);
      await refreshSessions();
      toast({ kind: "info", title: t("toast.renamed", { name }) });
      stop();
    } catch (err) {
      toast({ kind: "danger", title: t("toast.failed"), body: (err as Error).message });
    } finally {
      setBusy(false);
    }
  };

  return (
    <input
      ref={inputRef}
      className="h-5 min-w-0 flex-1 bg-transparent px-0.5 text-xs text-foreground outline-none ring-1 ring-ring/50"
      value={draft}
      disabled={busy}
      aria-label={t("session.renameLabel")}
      onChange={(e) => setDraft(e.target.value)}
      onBlur={() => void commit()}
      onClick={(e) => e.stopPropagation()}
      onMouseDown={(e) => e.stopPropagation()}
      onKeyDown={(e) => {
        e.stopPropagation();
        if (e.key === "Enter") {
          e.preventDefault();
          void commit();
        } else if (e.key === "Escape") {
          e.preventDefault();
          stop();
        }
      }}
    />
  );
}

/**
 * 常驻渲染——从 0 个 tab 到 1 个 tab 主区不再上下跳。
 * 关闭按钮的 tooltip 必须说清它会结束会话，以及 Shift 这条保留会话的出口。
 */
export function TabBar() {
  const { t } = useTranslation();
  const allTabs = useApp((s) => s.tabs);
  const sessions = useApp((s) => s.sessions);
  const projects = useApp((s) => s.projects);
  const pending = useApp((s) => s.pending);
  const active = useApp((s) => s.active);
  const selectedProjectId = useApp((s) => s.selectedProjectId);
  const tabs = useMemo(
    () => visibleTabs({ tabs: allTabs, sessions, pending, selectedProjectId }),
    [allTabs, sessions, pending, selectedProjectId]
  );
  const system = useApp((s) => s.system);
  const sidebarVisible = useApp(selectSidebarVisible);
  const toggleSidebar = useApp((s) => s.toggleSidebar);
  const openSession = useApp((s) => s.openSession);
  const closeTab = useApp((s) => s.closeTab);
  const detachTab = useApp((s) => s.detachTab);
  const newTerminal = useApp((s) => s.newTerminal);
  const [editingId, setEditingId] = useState<string | null>(null);

  useEffect(() => {
    if (editingId && !tabs.includes(editingId)) setEditingId(null);
  }, [editingId, tabs]);

  /** Shift 是那条"只收起、别杀"的出口，鼠标的两种关法都认它 */
  const close = (id: string, e: { shiftKey: boolean }) => {
    if (e.shiftKey) detachTab(id);
    else void closeTab(id);
  };

  const activeId = active.kind === "terminal" ? active.sessionId : null;

  /** ＋ 建在侧栏选中的项目里；没有选中就跟当前会话走 */
  const targetProjectId = (() => {
    if (selectedProjectId) return selectedProjectId;
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
        const canRename = !!session && session.state !== "dead";
        const editing = editingId === id;
        const tip = conn ? `${label} · ${conn}` : label;

        return (
          <div
            key={id}
            role="tab"
            aria-selected={on}
            tabIndex={editing ? -1 : 0}
            className={cn(
              "flex max-w-55 min-w-0 cursor-pointer items-center gap-2 border-t-2 border-r border-t-transparent px-2.5 text-xs outline-none select-none focus-visible:ring-[3px] focus-visible:ring-ring/50",
              on
                ? "bg-background text-foreground"
                : "text-muted-foreground hover:bg-accent/50",
              editing && "min-w-36"
            )}
            title={editing ? undefined : canRename ? `${tip}\n${t("tab.renameHint")}` : tip}
            style={{ borderTopColor: on ? (bar ?? "var(--primary)") : undefined }}
            onClick={() => openSession(id)}
            onDoubleClick={() => {
              if (canRename) setEditingId(id);
            }}
            onKeyDown={(e) => {
              if (editing) return;
              if (e.key === "Enter" || e.key === " ") {
                e.preventDefault();
                openSession(id);
              }
            }}
            onAuxClick={(e) => {
              if (e.button === 1) {
                e.preventDefault();
                close(id, e);
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
            {editing ? (
              <TabNameEditor id={id} initial={label} onStop={() => setEditingId(null)} />
            ) : (
              <span className="min-w-0 flex-1 truncate">{label}</span>
            )}
            <button
              className="grid size-4 shrink-0 place-items-center rounded-sm text-muted-foreground outline-none hover:bg-accent hover:text-foreground focus-visible:ring-[3px] focus-visible:ring-ring/50"
              aria-label={t("tab.closeHint")}
              title={`${t("tab.closeHint")}\n${t("tab.detachHint")}`}
              onClick={(e) => {
                e.stopPropagation();
                close(id, e);
              }}
              onDoubleClick={(e) => e.stopPropagation()}
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
