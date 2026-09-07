import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
  type MouseEvent,
  type ReactNode,
} from "react";
import { useTranslation } from "react-i18next";
import {
  ArrowLeft,
  ChevronDown,
  ChevronUp,
  Download,
  Eye,
  EyeOff,
  File,
  FileCode,
  FileImage,
  FileText,
  Folder,
  FolderPlus,
  FolderUp,
  RefreshCw,
  Trash2,
  Upload,
} from "lucide-react";
import { toast } from "sonner";
import type { WorkspaceEntry } from "@falcon/shared";
import { api, ApiRequestError } from "../api.js";
import { useApp, selectFocusProjectId, type MenuItemSpec } from "../store.js";
import { confirmAsync } from "../lib/confirmAsync.js";
import { pickFiles, pickFolder, triggerDownload } from "../lib/fileTransfer.js";
import { isMac } from "../lib/shortcuts.js";
import {
  deepestUploadDirs,
  formatMtime,
  formatSize,
  isHiddenName,
  joinHostPath,
  parentRel,
  parseNavPath,
  relDir,
} from "../lib/filePath.js";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { openContextMenu } from "@/components/common/Menu";

/**
 * 右侧「文件」面板：项目工作目录的目录浏览器（ADR 0009）。
 *
 * 与「修改」面板的分工：那边列的是 git 眼里"这次动了什么"，这边列的是磁盘上
 * 真正有什么——未跟踪的、被 ignore 的、生成出来的，都在。所以它不能复用
 * FileChangeView：那套结构一次拿到全部路径再树化，而这里目录可能有几万项，
 * 只能按层拉。
 *
 * 交互按文件管理器而不是树：顶上路径栏 + 图标工具栏，当前目录平铺成表
 * （复选框 / 名称 / 大小 / 修改时间）。点目录进去、点文件开查看 tab。多选走
 * 复选框和 Shift/⌘ 点击，右键复制路径 / 下载 / 重命名 / 删除。
 */

type SortKey = "name" | "size" | "mtime";
type SortDir = "asc" | "desc";

interface Listing {
  loading: boolean;
  entries?: WorkspaceEntry[];
  truncated?: boolean;
  error?: string;
}

type NameDraft =
  | { kind: "mkdir"; name: string }
  | { kind: "rename"; path: string; name: string };

function useListing(projectId: string | null, cwd: string) {
  const [listing, setListing] = useState<Listing>({ loading: true });
  const [tick, setTick] = useState(0);
  const gen = useRef(0);

  useEffect(() => {
    if (!projectId) {
      setListing({ loading: false });
      return;
    }
    const my = ++gen.current;
    setListing((cur) => ({ ...cur, loading: true, error: undefined }));
    api
      .listFiles(projectId, cwd || undefined)
      .then((res) => {
        if (gen.current !== my) return;
        setListing({ loading: false, entries: res.entries, truncated: res.truncated });
      })
      .catch((err) => {
        if (gen.current !== my) return;
        useApp.getState().handleApiError(err);
        setListing({ loading: false, error: (err as Error).message });
      });
  }, [projectId, cwd, tick]);

  const refresh = useCallback(() => setTick((n) => n + 1), []);
  return { listing, refresh };
}

/**
 * 逐个上传。顺序而不是并发：每个文件都可能要弹一次"覆盖吗"，并发的确认框会
 * 互相盖住；SSH 链路上并发也不会更快。进度走 toast，面板本身不摆进度条。
 */
