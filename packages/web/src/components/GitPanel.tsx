import {
  memo,
  useCallback,
  useDeferredValue,
  useEffect,
  useMemo,
  useRef,
  useState,
  type MouseEvent,
  type ReactNode,
} from "react";
import { useTranslation } from "react-i18next";
import {
  ArrowDown,
  ArrowUp,
  Check,
  ChevronDown,
  ChevronRight,
  CloudDownload,
  Copy,
  Filter,
  RefreshCw,
  Search,
  X,
} from "lucide-react";
import type {
  GitCommitDetail,
  GitFileChange,
  GitLogCommit,
  GitOpInput,
  GitRefLabel,
  GitRefsInfo,
  GitResetMode,
  GitSnapshot,
  GitUnavailableReason,
} from "@falcon/shared";
import { api } from "../api.js";
import { confirmAsync } from "../lib/confirmAsync.js";
import {
  selectFocusProjectId,
  selectMultiRepoDir,
  useApp,
  type MenuItemSpec,
} from "../store.js";
import { AppDialog } from "./common/AppDialog.js";
import { Field } from "./common/Field.js";
import { MultiRepoSelect } from "./common/MultiRepoSelect.js";
import { openContextMenu } from "./common/Menu.js";
import { cn, pollWhileVisible } from "@/lib/utils";
import { laneColor, layoutCommitGraph, type GraphRow } from "@/lib/gitGraph";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import {
  FileChangeView,
  FileViewToggle,
  loadFileViewMode,
  porcelainStatus,
  saveFileViewMode,
  statusText,
  type FileViewMode,
} from "./FileChangeView.js";

/** 头部 Pull / Push 计数的轮询间隔。列表本身不轮询，见 loadLog */
const POLL_MS = 5000;

/**
 * 提交行的几何（px）。行高**固定**，两个理由：
 * - 一屏能多放几条。标题一律 truncate，本来就不需要可变高度；
 * - 行高定了，SVG 的 viewBox 才能与真实像素 1:1。可变高度只能靠
 *   preserveAspectRatio="none" 去拉伸，而那会让 x / y 缩放比不同——
 *   圆点会被压成扁椭圆，斜线的粗细也跟着变。
 */
const ROW_H = 40;
const LANE_W = 14;
const DOT_R = 3.5;

/**
 * 右侧 History 面板：提交图 + 提交详情。
 *
 * 由 App 在面板打开时才挂载，关掉就卸载，不占往返。
 *
 * 三份数据各有各的节奏，刻意不合并成一个轮询：
 * - 头部的 ahead/behind 走 5s 轮询（Pull/Push 上的数字要跟得上远端）；
 * - 列表只在筛选变化 / 手动刷新 / pull-push 之后重取——正读着的历史在眼皮
 *   底下重排会让人跟丢位置，而历史本来就不怎么变；
 * - 详情随选中项走。
 */
