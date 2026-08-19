import { useEffect, useState, type ReactNode } from "react";
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

function DiffTable({ text }: { text: string }) {
  const rows = parseDiff(text);
  // 单文件段时不重复画文件名条——header 里已经有了；重命名等多段 diff 才需要分隔
  const fileCount = rows.filter((r) => r.kind === "file").length;
  const shown = fileCount > 1 ? rows : rows.filter((r) => r.kind !== "file");

  return (
    <table className="w-full border-separate border-spacing-0 font-mono text-xs leading-5.5">
      <tbody>
        {shown.map((row, i) => {
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
      </tbody>
    </table>
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
  const gutterTone =
    line?.kind === "del"
      ? "text-destructive bg-destructive/10"
      : line?.kind === "add"
        ? "text-success bg-success/10"
        : "text-muted-foreground/60";
  return (
    <>
      <td className={cn("w-10 min-w-10 pr-2 text-right align-top tabular-nums select-none", gutterTone)}>
        {line?.no ?? ""}
      </td>
      <td
        className={cn(
          "w-[calc(50%-2.5rem)] pr-3 pl-1.5 align-top break-words whitespace-pre-wrap",
          tone,
          !line && "bg-muted/40"
        )}
      >
        {line?.text ?? ""}
      </td>
    </>
  );
}

function SplitDiffTable({ text }: { text: string }) {
  const rows = toSplitRows(parseDiff(text));
  const fileCount = rows.filter((r) => r.kind === "file").length;
  const shown = fileCount > 1 ? rows : rows.filter((r) => r.kind !== "file");

  return (
    <table className="w-full table-fixed border-separate border-spacing-0 font-mono text-xs leading-5.5">
      <tbody>
        {shown.map((row, i) => {
          if (row.kind === "file") {
            return (
              <tr key={i}>
                <td
                  colSpan={4}
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
                <td colSpan={4} className="bg-accent/40 px-3 py-1 text-muted-foreground select-none">
                  {row.text}
                </td>
              </tr>
            );
          }
          if (row.kind === "note") {
            return (
              <tr key={i}>
                <td colSpan={4} className="px-3 py-0.5 text-muted-foreground italic select-none">
                  {row.text}
                </td>
              </tr>
            );
          }
          return (
            <tr key={i}>
              <SideCells line={row.left} />
              <SideCells line={row.right} />
            </tr>
          );
        })}
      </tbody>
    </table>
  );
}