async function uploadItems(
  projectId: string,
  items: { dir: string; file: File }[],
  t: (key: string, opts?: Record<string, unknown>) => string,
  hasEntry: (dir: string, name: string) => boolean
): Promise<boolean> {
  let any = false;
  for (const { dir, file } of items) {
    let overwrite = false;
    if (hasEntry(dir, file.name)) {
      overwrite = await askOverwrite(file.name, t);
      if (!overwrite) continue;
    }
    const id = toast.loading(t("files.uploading", { name: file.name, pct: 0 }));
    const onProgress = (sent: number, total: number) => {
      const pct = total > 0 ? Math.min(99, Math.floor((sent / total) * 100)) : 0;
      toast.loading(t("files.uploading", { name: file.name, pct }), { id });
    };
    try {
      try {
        await api.uploadFile(projectId, dir, file, { overwrite, onProgress });
      } catch (err) {
        if (!(err instanceof ApiRequestError) || err.status !== 409) throw err;
        toast.dismiss(id);
        if (!(await askOverwrite(file.name, t))) continue;
        await api.uploadFile(projectId, dir, file, { overwrite: true, onProgress });
      }
      toast.success(t("files.uploaded", { name: file.name }), { id, duration: 5000 });
      any = true;
    } catch (err) {
      useApp.getState().handleApiError(err);
      toast.error(t("files.uploadFailed", { name: file.name }), {
        id,
        description: (err as Error).message,
        duration: 8000,
      });
    }
  }
  return any;
}

function askOverwrite(
  name: string,
  t: (key: string, opts?: Record<string, unknown>) => string
): Promise<boolean> {
  return confirmAsync({
    title: t("files.overwriteTitle", { name }),
    body: t("files.overwriteBody"),
    confirmLabel: t("files.overwrite"),
  });
}

function uniqueName(base: string, taken: Iterable<string>): string {
  const set = new Set(taken);
  if (!set.has(base)) return base;
  for (let i = 2; ; i++) {
    const n = `${base} ${i}`;
    if (!set.has(n)) return n;
  }
}

function closeMatchingTabs(projectId: string, paths: string[]) {
  const { fileTabs, closeFile } = useApp.getState();
  for (const tab of [...fileTabs]) {
    if (tab.projectId !== projectId) continue;
    if (paths.some((p) => tab.path === p || tab.path.startsWith(`${p}/`))) closeFile(tab);
  }
}

function retargetTab(projectId: string, from: string, to: string) {
  const { fileTabs, closeFile, openFile } = useApp.getState();
  const wasOpen = fileTabs.some((f) => f.projectId === projectId && f.path === from);
  if (!wasOpen) return;
  closeFile({ projectId, path: from });
  openFile(projectId, to);
}

async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}

