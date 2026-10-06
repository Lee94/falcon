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
import { FileDiff, FileText, Maximize2, Minimize2, Pin, Plus, X } from "lucide-react";
import type { SessionWithProject } from "@falcon/shared";
import {
  useApp,
  isPendingId,
  layoutColumns,
  activeKey as activeViewKey,
  type DiffTabTarget,
} from "../store.js";
import { DIFF_KEY, fileKey, parsePaneKey, termKey } from "../lib/paneKey.js";
import {
  canvasGroups,
  canvasIndex,
  canvasThumb,
  canvasThumbSize,
  clampColumnWidth,
  clampPaneHeight,
  clampSpot,
  columnMaxWidth,
  dropSpot,
  findPane,
  layoutFrames,
  paneKeys,
  paneMaxHeight,
  CANVAS_GAP_PX,
  COLUMN_MIN_PX,
  PANE_MIN_PX,
  type CanvasGroup,
  type ColumnFrame,
  type ColumnRect,
  type DropSpot,
} from "../lib/layout.js";
import {
  CanvasSwipe,
  WheelAxisLock,
  innerTakesWheel,
  overflowScrollable,
  wheelDeltaPx,
  type OverflowBox,
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

/** 一扇窗口在画布上的位置；null = 此刻不在场（别的项目、别的画布、被最大化挤下去） */
type Frame = { x: number; y: number; width: number; height: number } | null;

/** 拖到画布条上"新画布"那一格 */
const NEW_CANVAS = "+";

/** 窗口在列里的位置决定它裁哪几个角（列的圆角由底下那层岛出） */
type Round = "all" | "top" | "bottom" | "none";

/**
 * 从事件目标往上走到画布（不含画布本身），收集可能接滚轮的内部容器。
 *
 * xterm 的滚动条在 `.xterm-viewport`，是 canvas 的兄弟不是祖先——滚轮打在
 * screen/canvas 上时祖先链里没有它，得从 `.xterm` 再查一次。
 */
function innerOverflowBoxes(target: EventTarget | null, root: HTMLElement): OverflowBox[] {
  const boxes: OverflowBox[] = [];
  const seen = new Set<HTMLElement>();
  const push = (el: HTMLElement) => {
    if (el === root || seen.has(el)) return;
    seen.add(el);
    const style = getComputedStyle(el);
    if (!overflowScrollable(style.overflowX) && !overflowScrollable(style.overflowY)) return;
    boxes.push({
      overflowX: style.overflowX,
      overflowY: style.overflowY,
      scrollLeft: el.scrollLeft,
      scrollTop: el.scrollTop,
      clientWidth: el.clientWidth,
      clientHeight: el.clientHeight,
      scrollWidth: el.scrollWidth,
      scrollHeight: el.scrollHeight,
    });
  };

  let node: Node | null = target instanceof Node ? target : null;
  if (node && node.nodeType !== Node.ELEMENT_NODE) node = (node as CharacterData).parentElement;

  while (node instanceof HTMLElement && node !== root) {
    push(node);
    if (node.classList.contains("xterm-screen") || node.classList.contains("xterm")) {
      const host = node.classList.contains("xterm") ? node : node.parentElement;
      const vp = host?.querySelector<HTMLElement>(":scope > .xterm-viewport");
      if (vp) push(vp);
    }
    node = node.parentElement;
  }
  return boxes;
}

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

/**
 * 窗口叫什么（菜单标题、拖拽标签、画布条的提示）。不能空着——它要说清"你在操作谁"，
 * 空闲会话兜底回项目名。标题栏不用它：那一格空着是正常的，见 TermPaneInfo。
 */
function paneLabel(
  key: string,
  sessions: SessionWithProject[],
  diffTab: DiffTabTarget | null,
  t: (k: string) => string
): string {
  const item = parsePaneKey(key);
  if (!item) return "";
  if (item.kind === "terminal") {
    const term = sessions.find((s) => s.id === item.id);
    return (term && (sessionTitle(term) ?? term.projectName)) ?? t("tab.creating");
  }
  if (item.kind === "file") return basename(item.path);
  return diffTab ? basename(diffTab.file.path) : t("tab.diff");
}

// ---------------- 分隔条 ----------------

/**
 * 两列之间那道缝。拖它只钉左边那一列的宽度，右边的自适应列跟着让位（画布不横向滚动，
 * 一块画布正好一屏宽）；最多拖到给右边每列留够下限。双击还原成自适应。
 */
function ColumnSplitter({
  columnId,
  width,
  max,
  style,
}: {
  columnId: string;
  width: number;
  /** 最多拖到多宽：右边每列都要留够下限（画布不再横向滚动，拖宽一列就是挤右边的） */
  max: number;
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
        setColumnWidth(columnId, clampColumnWidth(start.startWidth + (e.clientX - start.startX), max));
      }}
      onPointerUp={end}
      onPointerCancel={end}
      onLostPointerCapture={end}
      onKeyDown={(e) => {
        const step = e.shiftKey ? 50 : 10;
        if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
        e.preventDefault();
        setColumnWidth(columnId, clampColumnWidth(width + (e.key === "ArrowRight" ? step : -step), max));
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
 * **不横向滚动**：列分在一块块画布上，一次只显示一块，一块正好一屏宽（列平分视口）。
 * 新开的列在当前画布排不下就自动另起一块（store 的 settleCanvases）。有两块以上时底下
 * 出一条画布条：点数字切过去，把窗口拖到数字上就挪到那块，拖到 ＋ 上就新开一块。
 * 两指横滑 / 快捷键也能翻。
 *
 * **每扇窗口都绝对定位，DOM 顺序恒定**（跟着 tabs 走，不跟着排布走）。这不是图省事：
 * xterm 的画布换过父节点之后渲染尺寸就错了——实测拖到另一列后整屏空白，补一帧
 * （fit + 重建字形）能把内容找回来，但字被拉成两倍大（画布位图与 CSS 尺寸的 dpr
 * 比例对不上）。只改 left/top/width/height 的话，拖窗口在 xterm 眼里就只是一次
 * resize，而 resize 是它天天在处理的事。
 *
 * 同理，不在场的窗口（别的项目的、别的画布上的、被最大化挤下去的）不卸载也不换父节点，
 * 只是挪到画布外边：TerminalView 一卸载就 dispose 适配器并关 WS，再挂回来要重连 + replay。
 * 别的画布上的窗口停在外边时**保持它在自己那块画布上的尺寸**，切画布对它只是挪个位置，
 * PTY 不会收到 resize。
 *
 * 横向滚轮自己接管（见 lib/termCanvas.ts 的 WheelAxisLock / CanvasSwipe）：
 * - 终端引擎会把带 deltaY 的事件全吃掉并 preventDefault，触控板两指滑动又几乎没有纯
 *   横向的事件，所以按手势定轴：横向手势整段在 capture 阶段截下来翻画布，终端一条都
 *   看不见；纵向手势原样交给终端；
 * - 窗口内部（文件 / 差异的 overflow-auto、xterm 的 viewport）还能沿手势方向滚时让给
 *   内部，这一整段手势都不翻画布。
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
  const canvasId = useApp((s) => s.canvasId);
  const canvasFocus = useApp((s) => s.canvasFocus);
  const focusPane = useApp((s) => s.focusPane);
  const movePane = useApp((s) => s.movePane);
  const movePaneToCanvas = useApp((s) => s.movePaneToCanvas);
  const showCanvasById = useApp((s) => s.showCanvas);
  const setCanvasWidth = useApp((s) => s.setCanvasWidth);

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

  /** 当前视图的画布；正在显示的是第 current 块 */
  const groups = useMemo(() => canvasGroups(visible), [visible]);
  const current = canvasIndex(groups, activeKey, canvasId);
  const group: CanvasGroup | undefined = groups[current];

  /** 最大化：只留活动那一扇。它不在场（切到了别的项目）就当没最大化，不能让画布空掉 */
  const columns = useMemo(() => {
    const shown = group?.columns ?? [];
    if (!zoomed || !activeKey) return shown;
    const at = findPane(shown, activeKey);
    if (!at) return shown;
    const col = shown[at.col]!;
    return [{ ...col, basis: null, panes: [col.panes[at.index]!] }];
  }, [group, zoomed, activeKey]);

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

  const rootRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLDivElement>(null);
  const paneRefs = useRef(new Map<string, HTMLElement>());
  const colRefs = useRef(new Map<string, HTMLElement>());
  const targetRefs = useRef(new Map<string, HTMLElement>());

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

  // 新列排不排得下按画布宽算，报给 store（layout effect：首帧之前就分好画布，不闪一下）
  useLayoutEffect(() => {
    setCanvasWidth(viewport.width);
  }, [viewport.width, setCanvasWidth]);

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

  /**
   * 不在场的窗口停在外边时用的尺寸：别的画布上的（以及被最大化挤下去的）按它在自己
   * 那块画布上的尺寸，切回来不用 resize；别的项目的给画布大小。
   */
  const parkedSizes = useMemo(() => {
    const map = new Map<string, { width: number; height: number }>();
    for (const g of groups) {
      for (const col of layoutFrames(g.columns, viewport).columns) {
        for (const pane of col.panes) {
          if (!map.has(pane.key)) map.set(pane.key, { width: col.width, height: pane.height });
        }
      }
    }
    return map;
  }, [groups, viewport]);

  /** 固定列里的窗口：标题栏挂个图钉，右键菜单里那一项打勾 */
  const pinnedKeys = useMemo(
    () => new Set(storeColumns.flatMap((c) => (c.pinned ? c.panes.map((p) => p.key) : []))),
    [storeColumns]
  );

  /** 每扇（非固定的）窗口在第几块画布上：右键菜单里"移到画布 N"要排除它自己那块 */
  const canvasOf = useMemo(() => {
    const map = new Map<string, number>();
    groups.forEach((g, i) => {
      for (const c of g.columns) if (!c.pinned) for (const p of c.panes) map.set(p.key, i);
    });
    return map;
  }, [groups]);
  const canvasIds = useMemo(() => groups.map((g) => g.id), [groups]);
  /** 独占一块画布的窗口（那块上没有别的窗口，固定列不算）："移到新画布"对它等于没动 */
  const soloKeys = useMemo(() => {
    const set = new Set<string>();
    for (const g of groups) {
      const keys = g.columns.filter((c) => !c.pinned).flatMap((c) => c.panes.map((p) => p.key));
      if (keys.length === 1) set.add(keys[0]!);
    }
    return set;
  }, [groups]);

  const shownKeys = useMemo(() => new Set(paneKeys(columns)), [columns]);
  /** 只有一扇窗口时最大化没有可见效果，按钮藏起来；已经最大化的要留着出口 */
  const zoomable = zoomed || shownKeys.size > 1;
  /** 两扇以上才需要标出输入落在哪一扇 */
  const markActive = shownKeys.size > 1;
  const shownList = useMemo(() => paneKeys(columns), [columns]);

  useEffect(() => {
    const el = rootRef.current;
    if (!el) return;
    const lock = new WheelAxisLock();
    const swipe = new CanvasSwipe();
    const onCapture = (e: WheelEvent) => {
      const axis = lock.classify({ deltaX: e.deltaX, deltaY: e.deltaY, timeStamp: e.timeStamp });
      if (axis !== "x") return;
      const dx = wheelDeltaPx(e.deltaX, e.deltaMode, 16, el.clientWidth);
      // 文件长行、差异、xterm viewport 自己还能横滚时别抢——内部优先，这段手势也不翻画布
      if (innerTakesWheel(innerOverflowBoxes(e.target, el), "x", dx)) {
        swipe.hold(e.timeStamp);
        return;
      }
      // 横向手势整段归画布。翻不动（只有一块、已经到头）也要 preventDefault：macOS
      // 两指横滑在没人接的时候是浏览器的前进 / 后退手势，一不小心就把整个工作台翻走了
      e.stopPropagation();
      e.preventDefault();
      const step = swipe.push(dx, e.timeStamp);
      if (step) useApp.getState().stepCanvas(step);
    };
    el.addEventListener("wheel", onCapture, { capture: true, passive: false });
    return () => el.removeEventListener("wheel", onCapture, { capture: true });
  }, []);

  // ---- 拖窗口 ----
  const [drag, setDrag] = useState<{
    key: string;
    label: string;
    x: number;
    y: number;
    /** 落在画布条的哪一格上（画布 id 或 NEW_CANVAS）；null = 落在画布上，看 spot */
    target: string | null;
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

  /** 指针落在画布条的哪一格上 */
  const hitTarget = useCallback((x: number, y: number): string | null => {
    for (const [id, el] of targetRefs.current) {
      const r = el.getBoundingClientRect();
      if (x >= r.left - 2 && x <= r.right + 2 && y >= r.top - 4 && y <= r.bottom + 4) return id;
    }
    return null;
  }, []);

  const registerTarget = useCallback((id: string, el: HTMLElement | null) => {
    if (el) targetRefs.current.set(id, el);
    else targetRefs.current.delete(id);
  }, []);

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
        target: hitTarget(e.clientX, e.clientY),
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
        const target = hitTarget(e.clientX, e.clientY);
        if (target) movePaneToCanvas(d.key, target === NEW_CANVAS ? null : target);
        else movePane(d.key, clampSpot(columns, dropSpot({ x: e.clientX, y: e.clientY }, measure())));
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
  }, [columns, measure, hitTarget, movePane, movePaneToCanvas]);

  useEffect(() => () => document.body.classList.remove("pane-dragging"), []);

  const registerPane = useCallback((key: string, el: HTMLElement | null) => {
    if (el) paneRefs.current.set(key, el);
    else paneRefs.current.delete(key);
  }, []);

  /** 画布条：两块以上常驻在画布上方；只有一块时拖窗口才在画布顶上浮出一个"新画布"的落点 */
  const pager = groups.length > 1;

  return (
    <div
      ref={rootRef}
      className={cn("absolute inset-0 flex flex-col", !showCanvas && "pointer-events-none")}
    >
      {pager && (
        <CanvasPager
          groups={groups}
          current={current}
          viewport={viewport}
          activeKey={activeKey}
          canvasFocus={canvasFocus}
          dragging={drag != null}
          newTarget={drag != null && !soloKeys.has(drag.key)}
          hot={drag?.target ?? null}
          sessions={sessions}
          diffTab={diffTab}
          onShow={showCanvasById}
          register={registerTarget}
        />
      )}
      <div className="relative min-h-0 flex-1">
        <div
          ref={canvasRef}
          data-term-canvas=""
          className={cn(
            // 画布本身不着色也不留边：每一列各自是一座岛，列之间露出的就是窗口底。
            // -inset-px + p-px 是给岛的 1px 描边与投影留的呼吸位：列顶满高度，画布又要
            // 裁掉停在外边的窗口（overflow 一裁两轴都裁），不留这 1px 的话画在 border box
            // 外沿的东西全被切掉。往外扩 1px 再往里收 1px，列的尺寸与位置分毫不动。
            // clip 而不是 hidden：hidden 仍是滚动容器，焦点落进停在外边的窗口时浏览器
            // 会把它滚出来；clip 连程序化滚动都没有
            "absolute -inset-px overflow-clip p-px"
          )}
        >
          <div className="relative size-full">
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
                  parked={parkedSizes.get(key) ?? viewport}
                  isActive={activeKey === key}
                  markActive={markActive}
                  dimmed={drag?.key === key}
                  visible={showCanvas && shownKeys.has(key) && activeKey === key}
                  zoomed={zoomed}
                  zoomable={zoomable}
                  pinned={pinnedKeys.has(key)}
                  shownKeys={shownList}
                  canvasIds={canvasIds}
                  canvasAt={canvasOf.get(key) ?? null}
                  aloneOnCanvas={soloKeys.has(key)}
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
                right={frames.columns.length - ci - 1}
                width={viewport.width}
                height={viewport.height}
              />
            ))}
          </div>

          {drag && !drag.target && <DropIndicator spot={drag.spot} rects={drag.rects} />}
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

          {drag && !pager && !soloKeys.has(drag.key) && (
            <div className="pointer-events-none absolute inset-x-0 top-9 z-40 flex justify-center">
              <NewCanvasTarget hot={drag.target === NEW_CANVAS} register={registerTarget} lifted />
            </div>
          )}
        </div>
      </div>

    </div>
  );
}

