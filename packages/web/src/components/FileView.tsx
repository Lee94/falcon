import {
  lazy,
  Suspense,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
} from "react";
import { useTranslation } from "react-i18next";
import { Code2, Download, ExternalLink, Eye, FileText, RefreshCw, ZoomIn, ZoomOut } from "lucide-react";
import type { WorkspaceFile } from "@falcon/shared";
import { api } from "../api.js";
import { useApp, type FileTabTarget } from "../store.js";
import { langForPath, splitCodeLines, useHighlight } from "../lib/highlight.js";
import { rawUrl } from "../lib/rawUrl.js";
import { triggerDownload } from "../lib/fileTransfer.js";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { CodeLine } from "@/components/common/CodeLine";

// Markdown 渲染要拉进 marked，只有真的打开 .md 时才值得付这个 chunk
const Markdown = lazy(() => import("./Markdown.js").then((m) => ({ default: m.Markdown })));

/**
 * 文件查看窗口。目标由排布给（props）而不是 store.active：文件窗口是列里的一扇，
 * 焦点在别处时它照样要显示自己的内容。
 *
 * 打开 / 换文件 / 手动刷新时拉一次，不轮询——正看着的文件在眼皮底下换掉，
 * 比看到旧内容更让人困惑（要新的按刷新就是了）。
 *
 * 只读，没有编辑：改文件是终端里的事，这里不做半个编辑器。
 *
 * 图片与 HTML 预览的字节不经 JSON：readFile 只判类型并给出这个项目的原始字节
 * 前缀（rawBase，带作用域令牌），`<img src>` / `<iframe src>` 直接指向宿主机上的
 * 文件，页面里相对路径引用的 CSS / JS / 图片也就自然能解析到（ADR 0007）。
 */
