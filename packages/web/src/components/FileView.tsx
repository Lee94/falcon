import { lazy, Suspense, useEffect, useMemo, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { Code2, Eye, FileText, RefreshCw } from "lucide-react";
import type { FilePreview } from "@mojito/shared";
import { api } from "../api.js";
import { useApp } from "../store.js";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";

// Markdown 渲染要拉进 marked，只有真的打开 .md 时才值得付这个 chunk
const Markdown = lazy(() => import("./Markdown.js").then((m) => ({ default: m.Markdown })));

/**
 * 文件查看 tab 的主区。目标来自 store.fileTab。
 *
 * 打开 / 换文件 / 手动刷新时拉一次，不轮询——正看着的文件在眼皮底下换掉，
 * 比看到旧内容更让人困惑（要新的按刷新就是了）。
 *
 * 只读，没有编辑：改文件是终端里的事，这里不做半个编辑器。
 */
export function FileView() {
  const { t } = useTranslation();
  const target = useApp((s) => s.fileTab);
  const openFile = useApp((s) => s.openFile);
  const [result, setResult] = useState<FilePreview | null>(null);
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

  const markdown = isMarkdown(path);
  const preview = markdown && mode === "preview";
  const switchMode = (next: ViewMode) => {
    setMode(next);
    saveViewMode(next);
  };

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex h-8.5 shrink-0 items-center gap-2 border-b pr-1.5 pl-3">
        <FileText className="size-3.5 shrink-0 text-muted-foreground" />
        <span className="min-w-0 truncate font-mono text-xs" title={path}>
          {path}
        </span>
        {result?.kind === "text" && result.truncated && (
          <span className="shrink-0 text-[11px] text-warning">
            {t("files.viewTruncated", { size: formatBytes(result.size) })}
          </span>
        )}
        {result && result.kind !== "text" && (
          <span className="shrink-0 text-[11px] text-muted-foreground tabular-nums">
            {formatBytes(result.size)}
          </span>
        )}
        <span className="flex-1" />
        {markdown && (
          // 预览 / 源码只对 Markdown 有意义，别的文件类型不摆这两个按钮
          <>
            <Button
              variant="ghost"
              size="icon-xs"
              className={cn("text-muted-foreground", preview && "bg-accent text-foreground")}
              aria-label={t("files.preview")}
              title={t("files.preview")}
              aria-pressed={preview}
              onClick={() => switchMode("preview")}
            >
              <Eye />
            </Button>
            <Button
              variant="ghost"
              size="icon-xs"
              className={cn("text-muted-foreground", !preview && "bg-accent text-foreground")}
              aria-label={t("files.source")}
              title={t("files.source")}
              aria-pressed={!preview}
              onClick={() => switchMode("source")}
            >
              <Code2 />
            </Button>
          </>
        )}
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
        ) : !result ? (
          <Hint>{t("files.viewLoading")}</Hint>
        ) : result.kind === "binary" ? (
          <Hint>{t("files.viewBinary")}</Hint>
        ) : result.kind === "too-large" ? (
          <Hint>{t("files.viewTooLarge", { size: formatBytes(result.size) })}</Hint>
        ) : result.kind === "image" ? (
          <ImagePane mime={result.mime} base64={result.base64} name={path} />
        ) : !result.text ? (
          <Hint>{t("files.viewEmpty")}</Hint>
        ) : preview ? (
          <div className="h-full overflow-auto">
            <Suspense fallback={<Hint>{t("files.viewLoading")}</Hint>}>
              <Markdown
                text={result.text}
                dir={dirOf(path)}
                onOpenPath={(next) => openFile(projectId, next)}
              />
            </Suspense>
          </div>
        ) : (
          <CodePane text={result.text} />
        )}
      </div>
    </div>
  );
}

type ViewMode = "preview" | "source";

const VIEW_KEY = "mojito.fileViewMode";

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

function ImagePane({ mime, base64, name }: { mime: string; base64: string; name: string }) {
  return (
    <div className="grid h-full place-items-center overflow-auto p-6">
      {/* 图片字节在宿主机上，只能由后端读回来内联；棋盘底衬让透明 PNG 看得出边界 */}
      <img
        src={`data:${mime};base64,${base64}`}
        alt={name}
        className="max-h-full max-w-full rounded border bg-[repeating-conic-gradient(var(--muted)_0%_25%,transparent_0%_50%)] bg-[length:16px_16px] object-contain"
      />
    </div>
  );
}

// ---------------- 源码视图 ----------------
//
// 上限 2MB 的文件能有好几万行，全量渲染是几万个 DOM 节点、秒级卡死。
// 行高恒定（leading-5.5 = 22px，不换行），所以窗口位置直接由 scrollTop 算得出，
// 不需要 GitDiffView 那套按行类型累加的前缀偏移。

const LINE_H = 22;
const OVERSCAN = 30;
/** 还没收到 scroll 事件时先渲染这么多行，得盖住最高的屏幕 */
const INITIAL_ROWS = 140;

function CodePane({ text }: { text: string }) {
  const lines = useMemo(() => text.replace(/\n$/, "").split("\n"), [text]);
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
      <table className="w-max min-w-full border-separate border-spacing-0 font-mono text-xs leading-5.5">
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
              <td className="w-full pr-6 pl-1 align-top whitespace-pre">{line || " "}</td>
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