function Splitters({
  col,
  right,
  width,
  height,
}: {
  col: ColumnFrame;
  /** 它右边还有几列；0 = 最后一列，右边没有把手 */
  right: number;
  width: number;
  height: number;
}) {
  return (
    <>
      {right > 0 && (
        <ColumnSplitter
          columnId={col.id}
          width={col.width}
          max={columnMaxWidth({ width, left: col.x, right })}
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

// ---------------- 画布条 ----------------

/** 缩略图的外框：比图本身每边大 2px，留出着色底与圆角 */
const thumbBox =
  "grid shrink-0 place-items-center rounded-[4px] p-0.5 outline-none transition-colors focus-visible:ring-1 focus-visible:ring-ring";

/** 缩略图里窗口块的颜色：[普通窗口, 着重的那扇]（着重 = 正在输入的那扇 / 这块上次停的那扇） */
const THUMB_TONE = {
  idle: ["bg-muted-foreground/30", "bg-muted-foreground/65"],
  current: ["bg-tint-strong/35", "bg-tint-strong"],
  hot: ["bg-primary-foreground/45", "bg-primary-foreground/85"],
} as const;

/**
 * 画布条：主区顶上靠左、窗口底上的一排**布局缩略图**（不成岛，与右侧活动栏同一个做法），
 * 只在两块以上时出现。每张图按这块画布真实的列与窗口等比缩小（lib/layout 的
 * canvasThumb），一眼看出哪块是"左右两个终端"、哪块是"一列叠三扇"。当前那块着色，
 * 正在输入的那扇加深；别的画布加深它上次停的那扇——点过去焦点就落在那。
 * 点图切过去；拖窗口时图是落点（挪到那块画布），末尾多一格 ＋（新开一块）。
 */
function CanvasPager({
  groups,
  current,
  viewport,
  activeKey,
  canvasFocus,
  dragging,
  newTarget,
  hot,
  sessions,
  diffTab,
  onShow,
  register,
}: {
  groups: CanvasGroup[];
  current: number;
  /** 画布内容盒：缩略图的宽高比与几何都照它 */
  viewport: { width: number; height: number };
  activeKey: string | null;
  canvasFocus: Record<string, string>;
  dragging: boolean;
  /** 拖拽中出不出"新画布"那一格：拖的窗口独占一块画布时挪去新画布等于没动，不出 */
  newTarget: boolean;
  /** 拖拽中指针压着的那一格 */
  hot: string | null;
  sessions: SessionWithProject[];
  diffTab: DiffTabTarget | null;
  onShow: (id: string) => void;
  register: (id: string, el: HTMLElement | null) => void;
}) {
  const { t } = useTranslation();
  const keysHint = t("canvas.switchHint", {
    next: chord("nextCanvas"),
    prev: chord("prevCanvas"),
  });
  const size = canvasThumbSize(viewport);
  const thumbs = useMemo(
    () => groups.map((g) => canvasThumb(g.columns, viewport, size)),
    // size 由 viewport 算出来，跟着它变
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [groups, viewport]
  );
  return (
    <nav
      aria-label={t("canvas.pager")}
      // 靠左排：贴着画布的左边起头。别改回居中——拖拽一开始末尾才冒出 ＋，居中的话整排
      // 会左移，指针底下的那一格就换了人
      className="mb-1.5 flex h-5 shrink-0 items-center gap-1.5"
    >
      {groups.map((g, i) => {
        const on = i === current;
        const isHot = dragging && hot === g.id;
        // 提示里列出这块画布上有什么（固定列每块都有，不列）
        const names = g.columns
          .filter((c) => !c.pinned)
          .flatMap((c) => c.panes.map((p) => paneLabel(p.key, sessions, diffTab, t)));
        const label = t("canvas.page", { n: i + 1 });
        const title = t("canvas.pageTitle", { n: i + 1, names: names.join(" · ") });
        const tone = THUMB_TONE[isHot ? "hot" : on ? "current" : "idle"];
        const emphasis = on ? activeKey : (canvasFocus[g.id] ?? null);
        return (
          <button
            key={g.id}
            ref={(el) => register(g.id, el)}
            className={cn(
              thumbBox,
              // 拖拽命中时不能带 hover 的底色：指针正压在上面，hover 会盖过高亮
              isHot ? "bg-primary" : on ? "bg-tint" : "hover:bg-background/60"
            )}
            aria-label={label}
            aria-current={on ? "page" : undefined}
            title={`${title}\n${keysHint}`}
            onClick={() => onShow(g.id)}
          >
            <span className="relative block" style={size}>
              {thumbs[i]!.map((r) => (
                <span
                  key={r.key}
                  className={cn("absolute rounded-[1px]", tone[r.key === emphasis ? 1 : 0])}
                  style={{ left: r.x, top: r.y, width: r.width, height: r.height }}
                />
              ))}
            </span>
          </button>
        );
      })}
      {dragging && newTarget && (
        <NewCanvasTarget hot={hot === NEW_CANVAS} register={register} size={size} />
      )}
    </nav>
  );
}

/**
 * 拖窗口时"新开一块画布"的落点。画布条上是一张虚线框的空白缩略图；只有一块画布、
 * 没有画布条时浮在画布顶上，带字、像拖拽标签那样托起来（lifted）。
 */
function NewCanvasTarget({
  hot,
  register,
  size,
  lifted,
}: {
  hot: boolean;
  register: (id: string, el: HTMLElement | null) => void;
  /** 画布条上那一格与缩略图同尺寸 */
  size?: { width: number; height: number };
  lifted?: boolean;
}) {
  const { t } = useTranslation();
  if (!lifted && size) {
    return (
      <div
        ref={(el) => register(NEW_CANVAS, el)}
        aria-label={t("canvas.new")}
        title={t("canvas.new")}
        className={cn(
          "grid place-items-center rounded-[4px] border border-dashed [&_svg]:size-3",
          hot
            ? "border-primary bg-primary text-primary-foreground"
            : "border-muted-foreground/50 text-muted-foreground"
        )}
        // 外框与缩略图按钮一样大：图本身 + 每边 2px
        style={{ width: size.width + 4, height: size.height + 4 }}
      >
        <Plus />
      </div>
    );
  }
  return (
    <div
      ref={(el) => register(NEW_CANVAS, el)}
      className={cn(
        "island island-lifted flex h-7 items-center gap-1 whitespace-nowrap px-2.5 text-xs",
        hot ? "bg-primary text-primary-foreground" : "text-muted-foreground"
      )}
    >
      <Plus className="size-3" />
      {t("canvas.new")}
    </div>
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
  canvasIds,
  canvasAt,
  aloneOnCanvas,
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
  /** 当前视图的各块画布（"移到画布 N"） */
  canvasIds: string[];
  /** 它在第几块画布上；固定列里的、别的项目的是 null */
  canvasAt: number | null;
  aloneOnCanvas: boolean;
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
  const movePaneToCanvas = useApp((s) => s.movePaneToCanvas);
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

  const label = paneLabel(paneKey, sessions, diffTab, t);

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
    // 换画布：别的每一块各一项，再加"新画布"——已经独占一块的窗口挪去新画布等于没动，不给
    ...[
      ...canvasIds.flatMap((id, i) =>
        i === canvasAt || canvasIds.length < 2
          ? []
          : [{ label: t("pane.moveToCanvas", { n: i + 1 }), onSelect: () => movePaneToCanvas(paneKey, id) }]
      ),
      ...(canvasAt == null || !aloneOnCanvas
        ? [{ label: t("pane.moveToNewCanvas"), onSelect: () => movePaneToCanvas(paneKey, null) }]
        : []),
    ].map((entry, i) => (i === 0 ? { ...entry, separated: true } : entry)),
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
