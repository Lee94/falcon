import {
  lazy,
  Suspense,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type PointerEvent as ReactPointerEvent,
} from "react";
import { useTranslation } from "react-i18next";
import { FileDiff, FileText, Maximize2, Minimize2, Pin, X } from "lucide-react";
import { useApp, isPendingId, layoutColumns, activeKey as activeViewKey } from "../store.js";
import { DIFF_KEY, fileKey, parsePaneKey, termKey } from "../lib/paneKey.js";
import {
  clampColumnWidth,
  clampPaneHeight,
  clampSpot,
  dropSpot,
  findPane,
  layoutFrames,
  paneKeys,
  paneMaxHeight,
  CANVAS_GAP_PX,
  COLUMN_MIN_PX,
  PANE_MIN_PX,
  type ColumnFrame,
  type ColumnRect,
  type DropSpot,
} from "../lib/layout.js";
import {
  WHEEL_GESTURE_GAP_MS,
  WheelAxisLock,
  revealScrollLeft,
  settleTarget,
  wheelDeltaPx,
} from "../lib/termCanvas.js";
import { connLabel, sshBar } from "../lib/hostColor.js";
import { sessionTitle } from "../lib/sessionTitle.js";
import { chord } from "../lib/shortcuts.js";
import { cn } from "@/lib/utils";
import { StatusMark } from "./common/StatusMark.js";
import { openContextMenu } from "./common/Menu.js";

const TerminalView = lazy(() =>
  import("./TerminalView.js").then((m) => ({ default: m.TerminalView }))
);
const PendingPane = lazy(() =>
  import("./TerminalView.js").then((m) => ({ default: m.PendingPane }))
);
const GitDiffView = lazy(() =>
  import("./GitDiffView.js").then((m) => ({ default: m.GitDiffView }))
);
const FileView = lazy(() => import("./FileView.js").then((m) => ({ default: m.FileView })));

const headerBtn =
  "grid size-5 shrink-0 place-items-center rounded-md text-muted-foreground outline-none hover:bg-accent hover:text-foreground focus-visible:ring-1 focus-visible:ring-ring [&_svg]:size-3";

/** 拖动多少像素才算"在拖窗口"而不是"点了下标题栏" */
const DRAG_SLOP_PX = 4;
/** 把手压在缝上，够点得中又不挡住窗口 */
const HANDLE_PX = 8;

/** 一扇窗口在画布上的位置；null = 此刻不在场（别的项目、被最大化挤下去） */
type Frame = { x: number; y: number; width: number; height: number } | null;

/** 窗口在列里的位置决定它裁哪几个角（列的圆角由底下那层岛出） */
type Round = "all" | "top" | "bottom" | "none";

// ---------------- 标题栏 ----------------

/**
 * 终端窗口的标题栏内容：自动标题 + 工作目录（或连接串）。
 *
 * 标题这一格空着是正常的（没起名、也没在跑东西的 shell）——右边紧跟着完整工作
 * 目录，编一个 "Terminal 3" 填进去只会挤掉路径。
 *
 * 常驻渲染（不在场的窗口也带着）：窗口的几何在前台 / 后台必须一致，否则停在后台时
 * fit 量出来的行数会多一行，回到前台又缩一行，PTY 白白收两次 resize。
 */
function TermPaneInfo({ id, isActive }: { id: string; isActive: boolean }) {
  const { t } = useTranslation();
  const pendingEntry = useApp((s) => s.pending.find((p) => p.id === id));
  const session = useApp((s) => (isPendingId(id) ? undefined : s.sessions.find((x) => x.id === id)));
  const projectId = session?.projectId ?? pendingEntry?.projectId;
  const project = useApp((s) => s.projects.find((p) => p.id === projectId));
  const system = useApp((s) => s.system);

  const name = session ? sessionTitle(session) : t("tab.creating");
  const conn = connLabel(project, system, t("project.typeLocalShort"));
  const where = project?.workingDir ?? conn;
  const state = pendingEntry
    ? pendingEntry.error
      ? ("dead" as const)
      : ("creating" as const)
    : (session?.state ?? "creating");
  const bar = sshBar(project);

  return (
    <>
      {bar && <span className="h-3.5 w-[3px] shrink-0 rounded-full" style={{ background: bar }} />}
      {state !== "active" && (
        <StatusMark
          state={state}
          label={pendingEntry?.error ? t("session.createFailedTitle") : undefined}
        />
      )}
      {name && <span className="shrink-0 truncate font-medium">{name}</span>}
      <PaneWhere text={where} isActive={isActive} />
    </>
  );
}