export function GitPanel() {
  const { t } = useTranslation();
  const projectId = useApp(selectFocusProjectId);
  const openDiff = useApp((s) => s.openDiff);
  const project = useApp((s) =>
    projectId ? s.projects.find((p) => p.id === projectId) : undefined
  );
  const multiRepo = useApp((s) => s.multiRepo);
  // 多仓库项目：所有 git 请求打到当前选中的成员；单仓库项目恒为 undefined
  const repoDir = selectMultiRepoDir({ multiRepo }, project);

  const [snap, setSnap] = useState<GitSnapshot | null>(null);
  const [refs, setRefs] = useState<GitRefsInfo | null>(null);
  const [commits, setCommits] = useState<GitLogCommit[]>([]);
  const [hasMore, setHasMore] = useState(false);
  const [logError, setLogError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);

  const [query, setQuery] = useState("");
  const [branch, setBranch] = useState<string | null>(null);
  const [author, setAuthor] = useState<string | null>(null);
  const [filtersOpen, setFiltersOpen] = useState(true);
  const [selected, setSelected] = useState<string | null>(null);
  const [syncing, setSyncing] = useState<"pull" | "push" | "fetch" | "op" | null>(null);
  const [tick, setTick] = useState(0);
  const [branchDialog, setBranchDialog] = useState<{ startPoint: string; hint: string } | null>(
    null
  );
  const [resetDialog, setResetDialog] = useState<{ rev: string; hint: string } | null>(null);
  const [rewordDialog, setRewordDialog] = useState<{ sha: string; hint: string; message: string } | null>(
    null
  );
  const [treeOpen, setTreeOpen] = useState(true);

  // 搜索框每敲一个字都发一次 git log 太重，交给 React 的延迟值降频
  const deferredQuery = useDeferredValue(query);

  const reset = () => {
    setSnap(null);
    setRefs(null);
    setCommits([]);
    setSelected(null);
    setLogError(null);
    setQuery("");
    setBranch(null);
    setAuthor(null);
  };
  // 换项目 = 换仓库，筛选条件跟着清掉；留着上一个仓库的分支名只会得到空列表。
  // 多仓库项目切成员同理
  useEffect(reset, [projectId, repoDir]);

  // 头部计数：只有它轮询
  useEffect(() => {
    if (!projectId) return;
    let cancelled = false;
    let inFlight = false;
    const load = async () => {
      if (inFlight) return;
      inFlight = true;
      try {
        const next = await api.gitSnapshot(projectId, { repo: repoDir });
        if (!cancelled) setSnap(next);
      } catch (err) {
        if (!cancelled) useApp.getState().handleApiError(err);
      } finally {
        inFlight = false;
      }
    };
    void load();
    const stop = pollWhileVisible(() => void load(), POLL_MS);
    return () => {
      cancelled = true;
      stop();
    };
  }, [projectId, repoDir, tick]);

  // 筛选下拉的候选值。跟着 tick 刷新，新建的分支才会出现在下拉里
  useEffect(() => {
    if (!projectId) return;
    let cancelled = false;
    api
      .gitRefs(projectId, { repo: repoDir })
      .then((next) => !cancelled && setRefs(next))
      .catch((err) => !cancelled && useApp.getState().handleApiError(err));
    return () => {
      cancelled = true;
    };
  }, [projectId, repoDir, tick]);

  // 列表：筛选变化或手动刷新时重取第一页
  useEffect(() => {
    if (!projectId) return;
    let cancelled = false;
    setLoading(true);
    api
      .gitLog(projectId, {
        branch: branch ?? undefined,
        author: author ?? undefined,
        q: deferredQuery.trim() || undefined,
        repo: repoDir,
      })
      .then((page) => {
        if (cancelled) return;
        setCommits(page.commits);
        setHasMore(page.hasMore);
        setLogError(page.available ? null : (page.detail ?? t(reasonKey(page.reason))));
        // 选中项在新结果里还在就留着，否则收起详情——保持一个已经不在列表里的
        // 选中项，详情区会跟列表说着两件事
        setSelected((cur) => (cur && page.commits.some((c) => c.sha === cur) ? cur : null));
      })
      .catch((err) => {
        if (cancelled) return;
        useApp.getState().handleApiError(err);
        setLogError((err as Error).message);
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [projectId, repoDir, branch, author, deferredQuery, tick, t]);

  const loadMore = useCallback(async () => {
    if (!projectId || loadingMore) return;
    setLoadingMore(true);
    try {
      const page = await api.gitLog(projectId, {
        branch: branch ?? undefined,
        author: author ?? undefined,
        q: deferredQuery.trim() || undefined,
        skip: commits.length,
        repo: repoDir,
      });
      // 去重后再接，而且要拿**更新函数里的** cur 去比：翻页这一趟里若有
      // 新提交进来，skip 会让同一条出现两次，而 React 的 key 撞车会直接
      // 白屏一块。闭包里的 commits 可能已经被一次刷新换掉了，不能用
      setCommits((cur) => {
        const seen = new Set(cur.map((c) => c.sha));
        return [...cur, ...page.commits.filter((c) => !seen.has(c.sha))];
      });
      setHasMore(page.hasMore);
    } catch (err) {
      useApp.getState().handleApiError(err);
    } finally {
      setLoadingMore(false);
    }
  }, [projectId, repoDir, branch, author, deferredQuery, commits, loadingMore]);

  const sync = async (action: "pull" | "push" | "fetch") => {
    if (!projectId || syncing) return;
    setSyncing(action);
    try {
      const res =
        action === "fetch"
          ? await api.gitOp(projectId, { op: "fetch" }, { repo: repoDir })
          : await api.gitSync(projectId, action, { repo: repoDir });
      // 成功也要出提示：一次 --ff-only 的 pull 常常什么都不发生
      // （Already up to date），没有回声的话按钮像是没反应。
      // 失败的 body 是 git 的原话——"为什么被拒"只有它说得清，而且往往
      // 直接把该敲的命令印在里面（比如没有 upstream 时的 --set-upstream）
      useApp.getState().toast({
        kind: res.ok ? "success" : "danger",
        title: t(action === "pull" ? "git.pull" : action === "push" ? "git.push" : "git.fetch"),
        body: res.detail,
        sticky: !res.ok,
      });
      if (res.ok) setTick((n) => n + 1);
    } catch (err) {
      useApp.getState().handleApiError(err);
    } finally {
      setSyncing(null);
    }
  };

  const runOp = async (input: GitOpInput, title: string): Promise<boolean> => {
    if (!projectId || syncing) return false;
    setSyncing("op");
    try {
      const res = await api.gitOp(projectId, input, { repo: repoDir });
      useApp.getState().toast({
        kind: res.ok ? "success" : "danger",
        title,
        body: res.detail,
        sticky: !res.ok,
      });
      if (res.ok) setTick((n) => n + 1);
      return res.ok;
    } catch (err) {
      useApp.getState().handleApiError(err);
      return false;
    } finally {
      setSyncing(null);
    }
  };

  const confirmOp = async (
    spec: { title: string; body: string; confirmLabel: string },
    input: GitOpInput,
    title: string
  ) => {
    if (!(await confirmAsync(spec))) return;
    await runOp(input, title);
  };

  const copySha = async (sha: string) => {
    try {
      await navigator.clipboard.writeText(sha);
    } catch {
      // http 下没有 clipboard API，菜单项失败时不弹窗——复制不是主路径
    }
  };

  const commitMenu = (commit: GitLogCommit): MenuItemSpec[] => [
    { label: t("git.copySha"), onSelect: () => void copySha(commit.sha) },
    {
      label: t("git.checkoutRev"),
      onSelect: () =>
        void confirmOp(
          {
            title: t("git.checkoutRevTitle", { short: commit.short }),
            body: t("git.checkoutRevBody"),
            confirmLabel: t("git.checkoutRev"),
          },
          { op: "checkout", rev: commit.sha, detach: true },
          t("git.checkoutRev")
        ),
    },
    {
      label: t("git.newBranch"),
      onSelect: () => setBranchDialog({ startPoint: commit.sha, hint: commit.short }),
    },
    {
      label: t("git.cherryPick"),
      onSelect: () =>
        void confirmOp(
          {
            title: t("git.cherryPickTitle", { short: commit.short }),
            body: `${commit.subject}\n\n${t("git.cherryPickBody")}`,
            confirmLabel: t("git.cherryPick"),
          },
          { op: "cherry-pick", sha: commit.sha },
          t("git.cherryPick")
        ),
    },
    {
      label: t("git.merge"),
      onSelect: () =>
        void confirmOp(
          {
            title: t("git.mergeTitle", { name: commit.short }),
            body: `${commit.subject}\n\n${t("git.mergeBody")}`,
            confirmLabel: t("git.merge"),
          },
          { op: "merge", rev: commit.sha },
          t("git.merge")
        ),
    },
    {
      label: t("git.rebase"),
      onSelect: () =>
        void confirmOp(
          {
            title: t("git.rebaseTitle", { name: commit.short }),
            body: `${commit.subject}\n\n${t("git.rebaseBody")}`,
            confirmLabel: t("git.rebase"),
          },
          { op: "rebase", rev: commit.sha },
          t("git.rebase")
        ),
    },
    {
      label: t("git.revert"),
      separated: true,
      onSelect: () =>
        void confirmOp(
          {
            title: t("git.revertTitle", { short: commit.short }),
            body: `${commit.subject}\n\n${t("git.revertBody")}`,
            confirmLabel: t("git.revert"),
          },
          { op: "revert", sha: commit.sha },
          t("git.revert")
        ),
    },
    {
      label: t("git.reset"),
      onSelect: () => setResetDialog({ rev: commit.sha, hint: commit.short }),
    },
    {
      label: t("git.drop"),
      separated: true,
      onSelect: () =>
        void confirmOp(
          {
            title: t("git.dropTitle", { short: commit.short }),
            body: t("git.dropBody"),
            confirmLabel: t("git.drop"),
          },
          { op: "drop", sha: commit.sha },
          t("git.drop")
        ),
    },
    ...(isHeadCommit(commit, snap?.headSha)
      ? [
          {
            label: t("git.squash"),
            onSelect: () =>
              void confirmOp(
                {
                  title: t("git.squashTitle", { short: commit.short }),
                  body: t("git.squashBody"),
                  confirmLabel: t("git.squash"),
                },
                { op: "squash", sha: commit.sha },
                t("git.squash")
              ),
          },
          {
            label: t("git.reword"),
            onSelect: () =>
              setRewordDialog({ sha: commit.sha, hint: commit.short, message: commit.subject }),
          },
        ]
      : []),
  ];

  const refMenu = (refLabel: GitRefLabel): MenuItemSpec[] => {
    if (refLabel.kind === "tag") {
      return [
        {
          label: t("git.checkoutTag"),
          onSelect: () =>
            void confirmOp(
              {
                title: t("git.checkoutRevTitle", { short: refLabel.name }),
                body: t("git.checkoutRevBody"),
                confirmLabel: t("git.checkoutTag"),
              },
              { op: "checkout", rev: refLabel.name, detach: true },
              t("git.checkoutTag")
            ),
        },
        {
          label: t("git.merge"),
          onSelect: () =>
            void confirmOp(
              {
                title: t("git.mergeTitle", { name: refLabel.name }),
                body: t("git.mergeBody"),
                confirmLabel: t("git.merge"),
              },
              { op: "merge", rev: refLabel.name },
              t("git.merge")
            ),
        },
      ];
    }
    return [
      {
        label: t("git.checkoutBranch"),
        onSelect: () =>
          void confirmOp(
            {
              title: t("git.checkoutBranchTitle", { name: refLabel.name }),
              body: t("git.checkoutBranchBody"),
              confirmLabel: t("git.checkoutBranch"),
            },
            {
              op: "checkout-branch",
              branch: refLabel.name,
              createTracking: refLabel.kind === "remote",
            },
            t("git.checkoutBranch")
          ),
      },
      {
        label: t("git.merge"),
        onSelect: () =>
          void confirmOp(
            {
              title: t("git.mergeTitle", { name: refLabel.name }),
              body: t("git.mergeBody"),
              confirmLabel: t("git.merge"),
            },
            { op: "merge", rev: refLabel.name },
            t("git.merge")
          ),
      },
      {
        label: t("git.rebase"),
        onSelect: () =>
          void confirmOp(
            {
              title: t("git.rebaseTitle", { name: refLabel.name }),
              body: t("git.rebaseBody"),
              confirmLabel: t("git.rebase"),
            },
            { op: "rebase", rev: refLabel.name },
            t("git.rebase")
          ),
      },
      ...(refLabel.head
        ? [
            {
              label: t("git.forcePush"),
              separated: true as const,
              onSelect: () =>
                void confirmOp(
                  {
                    title: t("git.forcePushTitle"),
                    body: t("git.forcePushBody"),
                    confirmLabel: t("git.forcePush"),
                  },
                  { op: "push", forceWithLease: true },
                  t("git.forcePush")
                ),
            },
          ]
        : []),
    ];
  };

  function isHeadCommit(commit: GitLogCommit, headSha?: string): boolean {
    if (!headSha) return false;
    return commit.short === headSha || commit.sha.startsWith(headSha);
  }

  const graph = useMemo(() => layoutCommitGraph(commits), [commits]);
  const filtered = branch != null || author != null || deferredQuery.trim() !== "";

  /**
   * 作者下拉把"我"单独提到最上面——按自己筛是最常用的一档，而在一个
   * 几十人的仓库里，从按提交数排的长名单里找自己得翻半天。
   */
  const authorGroups = useMemo<FilterGroup[]>(() => {
    const authors = refs?.authors ?? [];
    const me = refs?.me;
    // me 得真的在作者名单里才给：配了 user.name 但没在这个仓库提交过的话，
    // 点下去只会得到一页空白
    const meOption: FilterOption[] =
      me && authors.includes(me) ? [{ value: me, label: t("git.me") }] : [];
    return [
      ...(meOption.length ? [{ options: meOption }] : []),
      ...(authors.length
        ? [
            {
              separated: meOption.length > 0,
              options: authors.map((a) => ({ value: a, label: a })),
            },
          ]
        : []),
    ];
  }, [refs, t]);

  const unavailable = snap && !snap.available;
  return (
    // 宽度由外层 ResizableSlot 统一管：四个右侧面板共用同一个槽位，
    // 来回切不能改宽度，否则终端会跟着 reflow。
    <aside className="flex min-h-0 flex-1 flex-col border-l bg-sidebar text-sidebar-foreground">
      <div className="flex h-11 shrink-0 items-center gap-1.5 border-b pr-2 pl-3">
        <span className="min-w-0 flex-1 truncate text-[13px] font-semibold">
          {t("git.history")}
        </span>
        <MultiRepoSelect project={project} />
        <SyncButton
          icon={<ArrowDown />}
          label={t("git.pull")}
          count={snap?.behind ?? 0}
          busy={syncing === "pull"}
          disabled={!projectId || !!unavailable || syncing !== null}
          onClick={() => void sync("pull")}
        />
        <SyncButton
          icon={<ArrowUp />}
          label={t("git.push")}
          count={snap?.ahead ?? 0}
          busy={syncing === "push"}
          disabled={!projectId || !!unavailable || syncing !== null}
          title={t("git.pushHint")}
          onClick={() => void sync("push")}
          onContextMenu={(e) => {
            e.preventDefault();
            e.stopPropagation();
            openContextMenu(e, [
              {
                label: t("git.forcePush"),
                onSelect: () =>
                  void confirmOp(
                    {
                      title: t("git.forcePushTitle"),
                      body: t("git.forcePushBody"),
                      confirmLabel: t("git.forcePush"),
                    },
                    { op: "push", forceWithLease: true },
                    t("git.forcePush")
                  ),
              },
            ]);
          }}
        />
        <Button
          variant="ghost"
          size="icon-xs"
          className="text-muted-foreground"
          aria-label={t("git.fetch")}
          title={t("git.fetch")}
          disabled={!projectId || !!unavailable || syncing !== null}
          onClick={() => void sync("fetch")}
        >
          {syncing === "fetch" ? (
            <RefreshCw className="animate-spin" />
          ) : (
            <CloudDownload />
          )}
        </Button>
        <Button
          variant="ghost"
          size="icon-xs"
          className="text-muted-foreground"
          aria-label={t("git.refresh")}
          title={t("git.refresh")}
          disabled={!projectId || loading}
          onClick={() => setTick((n) => n + 1)}
        >
          <RefreshCw className={cn(loading && "animate-spin")} />
        </Button>
      </div>

      {!projectId ? (
        <Hint>{t("git.noProject")}</Hint>
      ) : unavailable ? (
        <Hint>
          {t(reasonKey(snap.reason))}
          {snap.detail && <span className="mt-1 block font-mono text-[11px]">{snap.detail}</span>}
        </Hint>
      ) : (
        <>
          <div className="flex shrink-0 items-center gap-1.5 px-3 pt-2.5">
            <div className="relative min-w-0 flex-1">
              <Search className="pointer-events-none absolute top-1/2 left-2 size-3.5 -translate-y-1/2 text-muted-foreground" />
              <input
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                placeholder={t("git.searchCommits")}
                aria-label={t("git.searchCommits")}
                className="h-8 w-full rounded-md border bg-background pr-6 pl-7 text-xs outline-none placeholder:text-muted-foreground focus-visible:ring-[3px] focus-visible:ring-ring/50"
              />
              {query && (
                <button
                  type="button"
                  aria-label={t("git.clearSearch")}
                  onClick={() => setQuery("")}
                  className="absolute top-1/2 right-1.5 -translate-y-1/2 text-muted-foreground hover:text-foreground"
                >
                  <X className="size-3.5" />
                </button>
              )}
            </div>
            <Button
              variant="ghost"
              size="icon-xs"
              aria-label={t("git.filters")}
              title={t("git.filters")}
              className={cn("text-muted-foreground", filtered && "text-foreground")}
              onClick={() => setFiltersOpen((v) => !v)}
            >
              <Filter className={cn(filtered && "fill-current")} />
            </Button>
          </div>

          {filtersOpen && (
            <div className="flex shrink-0 items-center gap-1 px-2 pt-2">
              <FilterMenu
                label={t("git.user")}
                value={author}
                groups={authorGroups}
                onPick={setAuthor}
                allLabel={t("git.allUsers")}
                searchPlaceholder={t("git.searchUser")}
              />
            </div>
          )}

          <BranchTree
            open={treeOpen}
            onOpenChange={setTreeOpen}
            refs={refs}
            selected={branch}
            onPick={setBranch}
            onMenu={(e, refLabel) => openContextMenu(e, refMenu(refLabel))}
          />

          <div className="min-h-0 flex-1 overflow-y-auto pt-1.5">
            {logError ? (
              <Hint>
                {t("git.loadFailed")}
                <span className="mt-1 block font-mono text-[11px]">{logError}</span>
              </Hint>
            ) : commits.length === 0 ? (
              <Hint>{loading ? t("git.loading") : t(filtered ? "git.noMatch" : "git.noCommits")}</Hint>
            ) : (
              <>
                <ul>
                  {commits.map((commit, i) => (
                    <CommitRow
                      key={commit.sha}
                      commit={commit}
                      row={graph[i]!}
                      selected={commit.sha === selected}
                      onSelect={() =>
                        setSelected((cur) => (cur === commit.sha ? null : commit.sha))
                      }
                      onMenu={(e) => {
                        setSelected(commit.sha);
                        openContextMenu(e, commitMenu(commit));
                      }}
                      onRefMenu={(e, refLabel) => openContextMenu(e, refMenu(refLabel))}
                    />
                  ))}
                </ul>
                {hasMore && (
                  <div className="px-3 py-2">
                    <Button
                      variant="ghost"
                      size="sm"
                      className="h-7 w-full text-xs text-muted-foreground"
                      disabled={loadingMore}
                      onClick={() => void loadMore()}
                    >
                      {loadingMore ? t("git.loading") : t("git.loadMore")}
                    </Button>
                  </div>
                )}
              </>
            )}
          </div>

          {selected && (
            <CommitDetail
              projectId={projectId}
              sha={selected}
              repo={repoDir}
              onOpenFile={(file, commit) => openDiff(projectId, file, commit, repoDir)}
              onClose={() => setSelected(null)}
            />
          )}
        </>
      )}
      {branchDialog && (
        <NewBranchDialog
          hint={branchDialog.hint}
          busy={syncing !== null}
          onClose={() => setBranchDialog(null)}
          onCreate={async (name, checkout) => {
            const ok = await runOp(
              {
                op: "branch-create",
                name,
                startPoint: branchDialog.startPoint,
                checkout,
              },
              t("git.newBranch")
            );
            if (ok) setBranchDialog(null);
          }}
        />
      )}
      {resetDialog && (
        <ResetDialog
          hint={resetDialog.hint}
          busy={syncing !== null}
          onClose={() => setResetDialog(null)}
          onReset={async (mode) => {
            const ok = await runOp(
              { op: "reset", rev: resetDialog.rev, mode },
              t("git.reset")
            );
            if (ok) setResetDialog(null);
          }}
        />
      )}
      {rewordDialog && (
        <RewordDialog
          hint={rewordDialog.hint}
          initial={rewordDialog.message}
          busy={syncing !== null}
          onClose={() => setRewordDialog(null)}
          onSave={async (message) => {
            const ok = await runOp(
              { op: "reword", sha: rewordDialog.sha, message },
              t("git.reword")
            );
            if (ok) setRewordDialog(null);
          }}
        />
      )}
    </aside>
  );
}

/**
 * Pull / Push 按钮。
 *
 * 计数为 0 时**不禁用**：behind 是 5 秒前的事实，用户点它多半正是因为
 * "我知道远端有东西了"，拦住只会逼他去开终端。有待同步的那个用实底
 * （secondary）拉开对比，0 的那个退成描边——两者都还是按得动的按钮。
 */
function SyncButton({
  icon,
  label,
  count,
  busy,
  disabled,
  title,
  onClick,
  onContextMenu,
}: {
  icon: ReactNode;
  label: string;
  count: number;
  busy: boolean;
  disabled: boolean;
  title?: string;
  onClick: () => void;
  onContextMenu?: (e: MouseEvent<HTMLButtonElement>) => void;
}) {
  return (
    <Button
      variant={count > 0 ? "secondary" : "outline"}
      size="xs"
      className="h-7 gap-1 px-2 text-[11px] font-normal"
      title={title ?? label}
      disabled={disabled}
      onClick={onClick}
      onContextMenu={onContextMenu}
    >
      <span className="font-medium">{label}</span>
      {busy ? <RefreshCw className="animate-spin" /> : icon}
      <span className="tabular-nums">{count}</span>
    </Button>
  );
}

/** 筛选下拉里的一项。dot 是那个小圆点：标出"跟当前 HEAD 有关系"的分支 */
interface FilterOption {
  value: string;
  label: string;
  dot?: boolean;
}

interface FilterGroup {
  /** 分组名（Local / Remote）。不给就是一组匿名项 */
  title?: string;
  /** 组上方画一条分隔线。与 title 二选一，都不给就直接接在上一组后面 */
  separated?: boolean;
  options: FilterOption[];
}

/**
 * Branch / User 的筛选下拉。
 *
 * 用 Popover 而不是 DropdownMenu：菜单顶上要放一个搜索框，而 Radix 的
 * DropdownMenu 会把键盘焦点抢给菜单项（那是 menu role 的正确行为），
 * 输入框在里面根本敲不进字。Popover 没有这层焦点管理，正合适。
 *
 * 搜索在前端做：候选就是分支名与作者名，几十上百条，本地过滤比一次
 * 往返快得多，也不会在每敲一个字母时打一次 SSH。
 */
function FilterMenu({
  label,
  value,
  groups,
  onPick,
  allLabel,
  searchPlaceholder,
}: {
  label: string;
  value: string | null;
  groups: FilterGroup[];
  onPick: (next: string | null) => void;
  allLabel: string;
  searchPlaceholder: string;
}) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [q, setQ] = useState("");

  const needle = q.trim().toLowerCase();
  const visible = groups
    .map((g) => ({
      ...g,
      options: needle
        ? g.options.filter((o) => o.label.toLowerCase().includes(needle))
        : g.options,
    }))
    // 整组被筛空就连标题一起收掉，不然会剩一排没有内容的 Local / Remote
    .filter((g) => g.options.length > 0);

  const pick = (next: string | null) => {
    onPick(next);
    setOpen(false);
  };

  return (
    <Popover
      open={open}
      onOpenChange={(next) => {
        setOpen(next);
        // 关掉时清空搜索：下次打开该是完整列表，而不是上次翻到一半的样子
        if (!next) setQ("");
      }}
    >
      <PopoverTrigger asChild>
        <Button
          variant="ghost"
          size="sm"
          className={cn(
            "h-7 min-w-0 gap-1 px-1.5 text-xs text-muted-foreground",
            value != null && "text-foreground"
          )}
        >
          <span className="min-w-0 truncate">{value ?? label}</span>
          <ChevronDown className="shrink-0 opacity-60" />
        </Button>
      </PopoverTrigger>
      <PopoverContent align="start" className="w-64 p-0">
        <div className="flex h-9 items-center gap-2 border-b px-2.5">
          <Search className="size-3.5 shrink-0 text-muted-foreground" />
          <input
            value={q}
            onChange={(e) => setQ(e.target.value)}
            placeholder={searchPlaceholder}
            aria-label={searchPlaceholder}
            autoFocus
            className="min-w-0 flex-1 bg-transparent text-xs outline-none placeholder:text-muted-foreground"
          />
        </div>
        <div className="max-h-72 overflow-y-auto p-1">
          <FilterRow
            label={allLabel}
            selected={value == null}
            onPick={() => pick(null)}
            bold
          />
          {visible.map((group, i) => (
            <div key={group.title ?? `g${i}`}>
              {group.title && (
                <div className="px-2 pt-2 pb-0.5 text-[11px] text-muted-foreground">
                  {group.title}
                </div>
              )}
              {group.separated && <div className="my-1 border-t" />}
              {group.options.map((opt) => (
                <FilterRow
                  key={opt.value}
                  label={opt.label}
                  dot={opt.dot}
                  selected={value === opt.value}
                  onPick={() => pick(opt.value)}
                />
              ))}
            </div>
          ))}
          {visible.length === 0 && (
            <p className="px-2 py-3 text-center text-xs text-muted-foreground">
              {t("git.noFilterMatch")}
            </p>
          )}
        </div>
      </PopoverContent>
    </Popover>
  );
}

/** 下拉里的一行。选中项整行铺主色底，与参考稿一致 */
function FilterRow({
  label,
  dot,
  selected,
  bold,
  onPick,
}: {
  label: string;
  dot?: boolean;
  selected: boolean;
  bold?: boolean;
  onPick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onPick}
      title={label}
      className={cn(
        "flex h-7 w-full min-w-0 items-center gap-2 rounded-sm px-2 text-left text-xs",
        selected
          ? "bg-primary text-primary-foreground"
          : "hover:bg-accent hover:text-accent-foreground",
        bold && "font-medium"
      )}
    >
      {/* 圆点那一格恒定占位：有点没点的行左边缘要对齐，否则列表看着是歪的 */}
      <span className="flex size-1.5 shrink-0 items-center justify-center">
        {dot && (
          <span
            className={cn(
              "size-1.5 rounded-full",
              selected ? "bg-primary-foreground" : "bg-[var(--graph-1)]"
            )}
          />
        )}
      </span>
      <span className="min-w-0 truncate">{label}</span>
    </button>
  );
}

/**
 * 提交列表的一行：泳道图 + 标题 + ref 标签。
 *
 * memo 是必需的而不是优化：一页 60 行，每行都带一段 SVG，选中项一变就
 * 全量重画的话，方向键连按会明显掉帧。
 */
const CommitRow = memo(function CommitRow({
  commit,
  row,
  selected,
  onSelect,
  onMenu,
  onRefMenu,
}: {
  commit: GitLogCommit;
  row: GraphRow;
  selected: boolean;
  onSelect: () => void;
  onMenu: (e: MouseEvent) => void;
  onRefMenu: (e: MouseEvent, refLabel: GitRefLabel) => void;
}) {
  return (
    <li>
      <button
        type="button"
        onClick={onSelect}
        onContextMenu={onMenu}
        title={commit.subject}
        style={{ height: ROW_H }}
        className={cn(
          // 左右的 padding 放在按钮**内部**：选中态的底色仍然通栏，
          // 而图形与文字都不贴边
          "flex w-full min-w-0 items-center gap-2 pr-3 pl-2 text-left",
          selected ? "bg-accent text-accent-foreground" : "hover:bg-accent/50"
        )}
      >
        <GraphCell row={row} />
        <span className="flex min-w-0 flex-1 flex-col justify-center gap-px">
          <span className="flex min-w-0 items-center gap-1.5">
            <span className="min-w-0 truncate text-xs">{commit.subject}</span>
            {commit.refs.map((ref) => (
              <RefBadge
                key={`${ref.kind}:${ref.name}`}
                refLabel={ref}
                onMenu={(e) => onRefMenu(e, ref)}
              />
            ))}
          </span>
          <span
            className={cn(
              "min-w-0 truncate text-[11px] leading-4",
              selected ? "text-accent-foreground/70" : "text-muted-foreground"
            )}
          >
            {commit.author}
            {commit.authoredAt ? ` · ${formatWhen(commit.authoredAt)}` : ""}
          </span>
        </span>
      </button>
    </li>
  );
});

/**
 * 一行的泳道图。每行一张独立的 SVG，viewBox 与像素 1:1（见 ROW_H 的注释）。
 */
function GraphCell({ row }: { row: GraphRow }) {
  const width = row.width * LANE_W;
  const mid = ROW_H / 2;
  const cx = row.lane * LANE_W + LANE_W / 2;
  const x = (lane: number) => lane * LANE_W + LANE_W / 2;
  return (
    <svg
      aria-hidden
      width={width}
      height={ROW_H}
      viewBox={`0 0 ${width} ${ROW_H}`}
      className="block shrink-0"
    >
      {row.segments.map((seg, i) => {
        // from=null 从圆点出发，to=null 汇入圆点；两头都有就是穿过去的直线
        const x1 = seg.from == null ? cx : x(seg.from);
        const y1 = seg.from == null ? mid : 0;
        const x2 = seg.to == null ? cx : x(seg.to);
        const y2 = seg.to == null ? mid : ROW_H;
        return (
          <path
            key={i}
            d={
              x1 === x2
                ? `M${x1} ${y1}V${y2}`
                : // 斜线走三次贝塞尔，控制点取两端的中间高度：直接连直线的话
                  // 相邻两行的折角会拼不上，看起来像锯齿
                  `M${x1} ${y1}C${x1} ${(y1 + y2) / 2} ${x2} ${(y1 + y2) / 2} ${x2} ${y2}`
            }
            fill="none"
            stroke={laneColor(seg.lane)}
            strokeWidth={1.5}
          />
        );
      })}
      <circle
        cx={cx}
        cy={mid}
        r={DOT_R}
        // 合并提交画空心点，与 git 图形客户端的惯例一致
        fill={row.merge ? "var(--sidebar)" : laneColor(row.lane)}
        stroke={laneColor(row.lane)}
        strokeWidth={1.5}
      />
    </svg>
  );
}

/** 分支 / 标签徽标。本地分支与远程分支要一眼分得开，所以不共用一个色 */
function RefBadge({
  refLabel,
  onMenu,
}: {
  refLabel: GitRefLabel;
  onMenu?: (e: MouseEvent) => void;
}) {
  const tone =
    refLabel.kind === "tag"
      ? "bg-warning/15 text-warning"
      : refLabel.kind === "remote"
        ? "bg-muted text-muted-foreground"
        : "bg-primary/15 text-foreground";
  return (
    <span
      // leading 写死：行高固定成 ROW_H 之后，徽标不能再靠继承来的行高
      // 去撑，不然一个继承值变了就把整行顶破
      className={cn("max-w-28 shrink-0 truncate rounded-sm px-1 text-[10px] leading-4", tone)}
      title={refLabel.name}
      onContextMenu={onMenu}
    >
      {refLabel.name}
    </span>
  );
}

/**
 * 窄栏里的分支树。IDEA Log 左侧那栏在 260px 里放不下，所以叠在提交列表上面：
 * 点一行就是筛 log，右键走检出 / 合并 / 变基。
 */
function BranchTree({
  open,
  onOpenChange,
  refs,
  selected,
  onPick,
  onMenu,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  refs: GitRefsInfo | null;
  selected: string | null;
  onPick: (name: string | null) => void;
  onMenu: (e: MouseEvent, refLabel: GitRefLabel) => void;
}) {
  const { t } = useTranslation();
  const local = (refs?.branches ?? []).filter((b) => !b.remote);
  const remote = (refs?.branches ?? []).filter((b) => b.remote);
  const tags = refs?.tags ?? [];
  const headUpstream = local.find((b) => b.head)?.upstream;

  return (
    <div className="shrink-0 border-b">
      <button
        type="button"
        className="flex h-7 w-full items-center gap-1 px-3 text-[11px] text-muted-foreground hover:text-foreground"
        aria-expanded={open}
        aria-label={t(open ? "git.collapseBranches" : "git.expandBranches")}
        onClick={() => onOpenChange(!open)}
      >
        {open ? <ChevronDown className="size-3" /> : <ChevronRight className="size-3" />}
        <span className="min-w-0 flex-1 truncate text-left">{t("git.branchesTree")}</span>
      </button>
      {open && (
        <div className="max-h-36 overflow-y-auto pb-1">
          <TreeRow
            label={t("git.allRefs")}
            active={selected == null}
            onPick={() => onPick(null)}
          />
          {local.length > 0 && (
            <TreeGroup title={t("git.localBranches")}>
              {local.map((b) => (
                <TreeRow
                  key={`l:${b.name}`}
                  label={b.name}
                  dot={b.head}
                  active={selected === b.name}
                  onPick={() => onPick(b.name)}
                  onMenu={(e) => onMenu(e, { name: b.name, kind: "local", head: b.head })}
                />
              ))}
            </TreeGroup>
          )}
          {remote.length > 0 && (
            <TreeGroup title={t("git.remoteBranches")}>
              {remote.map((b) => (
                <TreeRow
                  key={`r:${b.name}`}
                  label={b.name}
                  dot={b.name === headUpstream}
                  active={selected === b.name}
                  onPick={() => onPick(b.name)}
                  onMenu={(e) => onMenu(e, { name: b.name, kind: "remote" })}
                />
              ))}
            </TreeGroup>
          )}
          {tags.length > 0 && (
            <TreeGroup title={t("git.tags")}>
              {tags.map((name) => (
                <TreeRow
                  key={`t:${name}`}
                  label={name}
                  active={selected === name}
                  onPick={() => onPick(name)}
                  onMenu={(e) => onMenu(e, { name, kind: "tag" })}
                />
              ))}
            </TreeGroup>
          )}
        </div>
      )}
    </div>
  );
}

function TreeGroup({ title, children }: { title: string; children: ReactNode }) {
  return (
    <div>
      <div className="px-3 pt-1.5 pb-0.5 text-[10px] text-muted-foreground">{title}</div>
      {children}
    </div>
  );
}

function TreeRow({
  label,
  dot,
  active,
  onPick,
  onMenu,
}: {
  label: string;
  dot?: boolean;
  active: boolean;
  onPick: () => void;
  onMenu?: (e: MouseEvent) => void;
}) {
  return (
    <button
      type="button"
      title={label}
      onClick={onPick}
      onContextMenu={onMenu}
      className={cn(
        "flex h-6 w-full min-w-0 items-center gap-1.5 px-3 text-left text-[11px]",
        active ? "bg-accent text-accent-foreground" : "hover:bg-accent/50"
      )}
    >
      <span className="flex size-1.5 shrink-0 items-center justify-center">
        {dot && (
          <span
            className={cn(
              "size-1.5 rounded-full",
              active ? "bg-accent-foreground" : "bg-[var(--graph-1)]"
            )}
          />
        )}
      </span>
      <span className="min-w-0 truncate">{label}</span>
    </button>
  );
}

function NewBranchDialog({
  hint,
  busy,
  onClose,
  onCreate,
}: {
  hint: string;
  busy: boolean;
  onClose: () => void;
  onCreate: (name: string, checkout: boolean) => void | Promise<void>;
}) {
  const { t } = useTranslation();
  const [name, setName] = useState("");
  const [checkout, setCheckout] = useState(true);
  return (
    <AppDialog title={t("git.newBranchTitle")} onClose={onClose} lockOverlay>
      <form
        className="grid gap-3"
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            e.stopPropagation();
            onClose();
          }
        }}
        onSubmit={(e) => {
          e.preventDefault();
          const next = name.trim();
          if (!next || busy) return;
          void onCreate(next, checkout);
        }}
      >
        <p className="font-mono text-[11px] text-muted-foreground">{hint}</p>
        <Field label={t("git.newBranchName")} htmlFor="git-branch-name">
          <Input
            id="git-branch-name"
            data-autofocus
            value={name}
            onChange={(e) => setName(e.target.value)}
          />
        </Field>
        <label className="flex items-center gap-2 text-xs">
          <Checkbox checked={checkout} onCheckedChange={(v) => setCheckout(v === true)} />
          {t("git.checkoutAfterCreate")}
        </label>
        <div className="mt-1 flex justify-end gap-2">
          <Button variant="outline" type="button" onClick={onClose}>
            {t("common.cancel")}
          </Button>
          <Button type="submit" disabled={busy || !name.trim()}>
            {t("git.createBranch")}
          </Button>
        </div>
      </form>
    </AppDialog>
  );
}

