import {
  useEffect,
  useRef,
  useState,
  type KeyboardEvent,
  type PointerEvent,
  type ReactNode,
} from "react";
import { useTranslation } from "react-i18next";
import { useApp } from "../../store.js";
import {
  PANEL_WIDTH_DEFAULT,
  PANEL_WIDTH_MAX,
  PANEL_WIDTH_MIN,
  resizePanelWidth,
  type ResizeEdge,
} from "../../lib/panelWidth.js";
import { cn } from "@/lib/utils";

const STEP = 10;
const STEP_LARGE = 50;

/**
 * 面板内侧那条可拖的缝。命中区域比视觉宽，压在 border 上 straddling 主区，
 * 所以要给外层槽位一个 z-index，否则后画的 main（尤其是 xterm canvas）会盖住左侧把手。
 */
function ResizeHandle({
  edge,
  value,
  onChange,
  onCommit,
  label,
}: {
  edge: ResizeEdge;
  value: number;
  onChange: (width: number) => void;
  onCommit: () => void;
  label: string;
}) {
  const drag = useRef<{ pointerId: number; startX: number; startWidth: number } | null>(null);
  const [active, setActive] = useState(false);

  useEffect(() => {
    return () => document.body.classList.remove("col-resizing");
  }, []);

  const endDrag = () => {
    if (!drag.current) return;
    drag.current = null;
    setActive(false);
    document.body.classList.remove("col-resizing");
    onCommit();
  };

  const onPointerDown = (e: PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    e.preventDefault();
    // 先记下起点再 capture：自动化派发的 untrusted PointerEvent 上
    // setPointerCapture 会抛，不能挡在 drag.current 赋值前面。
    drag.current = { pointerId: e.pointerId, startX: e.clientX, startWidth: value };
    setActive(true);
    document.body.classList.add("col-resizing");
    try {
      e.currentTarget.setPointerCapture(e.pointerId);
    } catch {
      // 没有 capture 时，只要 pointermove 还打在把手上就仍然能拖
    }
  };

  const onPointerMove = (e: PointerEvent<HTMLDivElement>) => {
    const start = drag.current;
    if (!start || e.pointerId !== start.pointerId) return;
    onChange(
      resizePanelWidth({
        startWidth: start.startWidth,
        startX: start.startX,
        clientX: e.clientX,
        edge,
      })
    );
  };

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const step = e.shiftKey ? STEP_LARGE : STEP;
    const dir = edge === "right" ? 1 : -1;
    let next: number | null = null;
    if (e.key === "ArrowLeft") next = value - dir * step;
    else if (e.key === "ArrowRight") next = value + dir * step;
    else if (e.key === "Home") next = PANEL_WIDTH_MIN;
    else if (e.key === "End") next = PANEL_WIDTH_MAX;
    if (next == null) return;
    e.preventDefault();
    onChange(next);
    onCommit();
  };

  return (
    <div
      role="separator"
      aria-orientation="vertical"
      aria-label={label}
      aria-valuemin={PANEL_WIDTH_MIN}
      aria-valuemax={PANEL_WIDTH_MAX}
      aria-valuenow={value}
      aria-valuetext={`${value}px`}
      title={label}
      tabIndex={0}
      data-active={active || undefined}
      className={cn(
        "absolute inset-y-0 z-10 w-1.5 cursor-col-resize touch-none outline-none",
        "after:absolute after:inset-y-0 after:left-1/2 after:w-0.5 after:-translate-x-1/2 after:bg-transparent",
        "hover:after:bg-ring focus-visible:after:bg-ring data-[active]:after:bg-primary",
        edge === "right" ? "-right-[3px]" : "-left-[3px]"
      )}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={endDrag}
      onPointerCancel={endDrag}
      onLostPointerCapture={endDrag}
      onDoubleClick={() => {
        onChange(PANEL_WIDTH_DEFAULT);
        onCommit();
      }}
      onKeyDown={onKeyDown}
    />
  );
}

/**
 * 左右栏共用的宽度槽。宽度由 store 管，松手才 persist——拖的时候每帧写
 * localStorage 会卡（同步），也没必要。
 */
export function ResizableSlot({
  side,
  children,
}: {
  side: "left" | "right";
  children: ReactNode;
}) {
  const { t } = useTranslation();
  const width = useApp((s) => (side === "left" ? s.sidebarWidth : s.rightWidth));
  const setWidth = useApp((s) => (side === "left" ? s.setSidebarWidth : s.setRightWidth));
  const persistLayout = useApp((s) => s.persistLayout);
  const edge: ResizeEdge = side === "left" ? "right" : "left";

  return (
    <div className="relative z-10 flex min-h-0 shrink-0 flex-col" style={{ width }}>
      {children}
      <ResizeHandle
        edge={edge}
        value={width}
        onChange={setWidth}
        onCommit={persistLayout}
        label={t(side === "left" ? "sidebar.resize" : "rightbar.resize")}
      />
    </div>
  );
}