function PaneWhere({ text, isActive }: { text: string; isActive: boolean }) {
  return (
    <span
      className={cn(
        "min-w-0 flex-1 truncate font-mono text-[11px]",
        isActive ? "text-muted-foreground" : "text-muted-foreground/70"
      )}
    >
      {text}
    </span>
  );
}

function basename(path: string): string {
  return path.split("/").pop() || path;
}

// ---------------- 分隔条 ----------------

/**
 * 两列之间那道缝。拖它只钉左边那一列的宽度——画布是横向可滚的，右边不必跟着让位，
 * 也就不会出现"拖宽一列，另一列被挤到没法用"。双击还原成自适应。
 */
function ColumnSplitter({
  columnId,
  width,
  style,
}: {
  columnId: string;
  width: number;
  style: CSSProperties;
}) {
  const { t } = useTranslation();
  const setColumnWidth = useApp((s) => s.setColumnWidth);
  const persistLayout = useApp((s) => s.persistLayout);
  const drag = useRef<{ pointerId: number; startX: number; startWidth: number } | null>(null);
  const [active, setActive] = useState(false);

  useEffect(() => () => document.body.classList.remove("col-resizing"), []);

  const end = () => {
    if (!drag.current) return;
    drag.current = null;
    setActive(false);
    document.body.classList.remove("col-resizing");
    persistLayout();
  };

  return (
    <div
      role="separator"
      aria-orientation="vertical"
      aria-label={t("pane.resizeColumn")}
      aria-valuenow={Math.round(width)}
      title={t("pane.resizeColumn")}
      tabIndex={0}
      data-active={active || undefined}
      style={style}
      className={cn(
        "absolute z-20 cursor-col-resize touch-none outline-none",
        "after:absolute after:inset-y-3 after:left-1/2 after:w-0.5 after:-translate-x-1/2 after:rounded-full after:bg-transparent after:transition-colors",
        "hover:after:bg-ring focus-visible:after:bg-ring data-[active]:after:bg-primary"
      )}
      onPointerDown={(e) => {
        if (e.button !== 0) return;
        e.preventDefault();
        drag.current = { pointerId: e.pointerId, startX: e.clientX, startWidth: width };
        setActive(true);
        document.body.classList.add("col-resizing");
        try {
          e.currentTarget.setPointerCapture(e.pointerId);
        } catch {
          // 自动化派发的 untrusted 事件上 capture 会抛；只要 move 还打在把手上就仍能拖
        }
      }}
      onPointerMove={(e) => {
        const start = drag.current;
        if (!start || e.pointerId !== start.pointerId) return;
        setColumnWidth(columnId, clampColumnWidth(start.startWidth + (e.clientX - start.startX)));
      }}
      onPointerUp={end}
      onPointerCancel={end}
      onLostPointerCapture={end}
      onKeyDown={(e) => {
        const step = e.shiftKey ? 50 : 10;
        if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
        e.preventDefault();
        setColumnWidth(columnId, clampColumnWidth(width + (e.key === "ArrowRight" ? step : -step)));
        persistLayout();
      }}
      onDoubleClick={() => {
        setColumnWidth(columnId, null);
        persistLayout();
      }}
    />
  );
}

/** 列内两扇窗口之间的缝。拖它只钉上面那扇的高度，下面的继续自适应 */
function PaneSplitter({
  paneKey,
  height,
  max,
  style,
}: {
  paneKey: string;
  height: number;
  max: number;
  style: CSSProperties;
}) {
  const { t } = useTranslation();
  const setPaneHeight = useApp((s) => s.setPaneHeight);
  const persistLayout = useApp((s) => s.persistLayout);
  const drag = useRef<{ pointerId: number; startY: number; startHeight: number } | null>(null);
  const [active, setActive] = useState(false);

  useEffect(() => () => document.body.classList.remove("row-resizing"), []);

  const end = () => {
    if (!drag.current) return;
    drag.current = null;
    setActive(false);
    document.body.classList.remove("row-resizing");
    persistLayout();
  };

  return (
    <div
      role="separator"
      aria-orientation="horizontal"
      aria-label={t("pane.resizeHeight")}
      aria-valuenow={Math.round(height)}
      title={t("pane.resizeHeight")}
      tabIndex={0}
      data-active={active || undefined}
      style={style}
      className={cn(
        "absolute z-20 cursor-row-resize touch-none outline-none",
        "after:absolute after:inset-x-3 after:top-1/2 after:h-0.5 after:-translate-y-1/2 after:rounded-full after:bg-transparent after:transition-colors",
        "hover:after:bg-ring focus-visible:after:bg-ring data-[active]:after:bg-primary"
      )}
      onPointerDown={(e) => {
        if (e.button !== 0) return;
        e.preventDefault();
        drag.current = { pointerId: e.pointerId, startY: e.clientY, startHeight: height };
        setActive(true);
        document.body.classList.add("row-resizing");
        try {
          e.currentTarget.setPointerCapture(e.pointerId);
        } catch {
          // 同 ColumnSplitter
        }
      }}
      onPointerMove={(e) => {
        const start = drag.current;
        if (!start || e.pointerId !== start.pointerId) return;
        setPaneHeight(paneKey, clampPaneHeight(start.startHeight + (e.clientY - start.startY), max));
      }}
      onPointerUp={end}
      onPointerCancel={end}
      onLostPointerCapture={end}
      onKeyDown={(e) => {
        const step = e.shiftKey ? 50 : 10;
        if (e.key !== "ArrowUp" && e.key !== "ArrowDown") return;
        e.preventDefault();
        setPaneHeight(paneKey, clampPaneHeight(height + (e.key === "ArrowDown" ? step : -step), max));
        persistLayout();
      }}
      onDoubleClick={() => {
        setPaneHeight(paneKey, null);
        persistLayout();
      }}
    />
  );
}