export function FileView({ target }: { target: FileTabTarget }) {
  const { t } = useTranslation();
  const openFile = useApp((s) => s.openFile);
  const [result, setResult] = useState<WorkspaceFile | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [tick, setTick] = useState(0);
  const [mode, setMode] = useState<ViewMode>(loadViewMode);

  const projectId = target?.projectId;
  const path = target?.path;

  useEffect(() => {
    setResult(null);
    setError(null);
    if (!projectId || !path) return;
    let cancelled = false;
    api
      .readFile(projectId, path)
      .then((next) => {
        if (!cancelled) setResult(next);
      })
      .catch((err) => {
        if (cancelled) return;
        useApp.getState().handleApiError(err);
        setError((err as Error).message);
      })
      .finally(() => {
        if (!cancelled) setBusy(false);
      });
    return () => {
      cancelled = true;
    };
  }, [projectId, path, tick]);

  if (!target || !path || !projectId) return null;

  const preview = result?.preview ?? null;
  const markdown = isMarkdown(path);
  const html = isHtml(path);
  const renderable = markdown || html;
  const previewing = renderable && mode === "preview";
  const switchMode = (next: ViewMode) => {
    setMode(next);
    saveViewMode(next);
  };
  // 图片与 HTML 预览用的是浏览器能直接打开的地址，顺手给个"在新标签页打开"：
  // 原始字节路由带 CSP sandbox，顶层打开也跑在 opaque origin 里，碰不到本站
  const raw = result && (preview?.kind === "image" || (html && previewing)) ? rawUrl(result.rawBase, path) : null;

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex h-8.5 shrink-0 items-center gap-2 border-b pr-1.5 pl-3">
        <FileText className="size-3.5 shrink-0 text-muted-foreground" />
        <span className="min-w-0 truncate font-mono text-xs" title={path}>
          {path}
        </span>
        {preview?.kind === "text" && preview.truncated && (
          <span className="shrink-0 text-[11px] text-warning">
            {t("files.viewTruncated", { size: formatBytes(preview.size) })}
          </span>
        )}
        {preview && preview.kind !== "text" && (
          <span className="shrink-0 text-[11px] text-muted-foreground tabular-nums">
            {formatBytes(preview.size)}
          </span>
        )}
        <span className="flex-1" />
        {renderable && (
          // 预览 / 源码只对 Markdown 与 HTML 有意义，别的文件类型不摆这两个按钮
          <>
            <Button
              variant="ghost"
              size="icon-xs"
              className={cn("text-muted-foreground", previewing && "bg-accent text-foreground")}
              aria-label={t("files.preview")}
              title={t("files.preview")}
              aria-pressed={previewing}
              onClick={() => switchMode("preview")}
            >
              <Eye />
            </Button>
            <Button
              variant="ghost"
              size="icon-xs"
              className={cn("text-muted-foreground", !previewing && "bg-accent text-foreground")}
              aria-label={t("files.source")}
              title={t("files.source")}
              aria-pressed={!previewing}
              onClick={() => switchMode("source")}
            >
              <Code2 />
            </Button>
          </>
        )}
        {raw && (
          <Button variant="ghost" size="icon-xs" className="text-muted-foreground" asChild>
            <a
              href={raw}
              target="_blank"
              rel="noreferrer noopener"
              aria-label={t("files.openRaw")}
              title={t("files.openRaw")}
            >
              <ExternalLink />
            </a>
          </Button>
        )}
        <Button
          variant="ghost"
          size="icon-xs"
          className="text-muted-foreground"
          aria-label={t("files.download")}
          title={t("files.download")}
          // 二进制 / 超大文件在这里"无法预览"，下载是它们唯一的出路，所以不看 preview 的种类
          onClick={() => triggerDownload(api.downloadUrl(projectId, path))}
        >
          <Download />
        </Button>
        <Button
          variant="ghost"
          size="icon-xs"
          className="text-muted-foreground"
          aria-label={t("files.refresh")}
          title={t("files.refresh")}
          disabled={busy}
          onClick={() => {
            setBusy(true);
            setTick((n) => n + 1);
          }}
        >
          <RefreshCw className={cn(busy && "animate-spin")} />
        </Button>
      </div>

      <div className="min-h-0 flex-1 overflow-hidden">
        {error ? (
          <Hint>
            {t("files.viewFailed")}
            <span className="mt-1 block font-mono text-[11px]">{error}</span>
          </Hint>
        ) : !result || !preview ? (
          <Hint>{t("files.viewLoading")}</Hint>
        ) : preview.kind === "binary" ? (
          <Hint>{t("files.viewBinary")}</Hint>
        ) : preview.kind === "too-large" ? (
          <Hint>{t("files.viewTooLarge", { size: formatBytes(preview.size) })}</Hint>
        ) : preview.kind === "image" ? (
          // key 挂 tick：同一个 src 换不了 <img> 的加载，刷新得靠重挂（响应是 no-store）
          <ImagePane key={tick} src={rawUrl(result.rawBase, path)} name={path} />
        ) : !preview.text ? (
          <Hint>{t("files.viewEmpty")}</Hint>
        ) : html && previewing ? (
          <HtmlPane key={tick} src={rawUrl(result.rawBase, path)} name={path} />
        ) : markdown && previewing ? (
          <div className="h-full overflow-auto">
            <Suspense fallback={<Hint>{t("files.viewLoading")}</Hint>}>
              <Markdown
                text={preview.text}
                dir={dirOf(path)}
                rawBase={result.rawBase}
                onOpenPath={(next) => openFile(projectId, next)}
              />
            </Suspense>
          </div>
        ) : (
          <CodePane text={preview.text} path={path} />
        )}
      </div>
    </div>
  );
}

type ViewMode = "preview" | "source";

const VIEW_KEY = "falcon.fileViewMode";

function loadViewMode(): ViewMode {
  try {
    return localStorage.getItem(VIEW_KEY) === "source" ? "source" : "preview";
  } catch {
    return "preview";
  }
}

function saveViewMode(mode: ViewMode) {
  try {
    localStorage.setItem(VIEW_KEY, mode);
  } catch {
    // 隐私模式下写不进去，不影响本次会话
  }
}

function isMarkdown(path: string): boolean {
  return /\.(md|markdown|mdx)$/i.test(path);
}

function isHtml(path: string): boolean {
  return /\.(html?|xhtml)$/i.test(path);
}

