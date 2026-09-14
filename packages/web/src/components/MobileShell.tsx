import {
  lazy,
  Suspense,
  useEffect,
  useRef,
  useState,
  useSyncExternalStore,
  type TouchEvent,
} from "react";
import { useTranslation } from "react-i18next";
import { ChevronDown, Ellipsis, Plus } from "lucide-react";
import { useApp, isPendingId } from "../store.js";
import { sshBar } from "../lib/hostColor.js";
import { useActions } from "../lib/useActions.js";
import { useSessionLabel } from "../lib/useSessionLabel.js";
import { siblingSession, swipeDir } from "../lib/mobileNav.js";
import {
  extraKeySeq,
  getTermInput,
  isCtrlArmed,
  setCtrlArmed,
  subscribeCtrl,
  type ExtraKey,
} from "../lib/termInput.js";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { menuAnchor } from "./common/Menu.js";
import { StatusMark } from "./common/StatusMark.js";

const TerminalView = lazy(() =>
  import("./TerminalView.js").then((m) => ({ default: m.TerminalView }))
);
const PendingPane = lazy(() =>
  import("./TerminalView.js").then((m) => ({ default: m.PendingPane }))
);
const MobileSwitcher = lazy(() =>
  import("./MobileSwitcher.js").then((m) => ({ default: m.MobileSwitcher }))
);

/**
 * 移动壳（方案 A）：48px 顶栏 + 终端 + 键位条，其余高度全给终端。
 * 没有 TabBar / 侧栏 / 右栏——切换会话走顶栏呼出的底部抽屉，或在终端区横滑。
 */
export function MobileShell() {
  const { t } = useTranslation();
  const tabs = useApp((s) => s.tabs);
  const active = useApp((s) => s.active);
  const sessions = useApp((s) => s.sessions);
  const sessionLabel = useSessionLabel();
  const pending = useApp((s) => s.pending);
  const projects = useApp((s) => s.projects);
  const selectedProjectId = useApp((s) => s.selectedProjectId);
  const openSession = useApp((s) => s.openSession);
  const newTerminal = useApp((s) => s.newTerminal);
  const openMenu = useApp((s) => s.openMenu);
  const actions = useActions();
  const [switcherOpen, setSwitcherOpen] = useState(false);

  const activeId = active.kind === "terminal" ? active.sessionId : null;
  const session =
    activeId && !isPendingId(activeId) ? sessions.find((s) => s.id === activeId) : undefined;
  const pendingEntry = activeId ? pending.find((p) => p.id === activeId) : undefined;
  const project = projects.find(
    (p) => p.id === (session?.projectId ?? pendingEntry?.projectId)
  );
  const bar = sshBar(project);

  /** ＋ 与桌面 TabBar 同一套优先级：选中项目 → 当前会话所属项目 → 第一个 */
  const targetProjectId =
    selectedProjectId ?? session?.projectId ?? pendingEntry?.projectId ?? projects[0]?.id ?? null;

  // iOS 的软键盘是覆盖层，不改 layout viewport——不处理的话键位条会被埋在
  // 键盘底下。键盘弹出时把壳压到可视视口高度（Android 走 viewport meta 的
  // interactive-widget=resizes-content，这里天然不触发）。真机行为需实测。
  const [vvHeight, setVvHeight] = useState<number | null>(null);
  useEffect(() => {
    const vv = window.visualViewport;
    if (!vv) return;
    const apply = () => {
      const shrunk = window.innerHeight - vv.height > 80;
      setVvHeight(shrunk ? Math.round(vv.height) : null);
      // 键盘顶起页面时 Safari 可能偷偷滚动 body，把壳拉回原点
      if (shrunk) window.scrollTo(0, 0);
    };
    vv.addEventListener("resize", apply);
    return () => vv.removeEventListener("resize", apply);
  }, []);

  // 终端区横滑 = 同项目相邻会话。start 记在 ref 里，move 不拦截——
  // 竖向滚动要原样归 xterm / zellij，只在抬手时判定一次
  const touchRef = useRef<{ x: number; y: number; t: number } | null>(null);
  const onTouchStart = (e: TouchEvent) => {
    if (e.touches.length !== 1) {
      touchRef.current = null;
      return;
    }
    const p = e.touches[0]!;
    touchRef.current = { x: p.clientX, y: p.clientY, t: e.timeStamp };
  };
  const onTouchEnd = (e: TouchEvent) => {
    const start = touchRef.current;
    touchRef.current = null;
    if (!start || !session) return;
    const p = e.changedTouches[0];
    if (!p) return;
    const dir = swipeDir(start, { x: p.clientX, y: p.clientY, t: e.timeStamp });
    if (dir === 0) return;
    const sibling = siblingSession(sessions, session.id, dir);
    if (sibling) openSession(sibling.id);
  };

  const markState = pendingEntry
    ? pendingEntry.error
      ? ("dead" as const)
      : ("creating" as const)
    : session?.state;

  const iconBtn =
    "grid size-11 shrink-0 place-items-center rounded-md text-muted-foreground outline-none active:bg-accent/50 focus-visible:ring-1 focus-visible:ring-ring disabled:pointer-events-none disabled:opacity-40 [&_svg]:size-4.5";

  return (
    <div
      className="flex min-h-0 flex-1 flex-col overflow-hidden bg-background"
      style={vvHeight != null ? { maxHeight: vvHeight } : undefined}
    >
      {/* 状态栏安全区与顶栏同色（viewport-fit=cover 后内容延伸到刘海下面） */}
      <div className="shrink-0 bg-sidebar" style={{ paddingTop: "env(safe-area-inset-top)" }}>
        <div className="flex h-12 items-center gap-0.5 border-b pr-1 pl-1">
          <button
            className="flex h-11 min-w-0 items-center gap-2 rounded-md px-2.5 outline-none active:bg-accent/50 focus-visible:ring-1 focus-visible:ring-ring"
            aria-label={t("mobile.openSwitcher")}
            aria-haspopup="dialog"
            onClick={() => setSwitcherOpen(true)}
          >
            {bar && (
              <span className="h-3.5 w-[3px] shrink-0 rounded-full" style={{ background: bar }} />
            )}
            <span className="truncate text-sm font-medium">
              {project?.name ?? t("appName")}
            </span>
            {(session || pendingEntry) && (
              <span className="truncate text-[13px] text-muted-foreground">
                {session ? sessionLabel(session) : t("tab.creating")}
              </span>
            )}
            {/* 与 TabBar 同一条规矩：运行中不摆状态记号，异常才值得占位置 */}
            {markState && markState !== "active" && (
              <StatusMark
                state={markState}
                label={pendingEntry?.error ? t("session.createFailedTitle") : undefined}
              />
            )}
            <ChevronDown className="size-3.5 shrink-0 text-muted-foreground" />
          </button>
          <span className="flex-1" />
          <button
            className={iconBtn}
            aria-label={t("sidebar.newTerminal")}
            disabled={!targetProjectId}
            onClick={() => targetProjectId && void newTerminal(targetProjectId)}
          >
            <Plus />
          </button>
          <button
            className={iconBtn}
            aria-label={t("session.moreActions")}
            disabled={!session}
            onClick={(e) =>
              session && openMenu({ ...menuAnchor(e), items: actions.sessionMenuItems(session) })
            }
          >
            <Ellipsis />
          </button>
        </div>
      </div>

      <div
        className="relative min-h-0 flex-1"
        onTouchStart={onTouchStart}
        onTouchEnd={onTouchEnd}
        onTouchCancel={() => (touchRef.current = null)}
      >
        {!activeId && (
          <div className="absolute inset-0 flex flex-col items-center justify-center gap-3 px-8 text-center">
            <p className="text-sm font-medium">{t("mobile.emptyTitle")}</p>
            <p className="text-xs leading-relaxed text-muted-foreground">
              {t("mobile.emptyBody")}
            </p>
            <Button variant="outline" size="sm" onClick={() => setSwitcherOpen(true)}>
              {t("mobile.openSwitcher")}
            </Button>
          </div>
        )}
        {/* 非活动 pane 只是移出视口，绝不卸载——与桌面 App.tsx 同一套 keep-alive
            （xterm 卸载 = 重连重放；translate 出视口才能让它真正暂停渲染） */}
        {tabs.map((id) => {
          const isActive = activeId === id;
          return (
            <div
              key={id}
              className={cn(
                "absolute inset-0 flex flex-col",
                isActive ? "" : "invisible -translate-x-[200%]"
              )}
            >
              <Suspense fallback={null}>
                {isPendingId(id) ? (
                  <PendingPane pendingId={id} />
                ) : (
                  <TerminalView sessionId={id} visible={isActive} />
                )}
              </Suspense>
            </div>
          );
        })}
      </div>

      {session && <MobileKeys sessionId={session.id} />}

      {switcherOpen && (
        <Suspense fallback={null}>
          <MobileSwitcher onClose={() => setSwitcherOpen(false)} />
        </Suspense>
      )}
    </div>
  );
}

