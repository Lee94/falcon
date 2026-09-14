import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type FormEvent,
  type KeyboardEvent,
  type ReactNode,
} from "react";
import { useTranslation } from "react-i18next";
import {
  ArrowLeft,
  ExternalLink,
  FileText,
  Layers,
  ListTodo,
  LogIn,
  Pencil,
  Pin,
  PinOff,
  RefreshCw,
  Search,
  Table2,
  X,
} from "lucide-react";
import {
  MEEGLE_HOSTS,
  type MeeglePage,
  type MeeglePin,
  type MeeglePinInput,
  type MeegleSearchResult,
  type MeegleSpace,
  type MeegleStatus,
  type MeegleTodoAction,
  type MeegleTodoItem,
  type MeegleView,
  type MeegleWorkItem,
  type MeegleWorkItemDetail,
  type MeegleWorkItemType,
} from "@falcon/shared";
import { api, ApiRequestError } from "../api.js";
import { useApp } from "../store.js";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Segmented } from "./common/Field.js";

/**
 * 右侧「飞书项目」面板：宿主机上 meegle CLI 的一个窗口。
 *
 * 和 Git / 文件面板不同，它不跟着当前项目走——飞书项目的登录态是整台机器一份，
 * 待办也是跨空间的。面板分三页：「待办」是 mywork 的四个列表；「空间」是选一个
 * 空间之后按关键字搜视图与工作项（CLI 没有"列出全部视图"的接口，视图只能搜）；
 * 「固定」是粘贴飞书项目链接直接打开，以及固定过的视图 / 工作项（存在后端 SQLite）。
 * 点视图 / 工作项在面板内下钻（栈），顶上一个返回键；真要看全貌就点外链去飞书。
 *
 * 状态由后端 `/api/meegle/status` 说了算：没装 CLI 给安装提示，没登录给登录卡片
 * （device-code 模式，后端拉起登录进程，这里只负责把授权链接给用户并轮询）。
 * 业务请求撞上 409 说明登录态在中途没了，重新拉一次状态让面板自己切过去。
 */

const TAB_KEY = "falcon.meegle.tab";
const SPACE_KEY = "falcon.meegle.space";
const LOGIN_POLL_MS = 2000;
const SEARCH_DEBOUNCE_MS = 350;
/** 视图下钻页「全部加载」一次最多翻的页数（每页 50 条），防手滑点在几千条的视图上 */
const MAX_AUTO_PAGES = 20;

type Tab = "todo" | "space" | "pins";
const TABS: Tab[] = ["todo", "space", "pins"];

/** 面板内的下钻栈。视图与全景视图共用一页，只是取数接口不同 */
type Drill =
  | {
      kind: "view";
      spaceKey: string;
      spaceName?: string;
      viewId: string;
      label: string;
      multi: boolean;
      typeKey?: string;
      typeName?: string;
      url?: string;
    }
  | {
      kind: "item";
      spaceKey: string;
      spaceName?: string;
      id: string;
      title: string;
      typeKey?: string;
      url?: string;
    };

interface PinsApi {
  pins: MeeglePin[] | null;
  find(kind: MeeglePin["kind"], spaceKey: string, targetId: string): MeeglePin | undefined;
  toggle(input: MeeglePinInput): Promise<void>;
  rename(id: string, label: string): Promise<void>;
  remove(id: string): Promise<void>;
}

const PinsContext = createContext<PinsApi | null>(null);

function usePins(): PinsApi {
  const v = useContext(PinsContext);
  if (!v) throw new Error("PinsContext 缺失");
  return v;
}