function ResetDialog({
  hint,
  busy,
  onClose,
  onReset,
}: {
  hint: string;
  busy: boolean;
  onClose: () => void;
  onReset: (mode: GitResetMode) => void | Promise<void>;
}) {
  const { t } = useTranslation();
  const [mode, setMode] = useState<GitResetMode>("mixed");
  const options: { value: GitResetMode; label: string }[] = [
    { value: "soft", label: t("git.resetSoft") },
    { value: "mixed", label: t("git.resetMixed") },
    { value: "hard", label: t("git.resetHard") },
  ];
  return (
    <AppDialog title={t("git.resetTitle", { short: hint })} onClose={onClose} lockOverlay>
      <form
        className="grid gap-3"
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            e.stopPropagation();
            onClose();
          }
        }}
        onSubmit={(e) => {
          e.preventDefault();
          if (busy) return;
          void onReset(mode);
        }}
      >
        <p className="text-xs leading-relaxed text-muted-foreground">{t("git.resetBody")}</p>
        <div className="grid gap-1">
          {options.map((opt) => (
            <label
              key={opt.value}
              className={cn(
                "flex cursor-pointer items-center gap-2 rounded-md border px-2 py-1.5 text-xs",
                mode === opt.value ? "border-primary bg-primary/5" : "hover:bg-accent/50"
              )}
            >
              <input
                type="radio"
                name="git-reset-mode"
                className="size-3 accent-primary"
                checked={mode === opt.value}
                onChange={() => setMode(opt.value)}
              />
              {opt.label}
            </label>
          ))}
        </div>
        <div className="mt-1 flex justify-end gap-2">
          <Button variant="outline" type="button" onClick={onClose}>
            {t("common.cancel")}
          </Button>
          <Button type="submit" variant={mode === "hard" ? "destructive" : "default"} disabled={busy}>
            {t("git.resetConfirm")}
          </Button>
        </div>
      </form>
    </AppDialog>
  );
}

