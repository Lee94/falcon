import { useEffect, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import {
  Container,
  Play,
  RefreshCw,
  RotateCw,
  ScrollText,
  Square,
  Trash2,
} from "lucide-react";
import type {
  DockerComposeFile,
  DockerComposeService,
  DockerContainer,
  DockerImage,
  DockerLogs,
  DockerOpInput,
  DockerSnapshot,
  DockerUnavailableReason,
} from "@falcon/shared";
import { api } from "../api.js";
import { confirmAsync } from "../lib/confirmAsync.js";
import { useApp, selectFocusProjectId } from "../store.js";
import { cn, pollWhileVisible } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { AppDialog } from "./common/AppDialog.js";
import { Segmented } from "./common/Field.js";

const POLL_MS = 5000;
const LOGS_POLL_MS = 2500;

type Tab = "containers" | "images" | "compose";

/**
 * 右侧 Docker 面板。按当前焦点项目的宿主机走，和 Git / 转发同一套挂载策略：
 * 打开才请求，关掉就卸载。
 *
 * 容器和镜像是宿主机级的（同一台 SSH 上的两个项目看到同一份列表）；
 * Compose 只在项目工作目录及往下两层子目录里找 compose.yaml 这类文件。
 */
export function DockerPanel() {
  const { t } = useTranslation();
  const projectId = useApp(selectFocusProjectId);
  const project = useApp((s) => s.projects.find((p) => p.id === selectFocusProjectId(s)));
  const [tab, setTab] = useState<Tab>("containers");
  const [snap, setSnap] = useState<DockerSnapshot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [tick, setTick] = useState(0);
  const [composeFile, setComposeFile] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [logs, setLogs] = useState<LogsTarget | null>(null);

  useEffect(() => {
    setSnap(null);
    setError(null);
    setComposeFile(null);
    setLogs(null);
    setLoading(true);
  }, [projectId]);

  useEffect(() => {
    if (!projectId) {
      setSnap(null);
      setError(null);
      setLoading(false);
      return;
    }
    let cancelled = false;
    let inFlight = false;
    const load = async () => {
      if (inFlight) return;
      inFlight = true;
      try {
        const next = await api.dockerSnapshot(projectId, composeFile ?? undefined);
        if (cancelled) return;
        setSnap(next);
        setError(null);
      } catch (err) {
        if (cancelled) return;
        useApp.getState().handleApiError(err);
        setError((err as Error).message);
      } finally {
        inFlight = false;
        if (!cancelled) setLoading(false);
      }
    };
    void load();
    const stop = pollWhileVisible(() => void load(), POLL_MS);
    return () => {
      cancelled = true;
      stop();
    };
  }, [projectId, composeFile, tick]);

  const refresh = () => setTick((n) => n + 1);

  const runOp = async (input: DockerOpInput, label: string) => {
    if (!projectId || busy) return;
    setBusy(label);
    try {
      const res = await api.dockerOp(projectId, input);
      if (!res.ok) {
        useApp.getState().toast({
          kind: "danger",
          title: t("toast.failed"),
          body: res.detail || t(reasonKey(res.reason)),
        });
        return;
      }
      if (input.op === "image-prune" && res.detail) {
        useApp.getState().toast({ kind: "success", title: label, body: res.detail });
      }
      refresh();
    } catch (err) {
      useApp.getState().handleApiError(err);
      useApp.getState().toast({
        kind: "danger",
        title: t("toast.failed"),
        body: (err as Error).message,
      });
    } finally {
      setBusy(null);
    }
  };

  const unavailable = snap && !snap.available;

  return (
    <aside className="flex min-h-0 flex-1 flex-col border-l bg-sidebar text-sidebar-foreground">
      <div className="flex h-8.5 shrink-0 items-center gap-2 border-b pr-1.5 pl-3">
        <Container className="size-3.5 shrink-0 text-muted-foreground" />
        <span className="min-w-0 flex-1 truncate text-xs font-medium">{t("docker.title")}</span>
        {project && (
          <span className="max-w-24 truncate font-mono text-[11px] text-muted-foreground">
            {project.name}
          </span>
        )}
        <Button
          variant="ghost"
          size="icon-xs"
          className="text-muted-foreground"
          aria-label={t("docker.refresh")}
          title={t("docker.refresh")}
          disabled={!projectId || loading}
          onClick={refresh}
        >
          <RefreshCw className={cn(loading && "animate-spin")} />
        </Button>
      </div>

      <div className="shrink-0 border-b px-2 py-1.5">
        <Segmented
          dense
          label={t("docker.tabs")}
          value={tab}
          onChange={setTab}
          options={[
            { value: "containers", label: t("docker.tabContainers") },
            { value: "images", label: t("docker.tabImages") },
            { value: "compose", label: t("docker.tabCompose") },
          ]}
        />
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto">
        {!projectId ? (
          <Hint>{t("docker.noProject")}</Hint>
        ) : error && !snap ? (
          <Hint>
            {t("docker.loadFailed")}
            <span className="mt-1 block font-mono text-[11px]">{error}</span>
          </Hint>
        ) : !snap || (loading && !snap.available && !snap.containers.length) ? (
          <Hint>{t("docker.loading")}</Hint>
        ) : unavailable ? (
          <Hint>
            {t(reasonKey(snap.reason))}
            {snap.detail && (
              <span className="mt-1 block font-mono text-[11px]">{snap.detail}</span>
            )}
          </Hint>
        ) : tab === "containers" ? (
          <ContainerList
            rows={snap.containers}
            busy={busy}
            onStart={(ref) => void runOp({ op: "start", ref }, t("docker.start"))}
            onStop={(ref) => void runOp({ op: "stop", ref }, t("docker.stop"))}
            onRestart={(ref) => void runOp({ op: "restart", ref }, t("docker.restart"))}
            onRemove={(row) => void removeContainer(row, runOp, t)}
            onLogs={(row) =>
              setLogs({
                kind: "container",
                ref: row.id,
                title: row.names[0] ?? row.id,
              })
            }
          />
        ) : tab === "images" ? (
          <ImageList
            rows={snap.images}
            busy={busy}
            onRemove={(ref) => void runOp({ op: "image-remove", ref }, t("docker.imageRemove"))}
            onPrune={() => void pruneImages(runOp, t)}
          />
        ) : (
          <ComposeView
            files={snap.composeFiles}
            file={composeFile}
            onFile={setComposeFile}
            available={snap.composeAvailable}
            reason={snap.composeReason}
            detail={snap.composeDetail}
            services={snap.compose?.services ?? []}
            hasWorkingDir={Boolean(project?.workingDir)}
            busy={busy}
            onUp={(file) => void runOp({ op: "compose-up", file }, t("docker.composeUp"))}
            onDown={(file) => void downCompose(file, runOp, t)}
            onLogs={(file) =>
              setLogs({ kind: "compose", file, title: file })
            }
          />
        )}
      </div>

      {logs && projectId && (
        <LogsDialog
          projectId={projectId}
          target={logs}
          onClose={() => setLogs(null)}
        />
      )}
    </aside>
  );
}

type LogsTarget =
  | { kind: "container"; ref: string; title: string }
  | { kind: "compose"; file: string; title: string };

function ContainerList({
  rows,
  busy,
  onStart,
  onStop,
  onRestart,
  onRemove,
  onLogs,
}: {
  rows: DockerContainer[];
  busy: string | null;
  onStart: (ref: string) => void;
  onStop: (ref: string) => void;
  onRestart: (ref: string) => void;
  onRemove: (row: DockerContainer) => void;
  onLogs: (row: DockerContainer) => void;
}) {
  const { t } = useTranslation();
  if (rows.length === 0) return <Hint>{t("docker.emptyContainers")}</Hint>;
  return (
    <ul className="flex flex-col">
      {rows.map((row) => {
        const running = row.state === "running" || row.state === "restarting";
        const name = row.names[0] ?? row.id.slice(0, 12);
        const locked = busy !== null;
        return (
          <li key={row.id} className="border-b px-3 py-2">
            <div className="flex items-start gap-1.5">
              <StateDot state={row.state} />
              <div className="min-w-0 flex-1">
                <div className="flex items-baseline gap-1.5">
                  <span className="min-w-0 truncate text-xs font-medium" title={name}>
                    {name}
                  </span>
                  <span className="shrink-0 font-mono text-[11px] text-muted-foreground">
                    {row.id.slice(0, 12)}
                  </span>
                </div>
                <div className="mt-0.5 truncate font-mono text-[11px] text-muted-foreground" title={row.image}>
                  {row.image}
                </div>
                {row.ports ? (
                  <div className="mt-0.5 truncate font-mono text-[11px]" title={row.ports}>
                    {row.ports}
                  </div>
                ) : null}
                <div className="mt-0.5 text-[11px] text-muted-foreground">{row.status || row.state}</div>
              </div>
            </div>
            <div className="mt-1.5 flex flex-wrap gap-0.5">
              {running ? (
                <IconBtn
                  label={t("docker.stop")}
                  disabled={locked}
                  onClick={() => onStop(row.id)}
                >
                  <Square />
                </IconBtn>
              ) : (
                <IconBtn
                  label={t("docker.start")}
                  disabled={locked}
                  onClick={() => onStart(row.id)}
                >
                  <Play />
                </IconBtn>
              )}
              <IconBtn
                label={t("docker.restart")}
                disabled={locked}
                onClick={() => onRestart(row.id)}
              >
                <RotateCw />
              </IconBtn>
              <IconBtn label={t("docker.logs")} disabled={locked} onClick={() => onLogs(row)}>
                <ScrollText />
              </IconBtn>
              <IconBtn
                label={t("docker.remove")}
                disabled={locked}
                danger
                onClick={() => onRemove(row)}
              >
                <Trash2 />
              </IconBtn>
            </div>
          </li>
        );
      })}
    </ul>
  );
}

function ImageList({
  rows,
  busy,
  onRemove,
  onPrune,
}: {
  rows: DockerImage[];
  busy: string | null;
  onRemove: (ref: string) => void;
  onPrune: () => void;
}) {
  const { t } = useTranslation();
  return (
    <>
      <div className="flex items-center justify-end border-b px-3 py-1.5">
        <Button
          variant="outline"
          size="xs"
          disabled={busy !== null}
          onClick={onPrune}
        >
          {t("docker.prune")}
        </Button>
      </div>
      {rows.length === 0 ? (
        <Hint>{t("docker.emptyImages")}</Hint>
      ) : (
        <ul className="flex flex-col">
          {rows.map((row) => {
            const label =
              row.dangling || !row.repository
                ? t("docker.dangling")
                : `${row.repository}:${row.tag || "latest"}`;
            return (
              <li key={row.id + label} className="border-b px-3 py-2">
                <div className="flex items-start gap-1.5">
                  <div className="min-w-0 flex-1">
                    <div className="truncate text-xs font-medium" title={label}>
                      {label}
                    </div>
                    <div className="mt-0.5 font-mono text-[11px] text-muted-foreground">
                      {row.id.slice(0, 19)}
                      {row.size ? ` · ${row.size}` : ""}
                      {row.created ? ` · ${row.created}` : ""}
                    </div>
                  </div>
                  <IconBtn
                    label={t("docker.imageRemove")}
                    disabled={busy !== null}
                    danger
                    onClick={() => onRemove(row.id)}
                  >
                    <Trash2 />
                  </IconBtn>
                </div>
              </li>
            );
          })}
        </ul>
      )}
    </>
  );
}

function ComposeView({
  files,
  file,
  onFile,
  available,
  reason,
  detail,
  services,
  hasWorkingDir,
  busy,
  onUp,
  onDown,
  onLogs,
}: {
  files: DockerComposeFile[];
  file: string | null;
  onFile: (path: string) => void;
  available: boolean;
  reason?: DockerUnavailableReason;
  detail?: string;
  services: DockerComposeService[];
  hasWorkingDir: boolean;
  busy: string | null;
  onUp: (file: string) => void;
  onDown: (file: string) => void;
  onLogs: (file: string) => void;
}) {
  const { t } = useTranslation();
  if (!hasWorkingDir) return <Hint>{t("docker.reason_no_working_dir")}</Hint>;
  if (!available) {
    return (
      <Hint>
        {t(reasonKey(reason ?? "compose-missing"))}
        {detail && <span className="mt-1 block font-mono text-[11px]">{detail}</span>}
      </Hint>
    );
  }
  if (files.length === 0) return <Hint>{t("docker.emptyCompose")}</Hint>;
  const current = file && files.some((f) => f.path === file) ? file : files[0]!.path;
  const locked = busy !== null;
  return (
    <>
      <div className="border-b px-3 py-2">
        <label className="block text-[11px] text-muted-foreground">{t("docker.composeFile")}</label>
        <select
          className="mt-1 h-7 w-full rounded-md border bg-background px-2 font-mono text-[11px] outline-none focus-visible:ring-[3px] focus-visible:ring-ring/50"
          value={current}
          aria-label={t("docker.composeFile")}
          onChange={(e) => onFile(e.target.value)}
        >
          {files.map((f) => (
            <option key={f.path} value={f.path}>
              {f.path}
            </option>
          ))}
        </select>
        <div className="mt-1.5 flex flex-wrap gap-1">
          <Button size="xs" disabled={locked} onClick={() => onUp(current)}>
            {t("docker.composeUp")}
          </Button>
          <Button variant="outline" size="xs" disabled={locked} onClick={() => onDown(current)}>
            {t("docker.composeDown")}
          </Button>
          <Button variant="ghost" size="xs" disabled={locked} onClick={() => onLogs(current)}>
            <ScrollText />
            {t("docker.logs")}
          </Button>
        </div>
        {detail && services.length === 0 && (
          <p className="mt-1.5 font-mono text-[11px] text-muted-foreground">{detail}</p>
        )}
      </div>
      {services.length === 0 ? (
        <Hint>{t("docker.emptyServices")}</Hint>
      ) : (
        <ul className="flex flex-col">
          {services.map((svc) => (
            <li key={svc.name} className="border-b px-3 py-2">
              <div className="flex items-start gap-1.5">
                <StateDot state={svc.state} />
                <div className="min-w-0 flex-1">
                  <div className="truncate text-xs font-medium">{svc.service || svc.name}</div>
                  <div className="mt-0.5 truncate font-mono text-[11px] text-muted-foreground">
                    {svc.name}
                  </div>
                  {svc.ports ? (
                    <div className="mt-0.5 truncate font-mono text-[11px]">{svc.ports}</div>
                  ) : null}
                  <div className="mt-0.5 text-[11px] text-muted-foreground">
                    {svc.status || svc.state}
                  </div>
                </div>
              </div>
            </li>
          ))}
        </ul>
      )}
    </>
  );
}

function LogsDialog({
  projectId,
  target,
  onClose,
}: {
  projectId: string;
  target: LogsTarget;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const [data, setData] = useState<DockerLogs | null>(null);
  const [follow, setFollow] = useState(true);

  useEffect(() => {
    let cancelled = false;
    let inFlight = false;
    const load = async () => {
      if (inFlight) return;
      inFlight = true;
      try {
        const next =
          target.kind === "container"
            ? await api.dockerLogs(projectId, { target: "container", ref: target.ref })
            : await api.dockerLogs(projectId, { target: "compose", file: target.file });
        if (cancelled) return;
        setData(next);
      } catch (err) {
        if (cancelled) return;
        setData({
          available: false,
          reason: "command-failed",
          detail: (err as Error).message,
          text: "",
        });
      } finally {
        inFlight = false;
      }
    };
    void load();
    if (!follow) return () => { cancelled = true; };
    const stop = pollWhileVisible(() => void load(), LOGS_POLL_MS);
    return () => {
      cancelled = true;
      stop();
    };
  }, [projectId, target, follow]);

  return (
    <AppDialog
      title={t("docker.logsTitle", { name: target.title })}
      onClose={onClose}
      wide
      className="sm:max-w-2xl"
    >
      <div
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            e.stopPropagation();
            onClose();
          }
        }}
      >
        <label className="mb-2 flex items-center gap-2 text-xs">
          <input
            type="checkbox"
            className="size-3.5 accent-primary"
            checked={follow}
            onChange={(e) => setFollow(e.target.checked)}
          />
          {t("docker.logsFollow")}
        </label>
        {!data ? (
          <p className="text-xs text-muted-foreground">{t("docker.logsLoading")}</p>
        ) : !data.available ? (
          <p className="text-xs text-destructive">
            {t(reasonKey(data.reason))}
            {data.detail ? ` · ${data.detail}` : ""}
          </p>
        ) : (
          <pre className="max-h-[min(24rem,50vh)] overflow-auto rounded-md border bg-background p-2 font-mono text-[11px] leading-relaxed whitespace-pre-wrap">
            {data.text.trim() ? data.text : t("docker.logsEmpty")}
            {data.truncated ? `\n\n${t("docker.logsTruncated")}` : ""}
          </pre>
        )}
        <div className="mt-3 flex justify-end">
          <Button variant="outline" type="button" onClick={onClose}>
            {t("common.close")}
          </Button>
        </div>
      </div>
    </AppDialog>
  );
}

