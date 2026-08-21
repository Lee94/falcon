import { useEffect, useMemo, useRef, useState, type ReactNode, type UIEvent } from "react";
import { useTranslation } from "react-i18next";
import { Columns2, FileDiff, RefreshCw, Rows3 } from "lucide-react";
import type { GitFileDiff } from "@mojito/shared";
import { api } from "../api.js";
import { useApp } from "../store.js";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { reasonKey, statusLabel } from "./GitPanel.js";

type ViewMode = "split" | "unified";

const VIEW_KEY = "mojito.diffView";

function loadViewMode(): ViewMode {
  try {
    return localStorage.getItem(VIEW_KEY) === "unified" ? "unified" : "split";
  } catch {
    return "split";
  }
}

function saveViewMode(mode: ViewMode) {
  try {
    localStorage.setItem(VIEW_KEY, mode);
  } catch {
    // 隐私模式下写不进去，不影响本次会话
  }
}

/**
 * 差异 tab 的主区视图。目标来自 store.diffTab；打开 / 切换文件 / 手动刷新时
 * 拉一次，不轮询——正看着的 diff 在眼皮底下变动只会让人跟丢行号。
 */
export function GitDiffView() {
  const { t } = useTranslation();
  const target = useApp((s) => s.diffTab);
  const [result, setResult] = useState<GitFileDiff | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [tick, setTick] = useState(0);
  const [mode, setMode] = useState<ViewMode>(loadViewMode);

  const switchMode = (next: ViewMode) => {
    setMode(next);
    saveViewMode(next);
  };

  const file = target?.file;
  useEffect(() => {
    setResult(null);
    setError(null);
    if (!target || !file) return;
    let cancelled = false;
    api
      .gitFileDiff(target.projectId, {
        path: file.path,
        origPath: file.origPath,
        untracked: file.index === "?",
      })
      .then((next) => {
        if (cancelled) return;
        setResult(next);
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
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [target?.projectId, file?.path, file?.origPath, file?.index, file?.work, tick]);

  if (!target || !file) return null;
  const path = file.origPath ? `${file.origPath} → ${file.path}` : file.path;

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex h-8.5 shrink-0 items-center gap-2 border-b pr-1.5 pl-3">
        <FileDiff className="size-3.5 shrink-0 text-muted-foreground" />
        <span className="min-w-0 truncate font-mono text-xs" title={path}>
          {path}
        </span>
        <span className="shrink-0 rounded-sm bg-muted px-1.5 py-px text-[11px] text-muted-foreground">
          {statusLabel(file, t)}
        </span>
        {result?.truncated && (
          <span className="shrink-0 text-[11px] text-warning">{t("git.diffTruncated")}</span>
        )}
        <span className="flex-1" />
        <Button
          variant="ghost"
          size="icon-xs"
          className={cn("text-muted-foreground", mode === "split" && "bg-accent text-foreground")}
          aria-label={t("git.viewSplit")}
          title={t("git.viewSplit")}
          onClick={() => switchMode("split")}
        >
          <Columns2 />
        </Button>
        <Button
          variant="ghost"
          size="icon-xs"
          className={cn("text-muted-foreground", mode === "unified" && "bg-accent text-foreground")}
          aria-label={t("git.viewUnified")}
          title={t("git.viewUnified")}
          onClick={() => switchMode("unified")}
        >
          <Rows3 />
        </Button>
        <Button
          variant="ghost"
          size="icon-xs"
          className="text-muted-foreground"
          aria-label={t("git.refresh")}
          title={t("git.refresh")}
          disabled={busy}
          onClick={() => {
            setBusy(true);
            setTick((n) => n + 1);
          }}
        >
          <RefreshCw className={cn(busy && "animate-spin")} />
        </Button>
      </div>

      <div className="min-h-0 flex-1 overflow-auto">
        {error ? (
          <Hint>
            {t("git.diffFailed")}
            <span className="mt-1 block font-mono text-[11px]">{error}</span>
          </Hint>
        ) : !result ? (
          <Hint>{t("git.diffLoading")}</Hint>
        ) : !result.available ? (
          <Hint>
            {t(reasonKey(result.reason))}
            {result.detail && (
              <span className="mt-1 block font-mono text-[11px]">{result.detail}</span>
            )}
          </Hint>
        ) : !result.diff.trim() ? (
          <Hint>{t("git.diffEmpty")}</Hint>
        ) : mode === "split" ? (
          <SplitDiffTable text={result.diff} />
        ) : (
          <DiffTable text={result.diff} />
        )}
      </div>
    </div>
  );
}

function Hint({ children }: { children: ReactNode }) {
  return (
    <p className="px-4 py-4 text-xs leading-relaxed text-muted-foreground">{children}</p>
  );
}

type DiffRow =
  /** 一个文件段的开头（diff --git ...） */
  | { kind: "file"; label: string }
  /** @@ hunk 头 */
  | { kind: "hunk"; text: string }
  /** 正文行 */
  | { kind: "line"; sign: "+" | "-" | " "; old: number | null; new: number | null; text: string }
  /** 值得保留的元信息：new file / deleted file / rename / Binary files / \ No newline */
  | { kind: "note"; text: string };

/**
 * 解析 unified diff 供表格渲染。行号从 @@ 头累加。
 * index / mode / --- / +++ 这些头部行不进表格——文件名与状态已经在别处说清了。
 */
function parseDiff(text: string): DiffRow[] {
  const rows: DiffRow[] = [];
  let oldNo = 0;
  let newNo = 0;
  let inHunk = false;
  for (const raw of text.replace(/\n$/, "").split("\n")) {
    if (inHunk && raw.startsWith("\\")) {
      // "\ No newline at end of file"
      rows.push({ kind: "note", text: raw.slice(1).trim() });
      continue;
    }
    if (inHunk && (raw.startsWith("+") || raw.startsWith("-") || raw.startsWith(" ") || raw === "")) {
      const sign = (raw[0] ?? " ") as "+" | "-" | " ";
      const body = raw.slice(1);
      if (sign === "+") rows.push({ kind: "line", sign, old: null, new: newNo++, text: body });
      else if (sign === "-") rows.push({ kind: "line", sign, old: oldNo++, new: null, text: body });
      else rows.push({ kind: "line", sign: " ", old: oldNo++, new: newNo++, text: body });
      continue;
    }
    inHunk = false;
    if (raw.startsWith("diff --git ")) {
      rows.push({ kind: "file", label: bPathOf(raw) });
    } else if (raw.startsWith("@@")) {
      const m = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(raw);
      if (m) {
        oldNo = Number(m[1]);
        newNo = Number(m[2]);
        inHunk = true;
      }
      rows.push({ kind: "hunk", text: raw });
    } else if (
      /^(new file|deleted file|rename from|rename to|copy from|copy to|Binary files )/.test(raw)
    ) {
      rows.push({ kind: "note", text: raw });
    }
    // 其余头部行（index / mode / --- / +++）不展示
  }
  return rows;
}

/**
 * 从 `diff --git a/x b/y` 里取 b 侧路径。路径含空格时 a/b 边界本就有歧义，
 * 按最后一个 " b/" 切足够准（左边是 a 路径，git 不会在 b 路径后再拼别的）。
 */
function bPathOf(line: string): string {
  const rest = line.slice("diff --git ".length);
  const at = rest.lastIndexOf(" b/");
  return at >= 0 ? rest.slice(at + 3) : rest;
}

// ---------------- 行窗口虚拟化 ----------------
//
// DIFF_CAP（500KB）的 diff 有上万行，全量渲染是几万个 DOM 节点、秒级卡死。
// 行高按 kind 固定（正文 = leading-5.5 = 22px），用前缀偏移 + 二分把渲染
// 限制在视口附近。小 diff 不启用，行为与虚拟化之前完全一致（含 sticky 文件头）。

const VIRTUAL_THRESHOLD = 500;
const OVERSCAN = 24;
/** 首屏渲染的行数（还没收到 scroll 事件时），得盖住最高的屏幕 */
const INITIAL_ROWS = 140;

function rowHeight(kind: string): number {
  // 22 = 正文行（leading-5.5）；file/hunk/note 在此基础上加各自的 padding 与边框
  return kind === "file" ? 36 : kind === "hunk" ? 30 : kind === "note" ? 26 : 22;
}

/** offsets 里最后一个 <= y 的下标 */
function offsetIndex(offsets: number[], y: number): number {
  let lo = 0;
  let hi = offsets.length - 1;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    if (offsets[mid]! <= y) lo = mid;
    else hi = mid - 1;
  }
  return lo;
}

function useRowWindow(rows: readonly { kind: string }[]) {
  const enabled = rows.length > VIRTUAL_THRESHOLD;
  const offsets = useMemo(() => {
    if (!enabled) return null;
    const arr: number[] = new Array(rows.length + 1);
    let acc = 0;
    for (let i = 0; i < rows.length; i++) {
      arr[i] = acc;
      acc += rowHeight(rows[i]!.kind);
    }
    arr[rows.length] = acc;
    return arr;
  }, [rows, enabled]);

  const [win, setWin] = useState({ start: 0, end: INITIAL_ROWS });
  useEffect(() => {
    setWin({ start: 0, end: INITIAL_ROWS });
  }, [rows]);

  if (!enabled || !offsets) {
    return { start: 0, end: rows.length, topPad: 0, bottomPad: 0, update: () => undefined };
  }
  const start = Math.min(win.start, rows.length);
  const end = Math.min(win.end, rows.length);
  return {
    start,
    end,
    topPad: offsets[start]!,
    bottomPad: offsets[rows.length]! - offsets[end]!,
    update: (el: HTMLElement) => {
      const nextStart = Math.max(0, offsetIndex(offsets, el.scrollTop) - OVERSCAN);
      const nextEnd = Math.min(
        rows.length,
        offsetIndex(offsets, el.scrollTop + el.clientHeight) + 1 + OVERSCAN
      );
      setWin((w) => (w.start === nextStart && w.end === nextEnd ? w : { start: nextStart, end: nextEnd }));
    },
  };
}

function PadRow({ height, colSpan }: { height: number; colSpan: number }) {
  if (height <= 0) return null;
  return (
    <tr aria-hidden>
      <td colSpan={colSpan} style={{ height, padding: 0 }} />
    </tr>
  );
}

function DiffTable({ text }: { text: string }) {
  // 解析结果跟着 text 走：切视图模式 / 父组件重渲不该重新解析上万行
  const shown = useMemo(() => {
    const rows = parseDiff(text);
    // 单文件段时不重复画文件名条——header 里已经有了；重命名等多段 diff 才需要分隔
    const fileCount = rows.filter((r) => r.kind === "file").length;
    return fileCount > 1 ? rows : rows.filter((r) => r.kind !== "file");
  }, [text]);
  const win = useRowWindow(shown);

  return (
    <div className="h-full overflow-auto" onScroll={(e) => win.update(e.currentTarget)}>
    <table className="w-full border-separate border-spacing-0 font-mono text-xs leading-5.5">
      <tbody>
        <PadRow height={win.topPad} colSpan={3} />
        {shown.slice(win.start, win.end).map((row, offset) => {
          const i = win.start + offset;
          if (row.kind === "file") {
            return (
              <tr key={i}>
                <td
                  colSpan={3}
                  className="sticky top-0 border-y bg-muted px-3 py-1.5 font-medium text-foreground"
                >
                  {row.label}
                </td>
              </tr>
            );
          }
          if (row.kind === "hunk") {
            return (
              <tr key={i}>
                <td colSpan={3} className="bg-accent/40 px-3 py-1 text-muted-foreground select-none">
                  {row.text}
                </td>
              </tr>
            );
          }
          if (row.kind === "note") {
            return (
              <tr key={i}>
                <td colSpan={3} className="px-3 py-0.5 text-muted-foreground italic select-none">
                  {row.text}
                </td>
              </tr>
            );
          }
          const tone =
            row.sign === "+"
              ? "bg-success/10"
              : row.sign === "-"
                ? "bg-destructive/10"
                : undefined;
          const gutterTone =
            row.sign === "+"
              ? "text-success bg-success/10"
              : row.sign === "-"
                ? "text-destructive bg-destructive/10"
                : "text-muted-foreground/60";
          return (
            <tr key={i} className={tone}>
              <td className={cn("w-10 min-w-10 pr-2 text-right tabular-nums select-none", gutterTone)}>
                {row.old ?? ""}
              </td>
              <td className={cn("w-10 min-w-10 pr-2 text-right tabular-nums select-none", gutterTone)}>
                {row.new ?? ""}
              </td>
              <td className="w-full pr-4 pl-1 whitespace-pre">
                <span
                  className={cn(
                    "inline-block w-3 select-none",
                    row.sign === "+" && "text-success",
                    row.sign === "-" && "text-destructive"
                  )}
                >
                  {row.sign === " " ? "" : row.sign}
                </span>
                {row.text}
              </td>
            </tr>
          );
        })}
        <PadRow height={win.bottomPad} colSpan={3} />
      </tbody>
    </table>
    </div>
  );
}

// ---------------- 分栏视图 ----------------

interface SideLine {
  no: number;
  text: string;
  kind: "del" | "add" | "ctx";
}

type SplitRow =
  | { kind: "file"; label: string }
  | { kind: "hunk"; text: string }
  | { kind: "note"; text: string }
  | { kind: "pair"; left: SideLine | null; right: SideLine | null };

/**
 * 把 unified 行配成左右两栏：hunk 里连续的 − 段与紧随的 + 段按序号对齐
 * （第 i 行删除对第 i 行新增），上下文两边同现。
 */
function toSplitRows(rows: DiffRow[]): SplitRow[] {
  const out: SplitRow[] = [];
  let dels: SideLine[] = [];
  let adds: SideLine[] = [];
  const flush = () => {
    const n = Math.max(dels.length, adds.length);
    for (let i = 0; i < n; i++) {
      out.push({ kind: "pair", left: dels[i] ?? null, right: adds[i] ?? null });
    }
    dels = [];
    adds = [];
  };
  for (const row of rows) {
    if (row.kind === "line") {
      if (row.sign === "-") {
        dels.push({ no: row.old!, text: row.text, kind: "del" });
        continue;
      }
      if (row.sign === "+") {
        adds.push({ no: row.new!, text: row.text, kind: "add" });
        continue;
      }
      flush();
      out.push({
        kind: "pair",
        left: { no: row.old!, text: row.text, kind: "ctx" },
        right: { no: row.new!, text: row.text, kind: "ctx" },
      });
      continue;
    }
    flush();
    out.push(row);
  }
  flush();
  return out;
}

function SideCells({ line }: { line: SideLine | null }) {
  const tone =
    line?.kind === "del"
      ? "bg-destructive/10"
      : line?.kind === "add"
        ? "bg-success/10"
        : undefined;
  // 行号栏横向滚动时要盖住滑到底下的代码，底色必须不透明：
  // 先铺 bg-background，再用 ::before 叠半透明色调
  const gutterTone =
    line?.kind === "del"
      ? "text-destructive before:bg-destructive/10"
      : line?.kind === "add"
        ? "text-success before:bg-success/10"
        : "text-muted-foreground/60";
  return (
    <>
      <td
        className={cn(
          "sticky left-0 w-10 min-w-10 bg-background pr-2 text-right align-top tabular-nums select-none before:absolute before:inset-0",
          gutterTone
        )}
      >
        {line?.no ?? ""}
      </td>
      <td className={cn("w-full pr-3 pl-1.5 align-top whitespace-pre", tone, !line && "bg-muted/40")}>
        {/* 空行也得占满一行高，否则该行在本栏塌缩，两栏行对不齐 */}
        {line?.text || " "}
      </td>
    </>
  );
}

/** 单栏内容：占位行（file/hunk/note）两栏都画一份，保证行高一致、上下对齐 */
function SidePane({
  rows,
  side,
  start,
  topPad,
  bottomPad,
}: {
  rows: SplitRow[];
  side: "left" | "right";
  /** 窗口首行在全量行里的下标，用作稳定 key */
  start: number;
  topPad: number;
  bottomPad: number;
}) {
  return (
    <table className="w-max min-w-full border-separate border-spacing-0 font-mono text-xs leading-5.5">
      <tbody>
        <PadRow height={topPad} colSpan={2} />
        {rows.map((row, offset) => {
          const i = start + offset;
          if (row.kind === "file") {
            return (
              <tr key={i}>
                <td
                  colSpan={2}
                  className="sticky top-0 z-10 border-y bg-muted px-3 py-1.5 font-medium whitespace-pre text-foreground"
                >
                  <span className="sticky left-3 inline-block">{row.label}</span>
                </td>
              </tr>
            );
          }
          if (row.kind === "hunk") {
            return (
              <tr key={i}>
                <td
                  colSpan={2}
                  className="bg-accent/40 px-3 py-1 whitespace-pre text-muted-foreground select-none"
                >
                  <span className="sticky left-3 inline-block">{row.text}</span>
                </td>
              </tr>
            );
          }
          if (row.kind === "note") {
            return (
              <tr key={i}>
                <td
                  colSpan={2}
                  className="px-3 py-0.5 whitespace-pre text-muted-foreground italic select-none"
                >
                  <span className="sticky left-3 inline-block">{row.text}</span>
                </td>
              </tr>
            );
          }
          return (
            <tr key={i}>
              <SideCells line={side === "left" ? row.left : row.right} />
            </tr>
          );
        })}
        <PadRow height={bottomPad} colSpan={2} />
      </tbody>
    </table>
  );
}

function SplitDiffTable({ text }: { text: string }) {
  const shown = useMemo(() => {
    const rows = toSplitRows(parseDiff(text));
    const fileCount = rows.filter((r) => r.kind === "file").length;
    return fileCount > 1 ? rows : rows.filter((r) => r.kind !== "file");
  }, [text]);
  const win = useRowWindow(shown);
  const windowed = shown.slice(win.start, win.end);

  const leftRef = useRef<HTMLDivElement>(null);
  const rightRef = useRef<HTMLDivElement>(null);
  const lockSide = useRef<"left" | "right" | null>(null);
  const lockTimer = useRef(0);
  useEffect(() => () => window.clearTimeout(lockTimer.current), []);

  // 双向同步滚动：谁先动谁做主，对侧短时间内的 scroll 事件当回声忽略——
  // 两栏内容宽度不同时 clamp 会触发对侧事件，不忽略就会互相拉扯
  const onScroll = (side: "left" | "right") => (e: UIEvent<HTMLDivElement>) => {
    if (lockSide.current && lockSide.current !== side) return;
    lockSide.current = side;
    window.clearTimeout(lockTimer.current);
    lockTimer.current = window.setTimeout(() => {
      lockSide.current = null;
    }, 120);
    const to = side === "left" ? rightRef.current : leftRef.current;
    if (to) {
      to.scrollTop = e.currentTarget.scrollTop;
      to.scrollLeft = e.currentTarget.scrollLeft;
    }
    win.update(e.currentTarget);
  };

  return (
    <div className="flex h-full min-h-0">
      <div ref={leftRef} className="min-w-0 flex-1 overflow-auto" onScroll={onScroll("left")}>
        <SidePane
          rows={windowed}
          side="left"
          start={win.start}
          topPad={win.topPad}
          bottomPad={win.bottomPad}
        />
      </div>
      <div
        ref={rightRef}
        className="min-w-0 flex-1 overflow-auto border-l"
        onScroll={onScroll("right")}
      >
        <SidePane
          rows={windowed}
          side="right"
          start={win.start}
          topPad={win.topPad}
          bottomPad={win.bottomPad}
        />
      </div>
    </div>
  );
}