function RewordDialog({
  hint,
  initial,
  busy,
  onClose,
  onSave,
}: {
  hint: string;
  initial: string;
  busy: boolean;
  onClose: () => void;
  onSave: (message: string) => void | Promise<void>;
}) {
  const { t } = useTranslation();
  const [message, setMessage] = useState(initial);
  return (
    <AppDialog title={t("git.rewordTitle", { short: hint })} onClose={onClose} lockOverlay>
      <form
        className="grid gap-3"
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            e.stopPropagation();
            onClose();
          }
        }}
        onSubmit={(e) => {
          e.preventDefault();
          const next = message.trim();
          if (!next || busy) return;
          void onSave(next);
        }}
      >
        <Field label={t("git.rewordLabel")} htmlFor="git-reword">
          <textarea
            id="git-reword"
            data-autofocus
            rows={5}
            value={message}
            onChange={(e) => setMessage(e.target.value)}
            className="w-full resize-none rounded-md border bg-background px-2 py-1.5 text-xs outline-none focus-visible:ring-[3px] focus-visible:ring-ring/50"
          />
        </Field>
        <div className="mt-1 flex justify-end gap-2">
          <Button variant="outline" type="button" onClick={onClose}>
            {t("common.cancel")}
          </Button>
          <Button type="submit" disabled={busy || !message.trim()}>
            {t("git.rewordSave")}
          </Button>
        </div>
      </form>
    </AppDialog>
  );
}

