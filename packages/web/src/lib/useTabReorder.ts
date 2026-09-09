import { useCallback, useEffect, useRef, useState, type CSSProperties, type PointerEvent as ReactPointerEvent } from "react";
import { dropIndex, tabShift } from "./tabStrip.js";

const THRESHOLD = 6;
const EDGE = 40;

type DragLive = {
  pointerId: number;
  key: string;
  from: number;
  startX: number;
  startScroll: number;
  widths: number[];
  lefts: number[];
};

export type TabDrag = {
  key: string;
  from: number;
  to: number;
  dx: number;
  width: number;
};

/**
 * Chrome 式 tab 拖拽：过阈值才算拖，被拖的跟指针走，邻居让出空位，
 * 靠近两端自动滚。顺序只在松手时提交；Esc 取消。
 */
export function useTabReorder(opts: {
  keys: string[];
  enabled: boolean;
  onMove: (from: number, to: number) => void;
}) {
  const { keys, enabled, onMove } = opts;
  const listRef = useRef<HTMLDivElement>(null);
  const live = useRef<DragLive | null>(null);
  const lastX = useRef(0);
  const moved = useRef(false);
  const suppressClick = useRef(false);
  const [drag, setDrag] = useState<TabDrag | null>(null);
  const keysRef = useRef(keys);
  keysRef.current = keys;
  const onMoveRef = useRef(onMove);
  onMoveRef.current = onMove;

  const clearListeners = useRef<(() => void) | null>(null);

  const stop = useCallback((commit: boolean) => {
    clearListeners.current?.();
    clearListeners.current = null;
    const cur = live.current;
    live.current = null;
    moved.current = false;
    document.body.classList.remove("tab-dragging");
    if (commit && cur) {
      const to = dragTo(cur, lastX.current, listRef.current);
      if (to !== cur.from) {
        suppressClick.current = true;
        onMoveRef.current(cur.from, to);
      }
    }
    setDrag(null);
  }, []);

  useEffect(() => () => stop(false), [stop]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && live.current) {
        e.preventDefault();
        stop(false);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [stop]);

  const onPointerDown = useCallback(
    (e: ReactPointerEvent<HTMLElement>, key: string) => {
      if (!enabled || e.button !== 0) return;
      if ((e.target as HTMLElement).closest("button, input")) return;
      const list = listRef.current;
      if (!list) return;
      const els = [...list.querySelectorAll<HTMLElement>("[data-tab-key]")];
      const from = keysRef.current.indexOf(key);
      if (from < 0 || els.length !== keysRef.current.length) return;
      const listRect = list.getBoundingClientRect();
      live.current = {
        pointerId: e.pointerId,
        key,
        from,
        startX: e.clientX,
        startScroll: list.scrollLeft,
        widths: els.map((el) => el.getBoundingClientRect().width),
        lefts: els.map(
          (el) => el.getBoundingClientRect().left - listRect.left + list.scrollLeft
        ),
      };
      lastX.current = e.clientX;
      moved.current = false;

      const onMovePtr = (ev: PointerEvent) => {
        const cur = live.current;
        if (!cur || ev.pointerId !== cur.pointerId) return;
        lastX.current = ev.clientX;
        const dx0 = ev.clientX - cur.startX;
        if (!moved.current) {
          if (Math.abs(dx0) < THRESHOLD) return;
          moved.current = true;
          document.body.classList.add("tab-dragging");
          list.setPointerCapture?.(ev.pointerId);
        }
        ev.preventDefault();
        const to = dragTo(cur, ev.clientX, list);
        const dx = ev.clientX - cur.startX + (list.scrollLeft - cur.startScroll);
        setDrag({
          key: cur.key,
          from: cur.from,
          to,
          dx,
          width: cur.widths[cur.from] ?? 0,
        });
        edgeScroll(list, ev.clientX);
      };

      const onUp = (ev: PointerEvent) => {
        if (live.current && ev.pointerId !== live.current.pointerId) return;
        stop(moved.current);
      };

      window.addEventListener("pointermove", onMovePtr);
      window.addEventListener("pointerup", onUp);
      window.addEventListener("pointercancel", onUp);
      clearListeners.current = () => {
        window.removeEventListener("pointermove", onMovePtr);
        window.removeEventListener("pointerup", onUp);
        window.removeEventListener("pointercancel", onUp);
      };
    },
    [enabled, stop]
  );

  const styleFor = useCallback(
    (index: number): CSSProperties | undefined => {
      if (!drag) return undefined;
      const isDragged = index === drag.from;
      const shift = isDragged ? drag.dx : tabShift(drag.from, drag.to, index, drag.width);
      return {
        transform: shift ? `translateX(${shift}px)` : undefined,
        zIndex: isDragged ? 20 : undefined,
        position: "relative",
        transition: isDragged ? "none" : "transform 120ms ease-out",
      };
    },
    [drag]
  );

  const consumeClick = useCallback(() => {
    if (!suppressClick.current) return false;
    suppressClick.current = false;
    return true;
  }, []);

  return { listRef, drag, onPointerDown, styleFor, consumeClick };
}

function dragTo(cur: DragLive, clientX: number, list: HTMLDivElement | null): number {
  if (!list) return cur.from;
  const dx = clientX - cur.startX + (list.scrollLeft - cur.startScroll);
  const origin = cur.lefts[cur.from] ?? 0;
  const width = cur.widths[cur.from] ?? 0;
  return dropIndex(origin + width / 2 + dx, cur.lefts, cur.widths);
}

function edgeScroll(list: HTMLDivElement, clientX: number) {
  const rect = list.getBoundingClientRect();
  if (clientX < rect.left + EDGE) {
    list.scrollLeft -= Math.max(2, Math.ceil((EDGE - (clientX - rect.left)) / 3));
  } else if (clientX > rect.right - EDGE) {
    list.scrollLeft += Math.max(2, Math.ceil((EDGE - (rect.right - clientX)) / 3));
  }
}
