import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type PointerEvent as ReactPointerEvent,
} from "react";
import { useTranslation } from "react-i18next";
import { FileDiff, FileText, PanelLeft, Plus, X } from "lucide-react";
import { api } from "../api.js";
import {
  useApp,
  isPendingId,
  selectSidebarVisible,
  visibleFileTabs,
  visibleStripKeys,
  visibleTabs,
  type FileTabTarget,
  type MenuItemSpec,
} from "../store.js";
import { connLabel, sshBar } from "../lib/hostColor.js";
import { chord } from "../lib/shortcuts.js";
import { keysOther, keysToLeft, keysToRight, parseStripKey, type StripItem } from "../lib/tabStrip.js";
import { useTabReorder } from "../lib/useTabReorder.js";
import { cn } from "@/lib/utils";
import { StatusMark } from "./common/StatusMark.js";
import { openContextMenu } from "./common/Menu.js";

const tabChrome =
  "flex max-w-55 min-w-0 cursor-pointer items-center gap-2 border-t-2 border-r border-t-transparent px-2.5 text-xs outline-none select-none touch-none focus-visible:ring-[3px] focus-visible:ring-ring/50";

const closeBtn =
  "grid size-4 shrink-0 place-items-center rounded-sm text-muted-foreground outline-none hover:bg-accent hover:text-foreground focus-visible:ring-[3px] focus-visible:ring-ring/50";

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

function tabShell({
  selected,
  className,
  title,
  style,
  onSelect,
  onClick,
  onClose,
  onContextMenu,
  onDoubleClick,
  onPointerDown,
}: {
  selected: boolean;
  className?: string;
  title?: string;
  style?: CSSProperties;
  onSelect: () => void;
  onClick: () => void;
  onClose: (shift: boolean) => void;
  onContextMenu: (e: {
    clientX: number;
    clientY: number;
    preventDefault(): void;
    stopPropagation(): void;
  }) => void;
  onDoubleClick?: () => void;
  onPointerDown: (e: ReactPointerEvent<HTMLElement>) => void;
}) {
  return {
    role: "tab" as const,
    "aria-selected": selected,
    title,
    style,
    className: cn(
      tabChrome,
      selected ? "bg-background text-foreground" : "text-muted-foreground hover:bg-accent/50",
      className
    ),
    onPointerDown: (e: ReactPointerEvent<HTMLElement>) => {
      if (e.button === 0 && !(e.target as HTMLElement).closest("button")) onSelect();
      onPointerDown(e);
    },
    onClick,
    onDoubleClick,
    onKeyDown: (e: React.KeyboardEvent) => {
      if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        onSelect();
      }
    },
    onAuxClick: (e: React.MouseEvent) => {
      if (e.button === 1) {
        e.preventDefault();
        onClose(e.shiftKey);
      }
    },
    onContextMenu,
  };
}

/**
 * 常驻渲染——从 0 个 tab 到 1 个 tab 主区不再上下跳。
 * 关闭按钮的 tooltip 必须说清它会结束会话，以及 Shift 这条保留会话的出口。
 *
 * 拖拽和右键按 Chrome 的 tab 栏来：整条 strip 一起排序，右键是「新建到右侧 /
 * 复制 / 重命名 / 收起 / 关闭 / 关其他 / 关左 / 关右」。
 */