function KeyCap({
  label,
  pressed,
  ariaLabel,
  onPress,
}: {
  label: string;
  pressed?: boolean;
  ariaLabel?: string;
  onPress: () => void;
}) {
  return (
    <button
      className={cn(
        "h-10 min-w-0 flex-1 rounded-md border bg-popover font-mono text-xs text-foreground outline-none active:bg-accent focus-visible:ring-1 focus-visible:ring-ring",
        pressed && "border-ring bg-accent"
      )}
      aria-label={ariaLabel ?? label}
      aria-pressed={pressed}
      // 不抢终端的焦点：焦点一走软键盘就收起来了
      onPointerDown={(e) => e.preventDefault()}
      onClick={onPress}
    >
      {label}
    </button>
  );
}

/** 键位条：补软键盘打不出的键。Ctrl 是粘滞键，点亮后对下一击生效 */
function MobileKeys({ sessionId }: { sessionId: string }) {
  const { t } = useTranslation();
  const ctrl = useSyncExternalStore(subscribeCtrl, isCtrlArmed);

  const press = (key: ExtraKey) => {
    const port = getTermInput(sessionId);
    if (!port) return;
    const withCtrl = isCtrlArmed();
    if (withCtrl) setCtrlArmed(false);
    port.send(extraKeySeq(key, port.appCursorKeys(), withCtrl));
  };

  return (
    <div
      className="flex shrink-0 items-center gap-1.5 border-t bg-sidebar px-2 pt-2"
      style={{ paddingBottom: "max(0.5rem, env(safe-area-inset-bottom))" }}
    >
      <KeyCap label="Esc" onPress={() => press("esc")} />
      <KeyCap label="Tab" onPress={() => press("tab")} />
      <KeyCap
        label="Ctrl"
        pressed={ctrl}
        ariaLabel={t("mobile.keyCtrl")}
        onPress={() => setCtrlArmed(!isCtrlArmed())}
      />
      <KeyCap label="←" onPress={() => press("left")} />
      <KeyCap label="↓" onPress={() => press("down")} />
      <KeyCap label="↑" onPress={() => press("up")} />
      <KeyCap label="→" onPress={() => press("right")} />
    </div>
  );
}