export function FilesPanel() {
  const { t } = useTranslation();
  const projectId = useApp(selectFocusProjectId);
  const project = useApp((s) => s.projects.find((p) => p.id === projectId));
  const openFile = useApp((s) => s.openFile);
  const active = useApp((s) => s.active);

  const [cwd, setCwd] = useState("");
  const [showHidden, setShowHidden] = useState(false);
  const [sort, setSort] = useState<{ key: SortKey; dir: SortDir }>({ key: "name", dir: "asc" });
  const [selected, setSelected] = useState<Set<string>>(() => new Set());
  const [draft, setDraft] = useState<NameDraft | null>(null);
  const lastClicked = useRef<string | null>(null);
  const ignoreBlur = useRef(false);

  useEffect(() => {
    setCwd("");
    setSelected(new Set());
    setDraft(null);
    lastClicked.current = null;
  }, [projectId]);

  useEffect(() => {
    setSelected(new Set());
    setDraft(null);
    lastClicked.current = null;
  }, [cwd]);

  const { listing, refresh } = useListing(projectId, cwd);

  useEffect(() => {
    const live = new Set((listing.entries ?? []).map((e) => e.path));
    setSelected((cur) => {
      const next = new Set([...cur].filter((p) => live.has(p)));
      return next.size === cur.size ? cur : next;
    });
  }, [listing.entries]);

  const hostPath = joinHostPath(project?.workingDir, cwd);
  const [pathDraft, setPathDraft] = useState(hostPath);
  useEffect(() => setPathDraft(hostPath), [hostPath]);

  const activePath =
    active.kind === "file" && active.projectId === projectId ? active.path : null;

  const visible = useMemo(() => {
    const entries = listing.entries ?? [];
    const filtered = showHidden ? entries : entries.filter((e) => !isHiddenName(e.name));
    const dirSign = sort.dir === "asc" ? 1 : -1;
    return [...filtered].sort((a, b) => {
      if (a.kind !== b.kind) return a.kind === "dir" ? -1 : 1;
      let c = 0;
      if (sort.key === "name") {
        c = a.name.localeCompare(b.name, undefined, { sensitivity: "base" });
      } else if (sort.key === "size") {
        c = (a.size ?? -1) - (b.size ?? -1);
      } else {
        c = (a.mtime ?? 0) - (b.mtime ?? 0);
      }
      return c * dirSign;
    });
  }, [listing.entries, showHidden, sort]);

  const selectedEntries = useMemo(
    () => visible.filter((e) => selected.has(e.path)),
    [visible, selected]
  );
  const selectedFiles = selectedEntries.filter((e) => e.kind === "file");
  const allVisibleSelected = visible.length > 0 && visible.every((e) => selected.has(e.path));
  const someVisibleSelected = visible.some((e) => selected.has(e.path));

  const hasEntry = useCallback(
    (dir: string, name: string) => {
      if (dir !== cwd) return false;
      return listing.entries?.some((e) => e.name === name) ?? false;
    },
    [cwd, listing.entries]
  );

  const goTo = useCallback((next: string) => setCwd(next), []);

  const commitPath = useCallback(() => {
    const next = parseNavPath(pathDraft, project?.workingDir);
    if (next == null) {
      toast.error(t("files.pathOutside"));
      setPathDraft(hostPath);
      return;
    }
    if (next === cwd) {
      setPathDraft(hostPath);
      return;
    }
    goTo(next);
  }, [pathDraft, project?.workingDir, hostPath, cwd, goTo, t]);

  const downloadPaths = useCallback(
    (paths: string[]) => {
      if (!projectId) return;
      const kindOf = (p: string) => listing.entries?.find((e) => e.path === p)?.kind;
      const files = paths.filter((p) => kindOf(p) === "file");
      const dirs = paths.length - files.length;
      void (async () => {
        for (const p of files) {
          triggerDownload(api.downloadUrl(projectId, p));
          await new Promise((r) => setTimeout(r, 250));
        }
        if (dirs > 0) toast.message(t("files.downloadSkipDirs", { count: dirs }));
      })();
    },
    [projectId, listing.entries, t]
  );

  const uploadTo = useCallback(
    async (dir: string) => {
      if (!projectId) return;
      const files = await pickFiles();
      if (files.length === 0) return;
      const changed = await uploadItems(
        projectId,
        files.map((file) => ({ dir, file })),
        t,
        hasEntry
      );
      if (changed) refresh();
    },
    [projectId, t, hasEntry, refresh]
  );

  const uploadFolderTo = useCallback(
    async (dir: string) => {
      if (!projectId) return;
      const files = await pickFolder();
      if (files.length === 0) return;
      const rels = files.map((f) => f.webkitRelativePath || f.name);
      for (const nested of deepestUploadDirs(dir, rels)) {
        try {
          await api.mkdir(projectId, nested, true);
        } catch (err) {
          useApp.getState().handleApiError(err);
          toast.error(t("files.mkdirFailed"), { description: (err as Error).message });
          return;
        }
      }
      const items = files.map((file) => {
        const rel = file.webkitRelativePath || file.name;
        const parent = relDir(rel);
        const dest = dir ? (parent ? `${dir}/${parent}` : dir) : parent;
        return { dir: dest, file };
      });
      const changed = await uploadItems(projectId, items, t, hasEntry);
      if (changed) refresh();
    },
    [projectId, t, hasEntry, refresh]
  );

  const removePaths = useCallback(
    async (paths: string[]) => {
      if (!projectId || paths.length === 0) return;
      const targets = (listing.entries ?? []).filter((e) => paths.includes(e.path));
      const ok = await confirmAsync({
        title: t("files.deleteTitle", { count: paths.length }),
        body: t("files.deleteBody"),
        list: targets.map((e) => ({
          name: e.name,
          meta: e.kind === "dir" ? t("files.folder") : formatSize(e.size),
        })),
        confirmLabel: t("files.delete"),
      });
      if (!ok) return;
      try {
        const res = await api.removeFiles(projectId, paths);
        closeMatchingTabs(projectId, res.removed);
        if (res.errors.length === 0) {
          toast.success(t("files.deleted", { count: res.removed.length }));
        } else if (res.removed.length > 0) {
          toast.error(t("files.deletePartial", { ok: res.removed.length, fail: res.errors.length }), {
            description: res.errors[0]?.error,
          });
        } else {
          toast.error(t("files.deleteFailed"), { description: res.errors[0]?.error });
        }
        if (res.removed.length > 0) refresh();
      } catch (err) {
        useApp.getState().handleApiError(err);
        toast.error(t("files.deleteFailed"), { description: (err as Error).message });
      }
    },
    [projectId, listing.entries, t, refresh]
  );

  const startMkdir = useCallback(() => {
    const taken = (listing.entries ?? []).map((e) => e.name);
    setDraft({ kind: "mkdir", name: uniqueName(t("files.newFolderName"), taken) });
  }, [listing.entries, t]);

  const startRename = useCallback((entry: WorkspaceEntry) => {
    setDraft({ kind: "rename", path: entry.path, name: entry.name });
  }, []);

  const commitDraft = useCallback(async () => {
    if (!projectId || !draft) return;
    const name = draft.name.trim();
    if (!name) {
      setDraft(null);
      return;
    }
    if (draft.kind === "mkdir") {
      const path = cwd ? `${cwd}/${name}` : name;
      try {
        await api.mkdir(projectId, path, false);
        toast.success(t("files.createdFolder", { name }));
        setDraft(null);
        refresh();
      } catch (err) {
        useApp.getState().handleApiError(err);
        toast.error(t("files.mkdirFailed"), { description: (err as Error).message });
      }
      return;
    }
    const entry = listing.entries?.find((e) => e.path === draft.path);
    if (!entry || name === entry.name) {
      setDraft(null);
      return;
    }
    try {
      const res = await api.renameFile(projectId, draft.path, name);
      retargetTab(projectId, draft.path, res.path);
      toast.success(t("files.renamed", { name }));
      setDraft(null);
      refresh();
    } catch (err) {
      useApp.getState().handleApiError(err);
      toast.error(t("files.renameFailed"), { description: (err as Error).message });
    }
  }, [projectId, draft, cwd, listing.entries, t, refresh]);

  const copyPath = useCallback(
    async (path: string) => {
      const text = joinHostPath(project?.workingDir, path);
      const ok = await copyText(text);
      if (ok) toast.success(t("files.copiedPath"));
      else toast.error(t("files.copyFailed"));
    },
    [project?.workingDir, t]
  );

  const toggleSort = (key: SortKey) => {
    setSort((cur) =>
      cur.key === key ? { key, dir: cur.dir === "asc" ? "desc" : "asc" } : { key, dir: key === "name" ? "asc" : "desc" }
    );
  };

  const toggleOne = (path: string) => {
    setSelected((cur) => {
      const next = new Set(cur);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
    lastClicked.current = path;
  };

  const selectRange = (path: string, additive: boolean) => {
    const idx = visible.findIndex((e) => e.path === path);
    const from = lastClicked.current
      ? visible.findIndex((e) => e.path === lastClicked.current)
      : idx;
    if (idx < 0 || from < 0) {
      toggleOne(path);
      return;
    }
    const lo = Math.min(from, idx);
    const hi = Math.max(from, idx);
    const range = visible.slice(lo, hi + 1).map((e) => e.path);
    setSelected((cur) => {
      const next = additive ? new Set(cur) : new Set<string>();
      for (const p of range) next.add(p);
      return next;
    });
  };

  const onActivate = (entry: WorkspaceEntry) => {
    setSelected(new Set([entry.path]));
    lastClicked.current = entry.path;
    if (entry.kind === "dir") goTo(entry.path);
    else if (projectId) openFile(projectId, entry.path);
  };

  const onRowPointer = (e: MouseEvent, entry: WorkspaceEntry) => {
    // macOS 的 ctrl+click 就是右键，会先来 click 再来 contextmenu，别当成多选
    if (isMac && e.ctrlKey && !e.metaKey) return;
    const additive = isMac ? e.metaKey : e.ctrlKey || e.metaKey;
    if (e.shiftKey) {
      e.preventDefault();
      selectRange(entry.path, additive);
      return;
    }
    if (additive) {
      e.preventDefault();
      toggleOne(entry.path);
      return;
    }
    setSelected(new Set([entry.path]));
    lastClicked.current = entry.path;
  };

  const openRowMenu = (e: MouseEvent, entry: WorkspaceEntry) => {
    // 右键只开菜单，不改选中：点到未勾的项就只对这一项动手，已勾的多项仍对整组
    const inSelection = selected.has(entry.path);
    const targets = inSelection && selected.size > 1 ? selectedEntries : [entry];
    const multi = targets.length > 1;
    const files = targets.filter((x) => x.kind === "file");
    const items: MenuItemSpec[] = [];
    if (!multi) {
      items.push({
        label: entry.kind === "dir" ? t("files.openFolder") : t("files.open"),
        onSelect: () => onActivate(entry),
      });
    }
    items.push({
      label: t("files.copyPath"),
      onSelect: () => {
        void (multi
          ? copyText(targets.map((x) => joinHostPath(project?.workingDir, x.path)).join("\n")).then((ok) => {
              toast[ok ? "success" : "error"](ok ? t("files.copiedPath") : t("files.copyFailed"));
            })
          : copyPath(entry.path));
      },
    });
    if (files.length > 0) {
      items.push({
        label: t("files.download"),
        onSelect: () => downloadPaths(files.map((x) => x.path)),
      });
    }
    if (!multi && entry.kind === "dir") {
      items.push({
        label: t("files.uploadHere"),
        onSelect: () => void uploadTo(entry.path),
      });
    }
    if (!multi) {
      items.push({
        label: t("files.rename"),
        onSelect: () => startRename(entry),
      });
    }
    items.push({
      label: t("files.delete"),
      danger: true,
      separated: true,
      onSelect: () => void removePaths(targets.map((x) => x.path)),
    });
    openContextMenu(e, items);
  };

  const parent = parentRel(cwd);
  const canParent = parent !== null;
  const canMutate = Boolean(projectId);

  return (
    <aside className="@container flex min-h-0 flex-1 flex-col border-l bg-sidebar text-sidebar-foreground">
      <div className="shrink-0 border-b px-2 py-1.5">
        <Input
          value={pathDraft}
          onChange={(e) => setPathDraft(e.target.value)}
          onBlur={commitPath}
          onKeyDown={(e) => {
            if (e.nativeEvent.isComposing) return;
            if (e.key === "Enter") {
              e.preventDefault();
              commitPath();
            }
            if (e.key === "Escape") {
              e.preventDefault();
              setPathDraft(hostPath);
              (e.target as HTMLInputElement).blur();
            }
          }}
          disabled={!projectId}
          spellCheck={false}
          aria-label={t("files.title")}
          className="h-7 bg-background px-2 font-mono text-[12px] dark:bg-background"
        />
        <div className="mt-1 flex items-center gap-0.5">
          <IconBtn
            label={t("files.parent")}
            disabled={!canMutate || !canParent}
            onClick={() => parent !== null && goTo(parent)}
          >
            <ArrowLeft />
          </IconBtn>
          <IconBtn
            label={t("files.refresh")}
            disabled={!canMutate}
            onClick={refresh}
          >
            <RefreshCw className={cn(listing.loading && "animate-spin")} />
          </IconBtn>
          <IconBtn
            label={showHidden ? t("files.hideHidden") : t("files.showHidden")}
            disabled={!canMutate}
            pressed={showHidden}
            onClick={() => setShowHidden((v) => !v)}
          >
            {showHidden ? <Eye /> : <EyeOff />}
          </IconBtn>
          <IconBtn
            label={t("files.newFolder")}
            disabled={!canMutate}
            onClick={startMkdir}
          >
            <FolderPlus />
          </IconBtn>
          <IconBtn
            label={t("files.upload")}
            disabled={!canMutate}
            onClick={() => void uploadTo(cwd)}
          >
            <Upload />
          </IconBtn>
          <IconBtn
            label={t("files.uploadFolder")}
            disabled={!canMutate}
            onClick={() => void uploadFolderTo(cwd)}
          >
            <FolderUp />
          </IconBtn>
          <IconBtn
            label={t("files.downloadMany")}
            disabled={!canMutate || selectedFiles.length === 0}
            onClick={() => downloadPaths(selectedFiles.map((e) => e.path))}
          >
            <Download />
          </IconBtn>
          <span className="mx-0.5 h-4 w-px bg-border" />
          <IconBtn
            label={t("files.delete")}
            disabled={!canMutate || selected.size === 0}
            danger
            onClick={() => void removePaths([...selected])}
          >
            <Trash2 />
          </IconBtn>
        </div>
      </div>

      {!projectId ? (
        <Hint>{t("files.noProject")}</Hint>
      ) : listing.error && !listing.entries ? (
        <Hint>
          {t("files.loadFailed")}
          <span className="mt-1 block font-mono text-[11px]">{listing.error}</span>
        </Hint>
      ) : (
        <div className="min-h-0 flex-1 overflow-auto">
          <div
            className={cn(
              "sticky top-0 z-10 grid items-center border-b bg-sidebar text-[11px] text-muted-foreground",
              "grid-cols-[24px_minmax(0,1fr)]",
              "@[300px]:grid-cols-[24px_minmax(0,1fr)_3.5rem]",
              "@[360px]:grid-cols-[24px_minmax(0,1fr)_3.5rem_9.5rem]"
            )}
          >
            <div className="grid place-items-center py-1">
              <Checkbox
                checked={allVisibleSelected ? true : someVisibleSelected ? "indeterminate" : false}
                disabled={visible.length === 0}
                onCheckedChange={() => {
                  if (allVisibleSelected) setSelected(new Set());
                  else setSelected(new Set(visible.map((e) => e.path)));
                }}
                aria-label={t("files.selectAll")}
                className="size-3.5"
              />
            </div>
            <SortHead
              label={t("files.name")}
              active={sort.key === "name"}
              dir={sort.dir}
              onClick={() => toggleSort("name")}
            />
            <SortHead
              label={t("files.size")}
              active={sort.key === "size"}
              dir={sort.dir}
              onClick={() => toggleSort("size")}
              className="hidden text-right @[300px]:block"
            />
            <SortHead
              label={t("files.mtime")}
              active={sort.key === "mtime"}
              dir={sort.dir}
              onClick={() => toggleSort("mtime")}
              className="hidden pr-2 text-right @[360px]:block"
            />
          </div>

          {listing.loading && !listing.entries ? (
            <p className="px-3 py-3 text-[11px] text-muted-foreground">{t("files.loading")}</p>
          ) : visible.length === 0 && !draft ? (
            <p className="px-3 py-3 text-[11px] text-muted-foreground">{t("files.empty")}</p>
          ) : (
            <ul>
              {draft?.kind === "mkdir" && (
                <NameRow
                  icon={<Folder className="size-3.5 shrink-0 text-muted-foreground" />}
                  value={draft.name}
                  onChange={(name) => setDraft({ kind: "mkdir", name })}
                  onCommit={() => void commitDraft()}
                  onCancel={() => setDraft(null)}
                  ignoreBlur={ignoreBlur}
                />
              )}
              {visible.map((entry) => {
                const renaming = draft?.kind === "rename" && draft.path === entry.path;
                const Icon = entry.kind === "dir" ? Folder : iconFor(entry.name);
                const on = selected.has(entry.path);
                const opened = activePath === entry.path;
                return (
                  <li
                    key={entry.path}
                    onClick={(e) => {
                      if (renaming) return;
                      if ((e.target as HTMLElement).closest("[data-file-check]")) return;
                      onRowPointer(e, entry);
                    }}
                    onDoubleClick={(e) => {
                      e.preventDefault();
                      if (!renaming) onActivate(entry);
                    }}
                    onContextMenu={(e) => openRowMenu(e, entry)}
                    title={entry.path}
                    className={cn(
                      "grid h-7 cursor-default items-center border-b border-border/60 text-[11.5px] hover:bg-accent/50",
                      "grid-cols-[24px_minmax(0,1fr)]",
                      "@[300px]:grid-cols-[24px_minmax(0,1fr)_3.5rem]",
                      "@[360px]:grid-cols-[24px_minmax(0,1fr)_3.5rem_9.5rem]",
                      on && "bg-accent/70",
                      opened && !on && "bg-accent/30"
                    )}
                  >
                    <div className="grid place-items-center" data-file-check>
                      <Checkbox
                        checked={on}
                        onCheckedChange={() => toggleOne(entry.path)}
                        onClick={(e) => e.stopPropagation()}
                        aria-label={entry.name}
                        className="size-3.5"
                      />
                    </div>
                    <div className="flex min-w-0 items-center gap-1.5 pr-1">
                      <Icon className="size-3.5 shrink-0 text-muted-foreground" />
                      {renaming && draft.kind === "rename" ? (
                        <NameInput
                          value={draft.name}
                          onChange={(name) => setDraft({ kind: "rename", path: draft.path, name })}
                          onCommit={() => void commitDraft()}
                          onCancel={() => setDraft(null)}
                          ignoreBlur={ignoreBlur}
                        />
                      ) : (
                        <button
                          type="button"
                          className="min-w-0 flex-1 truncate text-left font-mono hover:underline"
                          onClick={(e) => {
                            e.stopPropagation();
                            if (isMac && e.ctrlKey && !e.metaKey) return;
                            onActivate(entry);
                          }}
                        >
                          {entry.name}
                        </button>
                      )}
                    </div>
                    <span className="hidden truncate pr-1 text-right font-mono text-[11px] text-muted-foreground @[300px]:block">
                      {entry.kind === "dir" ? "—" : formatSize(entry.size)}
                    </span>
                    <span className="hidden truncate pr-2 text-right font-mono text-[11px] text-muted-foreground @[360px]:block">
                      {formatMtime(entry.mtime)}
                    </span>
                  </li>
                );
              })}
            </ul>
          )}
          {listing.truncated && (
            <p className="px-3 py-2 text-[11px] text-muted-foreground">
              {t("files.truncated", { n: listing.entries?.length ?? 0 })}
            </p>
          )}
        </div>
      )}
    </aside>
  );
}

function IconBtn({
  label,
  disabled,
  pressed,
  danger,
  onClick,
  children,
}: {
  label: string;
  disabled?: boolean;
  pressed?: boolean;
  danger?: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <Button
      variant="ghost"
      size="icon-xs"
      className={cn(
        "text-muted-foreground",
        pressed && "bg-accent text-foreground",
        danger && "hover:text-destructive"
      )}
      aria-label={label}
      title={label}
      aria-pressed={pressed}
      disabled={disabled}
      onClick={onClick}
    >
      {children}
    </Button>
  );
}

function SortHead({
  label,
  active,
  dir,
  onClick,
  className,
}: {
  label: string;
  active: boolean;
  dir: SortDir;
  onClick: () => void;
  className?: string;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        "flex h-7 min-w-0 items-center gap-0.5 px-1 text-left hover:text-foreground",
        className
      )}
    >
      <span className="truncate">{label}</span>
      {active ? (
        dir === "asc" ? (
          <ChevronUp className="size-3 shrink-0" />
        ) : (
          <ChevronDown className="size-3 shrink-0" />
        )
      ) : null}
    </button>
  );
}

