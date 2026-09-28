import { useCallback, useEffect, useState, type FormEvent, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { ArrowRight, Check, Copy, ExternalLink, Globe, Monitor, Plus, Trash2 } from "lucide-react";
import type {
  ForwardKind,
  ForwardState,
  PortForward,
  PublicShare,
  RelayList,
  SshHost,
} from "@falcon/shared";
import { api } from "../api.js";
import { writeClipboardText } from "../lib/clipboard.js";
import { hostBarFromSsh, sshConn } from "../lib/hostColor.js";
import { useApp } from "../store.js";
import { cn, pollWhileVisible } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { Segmented } from "./common/Field.js";

const POLL_MS = 3000;

/**
 * 设置里的「中转」页（ADR 0016）：端口转发与公网发布按机器分块——
 * 「本机」只有公网发布，每台已保存的 SSH Host 有端口转发 + 公网发布。
 *
 * 开着才轮询，切走就卸载（设置对话框按页挂载）。同端口可以有多条，
 * 启用一条时后端顺手停掉同端口的其它规则，所以每次写完都重拉整张列表。
 */
export function RelaysPane() {
  const { t } = useTranslation();
  const hosts = useApp((s) => s.hosts);
  const openHostForm = useApp((s) => s.openHostForm);
  const [data, setData] = useState<RelayList | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const next = await api.listRelays();
      setData(next);
      setError(null);
    } catch (err) {
      useApp.getState().handleApiError(err);
      setError((err as Error).message);
    }
  }, []);

  useEffect(() => {
    let inFlight = false;
    const tick = async () => {
      if (inFlight) return;
      inFlight = true;
      await load();
      inFlight = false;
    };
    void tick();
    return pollWhileVisible(() => void tick(), POLL_MS);
  }, [load]);

  if (!data) {
    return (
      <Notice>
        {error ? (
          <>
            {t("forward.loadFailed")}
            <span className="mt-1 block font-mono text-[11px]">{error}</span>
          </>
        ) : (
          t("forward.loading")
        )}
      </Notice>
    );
  }

  const refresh = () => void load();
  const counts = slotCounts(data);

  return (
    <div className="flex flex-col gap-5">
      <MachineCard
        icon={<Monitor className="size-3.5 text-muted-foreground" />}
        name={t("forward.local")}
        conn={t("forward.localConn")}
      >
        <ShareGroup
          hostId={undefined}
          rows={data.shares.filter((s) => s.hostId == null)}
          counts={counts}
          onChanged={refresh}
        />
      </MachineCard>
      {hosts.map((host) => (
        <MachineCard
          key={host.id}
          icon={
            <span
              className="size-2 shrink-0 rounded-full"
              style={{ background: hostBarFromSsh(host) }}
            />
          }
          name={host.name}
          conn={sshConn(host)}
        >
          <ForwardGroup
            host={host}
            rows={data.forwards.filter((f) => f.hostId === host.id)}
            counts={counts}
            onChanged={refresh}
          />
          <ShareGroup
            hostId={host.id}
            rows={data.shares.filter((s) => s.hostId === host.id)}
            counts={counts}
            onChanged={refresh}
          />
        </MachineCard>
      ))}
      {hosts.length === 0 && (
        <div className="flex items-center justify-between gap-4 rounded-lg border border-dashed px-4 py-4">
          <p className="text-xs leading-relaxed text-muted-foreground">{t("forward.noHosts")}</p>
          <Button variant="outline" size="sm" className="shrink-0" onClick={() => openHostForm(null)}>
            {t("forward.goHosts")}
          </Button>
        </div>
      )}
    </div>
  );
}

function MachineCard({
  icon,
  name,
  conn,
  children,
}: {
  icon: ReactNode;
  name: string;
  conn: string;
  children: ReactNode;
}) {
  return (
    <section className="overflow-hidden rounded-lg border bg-card">
      <header className="flex items-center gap-2 border-b px-4 py-2.5">
        {icon}
        <span className="text-sm font-medium">{name}</span>
        <span className="min-w-0 truncate font-mono text-xs text-muted-foreground">{conn}</span>
      </header>
      <div className="divide-y">{children}</div>
    </section>
  );
}

function GroupTitle({ children }: { children: ReactNode }) {
  return (
    <div className="px-4 pt-3 pb-1 text-[11px] tracking-wide text-muted-foreground">{children}</div>
  );
}

