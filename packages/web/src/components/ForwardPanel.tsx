import { useEffect, useState, type FormEvent, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import {
  ArrowLeftRight,
  ArrowRight,
  Check,
  Copy,
  ExternalLink,
  Globe,
  Plus,
  Trash2,
} from "lucide-react";
import type {
  ForwardKind,
  ForwardState,
  PortForward,
  PublicShare,
  ShareOrigin,
} from "@falcon/shared";
import { api } from "../api.js";
import { writeClipboardText } from "../lib/clipboard.js";
import { useApp, selectFocusProjectId } from "../store.js";
import { cn, pollWhileVisible } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Segmented } from "./common/Field.js";

const POLL_MS = 3000;

/**
 * 右侧转发面板。SSH 端口转发只挂 SSH 项目；公网发布本地 / SSH 都行。
 * 和 Git 面板同一套挂载策略：打开才请求，关掉就卸载。
 */
export function ForwardPanel() {
  const { t } = useTranslation();
  const projectId = useApp(selectFocusProjectId);
  const project = useApp((s) => s.projects.find((p) => p.id === selectFocusProjectId(s)));
  const [rows, setRows] = useState<PortForward[] | null>(null);
  const [shares, setShares] = useState<PublicShare[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [shareError, setShareError] = useState<string | null>(null);
  const [tick, setTick] = useState(0);
  const isSsh = project?.type === "ssh";

  useEffect(() => {
    setRows(null);
    setShares(null);
    setError(null);
    setShareError(null);
  }, [projectId]);

  useEffect(() => {
    if (!projectId) {
      setRows(null);
      setShares(null);
      setError(null);
      setShareError(null);
      return;
    }
    let cancelled = false;
    let inFlight = false;
    const load = async () => {
      if (inFlight) return;
      inFlight = true;
      const tasks: Promise<void>[] = [
        api.listShares(projectId).then(
          (next) => {
            if (cancelled) return;
            setShares(next);
            setShareError(null);
          },
          (err) => {
            if (cancelled) return;
            useApp.getState().handleApiError(err);
            setShareError((err as Error).message);
          }
        ),
      ];
      if (isSsh) {
        tasks.push(
          api.listForwards(projectId).then(
            (next) => {
              if (cancelled) return;
              setRows(next);
              setError(null);
            },
            (err) => {
              if (cancelled) return;
              useApp.getState().handleApiError(err);
              setError((err as Error).message);
            }
          )
        );
      } else {
        setRows(null);
        setError(null);
      }
      await Promise.all(tasks);
      inFlight = false;
    };
    void load();
    const stop = pollWhileVisible(() => void load(), POLL_MS);
    return () => {
      cancelled = true;
      stop();
    };
  }, [projectId, isSsh, tick]);

  const refresh = () => setTick((n) => n + 1);

  return (
    <aside className="island flex min-h-0 flex-1 flex-col overflow-hidden text-sidebar-foreground">
      <div className="flex h-8.5 shrink-0 items-center gap-2 border-b pr-1.5 pl-3">
        <ArrowLeftRight className="size-3.5 shrink-0 text-muted-foreground" />
        <span className="min-w-0 flex-1 truncate text-xs font-medium">{t("forward.title")}</span>
        {project && (
          <span className="max-w-24 truncate font-mono text-[11px] text-muted-foreground">
            {project.name}
          </span>
        )}
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto">
        {!projectId ? (
          <Hint>{t("forward.noProject")}</Hint>
        ) : (
          <>
            {isSsh && (
              <section>
                <SectionTitle>{t("forward.sshSection")}</SectionTitle>
                {error && !rows ? (
                  <Hint>
                    {t("forward.loadFailed")}
                    <span className="mt-1 block font-mono text-[11px]">{error}</span>
                  </Hint>
                ) : !rows ? (
                  <Hint>{t("forward.loading")}</Hint>
                ) : (
                  <>
                    {rows.length === 0 ? (
                      <Hint>{t("forward.empty")}</Hint>
                    ) : (
                      <ul className="flex flex-col">
                        {rows.map((row) => (
                          <ForwardRow
                            key={row.id}
                            row={row}
                            projectId={projectId}
                            onChanged={refresh}
                          />
                        ))}
                      </ul>
                    )}
                    <AddForwardForm projectId={projectId} onCreated={refresh} />
                  </>
                )}
              </section>
            )}
            <section>
              <SectionTitle>{t("forward.shareSection")}</SectionTitle>
              {shareError && !shares ? (
                <Hint>
                  {t("forward.shareLoadFailed")}
                  <span className="mt-1 block font-mono text-[11px]">{shareError}</span>
                </Hint>
              ) : !shares ? (
                <Hint>{t("forward.shareLoading")}</Hint>
              ) : (
                <>
                  {shares.length === 0 ? (
                    <Hint>{t("forward.shareEmpty")}</Hint>
                  ) : (
                    <ul className="flex flex-col">
                      {shares.map((row) => (
                        <ShareRow
                          key={row.id}
                          row={row}
                          projectId={projectId}
                          onChanged={refresh}
                        />
                      ))}
                    </ul>
                  )}
                  <AddShareForm
                    projectId={projectId}
                    ssh={isSsh}
                    onCreated={refresh}
                  />
                </>
              )}
            </section>
          </>
        )}
      </div>
    </aside>
  );
}

function SectionTitle({ children }: { children: ReactNode }) {
  return (
    <div className="px-3 pt-2.5 pb-1 text-[11px] tracking-wide text-muted-foreground">
      {children}
    </div>
  );
}

function ForwardRow({
  row,
  projectId,
  onChanged,
}: {
  row: PortForward;
  projectId: string;
  onChanged: () => void;
}) {
  const { t } = useTranslation();
  const [busy, setBusy] = useState(false);

  const run = async (fn: () => Promise<unknown>) => {
    if (busy) return;
    setBusy(true);
    try {
      await fn();
      onChanged();
    } catch (err) {
      useApp.getState().handleApiError(err);
      useApp.getState().toast({
        kind: "danger",
        title: t("toast.failed"),
        body: (err as Error).message,
      });
    } finally {
      setBusy(false);
    }
  };

  return (
    <li className="border-b px-3 py-2">
      <div className="flex items-start gap-1.5">
        <StateDot state={row.state} />
        <div className="min-w-0 flex-1">
          <div className="flex items-baseline gap-1.5">
            <span className="shrink-0 text-[11px] text-muted-foreground">
              {t(row.kind === "local" ? "forward.kind_local" : "forward.kind_remote")}
            </span>
            {row.name && (
              <span className="min-w-0 truncate text-xs font-medium" title={row.name}>
                {row.name}
              </span>
            )}
          </div>
          <div className="mt-0.5 truncate font-mono text-[11px]" title={routeLabel(row)}>
            {routeLabel(row)}
          </div>
          <div className="mt-0.5 text-[11px] text-muted-foreground">
            {t(`forward.state_${row.state}`)}
            {row.error && (
              <span className="mt-0.5 block font-mono text-destructive" title={row.error}>
                {row.error}
              </span>
            )}
          </div>
        </div>
        <label className="flex shrink-0 items-center pt-0.5" title={t("forward.enabled")}>
          <input
            type="checkbox"
            className="size-3.5 accent-primary"
            checked={row.enabled}
            disabled={busy}
            aria-label={t("forward.enabled")}
            onChange={() =>
              void run(() => api.updateForward(projectId, row.id, { enabled: !row.enabled }))
            }
          />
        </label>
        <Button
          variant="ghost"
          size="icon-xs"
          className="text-muted-foreground"
          disabled={busy}
          aria-label={t("forward.delete")}
          title={t("forward.delete")}
          onClick={() => void run(() => api.deleteForward(projectId, row.id))}
        >
          <Trash2 />
        </Button>
      </div>
    </li>
  );
}

function ShareRow({
  row,
  projectId,
  onChanged,
}: {
  row: PublicShare;
  projectId: string;
  onChanged: () => void;
}) {
  const { t } = useTranslation();
  const [busy, setBusy] = useState(false);
  const [copied, setCopied] = useState(false);

  const run = async (fn: () => Promise<unknown>) => {
    if (busy) return;
    setBusy(true);
    try {
      await fn();
      onChanged();
    } catch (err) {
      useApp.getState().handleApiError(err);
      useApp.getState().toast({
        kind: "danger",
        title: t("toast.failed"),
        body: (err as Error).message,
      });
    } finally {
      setBusy(false);
    }
  };

  const copyUrl = async () => {
    if (!row.publicUrl) return;
    try {
      await writeClipboardText(() => Promise.resolve(row.publicUrl!));
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch {
      // http 下没有 clipboard API，复制不是主路径
    }
  };

  return (
    <li className="border-b px-3 py-2">
      <div className="flex items-start gap-1.5">
        <StateDot state={row.state} />
        <div className="min-w-0 flex-1">
          <div className="flex items-baseline gap-1.5">
            <span className="shrink-0 text-[11px] text-muted-foreground">
              {t(row.origin === "local" ? "forward.shareOrigin_local" : "forward.shareOrigin_remote")}
            </span>
            {row.name && (
              <span className="min-w-0 truncate text-xs font-medium" title={row.name}>
                {row.name}
              </span>
            )}
          </div>
          <div className="mt-0.5 truncate font-mono text-[11px]" title={shareTargetLabel(row)}>
            {shareTargetLabel(row)}
          </div>
          {row.publicUrl ? (
            <a
              href={row.publicUrl}
              target="_blank"
              rel="noreferrer"
              className="mt-0.5 block truncate font-mono text-[11px] text-primary hover:underline"
              title={row.publicUrl}
            >
              {row.publicUrl.replace(/^https:\/\//, "")}
            </a>
          ) : (
            <div className="mt-0.5 text-[11px] text-muted-foreground">
              {t(`forward.state_${row.state}`)}
            </div>
          )}
          {row.error && (
            <span className="mt-0.5 block font-mono text-[11px] text-destructive" title={row.error}>
              {row.error}
            </span>
          )}
        </div>
        {row.publicUrl && (
          <>
            <Button
              variant="ghost"
              size="icon-xs"
              className="text-muted-foreground"
              aria-label={copied ? t("forward.shareCopied") : t("forward.shareCopy")}
              title={copied ? t("forward.shareCopied") : t("forward.shareCopy")}
              onClick={() => void copyUrl()}
            >
              {copied ? <Check /> : <Copy />}
            </Button>
            <Button
              variant="ghost"
              size="icon-xs"
              className="text-muted-foreground"
              aria-label={t("forward.shareOpen")}
              title={t("forward.shareOpen")}
              asChild
            >
              <a href={row.publicUrl} target="_blank" rel="noreferrer">
                <ExternalLink />
              </a>
            </Button>
          </>
        )}
        <label className="flex shrink-0 items-center pt-0.5" title={t("forward.enabled")}>
          <input
            type="checkbox"
            className="size-3.5 accent-primary"
            checked={row.enabled}
            disabled={busy}
            aria-label={t("forward.enabled")}
            onChange={() =>
              void run(() => api.updateShare(projectId, row.id, { enabled: !row.enabled }))
            }
          />
        </label>
        <Button
          variant="ghost"
          size="icon-xs"
          className="text-muted-foreground"
          disabled={busy}
          aria-label={t("forward.shareDelete")}
          title={t("forward.shareDelete")}
          onClick={() => void run(() => api.deleteShare(projectId, row.id))}
        >
          <Trash2 />
        </Button>
      </div>
    </li>
  );
}

function AddShareForm({
  projectId,
  ssh,
  onCreated,
}: {
  projectId: string;
  ssh: boolean;
  onCreated: () => void;
}) {
  const { t } = useTranslation();
  const [origin, setOrigin] = useState<ShareOrigin>(ssh ? "remote" : "local");
  const [destPort, setDestPort] = useState("");
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);

  useEffect(() => {
    setOrigin(ssh ? "remote" : "local");
  }, [ssh]);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    const port = Number(destPort);
    if (!Number.isInteger(port) || port < 1 || port > 65535) {
      setFormError(t("forward.portInvalid"));
      return;
    }
    setBusy(true);
    setFormError(null);
    try {
      await api.createShare(projectId, {
        origin: ssh ? origin : "local",
        destPort: port,
        name: name.trim() || undefined,
      });
      setDestPort("");
      setName("");
      onCreated();
    } catch (err) {
      useApp.getState().handleApiError(err);
      setFormError((err as Error).message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <form className="border-t px-3 py-2.5" onSubmit={(e) => void submit(e)}>
      <div className="mb-1.5 text-[11px] tracking-wide text-muted-foreground">
        {t("forward.shareAdd")}
      </div>
      {ssh && (
        <Segmented
          dense
          label={t("forward.shareOrigin")}
          value={origin}
          onChange={setOrigin}
          options={[
            { value: "local", label: t("forward.shareOrigin_local") },
            { value: "remote", label: t("forward.shareOrigin_remote") },
          ]}
        />
      )}
      <p className="mt-1.5 text-[11px] leading-relaxed text-muted-foreground">
        {t(
          ssh && origin === "remote" ? "forward.shareHint_remote" : "forward.shareHint_local"
        )}
      </p>
      <div className="mt-1.5 flex gap-1">
        <PortInput
          label={
            ssh && origin === "remote" ? t("forward.remotePort") : t("forward.localPort")
          }
          value={destPort}
          onChange={setDestPort}
        />
        <Input
          className="h-7 min-w-0 flex-1 px-2 text-xs"
          placeholder={t("forward.namePlaceholder")}
          aria-label={t("forward.name")}
          value={name}
          onChange={(e) => setName(e.target.value)}
        />
        <Button type="submit" size="xs" className="h-7 shrink-0" disabled={busy || !destPort}>
          <Globe />
          {t("forward.addAction")}
        </Button>
      </div>
      {formError && <p className="mt-1.5 text-[11px] text-destructive">{formError}</p>}
    </form>
  );
}

/**
 * 只填两个端口。监听地址与目标地址一律走后端默认的 127.0.0.1——
 * 面板窄，而绑 0.0.0.0 / 具体网卡是少数派需求，真要改可以直接调 API。
 */
function AddForwardForm({ projectId, onCreated }: { projectId: string; onCreated: () => void }) {
  const { t } = useTranslation();
  const [kind, setKind] = useState<ForwardKind>("local");
  const [bindPort, setBindPort] = useState("");
  const [destPort, setDestPort] = useState("");
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    const listen = Number(bindPort);
    const target = destPort === "" ? listen : Number(destPort);
    if (!Number.isInteger(listen) || listen < 1 || listen > 65535) {
      setFormError(t("forward.portInvalid"));
      return;
    }
    if (!Number.isInteger(target) || target < 1 || target > 65535) {
      setFormError(t("forward.portInvalid"));
      return;
    }
    setBusy(true);
    setFormError(null);
    try {
      await api.createForward(projectId, {
        kind,
        name: name.trim() || undefined,
        bindPort: listen,
        destPort: target,
      });
      setBindPort("");
      setDestPort("");
      setName("");
      onCreated();
    } catch (err) {
      useApp.getState().handleApiError(err);
      setFormError((err as Error).message);
    } finally {
      setBusy(false);
    }
  };

  const localLabel = t("forward.localPort");
  const remoteLabel = t("forward.remotePort");

  return (
    <form className="border-t px-3 py-2.5" onSubmit={(e) => void submit(e)}>
      <div className="mb-1.5 text-[11px] tracking-wide text-muted-foreground">
        {t("forward.add")}
      </div>
      <Segmented
        dense
        label={t("forward.kind")}
        value={kind}
        onChange={setKind}
        options={[
          { value: "local", label: t("forward.kind_local") },
          { value: "remote", label: t("forward.kind_remote") },
        ]}
      />
      <p className="mt-1.5 text-[11px] leading-relaxed text-muted-foreground">
        {t(kind === "local" ? "forward.hint_local" : "forward.hint_remote")}
      </p>
      <div className="mt-1.5 flex items-center gap-1">
        <PortInput
          label={kind === "local" ? localLabel : remoteLabel}
          value={bindPort}
          // 目标端口没被单独改过就跟着监听端口走，两边同号是常态
          onChange={(value) => {
            setBindPort(value);
            if (destPort === "" || destPort === bindPort) setDestPort(value);
          }}
        />
        <ArrowRight className="size-3 shrink-0 text-muted-foreground" />
        <PortInput
          label={kind === "local" ? remoteLabel : localLabel}
          value={destPort}
          onChange={setDestPort}
        />
      </div>
      <div className="mt-1 flex gap-1">
        <Input
          className="h-7 px-2 text-xs"
          placeholder={t("forward.namePlaceholder")}
          aria-label={t("forward.name")}
          value={name}
          onChange={(e) => setName(e.target.value)}
        />
        <Button type="submit" size="xs" className="h-7 shrink-0" disabled={busy || !bindPort}>
          <Plus />
          {t("forward.addAction")}
        </Button>
      </div>
      {formError && <p className="mt-1.5 text-[11px] text-destructive">{formError}</p>}
    </form>
  );
}

/**
 * 端口输入。用 text + inputMode 而不是 type=number：窄栏里 number 的上下箭头
 * 会吃掉一截宽度，而且非数字字符在这里没有任何意义，直接过滤更省事。
 */
function PortInput({
  label,
  value,
  onChange,
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
}) {
  return (
    <Input
      inputMode="numeric"
      maxLength={5}
      className="h-7 min-w-0 flex-1 px-2 font-mono text-xs"
      placeholder={label}
      aria-label={label}
      value={value}
      onChange={(e) => onChange(e.target.value.replace(/\D/g, ""))}
    />
  );
}

/** 回环地址是默认值，显示出来只是噪音；非默认的（老规则或直接调 API 建的）才带上。 */
function routeLabel(row: PortForward): string {
  return `${endpointLabel(row.bindHost, row.bindPort)} → ${endpointLabel(row.destHost, row.destPort)}`;
}

function shareTargetLabel(row: PublicShare): string {
  return endpointLabel(row.destHost, row.destPort);
}

function endpointLabel(host: string, port: number): string {
  const loopback = host === "127.0.0.1" || host === "localhost" || host === "::1";
  return loopback ? String(port) : `${host}:${port}`;
}

function StateDot({ state }: { state: ForwardState }) {
  const { t } = useTranslation();
  return (
    <span
      role="img"
      aria-label={t(`forward.state_${state}`)}
      title={t(`forward.state_${state}`)}
      className={cn(
        "mt-1.5 size-1.5 shrink-0 rounded-full",
        state === "active" && "bg-success",
        state === "starting" && "animate-breathe bg-warning",
        state === "error" && "bg-destructive",
        state === "stopped" && "bg-muted-foreground/40"
      )}
    />
  );
}

function Hint({ children }: { children: ReactNode }) {
  return <p className="px-3 py-4 text-xs leading-relaxed text-muted-foreground">{children}</p>;
}