function NameRow({
  icon,
  value,
  onChange,
  onCommit,
  onCancel,
  ignoreBlur,
}: {
  icon: ReactNode;
  value: string;
  onChange: (name: string) => void;
  onCommit: () => void;
  onCancel: () => void;
  ignoreBlur: { current: boolean };
}) {
  return (
    <li
      className={cn(
        "grid h-7 items-center border-b border-border/60 bg-accent/40 text-[11.5px]",
        "grid-cols-[24px_minmax(0,1fr)]"
      )}
    >
      <span />
      <div className="flex min-w-0 items-center gap-1.5 pr-2">
        {icon}
        <NameInput
          value={value}
          onChange={onChange}
          onCommit={onCommit}
          onCancel={onCancel}
          ignoreBlur={ignoreBlur}
        />
      </div>
    </li>
  );
}

function NameInput({
  value,
  onChange,
  onCommit,
  onCancel,
  ignoreBlur,
}: {
  value: string;
  onChange: (name: string) => void;
  onCommit: () => void;
  onCancel: () => void;
  ignoreBlur: { current: boolean };
}) {
  return (
    <input
      value={value}
      onChange={(e) => onChange(e.target.value)}
      onKeyDown={(e: KeyboardEvent<HTMLInputElement>) => {
        e.stopPropagation();
        if (e.nativeEvent.isComposing) return;
        if (e.key === "Enter") {
          e.preventDefault();
          onCommit();
        }
        if (e.key === "Escape") {
          e.preventDefault();
          ignoreBlur.current = true;
          onCancel();
        }
      }}
      onBlur={() => {
        if (ignoreBlur.current) {
          ignoreBlur.current = false;
          return;
        }
        onCommit();
      }}
      autoFocus
      onFocus={(e) => e.currentTarget.select()}
      spellCheck={false}
      className="h-5 min-w-0 flex-1 rounded-sm border border-ring bg-background px-1 font-mono text-[11.5px] outline-none"
    />
  );
}

function Hint({ children }: { children: ReactNode }) {
  return <p className="px-3 py-4 text-xs leading-relaxed text-muted-foreground">{children}</p>;
}

const IMAGE_EXT = new Set(["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "avif", "svg"]);
const TEXT_EXT = new Set(["md", "markdown", "txt", "log", "rst", "adoc"]);
const CODE_EXT = new Set([
  "ts", "tsx", "js", "jsx", "mjs", "cjs", "json", "jsonc", "css", "scss", "html", "vue",
  "py", "go", "rs", "java", "kt", "swift", "c", "h", "cc", "cpp", "hpp", "cs", "rb", "php",
  "sh", "bash", "zsh", "fish", "ps1", "sql", "yml", "yaml", "toml", "ini", "conf", "xml",
]);

function iconFor(name: string): typeof File {
  const i = name.lastIndexOf(".");
  const ext = i <= 0 ? "" : name.slice(i + 1).toLowerCase();
  if (IMAGE_EXT.has(ext)) return FileImage;
  if (TEXT_EXT.has(ext)) return FileText;
  if (CODE_EXT.has(ext)) return FileCode;
  return File;
}