// ---- 同端口 ----

/**
 * 与服务端 relaySpec 同一套槽位口径：本地转发的监听都在后端本机，跨主机也算同端口；
 * 远端转发只和同一台主机比；公网发布按「哪台机器的哪个端口」比。
 */
function forwardSlot(row: PortForward): string {
  return row.kind === "local" ? `fl\0${row.bindPort}` : `fr\0${row.hostId}\0${row.bindPort}`;
}

function shareSlot(row: PublicShare): string {
  return `s\0${row.hostId ?? ""}\0${row.destPort}`;
}

function slotCounts(data: RelayList): Map<string, number> {
  const counts = new Map<string, number>();
  const bump = (key: string) => counts.set(key, (counts.get(key) ?? 0) + 1);
  for (const row of data.forwards) bump(forwardSlot(row));
  for (const row of data.shares) bump(shareSlot(row));
  return counts;
}

function SamePort({ n }: { n: number }) {
  const { t } = useTranslation();
  if (n < 2) return null;
  return (
    <span
      className="sunken shrink-0 rounded-md px-1.5 py-px text-[11px] text-muted-foreground"
      title={t("forward.samePortHint", { n })}
    >
      {t("forward.samePort", { n })}
    </span>
  );
}

// ---- 端口转发 ----

function ForwardGroup({
  host,
  rows,
  counts,
  onChanged,
}: {
  host: SshHost;
  rows: PortForward[];
  counts: Map<string, number>;
  onChanged: () => void;
}) {
  const { t } = useTranslation();
  return (
    <div>
      <GroupTitle>{t("forward.sshSection")}</GroupTitle>
      {rows.length === 0 ? (
        <Empty>{t("forward.empty")}</Empty>
      ) : (
        <Table>
          <TableHeader>
            <TableRow className="hover:bg-transparent">
              <TableHead className="w-40 pl-4">{t("forward.name")}</TableHead>
              <TableHead className="w-16">{t("forward.kind")}</TableHead>
              <TableHead className="w-64">{t("forward.port")}</TableHead>
              <TableHead />
              <TableHead className="w-20 pr-4" />
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((row) => (
              <ForwardRow
                key={row.id}
                row={row}
                same={counts.get(forwardSlot(row)) ?? 1}
                onChanged={onChanged}
              />
            ))}
          </TableBody>
        </Table>
      )}
      <AddForwardForm hostId={host.id} onCreated={onChanged} />
    </div>
  );
}

function ForwardRow({
  row,
  same,
  onChanged,
}: {
  row: PortForward;
  same: number;
  onChanged: () => void;
}) {
  const { t } = useTranslation();
  const { busy, run } = useRowAction(onChanged);
  return (
    <TableRow>
      <TableCell className="pl-4">
        <span className={cn("block truncate", !row.name && "text-muted-foreground")}>
          {row.name ?? "—"}
        </span>
      </TableCell>
      <TableCell className="text-xs text-muted-foreground">
        {t(row.kind === "local" ? "forward.kind_local" : "forward.kind_remote")}
      </TableCell>
      <TableCell>
        <span className="flex items-center gap-2">
          <span className="font-mono text-xs">{routeLabel(row)}</span>
          <SamePort n={same} />
        </span>
      </TableCell>
      <TableCell>
        <StateLine state={row.state} enabled={row.enabled} error={row.error} />
      </TableCell>
      <TableCell className="pr-4">
        <span className="flex items-center justify-end gap-2">
          <Checkbox
            checked={row.enabled}
            disabled={busy}
            aria-label={t("forward.enabled")}
            title={t("forward.enabled")}
            onCheckedChange={(checked) =>
              run(() => api.updateForward(row.id, { enabled: checked === true }))
            }
          />
          <Button
            variant="ghost"
            size="icon-xs"
            className="text-muted-foreground"
            disabled={busy}
            aria-label={t("forward.delete")}
            title={t("forward.delete")}
            onClick={() => run(() => api.deleteForward(row.id))}
          >
            <Trash2 />
          </Button>
        </span>
      </TableCell>
    </TableRow>
  );
}

/**
 * 只填两个端口。监听地址与目标地址一律走后端默认的 127.0.0.1——
 * 绑 0.0.0.0 / 具体网卡是少数派需求，真要改可以直接调 API。
 */