/**
 * 下方的提交详情。
 *
 * 独立组件是为了让取详情的 effect 跟着 sha 走：写在 GitPanel 里的话，
 * 列表每翻一页都会连带重跑一次。
 */
function CommitDetail({
  projectId,
  sha,
  repo,
  onOpenFile,
  onClose,
}: {
  projectId: string;
  sha: string;
  /** 多仓库项目：详情属于哪个成员仓库 */
  repo?: string;
  onOpenFile: (file: GitFileChange, commit: { sha: string; short: string; subject: string }) => void;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const [detail, setDetail] = useState<GitCommitDetail | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [expanded, setExpanded] = useState(false);
  const [copied, setCopied] = useState(false);
  const copyTimer = useRef<number | null>(null);
  // 列表 / 目录树的偏好与「修改」面板共用一个键，两处不会各记一份
  const [mode, setMode] = useState<FileViewMode>(loadFileViewMode);

  useEffect(() => {
    let cancelled = false;
    setDetail(null);
    setError(null);
    setExpanded(false);
    api
      .gitCommit(projectId, sha, { repo })
      .then((next) => {
        if (cancelled) return;
        if (next.available) setDetail(next);
        else setError(next.detail ?? t(reasonKey(next.reason)));
      })
      .catch((err) => {
        if (cancelled) return;
        useApp.getState().handleApiError(err);
        setError((err as Error).message);
      });
    return () => {
      cancelled = true;
    };
  }, [projectId, sha, repo, t]);

  useEffect(
    () => () => {
      if (copyTimer.current) window.clearTimeout(copyTimer.current);
    },
    []
  );

  const copySha = async () => {
    try {
      await navigator.clipboard.writeText(detail?.sha ?? sha);
      setCopied(true);
      if (copyTimer.current) window.clearTimeout(copyTimer.current);
      copyTimer.current = window.setTimeout(() => setCopied(false), 1500);
    } catch {
      // http 下没有 clipboard API，静默失败——为一个复制按钮弹错误提示不值得
    }
  };

  const subject = detail?.message.split("\n", 1)[0] ?? "";
  const body = detail ? detail.message.slice(subject.length).trim() : "";

  return (
    <div className="flex max-h-[55%] min-h-0 shrink-0 flex-col border-t">
      <div className="flex h-9 shrink-0 items-center gap-1.5 px-3">
        <span className="font-mono text-[11px] text-muted-foreground">
          {detail?.short ?? sha.slice(0, 7)}
        </span>
        <button
          type="button"
          aria-label={t("git.copySha")}
          title={t("git.copySha")}
          onClick={() => void copySha()}
          className="text-muted-foreground hover:text-foreground"
        >
          {copied ? <Check className="size-3" /> : <Copy className="size-3" />}
        </button>
        <span className="min-w-0 flex-1 truncate text-[11px] text-muted-foreground">
          {detail ? `${detail.author}, ${formatWhen(detail.authoredAt)}` : ""}
        </span>
        <FileViewToggle
          mode={mode}
          onChange={(next) => {
            setMode(next);
            saveFileViewMode(next);
          }}
        />
        <Button
          variant="ghost"
          size="icon-xs"
          className="text-muted-foreground"
          aria-label={t("git.closeDetail")}
          title={t("git.closeDetail")}
          onClick={onClose}
        >
          <X />
        </Button>
      </div>

      {error ? (
        <Hint>{error}</Hint>
      ) : !detail ? (
        <Hint>{t("git.loading")}</Hint>
      ) : (
        <>
          <div className="shrink-0 px-3 pb-2.5">
            <div className="flex items-start gap-1.5">
              <p
                className={cn(
                  "min-w-0 flex-1 text-xs leading-5 font-medium",
                  !expanded && "line-clamp-2"
                )}
              >
                {expanded ? detail.message : subject}
              </p>
              {body && (
                <button
                  type="button"
                  aria-label={t(expanded ? "git.collapse" : "git.expand")}
                  title={t(expanded ? "git.collapse" : "git.expand")}
                  onClick={() => setExpanded((v) => !v)}
                  className="shrink-0 text-muted-foreground hover:text-foreground"
                >
                  <ChevronDown className={cn("size-3.5", expanded && "rotate-180")} />
                </button>
              )}
            </div>
          </div>

          <div className="min-h-0 flex-1 overflow-y-auto pb-2">
            <FileChangeView
              files={detail.files}
              mode={mode}
              onOpen={(file) =>
                onOpenFile(
                  // 详情里的状态是 raw diff 的单字母，塞进 index 那一列——
                  // GitDiffView 的 statusLabel 认的正是这个位置
                  { path: file.path, origPath: file.origPath, index: file.status, work: " " },
                  { sha: detail.sha, short: detail.short, subject }
                )
              }
            />
            {detail.fileCount > detail.files.length && (
              <p className="px-3 py-1 text-[11px] text-muted-foreground">
                {t("git.moreFiles", { n: detail.fileCount - detail.files.length })}
              </p>
            )}
            {detail.fileCount === 0 && (
              <p className="px-3 py-1 text-[11px] text-muted-foreground">
                {t("git.noFileChanges")}
              </p>
            )}
          </div>
        </>
      )}
    </div>
  );
}

function Hint({ children }: { children: ReactNode }) {
  return <p className="px-3 py-4 text-xs leading-relaxed text-muted-foreground">{children}</p>;
}

export function reasonKey(reason?: GitUnavailableReason): string {
  switch (reason) {
    case "git-missing":
      return "git.reason_git_missing";
    case "not-a-repo":
      return "git.reason_not_a_repo";
    case "no-working-dir":
      return "git.reason_no_working_dir";
    default:
      return "git.reason_link_failed";
  }
}

/** porcelain 双列 → 一句人话。GitDiffView 的标题栏用它 */
export function statusLabel(file: GitFileChange, t: (key: string) => string): string {
  return statusText(porcelainStatus(file.index, file.work), t);
}

/**
 * 详情头上的时间。当天只给时分（图里就是这样），跨天才补日期——
 * 一屏里绝大多数提交都是今天的，天天重复同一个日期是噪音。
 */
function formatWhen(ts: number): string {
  if (!ts) return "";
  const d = new Date(ts);
  const time = d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
  const today = new Date();
  const sameDay =
    d.getFullYear() === today.getFullYear() &&
    d.getMonth() === today.getMonth() &&
    d.getDate() === today.getDate();
  return sameDay ? time : `${d.toLocaleDateString()} ${time}`;
}
