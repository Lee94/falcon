import { useEffect, useState, type FormEvent, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { ArrowLeftRight, Plus, Trash2 } from "lucide-react";
import type { ForwardKind, ForwardState, PortForward } from "@mojito/shared";
import { api } from "../api.js";
import { useApp, selectFocusProjectId } from "../store.js";
import { cn, pollWhileVisible } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Segmented } from "./common/Field.js";

const POLL_MS = 3000;

/**
 * 右侧端口转发面板。规则按当前焦点 SSH 项目走，和 Git 面板同一套挂载策略：
 * 打开才请求，关掉就卸载。
 */
export function ForwardPanel() {
  const { t } = useTranslation();
  const projectId = useApp(selectFocusProjectId);
  const project = useApp((s) => s.projects.find((p) => p.id === selectFocusProjectId(s)));
  const [rows, setRows] = useState<PortForward[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [tick, setTick] = useState(0);

  useEffect(() => {
    setRows(null);
    setError(null);
  }, [projectId]);

  useEffect(() => {
    if (!projectId || project?.type !== "ssh") {
      setRows(null);
      setError(null);
      return;
    }
    let cancelled = false;
    let inFlight = false;
    const load = async () => {
      if (inFlight) return;
      inFlight = true;
      try {
        const next = await api.listForwards(projectId);
        if (cancelled) return;
        setRows(next);
        setError(null);
      } catch (err) {
        if (cancelled) return;
        useApp.getState().handleApiError(err);
        setError((err as Error).message);
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
  }, [projectId, project?.type, tick]);

  const refresh = () => setTick((n) => n + 1);

  return (
    <aside className="flex w-65 shrink-0 flex-col border-l bg-sidebar text-sidebar-foreground">
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
        ) : project?.type !== "ssh" ? (
          <Hint>{t("forward.localOnly")}</Hint>
        ) : error && !rows ? (
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
                  <ForwardRow key={row.id} row={row} projectId={projectId} onChanged={refresh} />
                ))}
              </ul>
            )}
            <AddForwardForm projectId={projectId} onCreated={refresh} />
          </>
        )}
      </div>
    </aside>
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

function AddForwardForm({ projectId, onCreated }: { projectId: string; onCreated: () => void }) {
  const { t } = useTranslation();
  const [kind, setKind] = useState<ForwardKind>("local");
  const [bindHost, setBindHost] = useState("127.0.0.1");
  const [bindPort, setBindPort] = useState("");
  const [destHost, setDestHost] = useState("127.0.0.1");
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
        bindHost: bindHost.trim() || "127.0.0.1",
        bindPort: listen,
        destHost: destHost.trim() || "127.0.0.1",
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

  return (
    <form className="border-t px-3 py-2.5" onSubmit={(e) => void submit(e)}>
      <div className="mb-2 text-[11px] tracking-wide text-muted-foreground">{t("forward.add")}</div>
      <Segmented
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
      <div className="mt-2 grid gap-1.5">
        <EndpointRow
          tag={kind === "local" ? t("forward.tag_local") : t("forward.tag_remote")}
          hostLabel={t("forward.bindHost")}
          portLabel={kind === "local" ? t("forward.localPort") : t("forward.remotePort")}
          host={bindHost}
          port={bindPort}
          onHost={setBindHost}
          onPort={(value) => {
            setBindPort(value);
            if (destPort === "" || destPort === bindPort) setDestPort(value);
          }}
        />
        <EndpointRow
          tag={kind === "local" ? t("forward.tag_remote") : t("forward.tag_local")}
          hostLabel={t("forward.destHost")}
          portLabel={t("forward.destPort")}
          host={destHost}
          port={destPort}
          onHost={setDestHost}
          onPort={setDestPort}
        />
      </div>
      <div className="mt-1.5 flex gap-1.5">
        <Input
          className="h-8 text-[13px]"
          placeholder={t("forward.namePlaceholder")}
          aria-label={t("forward.name")}
          value={name}
          onChange={(e) => setName(e.target.value)}
        />
        <Button type="submit" size="sm" className="h-8 shrink-0" disabled={busy || !bindPort}>
          <Plus />
          {t("forward.addAction")}
        </Button>
      </div>
      {formError && <p className="mt-1.5 text-[11px] text-destructive">{formError}</p>}
    </form>
  );
}

function EndpointRow({
  tag,
  hostLabel,
  portLabel,
  host,
  port,
  onHost,
  onPort,
}: {
  tag: string;
  hostLabel: string;
  portLabel: string;
  host: string;
  port: string;
  onHost: (value: string) => void;
  onPort: (value: string) => void;
}) {
  return (
    <div className="grid grid-cols-[1.75rem_1fr_4.5rem] items-center gap-1">
      <span className="text-[11px] text-muted-foreground">{tag}</span>
      <Input
        className="h-8 font-mono text-[13px]"
        placeholder="127.0.0.1"
        aria-label={hostLabel}
        value={host}
        onChange={(e) => onHost(e.target.value)}
      />
      <Input
        type="number"
        min={1}
        max={65535}
        inputMode="numeric"
        className="h-8 px-1.5 font-mono text-[13px]"
        placeholder={portLabel}
        aria-label={portLabel}
        value={port}
        onChange={(e) => onPort(e.target.value)}
      />
    </div>
  );
}

function routeLabel(row: PortForward): string {
  const bind = `${row.bindHost}:${row.bindPort}`;
  const dest = `${row.destHost}:${row.destPort}`;
  return `${bind} → ${dest}`;
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