function loadPref(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function savePref(key: string, value: string) {
  try {
    localStorage.setItem(key, value);
  } catch {
    // 私密窗口 / 禁了存储：记不住就记不住
  }
}

function isUrl(s: string): boolean {
  return /^https?:\/\/\S+$/i.test(s);
}

function loadTab(): Tab {
  const v = loadPref(TAB_KEY);
  return (TABS as string[]).includes(v ?? "") ? (v as Tab) : "todo";
}

export function MeeglePanel() {
  const { t } = useTranslation();
  const [status, setStatus] = useState<MeegleStatus | null>(null);
  const [statusError, setStatusError] = useState<string | null>(null);
  const [checking, setChecking] = useState(false);
  const [tab, setTab] = useState<Tab>(loadTab);
  const [drill, setDrill] = useState<Drill[]>([]);
  const [pins, setPins] = useState<MeeglePin[] | null>(null);

  const loadStatus = useCallback(async () => {
    setChecking(true);
    try {
      setStatus(await api.meegleStatus());
      setStatusError(null);
    } catch (err) {
      useApp.getState().handleApiError(err);
      setStatusError((err as Error).message);
    } finally {
      setChecking(false);
    }
  }, []);

  useEffect(() => {
    void loadStatus();
  }, [loadStatus]);

  // 登录进行中：轮询，授权一完成登录卡片就换成正文
  useEffect(() => {
    if (!status?.login) return;
    const timer = window.setInterval(() => void loadStatus(), LOGIN_POLL_MS);
    return () => clearInterval(timer);
  }, [status?.login, loadStatus]);

  /** 业务请求撞上 409（没装 / 没登录）：面板要切到对应提示，重新问一次状态 */
  const onUnavailable = useCallback(
    (err: unknown) => {
      if (err instanceof ApiRequestError && err.status === 409) void loadStatus();
    },
    [loadStatus]
  );

  const ready = Boolean(status?.installed && status.authenticated);

  // 固定列表是 falcon 自己的数据，登录后拉一次；增删改都在本地同步，不重新拉
  useEffect(() => {
    if (!ready) return;
    let cancelled = false;
    api
      .meeglePins()
      .then((list) => {
        if (!cancelled) setPins(list);
      })
      .catch((err) => {
        if (cancelled) return;
        useApp.getState().handleApiError(err);
        setPins([]);
      });
    return () => {
      cancelled = true;
    };
  }, [ready]);

  const pinsApi = useMemo<PinsApi>(
    () => ({
      pins,
      find: (kind, spaceKey, targetId) =>
        pins?.find((p) => p.kind === kind && p.spaceKey === spaceKey && p.targetId === targetId),
      async toggle(input) {
        const existing = pins?.find(
          (p) => p.kind === input.kind && p.spaceKey === input.spaceKey && p.targetId === input.targetId
        );
        try {
          if (existing) {
            await api.meegleUnpin(existing.id);
            setPins((cur) => cur?.filter((p) => p.id !== existing.id) ?? cur);
          } else {
            const created = await api.meeglePin(input);
            setPins((cur) => (cur?.some((p) => p.id === created.id) ? cur : [...(cur ?? []), created]));
          }
        } catch (err) {
          useApp.getState().handleApiError(err);
          useApp.getState().toast({ kind: "danger", title: t("toast.failed"), body: (err as Error).message });
        }
      },
      async rename(id, label) {
        try {
          const next = await api.meegleRenamePin(id, label);
          setPins((cur) => cur?.map((p) => (p.id === id ? next : p)) ?? cur);
        } catch (err) {
          useApp.getState().handleApiError(err);
          useApp.getState().toast({ kind: "danger", title: t("toast.failed"), body: (err as Error).message });
        }
      },
      async remove(id) {
        try {
          await api.meegleUnpin(id);
          setPins((cur) => cur?.filter((p) => p.id !== id) ?? cur);
        } catch (err) {
          useApp.getState().handleApiError(err);
          useApp.getState().toast({ kind: "danger", title: t("toast.failed"), body: (err as Error).message });
        }
      },
    }),
    [pins, t]
  );

  const push = useCallback((d: Drill) => setDrill((s) => [...s, d]), []);
  const pop = useCallback(() => setDrill((s) => s.slice(0, -1)), []);
  const openItem = useCallback(
    (item: MeegleWorkItem) =>
      push({
        kind: "item",
        spaceKey: item.spaceKey,
        spaceName: item.spaceName,
        id: item.id,
        title: item.name,
        typeKey: item.typeKey,
        url: item.url,
      }),
    [push]
  );
  const openView = useCallback(
    (spaceKey: string, view: MeegleView, spaceName?: string) =>
      push({
        kind: "view",
        spaceKey,
        spaceName,
        viewId: view.id,
        label: view.name,
        multi: false,
        typeKey: view.typeKey,
        typeName: view.typeName,
      }),
    [push]
  );
  const openPin = useCallback(
    (pin: MeeglePin) => {
      if (pin.kind === "workitem") {
        push({
          kind: "item",
          spaceKey: pin.spaceKey,
          spaceName: pin.spaceName,
          id: pin.targetId,
          title: pin.label,
          typeKey: pin.typeKey,
          url: pin.url,
        });
      } else {
        push({
          kind: "view",
          spaceKey: pin.spaceKey,
          spaceName: pin.spaceName,
          viewId: pin.targetId,
          label: pin.label,
          multi: pin.kind === "multiProjectView",
          typeKey: pin.typeKey,
          url: pin.url,
        });
      }
    },
    [push]
  );

  /** 粘贴的飞书项目链接：后端解析成三类目标之一，直接下钻。返回错误文案给输入框显示 */
  const openUrl = useCallback(
    async (url: string): Promise<string | null> => {
      try {
        const target = await api.meegleResolveUrl(url);
        if (target.kind === "workitem") {
          push({
            kind: "item",
            spaceKey: target.spaceKey,
            spaceName: target.spaceName,
            id: target.id,
            title: "",
            typeKey: target.typeKey,
            url: target.url,
          });
        } else {
          const multi = target.kind === "multiProjectView";
          push({
            kind: "view",
            spaceKey: target.spaceKey,
            spaceName: target.spaceName,
            viewId: target.viewId,
            label: t(multi ? "meegle.multiViewLabel" : "meegle.viewLabel", { id: target.viewId }),
            multi,
            typeKey: target.kind === "view" ? target.typeKey : undefined,
            url: target.url,
          });
        }
        return null;
      } catch (err) {
        useApp.getState().handleApiError(err);
        onUnavailable(err);
        return (err as Error).message;
      }
    },
    [push, onUnavailable, t]
  );

  const switchTab = (next: Tab) => {
    setTab(next);
    setDrill([]);
    savePref(TAB_KEY, next);
  };

  const top = drill[drill.length - 1];

  return (
    <PinsContext.Provider value={pinsApi}>
      <aside className="island flex min-h-0 flex-1 flex-col overflow-hidden text-sidebar-foreground">
        <div className="flex h-8.5 shrink-0 items-center gap-2 border-b pr-1.5 pl-3">
          <ListTodo className="size-3.5 shrink-0 text-muted-foreground" />
          <span className="min-w-0 flex-1 truncate text-xs font-medium">{t("meegle.title")}</span>
          {status?.user && <UserChip user={status.user} />}
          <Button
            variant="ghost"
            size="icon-xs"
            className="text-muted-foreground"
            aria-label={t("meegle.refresh")}
            title={t("meegle.refresh")}
            disabled={checking}
            onClick={() => void loadStatus()}
          >
            <RefreshCw className={cn(checking && "animate-spin")} />
          </Button>
        </div>

        {!status ? (
          <Hint>{statusError ? `${t("meegle.statusFailed")}：${statusError}` : t("meegle.loading")}</Hint>
        ) : !status.installed ? (
          <InstallHint bin={status.bin} onRecheck={() => void loadStatus()} checking={checking} />
        ) : !status.authenticated ? (
          <LoginCard status={status} onChanged={() => void loadStatus()} />
        ) : top?.kind === "view" ? (
          <ViewItems
            key={`${top.spaceKey}/${top.multi ? "m" : "v"}/${top.viewId}`}
            drill={top}
            onBack={pop}
            onOpenItem={openItem}
            onUnavailable={onUnavailable}
          />
        ) : top?.kind === "item" ? (
          <ItemDetail
            key={`${top.spaceKey}/${top.id}`}
            drill={top}
            onBack={pop}
            onUnavailable={onUnavailable}
          />
        ) : (
          <>
            <div className="shrink-0 border-b px-2 py-1.5">
              <Segmented
                dense
                value={tab}
                onChange={switchTab}
                options={[
                  { value: "todo", label: t("meegle.tabTodo") },
                  { value: "space", label: t("meegle.tabSpace") },
                  { value: "pins", label: t("meegle.tabPins") },
                ]}
              />
            </div>
            {tab === "todo" ? (
              <TodoSection onOpenItem={openItem} onUnavailable={onUnavailable} />
            ) : tab === "space" ? (
              <SpaceSection
                onOpenView={openView}
                onOpenItem={openItem}
                onOpenUrl={openUrl}
                onUnavailable={onUnavailable}
              />
            ) : (
              <PinsSection onOpenUrl={openUrl} onOpenPin={openPin} />
            )}
          </>
        )}
        {ready && status?.expiresInMinutes != null && status.expiresInMinutes <= 15 && (
          <div className="shrink-0 border-t px-3 py-1 text-[11px] text-warning">
            {t("meegle.expires", { minutes: status.expiresInMinutes })}
          </div>
        )}
      </aside>
    </PinsContext.Provider>
  );
}

function UserChip({ user }: { user: NonNullable<MeegleStatus["user"]> }) {
  const [broken, setBroken] = useState(false);
  return (
    <span
      className="flex min-w-0 max-w-28 items-center gap-1 text-[11px] text-muted-foreground"
      title={user.email ? `${user.name} · ${user.email}` : user.name}
    >
      {user.avatarUrl && !broken ? (
        // 飞书 CDN 的头像带 referrer 校验，不带 referrer 反而稳
        <img
          src={user.avatarUrl}
          alt=""
          referrerPolicy="no-referrer"
          className="size-4 shrink-0 rounded-full"
          onError={() => setBroken(true)}
        />
      ) : (
        <span className="grid size-4 shrink-0 place-items-center rounded-full bg-accent text-[9px]">
          {user.name.slice(0, 1)}
        </span>
      )}
      <span className="truncate">{user.name}</span>
    </span>
  );
}

function InstallHint({
  bin,
  onRecheck,
  checking,
}: {
  bin?: string;
  onRecheck: () => void;
  checking: boolean;
}) {
  const { t } = useTranslation();
  return (
    <div className="px-3 py-4 text-xs leading-relaxed text-muted-foreground">
      <p>{t("meegle.notInstalled")}</p>
      {bin && (
        <p className="mt-1 break-all font-mono text-[11px]" title={bin}>
          {t("meegle.binTried")}：{bin}
        </p>
      )}
      <pre className="mt-2 overflow-x-auto rounded-md border bg-background px-2 py-1.5 font-mono text-[11px] text-foreground">
        npm install -g @lark-project/meegle
      </pre>
      <p className="mt-2">{t("meegle.installHint")}</p>
      <Button size="xs" variant="outline" className="mt-3" disabled={checking} onClick={onRecheck}>
        <RefreshCw className={cn(checking && "animate-spin")} />
        {t("meegle.recheck")}
      </Button>
    </div>
  );
}

const HOST_CUSTOM = "__custom";

/**
 * 登录卡片。device-code 模式：点「登录」让后端起 `meegle auth login --device-code`，
 * 拿到授权链接后在这里给一个新窗口链接；用户授权完成，外层轮询会把面板切走。
 */
function LoginCard({ status, onChanged }: { status: MeegleStatus; onChanged: () => void }) {
  const { t } = useTranslation();
  const known = (MEEGLE_HOSTS as readonly string[]).includes(status.host ?? "");
  const [choice, setChoice] = useState<string>(
    status.host ? (known ? status.host : HOST_CUSTOM) : MEEGLE_HOSTS[0]
  );
  const [custom, setCustom] = useState(status.host && !known ? status.host : "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const host = choice === HOST_CUSTOM ? custom.trim() : choice;

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (!host) {
      setError(t("meegle.hostInvalid"));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await api.meegleLogin(host);
      onChanged();
    } catch (err) {
      useApp.getState().handleApiError(err);
      setError((err as Error).message);
    } finally {
      setBusy(false);
    }
  };

  const cancel = async () => {
    try {
      await api.meegleCancelLogin();
    } finally {
      onChanged();
    }
  };

  if (status.login) {
    return (
      <div className="px-3 py-4 text-xs leading-relaxed text-muted-foreground">
        <p>{t("meegle.loginPending")}</p>
        <a
          href={status.login.url}
          target="_blank"
          rel="noreferrer noopener"
          className="mt-3 flex h-7 items-center justify-center gap-1.5 rounded-md bg-primary px-2 text-xs font-medium text-primary-foreground hover:bg-primary/90"
        >
          <ExternalLink className="size-3.5" />
          {t("meegle.openAuth")}
        </a>
        {status.login.code && (
          <p className="mt-2">
            {t("meegle.authCode")}：
            <span className="font-mono text-foreground select-all">{status.login.code}</span>
          </p>
        )}
        <p className="mt-1 truncate font-mono text-[11px]" title={status.login.host}>
          {status.login.host}
        </p>
        <Button size="xs" variant="outline" className="mt-3" onClick={() => void cancel()}>
          {t("meegle.cancelLogin")}
        </Button>
      </div>
    );
  }

  return (
    <form className="px-3 py-4" onSubmit={(e) => void submit(e)}>
      <p className="text-xs leading-relaxed text-muted-foreground">{t("meegle.notLoggedIn")}</p>
      <div className="mt-3 text-[11px] text-muted-foreground">{t("meegle.host")}</div>
      <Select value={choice} onValueChange={setChoice}>
        <SelectTrigger className="mt-1 h-7 w-full text-xs" aria-label={t("meegle.host")}>
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value={MEEGLE_HOSTS[0]}>
            {t("meegle.hostFeishu")}
            <span className="ml-2 font-mono text-xs text-muted-foreground">{MEEGLE_HOSTS[0]}</span>
          </SelectItem>
          <SelectItem value={MEEGLE_HOSTS[1]}>
            {t("meegle.hostMeegle")}
            <span className="ml-2 font-mono text-xs text-muted-foreground">{MEEGLE_HOSTS[1]}</span>
          </SelectItem>
          <SelectItem value={HOST_CUSTOM}>{t("meegle.hostCustom")}</SelectItem>
        </SelectContent>
      </Select>
      {choice === HOST_CUSTOM && (
        <Input
          className="mt-1.5 h-7 px-2 font-mono text-xs"
          placeholder={t("meegle.hostPlaceholder")}
          aria-label={t("meegle.hostCustom")}
          value={custom}
          onChange={(e) => setCustom(e.target.value)}
        />
      )}
      <Button type="submit" size="xs" className="mt-3 h-7" disabled={busy || !host}>
        <LogIn />
        {busy ? t("meegle.loginStarting") : t("meegle.login")}
      </Button>
      {error && <p className="mt-2 text-[11px] text-destructive">{error}</p>}
    </form>
  );
}

// ---- 待办 ----

const ACTIONS: MeegleTodoAction[] = ["todo", "this_week", "overdue", "done"];

function TodoSection({
  onOpenItem,
  onUnavailable,
}: {
  onOpenItem: (item: MeegleWorkItem) => void;
  onUnavailable: (err: unknown) => void;
}) {
  const { t } = useTranslation();
  const [action, setAction] = useState<MeegleTodoAction>("todo");
  const [items, setItems] = useState<MeegleTodoItem[]>([]);
  const [page, setPage] = useState(1);
  const [hasMore, setHasMore] = useState(false);
  const [total, setTotal] = useState<number | undefined>();
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const gen = useRef(0);

  const load = useCallback(
    async (nextPage: number) => {
      const my = ++gen.current;
      setLoading(true);
      setError(null);
      try {
        const res = await api.meegleTodo(action, nextPage);
        if (gen.current !== my) return;
        setItems((cur) => (nextPage === 1 ? res.items : [...cur, ...res.items]));
        setPage(res.page);
        setHasMore(res.hasMore);
        setTotal(res.total);
      } catch (err) {
        if (gen.current !== my) return;
        useApp.getState().handleApiError(err);
        onUnavailable(err);
        setError((err as Error).message);
      } finally {
        if (gen.current === my) setLoading(false);
      }
    },
    [action, onUnavailable]
  );

  useEffect(() => {
    setItems([]);
    setTotal(undefined);
    void load(1);
  }, [load]);

  const shown = useMemo(() => filterItems(items, filter), [items, filter]);

  return (
    <>
      <div className="shrink-0 border-b px-2 py-1.5">
        <Segmented
          dense
          value={action}
          onChange={setAction}
          options={ACTIONS.map((a) => ({ value: a, label: t(`meegle.action_${a}`) }))}
        />
        <SearchBox
          className="mt-1.5"
          value={filter}
          onChange={setFilter}
          placeholder={t("meegle.filterPlaceholder")}
        />
      </div>
      <ItemList
        items={items}
        shown={shown}
        loading={loading}
        error={error}
        page={page}
        hasMore={hasMore}
        total={total}
        onMore={(p) => void load(p)}
        renderRow={(it) => (
          <ItemRow key={`${it.spaceKey}/${it.id}`} item={it} onClick={() => onOpenItem(it)}>
            <TodoMeta item={it} action={action} />
          </ItemRow>
        )}
      />
    </>
  );
}

function TodoMeta({ item, action }: { item: MeegleTodoItem; action: MeegleTodoAction }) {
  const { t } = useTranslation();
  const parts: string[] = [];
  if (item.spaceName) parts.push(item.spaceName);
  if (item.typeName) parts.push(item.typeName);
  if (item.nodeName) parts.push(`${t("meegle.node")} ${item.nodeName}`);
  else if (item.stateName) parts.push(`${t("meegle.state")} ${item.stateName}`);
  else if (item.status) parts.push(item.status);
  if (action === "done" && item.finishedAt) parts.push(`${t("meegle.finishedAt")} ${item.finishedAt}`);
  else if (item.scheduleEnd) parts.push(`${t("meegle.due")} ${dateOnly(item.scheduleEnd)}`);
  return <>{parts.join(" · ")}</>;
}

// ---- 空间 ----

function SpaceSection({
  onOpenView,
  onOpenItem,
  onOpenUrl,
  onUnavailable,
}: {
  onOpenView: (spaceKey: string, view: MeegleView, spaceName?: string) => void;
  onOpenItem: (item: MeegleWorkItem) => void;
  onOpenUrl: (url: string) => Promise<string | null>;
  onUnavailable: (err: unknown) => void;
}) {
  const { t } = useTranslation();
  const [spaces, setSpaces] = useState<MeegleSpace[] | null>(null);
  const [spacesError, setSpacesError] = useState<string | null>(null);
  const [spaceKey, setSpaceKey] = useState<string>(() => loadPref(SPACE_KEY) ?? "");
  const [types, setTypes] = useState<MeegleWorkItemType[] | null>(null);
  const [typeKey, setTypeKey] = useState("");
  const [query, setQuery] = useState("");
  const keyword = useDebounced(query.trim(), SEARCH_DEBOUNCE_MS);
  const [result, setResult] = useState<MeegleSearchResult | null>(null);
  const [recent, setRecent] = useState<MeegleWorkItem[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const gen = useRef(0);

  useEffect(() => {
    let cancelled = false;
    api
      .meegleSpaces()
      .then((list) => {
        if (cancelled) return;
        setSpaces(list);
        // 记住的空间不在最近列表里就退回第一个
        setSpaceKey((cur) => (list.some((s) => s.key === cur) ? cur : (list[0]?.key ?? "")));
      })
      .catch((err) => {
        if (cancelled) return;
        useApp.getState().handleApiError(err);
        onUnavailable(err);
        setSpaces([]);
        setSpacesError((err as Error).message);
      });
    return () => {
      cancelled = true;
    };
  }, [onUnavailable]);

  useEffect(() => {
    setTypes(null);
    setTypeKey("");
    if (!spaceKey) return;
    savePref(SPACE_KEY, spaceKey);
    let cancelled = false;
    api
      .meegleTypes(spaceKey)
      .then((list) => {
        if (!cancelled) setTypes(list.filter((x) => !x.disabled));
      })
      .catch((err) => {
        if (cancelled) return;
        useApp.getState().handleApiError(err);
        onUnavailable(err);
        setTypes([]);
      });
    return () => {
      cancelled = true;
    };
  }, [spaceKey, onUnavailable]);

  // 贴进来的是链接就直接开，不当关键字搜；其余：有关键字就搜，没关键字但选了类型就看最近
  useEffect(() => {
    const my = ++gen.current;
    setResult(null);
    setRecent(null);
    setError(null);
    if (isUrl(keyword)) {
      setLoading(true);
      void onOpenUrl(keyword).then((err) => {
        if (gen.current !== my) return;
        setLoading(false);
        if (err) setError(err);
        else setQuery("");
      });
      return;
    }
    if (!spaceKey || (!keyword && !typeKey)) {
      setLoading(false);
      return;
    }
    setLoading(true);
    const req = keyword
      ? api.meegleSearch(spaceKey, keyword, typeKey || undefined).then((r) => {
          if (gen.current === my) setResult(r);
        })
      : api.meegleRecent(spaceKey, typeKey).then((r) => {
          if (gen.current === my) setRecent(r);
        });
    req
      .catch((err) => {
        if (gen.current !== my) return;
        useApp.getState().handleApiError(err);
        onUnavailable(err);
        setError((err as Error).message);
      })
      .finally(() => {
        if (gen.current === my) setLoading(false);
      });
  }, [spaceKey, keyword, typeKey, onUnavailable, onOpenUrl]);

  const space = spaces?.find((s) => s.key === spaceKey);

  return (
    <>
      <div className="shrink-0 border-b px-2 py-1.5">
        <Select value={spaceKey} onValueChange={setSpaceKey} disabled={!spaces?.length}>
          <SelectTrigger className="h-7 w-full text-xs" aria-label={t("meegle.space")}>
            <SelectValue placeholder={spaces ? t("meegle.spacePlaceholder") : t("meegle.loadingList")} />
          </SelectTrigger>
          <SelectContent>
            {spaces?.map((s) => (
              <SelectItem key={s.key} value={s.key}>
                {s.name}
                <span className="ml-2 font-mono text-[11px] text-muted-foreground">{s.simpleName}</span>
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <SearchBox
          className="mt-1.5"
          value={query}
          onChange={setQuery}
          placeholder={t("meegle.searchPlaceholder")}
          disabled={!spaceKey}
        />
        {types && types.length > 0 && (
          <div className="mt-1.5 flex flex-wrap gap-1" role="radiogroup" aria-label={t("meegle.d_type")}>
            <TypeChip on={typeKey === ""} onClick={() => setTypeKey("")}>
              {t("meegle.typeAll")}
            </TypeChip>
            {types.map((ty) => (
              <TypeChip key={ty.key} on={typeKey === ty.key} onClick={() => setTypeKey(ty.key)}>
                {ty.name}
              </TypeChip>
            ))}
          </div>
        )}
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto">
        {spaces && spaces.length === 0 ? (
          <Hint>{spacesError ?? t("meegle.noSpaces")}</Hint>
        ) : !spaceKey ? (
          <Hint>{t("meegle.loadingList")}</Hint>
        ) : error ? (
          <Hint>
            {t("meegle.loadFailed")}
            <span className="mt-1 block font-mono text-[11px]">{error}</span>
          </Hint>
        ) : loading ? (
          <Hint>{isUrl(keyword) ? t("meegle.resolving") : t("meegle.searching")}</Hint>
        ) : result ? (
          <SearchResults
            result={result}
            onOpenView={(v) => onOpenView(spaceKey, v, space?.name)}
            onOpenItem={onOpenItem}
          />
        ) : recent ? (
          <>
            <GroupTitle>
              {t("meegle.recentOf", { type: types?.find((x) => x.key === typeKey)?.name ?? "" })}
            </GroupTitle>
            {recent.length === 0 ? (
              <Hint>{t("meegle.empty")}</Hint>
            ) : (
              <ul>
                {recent.map((it) => (
                  <ItemRow key={it.id} item={it} onClick={() => onOpenItem(it)}>
                    {[it.status, it.updatedAt && dateOnly(it.updatedAt)].filter(Boolean).join(" · ")}
                  </ItemRow>
                ))}
              </ul>
            )}
          </>
        ) : (
          <Hint>{space ? t("meegle.searchHint") : t("meegle.loadingList")}</Hint>
        )}
      </div>
    </>
  );
}

function SearchResults({
  result,
  onOpenView,
  onOpenItem,
}: {
  result: MeegleSearchResult;
  onOpenView: (view: MeegleView) => void;
  onOpenItem: (item: MeegleWorkItem) => void;
}) {
  const { t } = useTranslation();
  const nothing = result.views.length === 0 && result.items.length === 0;
  return (
    <>
      {nothing && result.errors.length === 0 && <Hint>{t("meegle.noResults")}</Hint>}
      {result.views.length > 0 && (
        <>
          <GroupTitle>
            {t("meegle.views")} <span className="text-muted-foreground/70">{result.views.length}</span>
          </GroupTitle>
          <ul>
            {result.views.map((v) => (
              <li key={`${v.typeKey}/${v.id}`}>
                <button
                  type="button"
                  className="flex w-full items-baseline gap-2 border-b px-3 py-1.5 text-left outline-none hover:bg-accent/50 focus-visible:bg-accent/50"
                  onClick={() => onOpenView(v)}
                >
                  <span className="min-w-0 flex-1 truncate text-xs" title={v.name}>
                    {v.name}
                  </span>
                  <span className="shrink-0 text-[11px] text-muted-foreground">{v.typeName}</span>
                </button>
              </li>
            ))}
          </ul>
        </>
      )}
      {result.items.length > 0 && (
        <>
          <GroupTitle>
            {t("meegle.items")} <span className="text-muted-foreground/70">{result.items.length}</span>
          </GroupTitle>
          <ul>
            {result.items.map((it) => (
              <ItemRow key={`${it.typeKey}/${it.id}`} item={it} onClick={() => onOpenItem(it)}>
                {[it.typeName, it.status, it.updatedAt && dateOnly(it.updatedAt)].filter(Boolean).join(" · ")}
              </ItemRow>
            ))}
          </ul>
        </>
      )}
      {result.errors.length > 0 && (
        <p className="px-3 py-2 text-[11px] leading-relaxed text-destructive">
          {t("meegle.partialFailed")}
          {result.errors.map((e, i) => (
            <span key={i} className="block truncate font-mono" title={e}>
              {e}
            </span>
          ))}
        </p>
      )}
    </>
  );
}

// ---- 固定 ----

function PinsSection({
  onOpenUrl,
  onOpenPin,
}: {
  onOpenUrl: (url: string) => Promise<string | null>;
  onOpenPin: (pin: MeeglePin) => void;
}) {
  const { t } = useTranslation();
  const { pins } = usePins();
  const [url, setUrl] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const open = async (raw: string) => {
    const u = raw.trim();
    if (!u || busy) return;
    setBusy(true);
    setError(null);
    const err = await onOpenUrl(u);
    setBusy(false);
    if (err) setError(err);
    else setUrl("");
  };

  return (
    <>
      <form
        className="shrink-0 border-b px-2 py-1.5"
        onSubmit={(e) => {
          e.preventDefault();
          void open(url);
        }}
      >
        <div className="flex gap-1">
          <Input
            value={url}
            placeholder={t("meegle.urlPlaceholder")}
            aria-label={t("meegle.urlPlaceholder")}
            className="h-7 min-w-0 flex-1 px-2 text-xs"
            onChange={(e) => setUrl(e.target.value)}
            // 粘贴进来的是完整链接就直接开，省一次回车
            onPaste={(e) => {
              const text = e.clipboardData.getData("text").trim();
              if (isUrl(text)) {
                e.preventDefault();
                setUrl(text);
                void open(text);
              }
            }}
          />
          <Button type="submit" size="xs" className="h-7 shrink-0" disabled={busy || !url.trim()}>
            {busy ? t("meegle.resolving") : t("meegle.open")}
          </Button>
        </div>
        {error && <p className="mt-1 text-[11px] text-destructive">{error}</p>}
      </form>
      <div className="min-h-0 flex-1 overflow-y-auto">
        {!pins ? (
          <Hint>{t("meegle.loadingList")}</Hint>
        ) : pins.length === 0 ? (
          <Hint>{t("meegle.pinsEmpty")}</Hint>
        ) : (
          <ul>
            {pins.map((pin) => (
              <PinRow key={pin.id} pin={pin} onOpen={() => onOpenPin(pin)} />
            ))}
          </ul>
        )}
      </div>
    </>
  );
}

const PIN_ICONS = { view: Table2, multiProjectView: Layers, workitem: FileText } as const;

function PinRow({ pin, onOpen }: { pin: MeeglePin; onOpen: () => void }) {
  const { t } = useTranslation();
  const pins = usePins();
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(pin.label);
  const Icon = PIN_ICONS[pin.kind];

  const save = () => {
    const label = draft.trim();
    setEditing(false);
    if (label && label !== pin.label) void pins.rename(pin.id, label);
    else setDraft(pin.label);
  };

  const onKey = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") {
      e.preventDefault();
      save();
    } else if (e.key === "Escape") {
      e.preventDefault();
      setDraft(pin.label);
      setEditing(false);
    }
  };

  const meta = [t(`meegle.kind_${pin.kind}`), pin.spaceName].filter(Boolean).join(" · ");

  return (
    <li className="group relative border-b">
      {editing ? (
        <div className="flex items-start gap-2 px-3 py-1.5">
          <Icon className="mt-1 size-3.5 shrink-0 text-muted-foreground" />
          <Input
            autoFocus
            value={draft}
            placeholder={t("meegle.renamePlaceholder")}
            aria-label={t("meegle.rename")}
            className="h-6 px-1.5 text-xs"
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={onKey}
            onBlur={save}
          />
        </div>
      ) : (
        <>
          <button
            type="button"
            className="flex w-full items-start gap-2 px-3 py-1.5 pr-20 text-left outline-none hover:bg-accent/50 focus-visible:bg-accent/50"
            onClick={onOpen}
          >
            <Icon className="mt-0.5 size-3.5 shrink-0 text-muted-foreground" />
            <span className="min-w-0 flex-1">
              <span className="line-clamp-2 text-xs leading-snug break-words" title={pin.label}>
                {pin.label}
              </span>
              <span className="block truncate text-[11px] text-muted-foreground">{meta}</span>
            </span>
          </button>
          <span className="absolute top-1 right-1 flex gap-0.5 opacity-0 group-hover:opacity-100 focus-within:opacity-100">
            <RowAction label={t("meegle.rename")} onClick={() => setEditing(true)}>
              <Pencil className="size-3" />
            </RowAction>
            {pin.url && (
              <a
                href={pin.url}
                target="_blank"
                rel="noreferrer noopener"
                className="grid size-5 place-items-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground"
                aria-label={t("meegle.openExternal")}
                title={t("meegle.openExternal")}
              >
                <ExternalLink className="size-3" />
              </a>
            )}
            <RowAction label={t("meegle.unpin")} onClick={() => void pins.remove(pin.id)}>
              <PinOff className="size-3" />
            </RowAction>
          </span>
        </>
      )}
    </li>
  );
}

function RowAction({
  label,
  onClick,
  children,
}: {
  label: string;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      className="grid size-5 place-items-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground"
      aria-label={label}
      title={label}
      onClick={onClick}
    >
      {children}
    </button>
  );
}

/** 下钻页顶栏的图钉：已固定就取消，没固定就固定 */
function PinButton({ input }: { input: MeeglePinInput }) {
  const { t } = useTranslation();
  const pins = usePins();
  const pinned = Boolean(pins.find(input.kind, input.spaceKey, input.targetId));
  const label = t(pinned ? "meegle.unpin" : "meegle.pin");
  return (
    <Button
      variant="ghost"
      size="icon-xs"
      className={cn(pinned ? "text-primary" : "text-muted-foreground")}
      aria-label={label}
      aria-pressed={pinned}
      title={label}
      disabled={!pins.pins}
      onClick={() => void pins.toggle(input)}
    >
      <Pin className={cn(pinned && "fill-current")} />
    </Button>
  );
}

// ---- 视图下钻 ----

function ViewItems({
  drill,
  onBack,
  onOpenItem,
  onUnavailable,
}: {
  drill: Extract<Drill, { kind: "view" }>;
  onBack: () => void;
  onOpenItem: (item: MeegleWorkItem) => void;
  onUnavailable: (err: unknown) => void;
}) {
  const { t } = useTranslation();
  const { spaceKey, viewId, multi } = drill;
  const [items, setItems] = useState<MeegleWorkItem[]>([]);
  const [page, setPage] = useState(1);
  const [hasMore, setHasMore] = useState(false);
  const [total, setTotal] = useState<number | undefined>();
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const [facets, setFacets] = useState<FacetSelection>({});
  const gen = useRef(0);

  const load = useCallback(
    async (nextPage: number): Promise<MeeglePage<MeegleWorkItem> | null> => {
      const my = ++gen.current;
      setLoading(true);
      setError(null);
      try {
        const res = multi
          ? await api.meegleMultiViewItems(spaceKey, viewId, nextPage)
          : await api.meegleViewItems(spaceKey, viewId, nextPage);
        if (gen.current !== my) return null;
        setItems((cur) => (nextPage === 1 ? res.items : [...cur, ...res.items]));
        setPage(res.page);
        setHasMore(res.hasMore);
        setTotal(res.total);
        return res;
      } catch (err) {
        if (gen.current !== my) return null;
        useApp.getState().handleApiError(err);
        onUnavailable(err);
        setError((err as Error).message);
        return null;
      } finally {
        if (gen.current === my) setLoading(false);
      }
    },
    [spaceKey, viewId, multi, onUnavailable]
  );

  // 过滤器只筛已加载的条目（见 FacetBar），一页 50 条的视图动辄两三页，给一次把剩下翻完的路子
  const loadAll = useCallback(async () => {
    let next = page + 1;
    for (let i = 0; i < MAX_AUTO_PAGES; i++) {
      const res = await load(next);
      if (!res?.hasMore) break;
      next = res.page + 1;
    }
  }, [load, page]);

  useEffect(() => {
    void load(1);
  }, [load]);

  const shown = useMemo(() => filterItems(matchFacets(items, facets), filter), [items, facets, filter]);
  // 全景视图跨了空间才值得每行标空间名，单空间视图那一段每行都一样，白占本来就不宽的一行
  const manySpaces = useMemo(() => facetOptions(items, "spaceName").length > 1, [items]);

  return (
    <>
      <SubHeader
        onBack={onBack}
        title={drill.label}
        meta={drill.typeName ?? (multi ? t("meegle.kind_multiProjectView") : undefined)}
        actions={
          <>
            <PinButton
              input={{
                kind: multi ? "multiProjectView" : "view",
                spaceKey,
                spaceName: drill.spaceName,
                targetId: viewId,
                typeKey: drill.typeKey,
                label: drill.label,
                url: drill.url,
              }}
            />
            {drill.url && <ExternalIconLink url={drill.url} />}
          </>
        }
      />
      <div className="shrink-0 border-b px-2 py-1.5">
        <SearchBox value={filter} onChange={setFilter} placeholder={t("meegle.filterPlaceholder")} />
        <FacetBar items={items} value={facets} onChange={setFacets} />
      </div>
      <ItemList
        items={items}
        shown={shown}
        loading={loading}
        error={error}
        page={page}
        hasMore={hasMore}
        total={total}
        onMore={(p) => void load(p)}
        onAll={() => void loadAll()}
        renderRow={(it) => (
          <ItemRow key={`${it.spaceKey}/${it.id}`} item={it} onClick={() => onOpenItem(it)}>
            {[
              multi && manySpaces ? it.spaceName : undefined,
              multi ? it.typeName : undefined,
              it.status,
              it.updatedAt && dateOnly(it.updatedAt),
            ]
              .filter(Boolean)
              .join(" · ")}
          </ItemRow>
        )}
      />
    </>
  );
}

// ---- 工作项详情 ----

function ItemDetail({
  drill,
  onBack,
  onUnavailable,
}: {
  drill: Extract<Drill, { kind: "item" }>;
  onBack: () => void;
  onUnavailable: (err: unknown) => void;
}) {
  const { t } = useTranslation();
  const { spaceKey, id, title } = drill;
  const [detail, setDetail] = useState<MeegleWorkItemDetail | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    api
      .meegleWorkItem(spaceKey, id)
      .then((d) => {
        if (!cancelled) setDetail(d);
      })
      .catch((err) => {
        if (cancelled) return;
        useApp.getState().handleApiError(err);
        onUnavailable(err);
        setError((err as Error).message);
      });
    return () => {
      cancelled = true;
    };
  }, [spaceKey, id, onUnavailable]);

  const name = detail?.name || title || t("meegle.untitled", { id });
  const url = detail?.url ?? drill.url;

  return (
    <>
      <SubHeader
        onBack={onBack}
        title={name}
        meta={detail?.typeName}
        actions={
          <>
            <PinButton
              input={{
                kind: "workitem",
                spaceKey,
                spaceName: detail?.spaceName ?? drill.spaceName,
                targetId: id,
                typeKey: detail?.typeKey ?? drill.typeKey,
                label: name,
                url,
              }}
            />
            {url && <ExternalIconLink url={url} />}
          </>
        }
      />
      <div className="min-h-0 flex-1 overflow-y-auto">
        {error ? (
          <Hint>
            {t("meegle.detailFailed")}
            <span className="mt-1 block font-mono text-[11px]">{error}</span>
          </Hint>
        ) : !detail ? (
          <Hint>{t("meegle.loadingList")}</Hint>
        ) : (
          <div className="px-3 py-2.5">
            <h3 className="text-[13px] leading-snug font-medium break-words">{name}</h3>
            <div className="mt-1.5 flex flex-wrap gap-1">
              {detail.status && <Pill>{detail.status}</Pill>}
              {detail.priority && <Pill>{detail.priority}</Pill>}
              <Pill muted>#{detail.id}</Pill>
            </div>
            <dl className="mt-3 grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1.5 text-xs">
              <Row label={t("meegle.d_space")}>{detail.spaceName}</Row>
              <Row label={t("meegle.d_type")}>{detail.typeName}</Row>
              {detail.template && <Row label={t("meegle.d_template")}>{detail.template}</Row>}
              {detail.mode && <Row label={t("meegle.d_mode")}>{detail.mode}</Row>}
              {detail.currentNodes.length > 0 && (
                <Row label={t("meegle.d_currentNode")}>
                  {detail.currentNodes.map((n) => (
                    <span key={n.name} className="block">
                      {n.name}
                      {n.owners.length > 0 && (
                        <span className="text-muted-foreground"> · {n.owners.join("、")}</span>
                      )}
                    </span>
                  ))}
                </Row>
              )}
              {detail.operators.length > 0 && (
                <Row label={t("meegle.d_operators")}>{detail.operators.join("、")}</Row>
              )}
              <Row label={t("meegle.d_createdBy")}>
                {[detail.createdBy, detail.createdAt && dateOnly(detail.createdAt)]
                  .filter(Boolean)
                  .join(" · ")}
              </Row>
              <Row label={t("meegle.d_updatedBy")}>
                {[detail.updatedBy, detail.updatedAt && dateOnly(detail.updatedAt)]
                  .filter(Boolean)
                  .join(" · ")}
              </Row>
            </dl>
            {detail.description && (
              <>
                <GroupTitle flush>{t("meegle.d_description")}</GroupTitle>
                {/* 描述是飞书那边的富文本 Markdown；面板窄，按纯文本保留换行就够看个大概 */}
                <p className="text-xs leading-relaxed break-words whitespace-pre-wrap">
                  {detail.description}
                </p>
              </>
            )}
            {detail.roles.length > 0 && (
              <>
                <GroupTitle flush>{t("meegle.d_roles")}</GroupTitle>
                <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1 text-xs">
                  {detail.roles.map((r) => (
                    <Row key={r.name} label={r.name}>
                      {r.members.join("、")}
                    </Row>
                  ))}
                </dl>
              </>
            )}
            {url && (
              <a
                href={url}
                target="_blank"
                rel="noreferrer noopener"
                className="mt-4 flex h-7 items-center justify-center gap-1.5 rounded-md border px-2 text-xs hover:bg-accent/50"
              >
                <ExternalLink className="size-3.5" />
                {t("meegle.openExternal")}
              </a>
            )}
          </div>
        )}
      </div>
    </>
  );
}

// ---- 小件 ----

/** 待办 / 视图两种列表共用的正文：空态、错误、筛不到、翻页 */
function ItemList<T extends MeegleWorkItem>({
  items,
  shown,
  loading,
  error,
  page,
  hasMore,
  total,
  onMore,
  onAll,
  renderRow,
}: {
  items: T[];
  shown: T[];
  loading: boolean;
  error: string | null;
  page: number;
  hasMore: boolean;
  total?: number;
  onMore: (page: number) => void;
  /** 给了才有「全部加载」：过滤器按已加载的条目统计，翻完才算数 */
  onAll?: () => void;
  renderRow: (item: T) => ReactNode;
}) {
  const { t } = useTranslation();
  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      {error && items.length === 0 ? (
        <Hint>
          {t("meegle.loadFailed")}
          <span className="mt-1 block font-mono text-[11px]">{error}</span>
        </Hint>
      ) : loading && items.length === 0 ? (
        <Hint>{t("meegle.loadingList")}</Hint>
      ) : items.length === 0 ? (
        <Hint>{t("meegle.empty")}</Hint>
      ) : shown.length === 0 ? (
        <Hint>{t("meegle.noResults")}</Hint>
      ) : (
        <ul>{shown.map(renderRow)}</ul>
      )}
      {items.length > 0 && (
        <LoadMore
          count={items.length}
          total={total}
          hasMore={hasMore}
          loading={loading}
          error={error}
          onMore={() => onMore(page + 1)}
          onAll={onAll}
        />
      )}
    </div>
  );
}