async function removeContainer(
  row: DockerContainer,
  runOp: (input: DockerOpInput, label: string) => Promise<void>,
  t: (key: string, opts?: Record<string, unknown>) => string
) {
  const running = row.state === "running" || row.state === "restarting" || row.state === "paused";
  const name = row.names[0] ?? row.id.slice(0, 12);
  const ok = await confirmAsync({
    title: t("docker.removeTitle", { name }),
    body: running ? t("docker.removeRunningBody") : t("docker.removeBody"),
    confirmLabel: t("docker.remove"),
  });
  if (!ok) return;
  await runOp({ op: "remove", ref: row.id, force: running }, t("docker.remove"));
}

async function pruneImages(
  runOp: (input: DockerOpInput, label: string) => Promise<void>,
  t: (key: string) => string
) {
  const ok = await confirmAsync({
    title: t("docker.pruneTitle"),
    body: t("docker.pruneBody"),
    confirmLabel: t("docker.prune"),
  });
  if (!ok) return;
  await runOp({ op: "image-prune" }, t("docker.prune"));
}

async function downCompose(
  file: string,
  runOp: (input: DockerOpInput, label: string) => Promise<void>,
  t: (key: string, opts?: Record<string, unknown>) => string
) {
  const ok = await confirmAsync({
    title: t("docker.composeDownTitle", { file }),
    body: t("docker.composeDownBody"),
    confirmLabel: t("docker.composeDown"),
  });
  if (!ok) return;
  await runOp({ op: "compose-down", file }, t("docker.composeDown"));
}

