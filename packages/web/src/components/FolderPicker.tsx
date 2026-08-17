import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { ArrowUp, Folder, HardDrive, House } from "lucide-react";
import type { FsListing } from "@mojito/shared";
import { api } from "../api.js";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";

/**
 * 目录浏览。本地或 SSH 远端共用这一套 UI，列目录的请求由 listDir 注入。
 *
 * 嵌在新建/编辑项目对话框里，不另开一层 Dialog：两层 Radix Dialog 叠在一起
 * 会抢 pointer-events，关内层时外层也容易一起被拆掉。
 *
 * Esc 必须在捕获阶段拦下来。App 的 window 监听负责关最上面那一层浮层，
 * 但它看不见这个局部状态，不拦的话会把整个项目表单一并关掉。
 */
export function FolderPicker({
  initialPath,
  onSelect,
  onClose,
  listDir = (dir) => api.listDir(dir),
  remote = false,
  confirming = false,
}: {
  initialPath: string;
  onSelect: (path: string) => void;
  onClose: () => void;
  listDir?: (dir?: string) => Promise<FsListing>;
  remote?: boolean;
  /** 选完目录后正在创建项目，按钮不可再点 */
  confirming?: boolean;
}) {
  const { t } = useTranslation();
  const [listing, setListing] = useState<FsListing | null>(null);
  const [draft, setDraft] = useState(initialPath);
  const [filter, setFilter] = useState("");
  const [highlight, setHighlight] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const load = async (dir: string | undefined, keepError?: string) => {
    setBusy(true);
    if (!keepError) setError(null);
    try {
      const next = await listDir(dir);
      setListing(next);
      setDraft(next.path);
      setFilter("");
      setHighlight(0);
      if (keepError) setError(keepError);
    } catch (err) {
      const message = (err as Error).message;
      if (dir !== undefined && listing == null) {
        await load(undefined, message);
        return;
      }
      setError(message);
    } finally {
      setBusy(false);
    }
  };

  useEffect(() => {
    void load(initialPath.trim() || undefined);
    // 只在打开时读一次初始路径
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopImmediatePropagation();
      onClose();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onClose]);

  const visible = useMemo(() => {
    const q = filter.trim().toLowerCase();
    const entries = listing?.entries ?? [];
    return q ? entries.filter((e) => e.name.toLowerCase().includes(q)) : entries;
  }, [listing, filter]);

  useEffect(() => {
    setHighlight((h) => Math.min(h, Math.max(0, visible.length - 1)));
  }, [visible.length]);

  const enter = (dir: string) => {
    void load(dir);
  };

  const jump = () => {
    void load(draft.trim() === "" ? "" : draft.trim());
  };

  const canSelect = Boolean(listing && listing.path);

  return (
    <div className="grid gap-3">
      <div className="flex gap-1.5">
        <Button
          type="button"
          variant="outline"
          size="icon"
          title={t("project.pickHome")}
          aria-label={t("project.pickHome")}
          disabled={busy || !listing}
          onClick={() => listing && void load(listing.home)}
        >
          <House />
        </Button>
        <Button
          type="button"
          variant="outline"
          size="icon"
          title={t("project.pickUp")}
          aria-label={t("project.pickUp")}
          disabled={busy || listing?.parent == null}
          onClick={() => listing && listing.parent != null && void load(listing.parent)}
        >
          <ArrowUp />
        </Button>
        <Button
          type="button"
          variant="outline"
          size="icon"
          title={t("project.pickRoots")}
          aria-label={t("project.pickRoots")}
          disabled={busy || !listing}
          onClick={() => listing && void load(listing.roots[0] === "/" ? "/" : "")}
        >
          <HardDrive />
        </Button>
        <Input
          className="font-mono"
          value={draft}
          spellCheck={false}
          aria-label={t("project.workingDir")}
          placeholder={listing?.path === "" ? t("project.pickRoots") : undefined}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              jump();
            }
          }}
        />
        <Button type="button" variant="outline" disabled={busy} onClick={jump}>
          {t("project.pickJump")}
        </Button>
      </div>

      <Input
        value={filter}
        data-autofocus
        spellCheck={false}
        placeholder={t("project.pickFilter")}
        onChange={(e) => setFilter(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "ArrowDown") {
            e.preventDefault();
            setHighlight((h) => Math.min(h + 1, Math.max(0, visible.length - 1)));
          } else if (e.key === "ArrowUp") {
            e.preventDefault();
            setHighlight((h) => Math.max(h - 1, 0));
          } else if (e.key === "Enter") {
            e.preventDefault();
            const hit = visible[highlight];
            if (hit) enter(hit.path);
          } else if (e.key === "Backspace" && filter === "" && listing?.parent != null) {
            e.preventDefault();
            void load(listing.parent);
          }
        }}
      />

      <div
        role="listbox"
        aria-label={t("project.pickTitle")}
        className="h-72 overflow-y-auto rounded-md border bg-background/60"
      >
        {busy && !listing ? (
          <p className="px-3 py-6 text-center text-xs text-muted-foreground">
            {t(remote ? "project.pickConnecting" : "project.pickLoading")}
          </p>
        ) : visible.length === 0 ? (
          <p className="px-3 py-6 text-center text-xs text-muted-foreground">
            {filter.trim() ? t("project.pickEmptyFilter") : t("project.pickEmpty")}
          </p>
        ) : (
          visible.map((entry, i) => {
            const hidden = entry.name.startsWith(".");
            const on = i === highlight;
            return (
              <button
                key={entry.path}
                type="button"
                role="option"
                aria-selected={on}
                className={cn(
                  "flex h-8 w-full items-center gap-2 px-2.5 text-left font-mono text-[13px] outline-none",
                  on ? "bg-accent text-accent-foreground" : "hover:bg-accent/60",
                  hidden && !on && "text-muted-foreground"
                )}
                onMouseEnter={() => setHighlight(i)}
                onClick={() => enter(entry.path)}
              >
                <Folder className="size-3.5 shrink-0 text-muted-foreground" />
                <span className="min-w-0 truncate">{entry.name}</span>
              </button>
            );
          })
        )}
      </div>

      {error && <p className="text-[13px] text-destructive">{error}</p>}

      <div className="flex justify-end gap-2">
        <Button variant="outline" type="button" disabled={confirming} onClick={onClose}>
          {t("common.cancel")}
        </Button>
        <Button
          type="button"
          disabled={!canSelect || confirming}
          onClick={() => listing && listing.path && onSelect(listing.path)}
        >
          {confirming ? t("project.creating") : t("project.pickSelect")}
        </Button>
      </div>
    </div>
  );
}