/**
 * 视图里的"分类"过滤器。
 *
 * 飞书那边视图左侧的分类树 / 分组是页面自己的东西：`view get` 与
 * `list-multi-project-workitems` 都只回一串工作项，全景视图 URL 里的 `node=`（左侧树
 * 的定位）也没有对应入参，CLI 根本没开出来。所以这里不是"把分类找回来"，而是就
 * **已加载的条目**现推几个维度——类型 / 状态 / 空间——把混成一长条的列表分开看。
 * 没翻到的页不在统计里，底下那行"已加载 N / 总数"与「全部加载」就是配套的。
 */
const FACET_FIELDS = ["typeName", "status", "spaceName"] as const;
type FacetField = (typeof FACET_FIELDS)[number];
type FacetSelection = Partial<Record<FacetField, string>>;

/** Radix 的 Select 不收空串当值，用哨兵表示"这一维不筛" */
const FACET_ALL = "__all";

const FACET_LABELS: Record<FacetField, string> = {
  typeName: "meegle.d_type",
  status: "meegle.d_status",
  spaceName: "meegle.d_space",
};

/** 某一维上出现过的值与条数，多的排前面；值为空的条目（补不到状态 / 类型名）不计入 */
function facetOptions(items: MeegleWorkItem[], field: FacetField): { value: string; count: number }[] {
  const counts = new Map<string, number>();
  for (const it of items) {
    const v = it[field];
    if (v) counts.set(v, (counts.get(v) ?? 0) + 1);
  }
  return [...counts]
    .map(([value, count]) => ({ value, count }))
    .sort((a, b) => b.count - a.count || a.value.localeCompare(b.value));
}

