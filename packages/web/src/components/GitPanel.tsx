import {
  memo,
  useCallback,
  useDeferredValue,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { useTranslation } from "react-i18next";
import {
  ArrowDown,
  ArrowUp,
  Check,
  ChevronDown,
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
  GitRefLabel,
  GitRefsInfo,
  GitSnapshot,
  GitUnavailableReason,
} from "@mojito/shared";
import { api } from "../api.js";
import { useApp, selectFocusProjectId } from "../store.js";
import { cn, pollWhileVisible } from "@/lib/utils";
import { laneColor, layoutCommitGraph, type GraphRow } from "@/lib/gitGraph";
import { Button } from "@/components/ui/button";
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
const ROW_H = 28;
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
  const [syncing, setSyncing] = useState<"pull" | "push" | null>(null);
  const [tick, setTick] = useState(0);

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
  // 换项目 = 换仓库，筛选条件跟着清掉；留着上一个仓库的分支名只会得到空列表
  useEffect(reset, [projectId]);

  // 头部计数：只有它轮询
  useEffect(() => {
    if (!projectId) return;
    let cancelled = false;
    let inFlight = false;
    const load = async () => {
      if (inFlight) return;
      inFlight = true;
      try {
        const next = await api.gitSnapshot(projectId);
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
  }, [projectId, tick]);

  // 筛选下拉的候选值。跟着 tick 刷新，新建的分支才会出现在下拉里
  useEffect(() => {
    if (!projectId) return;
    let cancelled = false;
    api
      .gitRefs(projectId)
      .then((next) => !cancelled && setRefs(next))
      .catch((err) => !cancelled && useApp.getState().handleApiError(err));
    return () => {
      cancelled = true;
    };
  }, [projectId, tick]);

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
  }, [projectId, branch, author, deferredQuery, tick, t]);

  const loadMore = useCallback(async () => {
    if (!projectId || loadingMore) return;
    setLoadingMore(true);
    try {
      const page = await api.gitLog(projectId, {
        branch: branch ?? undefined,
        author: author ?? undefined,
        q: deferredQuery.trim() || undefined,
        skip: commits.length,
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
  }, [projectId, branch, author, deferredQuery, commits, loadingMore]);

  const sync = async (action: "pull" | "push") => {
    if (!projectId || syncing) return;
    setSyncing(action);
    try {
      const res = await api.gitSync(projectId, action);
      // 成功也要出提示：一次 --ff-only 的 pull 常常什么都不发生
      // （Already up to date），没有回声的话按钮像是没反应。
      // 失败的 body 是 git 的原话——"为什么被拒"只有它说得清，而且往往
      // 直接把该敲的命令印在里面（比如没有 upstream 时的 --set-upstream）
      useApp.getState().toast({
        kind: res.ok ? "success" : "danger",
        title: t(action === "pull" ? "git.pull" : "git.push"),
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

  const graph = useMemo(() => layoutCommitGraph(commits), [commits]);
  const filtered = branch != null || author != null || deferredQuery.trim() !== "";

  /**
   * 分支下拉分成 Local / Remote 两组，圆点标的是"与当前 HEAD 有关系"：
   * 当前分支本身，以及它跟踪的那条远程分支。一眼能看出自己站在哪儿、
   * 对着谁推——这正是打开这个下拉时想知道的事。
   */
  const branchGroups = useMemo<FilterGroup[]>(() => {
    const all = refs?.branches ?? [];
    const headUpstream = all.find((b) => b.head)?.upstream;
    const toOption = (b: (typeof all)[number]): FilterOption => ({
      value: b.name,
      label: b.name,
      dot: b.head || (b.remote && b.name === headUpstream),
    });
    const local = all.filter((b) => !b.remote).map(toOption);
    const remote = all.filter((b) => b.remote).map(toOption);
    return [
      ...(local.length ? [{ title: t("git.localBranches"), options: local }] : []),
      ...(remote.length ? [{ title: t("git.remoteBranches"), options: remote }] : []),
    ];
  }, [refs, t]);

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
          onClick={() => void sync("push")}
        />
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
                label={t("git.branch")}
                value={branch}
                groups={branchGroups}
                onPick={setBranch}
                allLabel={t("git.allBranches")}
                searchPlaceholder={t("git.searchBranch")}
              />
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
              onOpenFile={(file, commit) => openDiff(projectId, file, commit)}
              onClose={() => setSelected(null)}
            />
          )}
        </>
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
  onClick,
}: {
  icon: ReactNode;
  label: string;
  count: number;
  busy: boolean;
  disabled: boolean;
  onClick: () => void;
}) {
  return (
    <Button
      variant={count > 0 ? "secondary" : "outline"}
      size="xs"
      className="h-7 gap-1 px-2 text-[11px] font-normal"
      title={label}
      disabled={disabled}
      onClick={onClick}
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
}: {
  commit: GitLogCommit;
  row: GraphRow;
  selected: boolean;
  onSelect: () => void;
}) {
  return (
    <li>
      <button
        type="button"
        onClick={onSelect}
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
        <span className="flex min-w-0 flex-1 items-center gap-1.5">
          <span className="min-w-0 truncate text-xs">{commit.subject}</span>
          {commit.refs.map((ref) => (
            <RefBadge key={`${ref.kind}:${ref.name}`} refLabel={ref} />
          ))}
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
function RefBadge({ refLabel }: { refLabel: GitRefLabel }) {
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
    >
      {refLabel.name}
    </span>
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
  onOpenFile,
  onClose,
}: {
  projectId: string;
  sha: string;
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
      .gitCommit(projectId, sha)
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
  }, [projectId, sha, t]);

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