function dirOf(path: string): string {
  const i = path.lastIndexOf("/");
  return i < 0 ? "" : path.slice(0, i);
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

function Hint({ children }: { children: ReactNode }) {
  return <p className="px-4 py-4 text-xs leading-relaxed text-muted-foreground">{children}</p>;
}

// ---------------- HTML 预览 ----------------

/**
 * 直接把 HTML 当页面渲染。src 指向原始字节路由，所以页面里 `./style.css` 这类
 * 相对引用按 URL 规则就能解析到同一项目的文件。
 *
 * sandbox 不给 allow-same-origin：页面跑在 opaque origin 里，脚本碰不到本站的
 * cookie / localStorage，也调不动 /api——仓库里一个来路不明的 HTML 不能借着
 * 预览冒充用户操作。allow-scripts 得给，否则大半 HTML 页面白屏；弹窗与表单
 * 也在沙箱内（没有 allow-popups-to-escape-sandbox）。
 */
function HtmlPane({ src, name }: { src: string; name: string }) {
  return (
    <iframe
      src={src}
      title={name}
      sandbox="allow-scripts allow-forms allow-popups allow-modals"
      referrerPolicy="no-referrer"
      // 底色是浏览器的画布默认色而不是界面色：没设背景的 HTML 页面在浏览器里
      // 就是白底黑字，跟着本站深色主题走会把页面变成黑底黑字
      className="h-full w-full border-0 bg-white"
    />
  );
}

// ---------------- 图片预览 ----------------
//
// 缩放模型：`fit`（贴合窗口，但不放大小图——16px 的图标撑满屏幕只是一团马赛克）
// 或一个具体倍率。⌘/Ctrl + 滚轮与触控板捏合缩放、双击在 fit 与 1:1 之间切换、
// 放大后拖拽平移；缩放以指针位置为锚点——放大时光标底下那个像素不动。
// 锚定要等新尺寸排完版才能算滚动量，所以放在 useLayoutEffect 里做。

type Zoom = "fit" | number;
const ZOOM_MIN = 0.05;
const ZOOM_MAX = 32;
/** 贴合时四周留的空，与容器 padding 一致 */
const FIT_PAD = 24;

function clampZoom(z: number): number {
  return Math.min(ZOOM_MAX, Math.max(ZOOM_MIN, z));
}

function ImagePane({ src, name }: { src: string; name: string }) {
  const { t } = useTranslation();
  const scrollRef = useRef<HTMLDivElement>(null);
  const imgRef = useRef<HTMLImageElement>(null);
  const [natural, setNatural] = useState<{ w: number; h: number } | null>(null);
  const [failed, setFailed] = useState(false);
  const [box, setBox] = useState({ w: 0, h: 0 });
  const [zoom, setZoom] = useState<Zoom>("fit");
  /** 待应用的缩放锚点：图片上的相对位置 (fx, fy) 应停在视口坐标 (x, y) */
  const anchor = useRef<{ fx: number; fy: number; x: number; y: number } | null>(null);
  const drag = useRef<{ x: number; y: number; left: number; top: number } | null>(null);

  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setBox({ w: el.clientWidth, h: el.clientHeight }));
    ro.observe(el);
    setBox({ w: el.clientWidth, h: el.clientHeight });
    return () => ro.disconnect();
  }, []);

  // 没有固有尺寸的 SVG（只有 viewBox）naturalWidth 是 0，缩放算不出来，按浏览器默认画
  const sized = natural != null && natural.w > 0 && natural.h > 0;
  const fitScale = useMemo(() => {
    if (!sized || !box.w || !box.h) return 1;
    return Math.min(1, (box.w - FIT_PAD * 2) / natural.w, (box.h - FIT_PAD * 2) / natural.h);
  }, [sized, natural, box]);
  const scale = zoom === "fit" ? fitScale : zoom;

  useLayoutEffect(() => {
    const a = anchor.current;
    anchor.current = null;
    const el = scrollRef.current;
    const img = imgRef.current;
    if (!a || !el || !img) return;
    const r = img.getBoundingClientRect();
    el.scrollLeft += r.left + a.fx * r.width - a.x;
    el.scrollTop += r.top + a.fy * r.height - a.y;
  }, [scale]);

  const setAnchor = (at?: { x: number; y: number }) => {
    const img = imgRef.current;
    if (!at || !img) return;
    const r = img.getBoundingClientRect();
    anchor.current = { fx: (at.x - r.left) / r.width, fy: (at.y - r.top) / r.height, x: at.x, y: at.y };
  };
  const zoomTo = (next: Zoom, at?: { x: number; y: number }) => {
    setAnchor(at);
    setZoom(next === "fit" ? next : clampZoom(next));
  };
  // 相对缩放用函数式更新：连点两下"放大"落在同一帧里时，第二下要基于第一下的结果，
  // 而不是渲染闭包里那个还没更新的 scale
  const zoomBy = (factor: number, at?: { x: number; y: number }) => {
    setAnchor(at);
    setZoom((z) => clampZoom((z === "fit" ? fitScale : z) * factor));
  };

  useEffect(() => {
    const el = scrollRef.current;
    if (!el || !sized) return;
    // React 的 onWheel 是 passive 的，preventDefault 拦不住浏览器自己的页面缩放，
    // 得自己以 passive: false 挂。触控板捏合在 Chrome 里也是带 ctrlKey 的 wheel，
    // delta 很小；鼠标滚轮一格约 100。按 delta 指数缩放两种设备手感都自然
    const onWheel = (e: WheelEvent) => {
      if (!(e.ctrlKey || e.metaKey)) return;
      e.preventDefault();
      zoomBy(Math.exp(-e.deltaY * 0.0025), { x: e.clientX, y: e.clientY });
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  });

  const onPointerDown = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    const el = scrollRef.current;
    if (!el) return;
    drag.current = { x: e.clientX, y: e.clientY, left: el.scrollLeft, top: el.scrollTop };
    el.setPointerCapture(e.pointerId);
  };
  const onPointerMove = (e: ReactPointerEvent<HTMLDivElement>) => {
    const d = drag.current;
    const el = scrollRef.current;
    if (!d || !el) return;
    el.scrollLeft = d.left - (e.clientX - d.x);
    el.scrollTop = d.top - (e.clientY - d.y);
  };
  const endDrag = () => {
    drag.current = null;
  };

  if (failed) return <Hint>{t("files.viewImageFailed")}</Hint>;

  const overflowing = sized && (natural.w * scale > box.w || natural.h * scale > box.h);
  const percent = `${Math.round(scale * 100)}%`;

  return (
    <div className="relative h-full">
      <div
        ref={scrollRef}
        className={cn("h-full overflow-auto", overflowing && "cursor-grab active:cursor-grabbing")}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={endDrag}
        onPointerCancel={endDrag}
        onDoubleClick={(e) => zoomTo(zoom === "fit" && fitScale < 1 ? 1 : "fit", { x: e.clientX, y: e.clientY })}
      >
        {/* min-w/min-h 100% + w-max：图比窗口小时居中，比窗口大时撑开容器让它能滚 */}
        <div className="grid min-h-full w-max min-w-full place-items-center p-6">
          {/* 图片字节由浏览器直接从原始字节路由取；棋盘底衬让透明 PNG 看得出边界 */}
          <img
            ref={imgRef}
            src={src}
            alt={name}
            draggable={false}
            onLoad={(e) => setNatural({ w: e.currentTarget.naturalWidth, h: e.currentTarget.naturalHeight })}
            onError={() => setFailed(true)}
            className={cn(
              "rounded border bg-[repeating-conic-gradient(var(--muted)_0%_25%,transparent_0%_50%)] bg-[length:16px_16px] select-none",
              !sized && "max-h-full max-w-full object-contain"
            )}
            style={
              sized
                ? {
                    width: natural.w * scale,
                    height: natural.h * scale,
                    // 放大到 2 倍以上是在看像素，插值糊成一片反而看不清
                    imageRendering: scale >= 2 ? "pixelated" : undefined,
                  }
                : undefined
            }
          />
        </div>
      </div>
      {sized && (
        <div className="pointer-events-none absolute inset-x-0 bottom-3 flex justify-center">
          <div className="pointer-events-auto flex items-center gap-0.5 rounded-lg border bg-popover/80 px-1 py-0.5 text-[11px] text-muted-foreground shadow-sm backdrop-blur-xl">
            <span className="px-1.5 tabular-nums" title={t("files.imageDims")}>
              {natural.w} × {natural.h}
            </span>
            <span className="h-3.5 w-px bg-border" />
            <Button
              variant="ghost"
              size="icon-xs"
              className="text-muted-foreground"
              aria-label={t("files.zoomOut")}
              title={t("files.zoomOut")}
              disabled={scale <= ZOOM_MIN}
              onClick={() => zoomBy(1 / 1.25)}
            >
              <ZoomOut />
            </Button>
            <button
              type="button"
              className="min-w-11 rounded px-1 tabular-nums hover:bg-accent hover:text-foreground"
              title={t("files.zoomReset")}
              onClick={() => zoomTo("fit")}
            >
              {percent}
            </button>
            <Button
              variant="ghost"
              size="icon-xs"
              className="text-muted-foreground"
              aria-label={t("files.zoomIn")}
              title={t("files.zoomIn")}
              disabled={scale >= ZOOM_MAX}
              onClick={() => zoomBy(1.25)}
            >
              <ZoomIn />
            </Button>
            <span className="h-3.5 w-px bg-border" />
            <button
              type="button"
              className={cn(
                "rounded px-1.5 hover:bg-accent hover:text-foreground",
                zoom !== "fit" && scale === 1 && "bg-accent text-foreground"
              )}
              title={t("files.zoomActual")}
              aria-pressed={zoom !== "fit" && scale === 1}
              onClick={() => zoomTo(1)}
            >
              1:1
            </button>
            <button
              type="button"
              className={cn(
                "rounded px-1.5 hover:bg-accent hover:text-foreground",
                zoom === "fit" && "bg-accent text-foreground"
              )}
              title={t("files.zoomFit")}
              aria-pressed={zoom === "fit"}
              onClick={() => zoomTo("fit")}
            >
              {t("files.zoomFit")}
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

// ---------------- 源码视图 ----------------
//
// 上限 2MB 的文件能有好几万行，全量渲染是几万个 DOM 节点、秒级卡死。
// 行高恒定（leading-5.5 = 22px，不换行），所以窗口位置直接由 scrollTop 算得出，
// 不需要 GitDiffView 那套按行类型累加的前缀偏移。
//
// 语法高亮（lib/highlight.ts）是逐行 token、异步渐进到位：token 没到的行
// 先按纯文本画，到了再上色。行结构与虚拟窗口完全不因高亮而改变。

const LINE_H = 22;
const OVERSCAN = 30;
/** 还没收到 scroll 事件时先渲染这么多行，得盖住最高的屏幕 */
const INITIAL_ROWS = 140;

function CodePane({ text, path }: { text: string; path: string }) {
  const lines = useMemo(() => splitCodeLines(text), [text]);
  const hl = useHighlight(text, langForPath(path));
  const [win, setWin] = useState({ start: 0, end: INITIAL_ROWS });

  useEffect(() => {
    setWin({ start: 0, end: INITIAL_ROWS });
  }, [lines]);

  const start = Math.min(win.start, lines.length);
  const end = Math.min(win.end, lines.length);
  const onScroll = (el: HTMLElement) => {
    const nextStart = Math.max(0, Math.floor(el.scrollTop / LINE_H) - OVERSCAN);
    const nextEnd = Math.min(
      lines.length,
      Math.ceil((el.scrollTop + el.clientHeight) / LINE_H) + OVERSCAN
    );
    setWin((w) => (w.start === nextStart && w.end === nextEnd ? w : { start: nextStart, end: nextEnd }));
  };

  return (
    <div className="h-full overflow-auto" onScroll={(e) => onScroll(e.currentTarget)}>
      <table className="code-hl w-max min-w-full border-separate border-spacing-0 font-mono text-xs leading-5.5">
        <tbody>
          {start > 0 && (
            <tr aria-hidden>
              <td colSpan={2} style={{ height: start * LINE_H, padding: 0 }} />
            </tr>
          )}
          {lines.slice(start, end).map((line, offset) => (
            <tr key={start + offset}>
              {/* 行号栏横向滚动时要盖住滑到它底下的代码，底色必须不透明 */}
              <td className="sticky left-0 w-12 min-w-12 bg-background pr-3 text-right align-top tabular-nums text-muted-foreground/60 select-none">
                {start + offset + 1}
              </td>
              <td className="w-full pr-6 pl-1 align-top whitespace-pre">
                {/* 空行给个空格兜底，行高不塌（空行的 token 数组也是空的，会走纯文本分支） */}
                <CodeLine text={line || " "} tokens={hl?.[start + offset]} />
              </td>
            </tr>
          ))}
          {end < lines.length && (
            <tr aria-hidden>
              <td colSpan={2} style={{ height: (lines.length - end) * LINE_H, padding: 0 }} />
            </tr>
          )}
        </tbody>
      </table>
    </div>
  );
}