function matchFacets<T extends MeegleWorkItem>(items: T[], sel: FacetSelection): T[] {
  const picked = FACET_FIELDS.filter((f) => sel[f]);
  if (picked.length === 0) return items;
  return items.filter((it) => picked.every((f) => it[f] === sel[f]));
}

function FacetBar({
  items,
  value,
  onChange,
}: {
  items: MeegleWorkItem[];
  value: FacetSelection;
  onChange: (v: FacetSelection) => void;
}) {
  const { t } = useTranslation();
  // 只有一种取值的维度分不开任何东西（单类型视图的类型、单空间视图的空间），不占位
  const facets = useMemo(
    () =>
      FACET_FIELDS.map((field) => ({ field, options: facetOptions(items, field) })).filter(
        (f) => f.options.length > 1
      ),
    [items]
  );
  if (facets.length === 0) return null;
  return (
    <div className="mt-1.5 flex gap-1">
      {facets.map(({ field, options }) => {
        const name = t(FACET_LABELS[field]);
        const all = t("meegle.facetAll", { name });
        const picked = value[field];
        return (
          <Select
            key={field}
            value={picked ?? FACET_ALL}
            onValueChange={(v) => onChange({ ...value, [field]: v === FACET_ALL ? undefined : v })}
          >
            <SelectTrigger
              className={cn(
                "h-6 min-w-0 flex-1 gap-1 px-1.5 text-[11px]",
                picked && "bg-tint text-tint-foreground"
              )}
              aria-label={name}
              title={picked ?? all}
            >
              <span className="truncate">{picked ?? all}</span>
            </SelectTrigger>
            <SelectContent>
              <SelectItem value={FACET_ALL}>{all}</SelectItem>
              {options.map((o) => (
                <SelectItem key={o.value} value={o.value}>
                  {o.value}
                  <span className="ml-2 text-[11px] text-muted-foreground">{o.count}</span>
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        );
      })}
    </div>
  );
}

function filterItems<T extends MeegleWorkItem & Partial<MeegleTodoItem>>(items: T[], filter: string): T[] {
  const q = filter.trim().toLowerCase();
  if (!q) return items;
  return items.filter((it) =>
    [it.name, it.id, it.spaceName, it.typeName, it.nodeName, it.stateName, it.status]
      .filter((s): s is string => Boolean(s))
      .some((s) => s.toLowerCase().includes(q))
  );
}

function ItemRow({
  item,
  onClick,
  children,
}: {
  item: MeegleWorkItem;
  onClick: () => void;
  children?: ReactNode;
}) {
  const { t } = useTranslation();
  const name = item.name || t("meegle.untitled", { id: item.id });
  return (
    <li className="group relative border-b">
      <button
        type="button"
        className="block w-full px-3 py-1.5 pr-8 text-left outline-none hover:bg-accent/50 focus-visible:bg-accent/50"
        onClick={onClick}
      >
        <span className="line-clamp-2 text-xs leading-snug break-words" title={name}>
          {name}
        </span>
        {children && (
          <span className="mt-0.5 block truncate text-[11px] text-muted-foreground">{children}</span>
        )}
      </button>
      {item.url && (
        <a
          href={item.url}
          target="_blank"
          rel="noreferrer noopener"
          className="absolute top-1.5 right-1.5 grid size-5 place-items-center rounded-md text-muted-foreground opacity-0 hover:bg-accent hover:text-foreground focus-visible:opacity-100 group-hover:opacity-100"
          aria-label={t("meegle.openExternal")}
          title={t("meegle.openExternal")}
          onClick={(e) => e.stopPropagation()}
        >
          <ExternalLink className="size-3" />
        </a>
      )}
    </li>
  );
}

function ExternalIconLink({ url }: { url: string }) {
  const { t } = useTranslation();
  return (
    <a
      href={url}
      target="_blank"
      rel="noreferrer noopener"
      className="grid size-6 shrink-0 place-items-center rounded-md text-muted-foreground hover:bg-accent/50 hover:text-foreground"
      aria-label={t("meegle.openExternal")}
      title={t("meegle.openExternal")}
    >
      <ExternalLink className="size-3.5" />
    </a>
  );
}

function SubHeader({
  onBack,
  title,
  meta,
  actions,
}: {
  onBack: () => void;
  title: string;
  meta?: string;
  actions?: ReactNode;
}) {
  const { t } = useTranslation();
  return (
    <div className="flex h-8 shrink-0 items-center gap-1 border-b pr-1 pl-1">
      <Button
        variant="ghost"
        size="icon-xs"
        className="text-muted-foreground"
        aria-label={t("meegle.back")}
        title={t("meegle.back")}
        onClick={onBack}
      >
        <ArrowLeft />
      </Button>
      <span className="min-w-0 flex-1 truncate text-xs font-medium" title={title}>
        {title}
      </span>
      {meta && <span className="shrink-0 text-[11px] text-muted-foreground">{meta}</span>}
      {actions}
    </div>
  );
}

function SearchBox({
  value,
  onChange,
  placeholder,
  className,
  disabled,
}: {
  value: string;
  onChange: (v: string) => void;
  placeholder: string;
  className?: string;
  disabled?: boolean;
}) {
  const { t } = useTranslation();
  return (
    <div className={cn("relative", className)}>
      <Search className="pointer-events-none absolute top-1/2 left-2 size-3.5 -translate-y-1/2 text-muted-foreground" />
      <Input
        type="search"
        value={value}
        placeholder={placeholder}
        aria-label={placeholder}
        disabled={disabled}
        className="h-7 pr-6 pl-7 text-xs [&::-webkit-search-cancel-button]:hidden"
        onChange={(e) => onChange(e.target.value)}
      />
      {value && (
        <button
          type="button"
          className="absolute top-1/2 right-1 grid size-5 -translate-y-1/2 place-items-center rounded-md text-muted-foreground hover:text-foreground"
          aria-label={t("meegle.clearSearch")}
          onClick={() => onChange("")}
        >
          <X className="size-3" />
        </button>
      )}
    </div>
  );
}

function LoadMore({
  count,
  total,
  hasMore,
  loading,
  error,
  onMore,
  onAll,
}: {
  count: number;
  total?: number;
  hasMore: boolean;
  loading: boolean;
  error: string | null;
  onMore: () => void;
  onAll?: () => void;
}) {
  const { t } = useTranslation();
  return (
    <div className="flex items-center gap-2 px-3 py-2 text-[11px] text-muted-foreground">
      <span className="min-w-0 flex-1 truncate">
        {total != null ? t("meegle.loadedOf", { count, total }) : count}
        {error && <span className="ml-1 text-destructive">{error}</span>}
      </span>
      {hasMore && onAll && (
        <Button size="xs" variant="ghost" className="h-6" disabled={loading} onClick={onAll}>
          {t("meegle.loadAll")}
        </Button>
      )}
      {hasMore && (
        <Button size="xs" variant="outline" className="h-6" disabled={loading} onClick={onMore}>
          {loading ? t("meegle.loadingList") : t("meegle.loadMore")}
        </Button>
      )}
    </div>
  );
}

function TypeChip({ on, onClick, children }: { on: boolean; onClick: () => void; children: ReactNode }) {
  return (
    <button
      type="button"
      role="radio"
      aria-checked={on}
      className={cn(
        "h-5 rounded-full border px-2 text-[11px] leading-none whitespace-nowrap outline-none transition-colors focus-visible:ring-1 focus-visible:ring-ring",
        on
          ? "border-primary bg-primary text-primary-foreground"
          : "text-muted-foreground hover:bg-accent hover:text-accent-foreground"
      )}
      onClick={onClick}
    >
      {children}
    </button>
  );
}

function GroupTitle({ children, flush }: { children: ReactNode; flush?: boolean }) {
  return (
    <div
      className={cn(
        "text-[11px] tracking-wide text-muted-foreground",
        flush ? "mt-3 mb-1" : "border-b px-3 pt-2 pb-1"
      )}
    >
      {children}
    </div>
  );
}

function Pill({ children, muted }: { children: ReactNode; muted?: boolean }) {
  return (
    <span
      className={cn(
        "inline-flex h-5 items-center rounded-full border px-2 text-[11px] leading-none whitespace-nowrap",
        muted ? "font-mono text-muted-foreground" : "bg-accent/60"
      )}
    >
      {children}
    </span>
  );
}

function Row({ label, children }: { label: string; children?: ReactNode }) {
  const { t } = useTranslation();
  const empty = children == null || children === "" || children === false;
  return (
    <>
      <dt className="text-muted-foreground whitespace-nowrap">{label}</dt>
      <dd className="min-w-0 break-words">{empty ? t("meegle.d_empty") : children}</dd>
    </>
  );
}

function Hint({ children }: { children: ReactNode }) {
  return <p className="px-3 py-4 text-xs leading-relaxed text-muted-foreground">{children}</p>;
}

/** 飞书给的时间有 ISO（带时区）也有 `YYYY-MM-DD HH:mm`，面板里只看到天 */
function dateOnly(s: string): string {
  return s.slice(0, 10);
}

function useDebounced<T>(value: T, ms: number): T {
  const [v, setV] = useState(value);
  useEffect(() => {
    const timer = window.setTimeout(() => setV(value), ms);
    return () => clearTimeout(timer);
  }, [value, ms]);
  return v;
}
