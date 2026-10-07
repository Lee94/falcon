import {
  useEffect,
  useImperativeHandle,
  useRef,
  useState,
  type PointerEvent,
  type Ref,
} from "react";
import { useTranslation } from "react-i18next";
import { cn } from "@/lib/utils";
import { positionForThumbTop, thumbGeometry, type ScrollState } from "../lib/termScroll.js";

/**
 * 终端滚动条（ADR 0019）。叠在终端右缘的浮层，与渲染引擎无关。
 *
 * 位置不是前端自己知道的：滚动发生在宿主机的 zellij 里，前端问一次（onRequest()）、
 * 服务端问 zellij 里的插件、再推回来（update）。所以它是「问了才知道」的——滚轮、
 * 悬停、拖动时问；平时不问。拖动时 onRequest(position) 让 zellij 滚过去。
 *
 * 外观照 macOS 的浮层滚动条：平时不画，滚过之后亮一会儿，悬停 / 拖动时常亮。
 */

export interface TerminalScrollbarHandle {
  /** 服务端推来的滚动位置 */
  update(state: ScrollState): void;
  /** 用户刚滚过（滚轮 / 触摸）：亮一会儿 */
  poke(): void;
}

/** 滚过之后亮多久 */
const LINGER_MS = 1200;
/** 亮着时隔多久再问一次：期间输出还在涨，历史长度会变 */
const REFRESH_MS = 1000;
/** 拖动时发 seek 的最小间隔。服务端会把在飞期间的请求合并成最后一个，这里只是少发点 */
const SEEK_INTERVAL_MS = 16;

export function TerminalScrollbar({
  ref,
  onRequest,
}: {
  ref: Ref<TerminalScrollbarHandle>;
  /** 不带参数 = 问位置；带参数 = 滚到「视口下方还剩 seek 行」处 */
  onRequest: (seek?: number) => void;
}) {
  const { t } = useTranslation();
  const trackRef = useRef<HTMLDivElement>(null);
  const [state, setState] = useState<ScrollState | null>(null);
  const [trackPx, setTrackPx] = useState(0);
  const [lit, setLit] = useState(false);
  const [hover, setHover] = useState(false);
  /** 拖动中：滑块先跟手（本地像素），不等服务端回话，否则一顿一顿的 */
  const [drag, setDrag] = useState<{ startY: number; startTop: number; top: number } | null>(null);
  /** 松手后到下一次回话之前，滑块停在松手处，免得先跳回旧位置再跳过来 */
  const [settleTop, setSettleTop] = useState<number | null>(null);
  const litTimer = useRef<number | null>(null);
  const lastSeek = useRef(0);
  const request = useRef(onRequest);
  request.current = onRequest;

  useImperativeHandle(
    ref,
    () => ({
      update: (s) => {
        setState(s);
        setSettleTop(null);
      },
      poke: () => {
        setLit(true);
        if (litTimer.current != null) clearTimeout(litTimer.current);
        litTimer.current = window.setTimeout(() => setLit(false), LINGER_MS);
      },
    }),
    []
  );

  useEffect(
    () => () => {
      if (litTimer.current != null) clearTimeout(litTimer.current);
    },
    []
  );

  const supported = state != null;
  useEffect(() => {
    const el = trackRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setTrackPx(el.clientHeight));
    ro.observe(el);
    setTrackPx(el.clientHeight);
    return () => ro.disconnect();
  }, [supported]);

  const visible = lit || hover || drag != null;
  useEffect(() => {
    if (!visible) return;
    const id = window.setInterval(() => request.current(), REFRESH_MS);
    return () => clearInterval(id);
  }, [visible]);

  // 从没收到过回话 = 这个会话不支持（非持久、或升级前建的），整个不画
  if (!state) return null;
  const geo = thumbGeometry(state, trackPx);
  const top = drag?.top ?? settleTop ?? geo?.top ?? 0;

  const seekTo = (topPx: number, force = false) => {
    if (!geo) return;
    const now = performance.now();
    if (!force && now - lastSeek.current < SEEK_INTERVAL_MS) return;
    lastSeek.current = now;
    request.current(positionForThumbTop(state, trackPx, geo.height, topPx));
  };

  const onPointerDown = (e: PointerEvent<HTMLDivElement>) => {
    if (!geo || e.button !== 0) return;
    // 别让终端拿去开始选区，也别让外层的窗口拖拽接手
    e.preventDefault();
    e.stopPropagation();
    const y = e.clientY - e.currentTarget.getBoundingClientRect().top;
    const grabbed = y >= geo.top && y <= geo.top + geo.height;
    // 点在滑块外：滑块中心跳到点击处，然后照样可以接着拖
    const startTop = grabbed ? geo.top : clamp(y - geo.height / 2, 0, trackPx - geo.height);
    e.currentTarget.setPointerCapture(e.pointerId);
    setDrag({ startY: e.clientY, startTop, top: startTop });
    if (!grabbed) seekTo(startTop, true);
  };

  const onPointerMove = (e: PointerEvent<HTMLDivElement>) => {
    if (!drag || !geo) return;
    const next = clamp(drag.startTop + e.clientY - drag.startY, 0, trackPx - geo.height);
    if (next === drag.top) return;
    setDrag({ ...drag, top: next });
    seekTo(next);
  };

  const onPointerUp = (e: PointerEvent<HTMLDivElement>) => {
    if (!drag) return;
    e.currentTarget.releasePointerCapture(e.pointerId);
    // 节流可能吞掉了最后一下，松手时补发落点
    seekTo(drag.top, true);
    setSettleTop(drag.top);
    setDrag(null);
  };

  return (
    <div
      ref={trackRef}
      role="scrollbar"
      aria-label={t("term.scrollbar")}
      aria-orientation="vertical"
      aria-valuemin={0}
      aria-valuemax={state.length}
      aria-valuenow={state.length - state.position}
      className={cn(
        "absolute top-1.5 right-0 bottom-1.5 z-10 w-2.5 touch-none",
        // 没有可滚的历史（含 vim 这类备用屏程序）时不挡终端最右一列的点击
        !geo && "pointer-events-none"
      )}
      onPointerEnter={() => {
        setHover(true);
        request.current();
      }}
      onPointerLeave={() => setHover(false)}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerCancel={onPointerUp}
    >
      {geo && (
        <div
          className={cn(
            "absolute right-0.5 w-1.5 rounded-full transition-[opacity,background-color] duration-200",
            visible ? "opacity-100" : "opacity-0",
            hover || drag ? "bg-foreground/45" : "bg-foreground/25"
          )}
          style={{ top, height: geo.height }}
        />
      )}
    </div>
  );
}

function clamp(v: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, v));
}