// ---------------- 画布 ----------------

/**
 * 主区工作画布：窗口排成从左到右的列，每列从上到下若干扇，列宽与窗口高度都能拖，
 * 抓标题栏能把窗口拖到别的列或另起一列。排布在 store.columns，算坐标的纯函数在
 * lib/layout.ts。
 *
 * **每扇窗口都绝对定位，DOM 顺序恒定**（跟着 tabs 走，不跟着排布走）。这不是图省事：
 * xterm 的画布换过父节点之后渲染尺寸就错了——实测拖到另一列后整屏空白，补一帧
 * （fit + 重建字形）能把内容找回来，但字被拉成两倍大（画布位图与 CSS 尺寸的 dpr
 * 比例对不上）。只改 left/top/width/height 的话，拖窗口在 xterm 眼里就只是一次
 * resize，而 resize 是它天天在处理的事。
 *
 * 同理，不在场的窗口（别的项目的、被最大化挤下去的）不卸载也不换父节点，只是挪到
 * 画布外边：TerminalView 一卸载就 dispose 适配器并关 WS，再挂回来要重连 + replay。
 *
 * 横向滚动自己接管（见 lib/termCanvas.ts 的 WheelAxisLock）：
 * - 终端引擎会把带 deltaY 的事件全吃掉并 preventDefault，浏览器原生的横向滚动
 *   在终端上根本走不到；触控板两指滑动又几乎没有纯横向的事件；
 * - 所以按手势定轴：横向手势整段在 capture 阶段截下来改 scrollLeft，终端一条都
 *   看不见；纵向手势原样交给终端，只在 bubble 阶段兜住它没吃掉的事件；
 * - 不用 CSS scroll-snap：手动改 scrollLeft 会被当作程序化滚动立刻吸附。
 */