export function TabBar() {
  const { t } = useTranslation();
  const allTabs = useApp((s) => s.tabs);
  const tabOrder = useApp((s) => s.tabOrder);
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
  const diffTab = useApp((s) => s.diffTab);
  const showDiff = useApp((s) => s.showDiff);
  const closeDiff = useApp((s) => s.closeDiff);
  const fileTabs = useApp((s) => s.fileTabs);
  const openFile = useApp((s) => s.openFile);
  const closeFile = useApp((s) => s.closeFile);
  const moveStrip = useApp((s) => s.moveStrip);
  const closeStripKeys = useApp((s) => s.closeStripKeys);
  const files = useMemo(
    () => visibleFileTabs({ fileTabs, selectedProjectId }),
    [fileTabs, selectedProjectId]
  );
  const [editingId, setEditingId] = useState<string | null>(null);

  const keys = useMemo(
    () =>
      visibleStripKeys({
        tabs: allTabs,
        sessions,
        pending,
        selectedProjectId,
        fileTabs,
        diffTab,
        tabOrder,
      }),
    [allTabs, sessions, pending, selectedProjectId, fileTabs, diffTab, tabOrder]
  );

  const reorder = useTabReorder({
    keys,
    enabled: keys.length > 1 && !editingId,
    onMove: moveStrip,
  });

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

  const projectOf = (item: StripItem): string | null => {
    if (item.kind === "terminal") {
      const pendingEntry = pending.find((p) => p.id === item.id);
      if (pendingEntry) return pendingEntry.projectId;
      return sessions.find((s) => s.id === item.id)?.projectId ?? targetProjectId;
    }
    if (item.kind === "file") return item.projectId;
    return diffTab?.projectId ?? targetProjectId;
  };

  const menuFor = (item: StripItem, index: number): MenuItemSpec[] => {
    const projectId = projectOf(item);
    const others = keysOther(keys, index);
    const left = keysToLeft(keys, index);
    const right = keysToRight(keys, index);
    const items: MenuItemSpec[] = [
      {
        label: t("tab.newToRight"),
        disabled: !projectId,
        onSelect: () => projectId && void newTerminal(projectId, item.key),
      },
    ];

    if (item.kind === "terminal") {
      const session = isPendingId(item.id) ? undefined : sessions.find((s) => s.id === item.id);
      const canRename = !!session && session.state !== "dead";
      if (projectId) {
        items.push({
          label: t("tab.duplicate"),
          onSelect: () => void newTerminal(projectId, item.key),
        });
      }
      if (canRename) {
        items.push({
          label: t("session.rename"),
          onSelect: () => setEditingId(item.id),
        });
      }
      if (session && session.state !== "dead") {
        items.push({
          label: t("tab.detach"),
          separated: true,
          onSelect: () => detachTab(item.id),
        });
      }
      items.push({
        label: t("tab.close"),
        kbd: chord("closeTab"),
        separated: !session || session.state === "dead",
        danger: !!session && session.state !== "dead",
        onSelect: () => void closeTab(item.id),
      });
    } else {
      items.push({
        label: t("tab.closeView"),
        kbd: chord("closeTab"),
        separated: true,
        onSelect: () => {
          if (item.kind === "diff") closeDiff();
          else closeFile({ projectId: item.projectId, path: item.path });
        },
      });
    }

    const hasTerm = (ks: string[]) => ks.some((k) => parseStripKey(k)?.kind === "terminal");
    items.push({
      label: t("tab.closeOthers"),
      disabled: others.length === 0,
      danger: hasTerm(others),
      onSelect: () => void closeStripKeys(others),
    });
    items.push({
      label: t("tab.closeToRight"),
      disabled: right.length === 0,
      danger: hasTerm(right),
      onSelect: () => void closeStripKeys(right),
    });
    items.push({
      label: t("tab.closeToLeft"),
      disabled: left.length === 0,
      danger: hasTerm(left),
      onSelect: () => void closeStripKeys(left),
    });
    return items;
  };

  const plain =
    "grid w-8.5 shrink-0 place-items-center text-muted-foreground outline-none hover:bg-accent/50 hover:text-foreground focus-visible:ring-[3px] focus-visible:ring-ring/50 disabled:pointer-events-none disabled:opacity-40 [&_svg]:size-3.5";

  const select = (item: StripItem) => {
    if (item.kind === "terminal") openSession(item.id);
    else if (item.kind === "file") openFile(item.projectId, item.path);
    else showDiff();
  };

  const onTabClick = (item: StripItem) => {
    if (reorder.consumeClick()) return;
    select(item);
  };

  return (
    <div
      role="tablist"
      ref={reorder.listRef}
      className="flex h-8.5 shrink-0 items-stretch overflow-x-auto border-b bg-sidebar [scrollbar-width:none] [&::-webkit-scrollbar]:h-0"
      onContextMenu={(e) => {
        if ((e.target as HTMLElement).closest("[data-tab-key]")) return;
        if (!targetProjectId) return;
        openContextMenu(e, [
          {
            label: t("sidebar.newTerminal"),
            kbd: chord("newTerminal"),
            onSelect: () => void newTerminal(targetProjectId),
          },
        ]);
      }}
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

      {keys.map((key, index) => {
        const item = parseStripKey(key);
        if (!item) return null;
        const style = reorder.styleFor(index);
        const onPointerDown = (e: ReactPointerEvent<HTMLElement>) =>
          reorder.onPointerDown(e, key);
        const onMenu = (
          e: { clientX: number; clientY: number; preventDefault(): void; stopPropagation(): void }
        ) => openContextMenu(e, menuFor(item, index));

        if (item.kind === "terminal") {
          const pendingEntry = pending.find((p) => p.id === item.id);
          const session = isPendingId(item.id) ? undefined : sessions.find((s) => s.id === item.id);
          const project = projects.find(
            (p) => p.id === (session?.projectId ?? pendingEntry?.projectId)
          );
          const bar = sshBar(project);
          const on = activeId === item.id;
          const label = session?.name ?? t("tab.creating");
          const conn = connLabel(project, system, t("project.typeLocalShort"));
          const state = pendingEntry
            ? pendingEntry.error
              ? ("dead" as const)
              : ("creating" as const)
            : (session?.state ?? "creating");
          const showMark = state !== "active";
          const canRename = !!session && session.state !== "dead";
          const editing = editingId === item.id;
          const tip = conn ? `${label} · ${conn}` : label;

          return (
            <div
              key={key}
              data-tab-key={key}
              {...tabShell({
                selected: on,
                className: cn(editing && "min-w-36", reorder.drag?.from === index && "shadow-md"),
                title: editing ? undefined : canRename ? `${tip}\n${t("tab.renameHint")}` : tip,
                style: { ...style, borderTopColor: on ? (bar ?? "var(--primary)") : undefined },
                onSelect: () => select(item),
                onClick: () => onTabClick(item),
                onClose: (shift) => close(item.id, { shiftKey: shift }),
                onContextMenu: onMenu,
                onDoubleClick: () => {
                  if (canRename) setEditingId(item.id);
                },
                onPointerDown,
              })}
              tabIndex={editing ? -1 : 0}
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
                <TabNameEditor id={item.id} initial={label} onStop={() => setEditingId(null)} />
              ) : (
                <span className="min-w-0 flex-1 truncate">{label}</span>
              )}
              <button
                className={closeBtn}
                aria-label={t("tab.closeHint")}
                title={`${t("tab.closeHint")}\n${t("tab.detachHint")}`}
                onClick={(e) => {
                  e.stopPropagation();
                  close(item.id, e);
                }}
                onDoubleClick={(e) => e.stopPropagation()}
              >
                <X className="size-3" />
              </button>
            </div>
          );
        }

        if (item.kind === "diff") {
          if (!diffTab) return null;
          const on = active.kind === "diff";
          const label = diffTab.file.path.split("/").pop() ?? diffTab.file.path;
          const path = diffTab.file.origPath
            ? `${diffTab.file.origPath} → ${diffTab.file.path}`
            : diffTab.file.path;
          return (
            <div
              key={key}
              data-tab-key={key}
              {...tabShell({
                selected: on,
                className: cn(on && "border-t-primary", reorder.drag?.from === index && "shadow-md"),
                title: path,
                style,
                onSelect: () => select(item),
                onClick: () => onTabClick(item),
                onClose: () => closeDiff(),
                onContextMenu: onMenu,
                onPointerDown,
              })}
              tabIndex={0}
            >
              <FileDiff className="size-3.5 shrink-0" />
              <span className="min-w-0 flex-1 truncate">{label}</span>
              <button
                className={closeBtn}
                aria-label={t("common.close")}
                title={t("common.close")}
                onClick={(e) => {
                  e.stopPropagation();
                  closeDiff();
                }}
              >
                <X className="size-3" />
              </button>
            </div>
          );
        }

        const file: FileTabTarget = { projectId: item.projectId, path: item.path };
        const on =
          active.kind === "file" &&
          active.projectId === file.projectId &&
          active.path === file.path;
        const label = file.path.split("/").pop() ?? file.path;
        return (
          <div
            key={key}
            data-tab-key={key}
            {...tabShell({
              selected: on,
              className: cn(on && "border-t-primary", reorder.drag?.from === index && "shadow-md"),
              title: file.path,
              style,
              onSelect: () => select(item),
              onClick: () => onTabClick(item),
              onClose: () => closeFile(file),
              onContextMenu: onMenu,
              onPointerDown,
            })}
            tabIndex={0}
          >
            <FileText className="size-3.5 shrink-0" />
            <span className="min-w-0 flex-1 truncate">{label}</span>
            <button
              className={closeBtn}
              aria-label={t("common.close")}
              title={t("common.close")}
              onClick={(e) => {
                e.stopPropagation();
                closeFile(file);
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