function AddForwardForm({ hostId, onCreated }: { hostId: string; onCreated: () => void }) {
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
    if (!validPort(listen) || !validPort(target)) {
      setFormError(t("forward.portInvalid"));
      return;
    }
    setBusy(true);
    setFormError(null);
    try {
      await api.createForward({
        hostId,
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
    <form className="px-4 pt-2 pb-3" onSubmit={(e) => void submit(e)}>
      <div className="flex flex-wrap items-center gap-2">
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
        <NameInput value={name} onChange={setName} />
        <Button type="submit" size="xs" className="h-7" disabled={busy || !bindPort}>
          <Plus />
          {t("forward.addAction")}
        </Button>
      </div>
      <p className="mt-1.5 text-[11px] leading-relaxed text-muted-foreground">
        {t(kind === "local" ? "forward.hint_local" : "forward.hint_remote")}
      </p>
      {formError && <p className="mt-1 text-[11px] text-destructive">{formError}</p>}
    </form>
  );
}

// ---- 公网发布 ----

function ShareGroup({
  hostId,
  rows,
  counts,
  onChanged,
}: {
  /** undefined = 本机 */
  hostId: string | undefined;
  rows: PublicShare[];
  counts: Map<string, number>;
  onChanged: () => void;
}) {
  const { t } = useTranslation();
  return (
    <div>
      <GroupTitle>{t("forward.shareSection")}</GroupTitle>
      {rows.length === 0 ? (
        <Empty>{t("forward.shareEmpty")}</Empty>
      ) : (
        <Table>
          <TableHeader>
            <TableRow className="hover:bg-transparent">
              <TableHead className="w-40 pl-4">{t("forward.name")}</TableHead>
              <TableHead className="w-40">{t("forward.port")}</TableHead>
              <TableHead />
              <TableHead className="w-20 pr-4" />
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((row) => (
              <ShareRow
                key={row.id}
                row={row}
                same={counts.get(shareSlot(row)) ?? 1}
                onChanged={onChanged}
              />
            ))}
          </TableBody>
        </Table>
      )}
      <AddShareForm hostId={hostId} onCreated={onChanged} />
    </div>
  );
}

function ShareRow({
  row,
  same,
  onChanged,
}: {
  row: PublicShare;
  same: number;
  onChanged: () => void;
}) {
  const { t } = useTranslation();
  const { busy, run } = useRowAction(onChanged);
  const [copied, setCopied] = useState(false);

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
    <TableRow>
      <TableCell className="pl-4">
        <span className={cn("block truncate", !row.name && "text-muted-foreground")}>
          {row.name ?? "—"}
        </span>
      </TableCell>
      <TableCell>
        <span className="flex items-center gap-2">
          <span className="font-mono text-xs">{endpointLabel(row.destHost, row.destPort)}</span>
          <SamePort n={same} />
        </span>
      </TableCell>
      <TableCell>
        {row.publicUrl ? (
          <span className="flex min-w-0 items-center gap-1">
            <StateDot state={row.state} enabled={row.enabled} />
            <a
              href={row.publicUrl}
              target="_blank"
              rel="noreferrer"
              className="ml-1 min-w-0 truncate font-mono text-xs text-primary hover:underline"
              title={row.publicUrl}
            >
              {row.publicUrl.replace(/^https:\/\//, "")}
            </a>
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
          </span>
        ) : (
          <StateLine state={row.state} enabled={row.enabled} error={row.error} />
        )}
      </TableCell>
      <TableCell className="pr-4">
        <span className="flex items-center justify-end gap-2">
          <Checkbox
            checked={row.enabled}
            disabled={busy}
            aria-label={t("forward.enabled")}
            title={t("forward.enabled")}
            onCheckedChange={(checked) =>
              run(() => api.updateShare(row.id, { enabled: checked === true }))
            }
          />
          <Button
            variant="ghost"
            size="icon-xs"
            className="text-muted-foreground"
            disabled={busy}
            aria-label={t("forward.shareDelete")}
            title={t("forward.shareDelete")}
            onClick={() => run(() => api.deleteShare(row.id))}
          >
            <Trash2 />
          </Button>
        </span>
      </TableCell>
    </TableRow>
  );
}

function AddShareForm({ hostId, onCreated }: { hostId: string | undefined; onCreated: () => void }) {
  const { t } = useTranslation();
  const [destPort, setDestPort] = useState("");
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    const port = Number(destPort);
    if (!validPort(port)) {
      setFormError(t("forward.portInvalid"));
      return;
    }
    setBusy(true);
    setFormError(null);
    try {
      await api.createShare({ hostId, destPort: port, name: name.trim() || undefined });
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
    <form className="px-4 pt-2 pb-3" onSubmit={(e) => void submit(e)}>
      <div className="flex flex-wrap items-center gap-2">
        <PortInput label={t("forward.port")} value={destPort} onChange={setDestPort} />
        <NameInput value={name} onChange={setName} />
        <Button type="submit" size="xs" className="h-7" disabled={busy || !destPort}>
          <Globe />
          {t("forward.shareAddAction")}
        </Button>
      </div>
      <p className="mt-1.5 text-[11px] leading-relaxed text-muted-foreground">
        {t(hostId ? "forward.shareHint_remote" : "forward.shareHint_local")}
      </p>
      {formError && <p className="mt-1 text-[11px] text-destructive">{formError}</p>}
    </form>
  );
}

// ---- 小件 ----

function useRowAction(onChanged: () => void) {
  const { t } = useTranslation();
  const [busy, setBusy] = useState(false);
  const run = (fn: () => Promise<unknown>) => {
    if (busy) return;
    setBusy(true);
    void fn()
      .then(onChanged, (err) => {
        useApp.getState().handleApiError(err);
        useApp.getState().toast({
          kind: "danger",
          title: t("toast.failed"),
          body: (err as Error).message,
        });
      })
      .finally(() => setBusy(false));
  };
  return { busy, run };
}

/**
 * 端口输入。用 text + inputMode 而不是 type=number：number 的上下箭头
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
      className="h-7 w-24 px-2 font-mono text-xs"
      placeholder={label}
      aria-label={label}
      value={value}
      onChange={(e) => onChange(e.target.value.replace(/\D/g, ""))}
    />
  );
}

function NameInput({ value, onChange }: { value: string; onChange: (value: string) => void }) {
  const { t } = useTranslation();
  return (
    <Input
      className="h-7 w-40 px-2 text-xs"
      placeholder={t("forward.namePlaceholder")}
      aria-label={t("forward.name")}
      value={value}
      onChange={(e) => onChange(e.target.value)}
    />
  );
}

function StateLine({
  state,
  enabled,
  error,
}: {
  state: ForwardState;
  enabled: boolean;
  error?: string;
}) {
  const { t } = useTranslation();
  return (
    <span className="flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
      <StateDot state={state} enabled={enabled} />
      <span className="shrink-0">{t(`forward.state_${state}`)}</span>
      {error && (
        <span className="min-w-0 truncate font-mono text-[11px] text-destructive" title={error}>
          {error}
        </span>
      )}
    </span>
  );
}

function StateDot({ state, enabled }: { state: ForwardState; enabled: boolean }) {
  const { t } = useTranslation();
  return (
    <span
      role="img"
      aria-label={t(`forward.state_${state}`)}
      title={t(`forward.state_${state}`)}
      className={cn(
        "size-1.5 shrink-0 rounded-full",
        state === "active" && "bg-success",
        state === "starting" && "animate-breathe bg-warning",
        state === "error" && "bg-destructive",
        state === "stopped" && (enabled ? "bg-muted-foreground/60" : "bg-muted-foreground/30")
      )}
    />
  );
}

function Empty({ children }: { children: ReactNode }) {
  return <p className="px-4 pb-1 text-xs text-muted-foreground">{children}</p>;
}

function Notice({ children }: { children: ReactNode }) {
  return (
    <div className="rounded-lg border bg-card px-4 py-6 text-center text-xs text-muted-foreground">
      {children}
    </div>
  );
}

function validPort(n: number): boolean {
  return Number.isInteger(n) && n >= 1 && n <= 65535;
}

/** 回环地址是默认值，显示出来只是噪音；非默认的（直接调 API 建的）才带上。 */
function routeLabel(row: PortForward): string {
  return `${endpointLabel(row.bindHost, row.bindPort)} → ${endpointLabel(row.destHost, row.destPort)}`;
}

function endpointLabel(host: string, port: number): string {
  const loopback = host === "127.0.0.1" || host === "localhost" || host === "::1";
  return loopback ? String(port) : `${host}:${port}`;
}