export function WorkCanvas() {
  const { t } = useTranslation();
  const tabs = useApp((s) => s.tabs);
  const sessions = useApp((s) => s.sessions);
  const pending = useApp((s) => s.pending);
  const selectedProjectId = useApp((s) => s.selectedProjectId);
  const fileTab = useApp((s) => s.fileTab);
  const diffTab = useApp((s) => s.diffTab);
  const storeColumns = useApp((s) => s.columns);
  const active = useApp((s) => s.active);
  const zoomed = useApp((s) => s.termZoomed);
  const focusPane = useApp((s) => s.focusPane);
  const movePane = useApp((s) => s.movePane);

  const visible = useMemo(
    () =>
      layoutColumns({
        tabs,
        sessions,
        pending,
        selectedProjectId,
        fileTab,
        diffTab,
        columns: storeColumns,
      }),
    [tabs, sessions, pending, selectedProjectId, fileTab, diffTab, storeColumns]
  );

  const activeKey = activeViewKey(active);
  const showCanvas = active.kind !== "overview" && active.kind !== "project";

  /** 最大化：只留活动那一扇。它不在场（切到了别的项目）就当没最大化，不能让画布空掉 */
  const columns = useMemo(() => {
    if (!zoomed || !activeKey) return visible;
    const at = findPane(visible, activeKey);
    if (!at) return visible;
    const col = visible[at.col]!;
    return [{ ...col, basis: null, panes: [col.panes[at.index]!] }];
  }, [visible, zoomed, activeKey]);

  /**
   * 画布上所有窗口的 React 顺序。**只跟着数据源走，不跟着排布走**——排布一变就重排
   * DOM 的话，xterm 的画布会被 insertBefore 挪走，渲染尺寸就毁了（见组件顶上的注释）。
   */
  const allKeys = useMemo(() => {
    const keys = tabs.map(termKey);
    if (fileTab) keys.push(fileKey(fileTab));
    if (diffTab) keys.push(DIFF_KEY);
    return keys;
  }, [tabs, fileTab, diffTab]);

  const canvasRef = useRef<HTMLDivElement>(null);
  const paneRefs = useRef(new Map<string, HTMLElement>());
  const colRefs = useRef(new Map<string, HTMLElement>());
  /** 首次揭示直接跳（页面刚打开不该看到一段滚动动画），之后的切窗口平滑滚过去 */
  const revealed = useRef(false);

  // 画布内容盒尺寸：所有几何都从它算（扣掉留给岛描边的那 1px 内边距）
  const [viewport, setViewport] = useState({ width: 0, height: 0 });
  useLayoutEffect(() => {
    const el = canvasRef.current;
    if (!el) return;
    const apply = () => {
      const cs = getComputedStyle(el);
      const padX = (parseFloat(cs.paddingLeft) || 0) + (parseFloat(cs.paddingRight) || 0);
      const padY = (parseFloat(cs.paddingTop) || 0) + (parseFloat(cs.paddingBottom) || 0);
      const width = el.clientWidth - padX;
      const height = el.clientHeight - padY;
      setViewport((v) => (v.width === width && v.height === height ? v : { width, height }));
    };
    apply();
    const ro = new ResizeObserver(apply);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const frames = useMemo(() => layoutFrames(columns, viewport), [columns, viewport]);

  /** key → 位置 / 圆角 / 所在列。不在场的窗口不在表里 */
  const placed = useMemo(() => {
    const map = new Map<string, { frame: NonNullable<Frame>; round: Round; col: ColumnFrame }>();
    for (const col of frames.columns) {
      col.panes.forEach((pane, i) => {
        const round: Round =
          col.panes.length === 1
            ? "all"
            : i === 0
              ? "top"
              : i === col.panes.length - 1
                ? "bottom"
                : "none";
        map.set(pane.key, {
          frame: { x: col.x, y: pane.y, width: col.width, height: pane.height },
          round,
          col,
        });
      });
    }
    return map;
  }, [frames]);

  /** 固定列里的窗口：标题栏挂个图钉，右键菜单里那一项打勾 */
  const pinnedKeys = useMemo(
    () => new Set(storeColumns.flatMap((c) => (c.pinned ? c.panes.map((p) => p.key) : []))),
    [storeColumns]
  );

  const shownKeys = useMemo(() => new Set(paneKeys(columns)), [columns]);
  /** 只有一扇窗口时最大化没有可见效果，按钮藏起来；已经最大化的要留着出口 */
  const zoomable = zoomed || shownKeys.size > 1;
  /** 两扇以上才需要标出输入落在哪一扇 */
  const markActive = shownKeys.size > 1;
  const shownList = useMemo(() => paneKeys(columns), [columns]);

  /** 列的成员与顺序：给"把活动窗口滚进视口"当依赖，数组每轮轮询都是新引用，不能直接依赖 */
  const shape = columns.map((c) => `${c.id}:${c.panes.map((p) => p.key).join(",")}`).join("|");

  useLayoutEffect(() => {
    if (!showCanvas || !activeKey) return;
    const canvas = canvasRef.current;
    const spot = placed.get(activeKey);
    if (!canvas || !spot) return;
    const next = revealScrollLeft({
      scrollLeft: canvas.scrollLeft,
      viewport: viewport.width,
      left: spot.col.x,
      width: spot.col.width,
    });
    if (next != null) {
      canvas.scrollTo({ left: next, behavior: revealed.current ? "smooth" : "auto" });
    }
    revealed.current = true;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [showCanvas, activeKey, shape, viewport.width]);

  useEffect(() => {
    const el = canvasRef.current;
    if (!el) return;
    const lock = new WheelAxisLock();
    let settleTimer: number | null = null;
    const scrollable = () => el.scrollWidth > el.clientWidth + 1;

    // 手势（含惯性尾巴）停下来之后，离哪列的左边近就吸过去；离得远就停在原地
    const scheduleSettle = () => {
      if (settleTimer != null) clearTimeout(settleTimer);
      settleTimer = window.setTimeout(() => {
        settleTimer = null;
        if (!scrollable()) return;
        const cols = Array.from(el.querySelectorAll<HTMLElement>("[data-col-id]"));
        if (cols.length === 0) return;
        const proximity = Math.min(80, (cols[0]!.offsetWidth || 0) * 0.25);
        const target = settleTarget(
          el.scrollLeft,
          cols.map((c) => c.offsetLeft),
          proximity,
          el.scrollWidth - el.clientWidth
        );
        if (target != null) el.scrollTo({ left: target, behavior: "smooth" });
      }, WHEEL_GESTURE_GAP_MS + 30);
    };

    const onCapture = (e: WheelEvent) => {
      const axis = lock.classify({ deltaX: e.deltaX, deltaY: e.deltaY, timeStamp: e.timeStamp });
      if (axis !== "x") return;
      // 横向手势整段归画布。没得滚也要 preventDefault：macOS 两指横滑在没人接的
      // 时候是浏览器的前进 / 后退手势，一不小心就把整个工作台翻走了
      e.stopPropagation();
      e.preventDefault();
      if (!scrollable()) return;
      el.scrollLeft += wheelDeltaPx(e.deltaX, e.deltaMode, 16, el.clientWidth);
      scheduleSettle();
    };
    // 纵向手势：终端没吃掉的事件（滚到头了、标题栏上、空隙里）会落到浏览器原生
    // 滚动，事件里混着的 deltaX 就会让画布横漂几像素。画布上没有别的纵向可滚，
    // 拦掉不损失任何东西
    const onBubble = (e: WheelEvent) => {
      if (!e.defaultPrevented && scrollable()) e.preventDefault();
    };
    el.addEventListener("wheel", onCapture, { capture: true, passive: false });
    el.addEventListener("wheel", onBubble, { passive: false });
    return () => {
      el.removeEventListener("wheel", onCapture, { capture: true });
      el.removeEventListener("wheel", onBubble);
      if (settleTimer != null) clearTimeout(settleTimer);
    };
  }, []);

  // ---- 拖窗口 ----
  const [drag, setDrag] = useState<{
    key: string;
    label: string;
    x: number;
    y: number;
    spot: DropSpot;
    /** 落点指示照着画的矩形，与算落点用的是同一份量测 */
    rects: ColumnRect[];
  } | null>(null);
  const dragRef = useRef<{
    key: string;
    label: string;
    pointerId: number;
    startX: number;
    startY: number;
    moved: boolean;
  } | null>(null);

  /** 量出当前各列与各窗口的矩形（视口坐标，与指针同一套） */
  const measure = useCallback((): ColumnRect[] => {
    return columns.map((col) => {
      const box = colRefs.current.get(col.id)?.getBoundingClientRect();
      return {
        left: box?.left ?? 0,
        right: box?.right ?? 0,
        panes: col.panes.map((p) => {
          const pb = paneRefs.current.get(p.key)?.getBoundingClientRect();
          return { top: pb?.top ?? 0, bottom: pb?.bottom ?? 0 };
        }),
      };
    });
  }, [columns]);

  const startDrag = useCallback(
    (key: string, label: string) => (e: ReactPointerEvent) => {
      if (e.button !== 0) return;
      dragRef.current = {
        key,
        label,
        pointerId: e.pointerId,
        startX: e.clientX,
        startY: e.clientY,
        moved: false,
      };
    },
    []
  );

  // 监听常驻：pointerdown 只写 ref 不触发重渲，挂在"拖拽中"这个 state 上就永远等不到
  useEffect(() => {
    const onMove = (e: PointerEvent) => {
      const d = dragRef.current;
      if (!d || e.pointerId !== d.pointerId) return;
      if (!d.moved) {
        if (Math.abs(e.clientX - d.startX) + Math.abs(e.clientY - d.startY) < DRAG_SLOP_PX) return;
        d.moved = true;
        document.body.classList.add("pane-dragging");
      }
      const rects = measure();
      setDrag({
        key: d.key,
        label: d.label,
        x: e.clientX,
        y: e.clientY,
        // 指示线也要认固定列，不然松手之后窗口会跳到指示线左边去
        spot: clampSpot(columns, dropSpot({ x: e.clientX, y: e.clientY }, rects)),
        rects,
      });
    };
    const onUp = (e: PointerEvent) => {
      const d = dragRef.current;
      if (!d || e.pointerId !== d.pointerId) return;
      dragRef.current = null;
      document.body.classList.remove("pane-dragging");
      if (d.moved) {
        movePane(d.key, clampSpot(columns, dropSpot({ x: e.clientX, y: e.clientY }, measure())));
      }
      setDrag(null);
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
    window.addEventListener("pointercancel", onUp);
    return () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      window.removeEventListener("pointercancel", onUp);
    };
  }, [columns, measure, movePane]);

  useEffect(() => () => document.body.classList.remove("pane-dragging"), []);

  const registerPane = useCallback((key: string, el: HTMLElement | null) => {
    if (el) paneRefs.current.set(key, el);
    else paneRefs.current.delete(key);
  }, []);

  return (
    <div
      ref={canvasRef}
      data-term-canvas=""
      className={cn(
        // 画布本身不着色也不留边：每一列各自是一座岛，列之间露出的就是窗口底。
        // -inset-px + p-px 是给岛的 1px 描边与投影留的呼吸位：列顶满高度，画布又是
        // 滚动容器（overflow 一裁两轴都裁），不留这 1px 的话画在 border box 外沿的
        // 东西全被切掉。往外扩 1px 再往里收 1px，列的尺寸与位置分毫不动。
        "absolute -inset-px overflow-x-auto overflow-y-hidden overscroll-x-contain p-px [scrollbar-width:thin]",
        !showCanvas && "pointer-events-none"
      )}
    >
      <div className="relative h-full" style={{ width: Math.max(frames.width, viewport.width) }}>
        {/* 列的底：岛的圆角、描边与面板底色都在这一层，窗口浮在上面 */}
        {frames.columns.map((col) => (
          <div
            key={col.id}
            ref={(el) => {
              if (el) colRefs.current.set(col.id, el);
              else colRefs.current.delete(col.id);
            }}
            data-col-id={col.id}
            className="island absolute inset-y-0"
            style={{ left: col.x, width: col.width }}
          />
        ))}

        {allKeys.map((key) => {
          const spot = placed.get(key);
          return (
            <PaneView
              key={key}
              paneKey={key}
              frame={spot ? spot.frame : null}
              round={spot?.round ?? "all"}
              parked={viewport}
              isActive={activeKey === key}
              markActive={markActive}
              dimmed={drag?.key === key}
              visible={showCanvas && shownKeys.has(key) && activeKey === key}
              zoomed={zoomed}
              zoomable={zoomable}
              pinned={pinnedKeys.has(key)}
              shownKeys={shownList}
              onFocus={focusPane}
              onDragStart={startDrag}
              registerPane={registerPane}
            />
          );
        })}

        {/* 缝上的把手：列之间一条竖的，列内每两扇窗口之间一条横的 */}
        {frames.columns.map((col, ci) => (
          <Splitters
            key={col.id}
            col={col}
            last={ci === frames.columns.length - 1}
            height={viewport.height}
          />
        ))}
      </div>

      {drag && <DropIndicator spot={drag.spot} rects={drag.rects} />}
      {drag && (
        <div
          className="island island-lifted pointer-events-none fixed z-50 flex h-7 items-center gap-1.5 px-2.5 text-xs opacity-90"
          style={{ left: drag.x + 12, top: drag.y + 12 }}
        >
          {drag.label}
        </div>
      )}

      {columns.length === 0 && showCanvas && (
        <p className="absolute inset-0 grid place-items-center text-xs text-muted-foreground">
          {t("canvas.empty")}
        </p>
      )}
    </div>
  );
}

function Splitters({ col, last, height }: { col: ColumnFrame; last: boolean; height: number }) {
  return (
    <>
      {!last && (
        <ColumnSplitter
          columnId={col.id}
          width={col.width}
          style={{
            left: col.x + col.width - HANDLE_PX / 2,
            top: 0,
            bottom: 0,
            width: CANVAS_GAP_PX + HANDLE_PX,
          }}
        />
      )}
      {col.panes.slice(0, -1).map((pane, i) => (
        <PaneSplitter
          key={pane.key}
          paneKey={pane.key}
          height={pane.height}
          max={paneMaxHeight({
            columnHeight: height,
            above: pane.y,
            below: col.panes.length - i - 1,
          })}
          style={{
            left: col.x,
            width: col.width,
            top: pane.y + pane.height - HANDLE_PX / 2,
            height: HANDLE_PX,
          }}
        />
      ))}
    </>
  );
}

function DropIndicator({ spot, rects }: { spot: DropSpot; rects: ColumnRect[] }) {
  if (spot.kind === "column") {
    const before = rects[spot.at];
    const after = rects[spot.at - 1];
    const x = before ? before.left - 4 : after ? after.right + 4 : 0;
    const top = before?.panes[0]?.top ?? after?.panes[0]?.top ?? 0;
    const bottom =
      before?.panes[before.panes.length - 1]?.bottom ??
      after?.panes[after.panes.length - 1]?.bottom ??
      0;
    return (
      <div
        className="pointer-events-none fixed z-40 w-0.5 rounded-full bg-primary"
        style={{ left: x, top, height: Math.max(0, bottom - top) }}
      />
    );
  }
  const rect = rects[spot.col];
  if (!rect) return null;
  const pane = rect.panes[spot.index];
  const prev = rect.panes[spot.index - 1];
  const y = pane ? pane.top : prev ? prev.bottom : 0;
  return (
    <div
      className="pointer-events-none fixed z-40 h-0.5 rounded-full bg-primary"
      style={{ left: rect.left, top: y - 1, width: Math.max(0, rect.right - rect.left) }}
    />
  );
}

// ---------------- 一扇窗口 ----------------

const ROUND_CLASS: Record<Round, string> = {
  all: "rounded-lg",
  top: "rounded-t-lg",
  bottom: "rounded-b-lg",
  none: "",
};

function PaneView({
  paneKey,
  frame,
  round,
  parked,
  isActive,
  markActive,
  dimmed,
  visible,
  zoomed,
  zoomable,
  pinned,
  shownKeys,
  onFocus,
  onDragStart,
  registerPane,
}: {
  paneKey: string;
  frame: Frame;
  round: Round;
  /** 不在场时停在画布外用的尺寸：与在场时同量级，fit 出来的行数才不会来回跳 */
  parked: { width: number; height: number };
  isActive: boolean;
  markActive: boolean;
  dimmed: boolean;
  visible: boolean;
  zoomed: boolean;
  zoomable: boolean;
  /** 所在的列钉在了最右（见 lib/layout 的 ColumnLayout.pinned） */
  pinned: boolean;
  /** 此刻在场的窗口，从左到右、列内从上到下——"关闭其他 / 左 / 右"照它算 */
  shownKeys: string[];
  onFocus: (key: string) => void;
  onDragStart: (key: string, label: string) => (e: ReactPointerEvent) => void;
  registerPane: (key: string, el: HTMLElement | null) => void;
}) {
  const { t } = useTranslation();
  const closeTab = useApp((s) => s.closeTab);
  const detachTab = useApp((s) => s.detachTab);
  const closeFile = useApp((s) => s.closeFile);
  const closeDiff = useApp((s) => s.closeDiff);
  const closePaneKeys = useApp((s) => s.closePaneKeys);
  const newTerminal = useApp((s) => s.newTerminal);
  const toggleTermZoom = useApp((s) => s.toggleTermZoom);
  const togglePinPane = useApp((s) => s.togglePinPane);
  const openRename = useApp((s) => s.openRename);
  const sessions = useApp((s) => s.sessions);
  const pendingList = useApp((s) => s.pending);
  const diffTab = useApp((s) => s.diffTab);

  const item = parsePaneKey(paneKey);
  if (!item) return null;

  const projectId =
    item.kind === "terminal"
      ? (sessions.find((s) => s.id === item.id)?.projectId ??
        pendingList.find((p) => p.id === item.id)?.projectId)
      : item.kind === "file"
        ? item.projectId
        : diffTab?.projectId;

  const term = item.kind === "terminal" ? sessions.find((s) => s.id === item.id) : undefined;
  const label =
    item.kind === "terminal"
      ? // 菜单标题不能空着（它要说清"你在操作谁"），空闲会话兜底回项目名
        ((term && (sessionTitle(term) ?? term.projectName)) ?? t("tab.creating"))
      : item.kind === "file"
        ? basename(item.path)
        : diffTab
          ? basename(diffTab.file.path)
          : t("tab.diff");

  const close = (shift: boolean) => {
    if (item.kind === "terminal") {
      if (shift) detachTab(item.id);
      else void closeTab(item.id);
      return;
    }
    if (item.kind === "file") closeFile({ projectId: item.projectId, path: item.path });
    else closeDiff();
  };

  const zoom = () => {
    onFocus(paneKey);
    toggleTermZoom();
  };

  /** 标题栏上的按钮不许把 pointerdown 漏给窗口：关掉非活动窗口时 active 不该先跳过去再回落 */
  const isolate = (e: { stopPropagation: () => void }) => e.stopPropagation();

  const index = shownKeys.indexOf(paneKey);
  const menuItems = [
    ...(projectId
      ? [
          {
            label: t("tab.newToRight"),
            kbd: chord("newTerminal"),
            onSelect: () => void newTerminal(projectId, { after: paneKey }),
          },
        ]
      : []),
    {
      label: t("pane.pinRight"),
      checked: pinned,
      onSelect: () => togglePinPane(paneKey),
    },
    ...(item.kind === "terminal" && !isPendingId(item.id)
      ? [{ label: t("session.rename"), onSelect: () => openRename(item.id) }]
      : []),
    {
      label: item.kind === "terminal" ? t("tab.close") : t("tab.closeView"),
      separated: true,
      onSelect: () => close(false),
    },
    ...(item.kind === "terminal"
      ? [{ label: t("tab.detach"), onSelect: () => detachTab(item.id) }]
      : []),
    ...(shownKeys.length > 1 && index >= 0
      ? [
          {
            label: t("tab.closeOthers"),
            separated: true,
            onSelect: () => void closePaneKeys(shownKeys.filter((k) => k !== paneKey)),
          },
          ...(index > 0
            ? [
                {
                  label: t("tab.closeToLeft"),
                  onSelect: () => void closePaneKeys(shownKeys.slice(0, index)),
                },
              ]
            : []),
          ...(index < shownKeys.length - 1
            ? [
                {
                  label: t("tab.closeToRight"),
                  onSelect: () => void closePaneKeys(shownKeys.slice(index + 1)),
                },
              ]
            : []),
        ]
      : []),
  ];

  // 不在场：挪到画布外边继续跑。不能走 transform（会撑开可滚区域），也不能 width:0
  // （fit 会把 0×0 发给 PTY），所以给一个与画布同量级的盒子，挪到左边很远的地方
  const style: CSSProperties = frame
    ? { left: frame.x, top: frame.y, width: frame.width, height: frame.height }
    : {
        left: -10000,
        top: 0,
        width: Math.max(parked.width, COLUMN_MIN_PX),
        height: Math.max(parked.height, PANE_MIN_PX),
        visibility: "hidden",
        pointerEvents: "none",
      };

  return (
    <div
      ref={(el) => registerPane(paneKey, el)}
      data-pane-key={paneKey}
      className={cn(
        "absolute flex flex-col overflow-hidden",
        // 岛的圆角在底下那层，窗口自己也要裁一下——终端底色是实心矩形，不裁就把角盖住了
        ROUND_CLASS[round],
        dimmed && "opacity-50"
      )}
      style={style}
      onPointerDown={() => {
        if (frame && !isActive) onFocus(paneKey);
      }}
    >
      <div
        className={cn(
          "flex h-7 shrink-0 cursor-grab items-center gap-1.5 border-b px-2 text-xs select-none active:cursor-grabbing",
          isActive ? "text-foreground" : "text-muted-foreground",
          // 输入落在哪一扇：标题栏铺一层极弱的着色，不是一圈光晕
          isActive && markActive && "bg-tint"
        )}
        title={t("pane.dragHint")}
        onPointerDown={onDragStart(paneKey, label)}
        onContextMenu={(e) => openContextMenu(e, menuItems)}
        onDoubleClick={() => {
          if (zoomable) zoom();
        }}
      >
        {item.kind === "terminal" ? (
          <TermPaneInfo id={item.id} isActive={isActive} />
        ) : item.kind === "file" ? (
          <>
            <FileText className="size-3.5 shrink-0 text-muted-foreground" />
            <span className="shrink-0 truncate font-medium">{basename(item.path)}</span>
            <PaneWhere text={item.path} isActive={isActive} />
          </>
        ) : (
          <>
            <FileDiff className="size-3.5 shrink-0 text-muted-foreground" />
            <span className="shrink-0 truncate font-medium">
              {diffTab ? basename(diffTab.file.path) : t("tab.diff")}
            </span>
            <PaneWhere text={diffTab?.file.path ?? ""} isActive={isActive} />
          </>
        )}
        {/* lucide 的组件把 title 透传成 svg 属性，浏览器不给它 tooltip——套一层 span */}
        {pinned && (
          <span
            className="grid shrink-0 place-items-center text-muted-foreground"
            title={t("pane.pinned")}
            aria-label={t("pane.pinned")}
          >
            <Pin className="size-3" />
          </span>
        )}
        {zoomable && (
          <button
            className={headerBtn}
            aria-label={zoomed ? t("pane.restore") : t("pane.maximize")}
            aria-pressed={zoomed}
            title={zoomed ? t("pane.restoreHint") : t("pane.maximizeHint")}
            onPointerDown={isolate}
            onDoubleClick={isolate}
            onClick={zoom}
          >
            {zoomed ? <Minimize2 /> : <Maximize2 />}
          </button>
        )}
        <button
          className={headerBtn}
          aria-label={item.kind === "terminal" ? t("tab.closeHint") : t("common.close")}
          title={
            item.kind === "terminal"
              ? `${t("tab.closeHint")}\n${t("tab.detachHint")}`
              : t("common.close")
          }
          onPointerDown={isolate}
          onDoubleClick={isolate}
          onClick={(e) => close(e.shiftKey)}
        >
          <X />
        </button>
      </div>

      <Suspense fallback={null}>
        {item.kind === "terminal" ? (
          isPendingId(item.id) ? (
            <PendingPane pendingId={item.id} />
          ) : (
            <TerminalView sessionId={item.id} visible={visible} />
          )
        ) : item.kind === "file" ? (
          <FileView target={{ projectId: item.projectId, path: item.path }} />
        ) : (
          <GitDiffView />
        )}
      </Suspense>
    </div>
  );
}
