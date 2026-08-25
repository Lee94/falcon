import { useEffect, useMemo, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { RefreshCw } from "lucide-react";
import type { GitWorkingChanges } from "@falcon/shared";
import { api } from "../api.js";
import { selectFocusProjectId, selectMultiRepoDir, useApp } from "../store.js";
import { MultiRepoSelect } from "./common/MultiRepoSelect.js";
import { cn, pollWhileVisible } from "@/lib/utils";
import { isMac } from "../lib/shortcuts.js";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { reasonKey } from "./GitPanel.js";
import {
  FileChangeView,
  FileViewToggle,
  loadFileViewMode,
  porcelainStatus,
  saveFileViewMode,
  type FileChangeItem,
  type FileViewMode,
} from "./FileChangeView.js";

/**
 * 轮询间隔。比侧栏那条（8s）快一点：这个面板是用户正盯着看的，
 * 改完文件切回来还显示旧列表会让人以为坏了。
 */
const POLL_MS = 4000;

/**
 * 提交的键位提示。不走 shortcuts.ts 的全局命令表——它只在提交框里生效，
 * 注册成全局绑定会在终端里抢走 ⌘↵。
 */
const COMMIT_CHORD = isMac ? "⌘↵" : "Ctrl+↵";

/**
 * 右侧「修改」面板：工作区里全部未提交的改动，勾选后提交。
 *
 * 勾选状态**不写 index**，只是"这次要提交哪些"的前端选择。服务端按
 * pathspec 提交那些路径，用户在终端里 `git add` 的暂存内容不受影响
 * （实测过，见 git/command.ts 的 commitArgs 注释）。
 */
export function ChangesPanel() {
  const { t } = useTranslation();
  const projectId = useApp(selectFocusProjectId);
  const openDiff = useApp((s) => s.openDiff);
  const project = useApp((s) =>
    projectId ? s.projects.find((p) => p.id === projectId) : undefined
  );
  const multiRepo = useApp((s) => s.multiRepo);
  // 多仓库项目：所有 git 请求打到当前选中的成员；单仓库项目恒为 undefined
  const repoDir = selectMultiRepoDir({ multiRepo }, project);

  const [data, setData] = useState<GitWorkingChanges | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [tick, setTick] = useState(0);
  const [message, setMessage] = useState("");
  const [committing, setCommitting] = useState(false);
  /**
   * 存的是**取消勾选**的那些，不是选中的。
   *
   * 于是轮询里新出现的文件天然是选中的——反过来存选中集的话，每轮都要
   * 跟新列表做一次并集，而"刚保存的文件没被勾上"是最容易漏掉的那种 bug。
   */
  const [excluded, setExcluded] = useState<Set<string>>(new Set());
  // 视图偏好与 History 详情共用一个键：同一个人对"列表还是树"的偏好
  // 不会在两个面板之间反复横跳
  const [mode, setMode] = useState<FileViewMode>(loadFileViewMode);

  const switchMode = (next: FileViewMode) => {
    setMode(next);
    saveFileViewMode(next);
  };

  useEffect(() => {
    setData(null);
    setError(null);
    setExcluded(new Set());
    setMessage("");
  }, [projectId, repoDir]);

  useEffect(() => {
    if (!projectId) return;
    let cancelled = false;
    let inFlight = false;
    const load = async () => {
      if (inFlight) return;
      inFlight = true;
      try {
        const next = await api.gitWorking(projectId, { repo: repoDir });
        if (cancelled) return;
        setData(next);
        setError(null);
      } catch (err) {
        if (cancelled) return;
        useApp.getState().handleApiError(err);
        setError((err as Error).message);
      } finally {
        inFlight = false;
        if (!cancelled) setBusy(false);
      }
    };
    void load();
    const stop = pollWhileVisible(() => void load(), POLL_MS);
    return () => {
      cancelled = true;
      stop();
    };
  }, [projectId, repoDir, tick]);

  const files: FileChangeItem[] = useMemo(
    () =>
      (data?.files ?? []).map((f) => ({
        path: f.path,
        origPath: f.origPath,
        status: porcelainStatus(f.index, f.work),
        added: f.added,
        deleted: f.deleted,
      })),
    [data]
  );

  const selected = files.filter((f) => !excluded.has(f.path));
  const allSelected = selected.length === files.length && files.length > 0;
  const selectState: boolean | "indeterminate" =
    selected.length === 0 ? false : allSelected ? true : "indeterminate";

  const toggle = (paths: string[], next: boolean) =>
    setExcluded((cur) => {
      const out = new Set(cur);
      for (const p of paths) {
        if (next) out.delete(p);
        else out.add(p);
      }
      return out;
    });

  const commit = async () => {
    if (!projectId || committing || !message.trim() || selected.length === 0) return;
    setCommitting(true);
    try {
      // 全选时走 all（服务端用 git add -A + 无 pathspec 的 commit）：命令行长度
      // 恒定，所以"提交全部改动"不受文件数限制。列表被 CAP 截断时更是只能走它
      const all = allSelected;
      const res = await api.gitCommitChanges(
        projectId,
        {
          message: message.trim(),
          all,
          // 重命名要把新旧两个路径都给 git，只给新路径会丢掉删除那一半
          paths: all
            ? undefined
            : selected.flatMap((f) => (f.origPath ? [f.origPath, f.path] : [f.path])),
        },
        { repo: repoDir }
      );
      useApp.getState().toast({
        kind: res.ok ? "success" : "danger",
        title: t("changes.commit"),
        body: res.detail,
        sticky: !res.ok,
      });
      if (res.ok) {
        setMessage("");
        setExcluded(new Set());
        setTick((n) => n + 1);
      }
    } catch (err) {
      useApp.getState().handleApiError(err);
    } finally {
      setCommitting(false);
    }
  };

  const canCommit = !!message.trim() && selected.length > 0 && !committing;

  return (
    <aside className="flex min-h-0 flex-1 flex-col border-l bg-sidebar text-sidebar-foreground">
      <div className="flex h-11 shrink-0 items-center gap-1 border-b pr-2 pl-3">
        <span className="min-w-0 flex-1 truncate text-[13px] font-semibold">
          {t("changes.title")}
        </span>
        <MultiRepoSelect project={project} />
        <FileViewToggle mode={mode} onChange={switchMode} />
        <Button
          variant="ghost"
          size="icon-xs"
          className="text-muted-foreground"
          aria-label={t("git.refresh")}
          title={t("git.refresh")}
          disabled={!projectId || busy}
          onClick={() => {
            setBusy(true);
            setTick((n) => n + 1);
          }}
        >
          <RefreshCw className={cn(busy && "animate-spin")} />
        </Button>
      </div>

      {!projectId ? (
        <Hint>{t("changes.noProject")}</Hint>
      ) : error && !data ? (
        <Hint>
          {t("changes.loadFailed")}
          <span className="mt-1 block font-mono text-[11px]">{error}</span>
        </Hint>
      ) : !data ? (
        <Hint>{t("git.loading")}</Hint>
      ) : !data.available ? (
        <Hint>
          {t(reasonKey(data.reason))}
          {data.detail && <span className="mt-1 block font-mono text-[11px]">{data.detail}</span>}
        </Hint>
      ) : data.fileCount === 0 ? (
        <Hint>{t("changes.clean")}</Hint>
      ) : (
        <>
          <div className="flex h-7 shrink-0 items-center gap-2 border-b bg-muted/40 pr-2 pl-3">
            <span className="min-w-0 flex-1 truncate text-[11px] text-muted-foreground tabular-nums">
              {t("changes.count", { n: selected.length, total: data.fileCount })}
            </span>
            <Checkbox
              className="size-3.5"
              checked={selectState}
              // 半选点一下变全选（"我要全部"），与目录行的行为一致
              onCheckedChange={() =>
                toggle(
                  files.map((f) => f.path),
                  selectState !== true
                )
              }
              aria-label={t("changes.selectAll")}
              title={t("changes.selectAll")}
            />
          </div>

          <div className="min-h-0 flex-1 overflow-y-auto py-1">
            <FileChangeView
              files={files}
              mode={mode}
              rootName={data.repoName}
              selection={{
                isSelected: (path) => !excluded.has(path),
                toggle,
              }}
              onOpen={(file) =>
                openDiff(
                  projectId,
                  {
                    path: file.path,
                    origPath: file.origPath,
                    // GitDiffView 靠 index === "?" 判断走不走 --no-index 伪 diff
                    index: file.status,
                    work: " ",
                  },
                  undefined,
                  repoDir
                )
              }
            />
            {data.fileCount > data.files.length && (
              <p className="px-3 py-1 text-[11px] text-muted-foreground">
                {t("git.moreFiles", { n: data.fileCount - data.files.length })}
              </p>
            )}
          </div>

          <div className="shrink-0 border-t p-2">
            <textarea
              value={message}
              onChange={(e) => setMessage(e.target.value)}
              onKeyDown={(e) => {
                // ⌘/Ctrl+Enter 提交。光 Enter 不行——提交信息是多行的，
                // 正文换行比少敲一个修饰键重要得多
                if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
                  e.preventDefault();
                  void commit();
                }
              }}
              rows={3}
              placeholder={t("changes.messagePlaceholder")}
              aria-label={t("changes.messagePlaceholder")}
              className="w-full resize-none rounded-md border bg-background px-2 py-1.5 text-xs outline-none placeholder:text-muted-foreground focus-visible:ring-[3px] focus-visible:ring-ring/50"
            />
            <Button
              size="sm"
              className="mt-1.5 h-7 w-full text-xs"
              disabled={!canCommit}
              title={`${t("changes.commit")} · ${COMMIT_CHORD}`}
              onClick={() => void commit()}
            >
              {committing ? (
                <RefreshCw className="animate-spin" />
              ) : (
                <>
                  {t("changes.commit")}
                  <span className="text-primary-foreground/60">{COMMIT_CHORD}</span>
                </>
              )}
            </Button>
          </div>
        </>
      )}
    </aside>
  );
}

function Hint({ children }: { children: ReactNode }) {
  return <p className="px-3 py-4 text-xs leading-relaxed text-muted-foreground">{children}</p>;
}