function IconBtn({
  label,
  disabled,
  danger,
  onClick,
  children,
}: {
  label: string;
  disabled?: boolean;
  danger?: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <Button
      variant="ghost"
      size="icon-xs"
      className={danger ? "text-destructive" : "text-muted-foreground"}
      disabled={disabled}
      aria-label={label}
      title={label}
      onClick={onClick}
    >
      {children}
    </Button>
  );
}

function StateDot({ state }: { state: string }) {
  const { t } = useTranslation();
  const running = state === "running";
  const paused = state === "paused" || state === "restarting";
  const label = t(`docker.state_${state}`, { defaultValue: state });
  return (
    <span
      role="img"
      aria-label={label}
      title={label}
      className={cn(
        "mt-1.5 size-1.5 shrink-0 rounded-full",
        running && "bg-success",
        paused && "animate-breathe bg-warning",
        !running && !paused && "bg-muted-foreground/40"
      )}
    />
  );
}

function Hint({ children }: { children: ReactNode }) {
  return <p className="px-3 py-4 text-xs leading-relaxed text-muted-foreground">{children}</p>;
}

function reasonKey(reason?: DockerUnavailableReason): string {
  switch (reason) {
    case "docker-missing":
      return "docker.reason_docker_missing";
    case "docker-permission":
      return "docker.reason_docker_permission";
    case "docker-daemon":
      return "docker.reason_docker_daemon";
    case "compose-missing":
      return "docker.reason_compose_missing";
    case "no-working-dir":
      return "docker.reason_no_working_dir";
    case "command-failed":
      return "docker.reason_command_failed";
    default:
      return "docker.reason_link_failed";
  }
}
