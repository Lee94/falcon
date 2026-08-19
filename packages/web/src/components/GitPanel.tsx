import { useEffect, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { GitBranch, RefreshCw } from "lucide-react";
import type { GitFileChange, GitSnapshot, GitUnavailableReason } from "@mojito/shared";
import { api } from "../api.js";
import { useApp, selectFocusProjectId } from "../store.js";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";

const POLL_MS = 5000;

/**
 * 右侧 Git 信息面板。只读：分支、工作区改动、最近提交、worktree、远程。
 * 由 App 在面板打开时才挂载，关掉就卸载，不占往返。
 */
export function GitPanel() {
  const { t } = useTranslation();
  const projectId = useApp(selectFocusProjectId);
  const project = useApp((s) => s.projects.find((p) => p.id === selectFocusProjectId(s)));
  const openDiff = useApp((s) => s.openDiff);
  const [snap, setSnap] = useState<GitSnapshot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [tick, setTick] = useState(0);

  useEffect(() => {
    setSnap(null);
    setError(null);
  }, [projectId]);

  useEffect(() => {
    if (!projectId) return;
    let cancelled = false;
    let inFlight = false;
    const load = async () => {
      if (inFlight) return;
      inFlight = true;
      try {
        const next = await api.gitSnapshot(projectId);
        if (cancelled) return;
        setSnap(next);
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
    const timer = setInterval(() => void load(), POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [projectId, tick]);

  return (
    <aside className="flex w-65 shrink-0 flex-col border-l bg-sidebar text-sidebar-foreground">
      <div className="flex h-8.5 shrink-0 items-center gap-2 border-b pr-1.5 pl-3">
        <GitBranch className="size-3.5 shrink-0 text-muted-foreground" />
        <span className="min-w-0 flex-1 truncate text-xs font-medium">{t("git.title")}</span>
        {project && (
          <span className="max-w-24 truncate font-mono text-[11px] text-muted-foreground">
            {project.name}
          </span>
        )}
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

      <div className="min-h-0 flex-1 overflow-y-auto">
        {!projectId ? (
          <Hint>{t("git.noProject")}</Hint>
        ) : error && !snap ? (
          <Hint>
            {t("git.loadFailed")}
            <span className="mt-1 block font-mono text-[11px]">{error}</span>
          </Hint>
        ) : !snap ? (
          <Hint>{t("git.loading")}</Hint>
        ) : !snap.available ? (
          <Hint>
            {t(reasonKey(snap.reason))}
            {snap.detail && (
              <span className="mt-1 block font-mono text-[11px]">{snap.detail}</span>
            )}
          </Hint>
        ) : (
          <SnapshotBody snap={snap} onOpenFile={(file) => openDiff(projectId, file)} />
        )}
      </div>
    </aside>
  );
}

function SnapshotBody({
  snap,
  onOpenFile,
}: {
  snap: GitSnapshot;
  onOpenFile: (file: GitFileChange) => void;
}) {
  const { t } = useTranslation();
  const branch = snap.detached
    ? t("git.detached")
    : (snap.headBranch ?? snap.headSha ?? "HEAD");
  const tracking: string[] = [];
  if (snap.ahead != null && snap.ahead > 0) tracking.push(t("git.ahead", { n: snap.ahead }));
  if (snap.behind != null && snap.behind > 0) tracking.push(t("git.behind", { n: snap.behind }));

  return (
    <>
      <div className="px-3 py-2.5">
        <div className="truncate text-[13px] font-medium" title={branch}>
          {branch}
        </div>
        <div className="mt-0.5 flex flex-wrap items-center gap-x-2 font-mono text-[11px] text-muted-foreground">
          {snap.headSha && <span title={snap.headSha}>{snap.headSha}</span>}
          {tracking.length > 0 && <span>{tracking.join(" ")}</span>}
          {snap.upstream && (
            <span className="truncate" title={snap.upstream}>
              {snap.upstream}
            </span>
          )}
        </div>
        {snap.repoDir && (
          <div className="mt-1 truncate font-mono text-[11px] text-muted-foreground/80" title={snap.repoDir}>
            {snap.repoDir}
          </div>
        )}
      </div>

      <Section title={t("git.changes")} count={snap.fileCount}>
        {snap.fileCount === 0 ? (
          <p className="text-[11px] text-muted-foreground">{t("git.clean")}</p>
        ) : (
          <ul className="flex max-h-64 flex-col gap-px overflow-y-auto">
            {snap.files.map((file) => (
              <FileRow
                key={`${file.index}${file.work}:${file.path}`}
                file={file}
                onOpen={() => onOpenFile(file)}
              />
            ))}
          </ul>
        )}
        {snap.fileCount > snap.files.length && (
          <p className="mt-1 text-[11px] text-muted-foreground">
            {t("git.moreFiles", { n: snap.fileCount - snap.files.length })}
          </p>
        )}
      </Section>

      <Section title={t("git.commits")} count={snap.commits.length || undefined}>
        {snap.commits.length === 0 ? (
          <p className="text-[11px] text-muted-foreground">{t("git.noCommits")}</p>
        ) : (
          <ul className="flex flex-col gap-1.5">
            {snap.commits.map((c) => (
              <li key={c.sha} className="min-w-0">
                <div className="flex items-baseline gap-1.5">
                  <span className="shrink-0 font-mono text-[11px] text-muted-foreground">{c.sha}</span>
                  <span className="min-w-0 truncate text-xs" title={c.subject}>
                    {c.subject}
                  </span>
                </div>
                <div className="truncate pl-[3.25rem] text-[11px] text-muted-foreground">
                  {c.author} · {formatAgo(c.authoredAt, t)}
                </div>
              </li>
            ))}
          </ul>
        )}
      </Section>

      {snap.worktrees.length > 0 && (
        <Section title={t("git.worktrees")} count={snap.worktrees.length}>
          <ul className="flex flex-col gap-1">
            {snap.worktrees.map((wt) => (
              <li key={wt.path} className="min-w-0">
                <div className="flex items-center gap-1.5">
                  <span
                    className={cn(
                      "size-1.5 shrink-0 rounded-full",
                      wt.current ? "bg-success" : "bg-muted-foreground/40"
                    )}
                    title={wt.current ? t("git.current") : undefined}
                  />
                  <span className="min-w-0 truncate text-xs">
                    {wt.branch ?? t("git.detached")}
                  </span>
                  {wt.head && (
                    <span className="shrink-0 font-mono text-[11px] text-muted-foreground">
                      {wt.head}
                    </span>
                  )}
                </div>
                <div className="truncate pl-3 font-mono text-[11px] text-muted-foreground" title={wt.path}>
                  {wt.path}
                </div>
              </li>
            ))}
          </ul>
        </Section>
      )}

      <Section title={t("git.remotes")} count={snap.remotes.length || undefined}>
        {snap.remotes.length === 0 ? (
          <p className="text-[11px] text-muted-foreground">{t("git.noRemotes")}</p>
        ) : (
          <ul className="flex flex-col gap-1">
            {snap.remotes.map((r) => (
              <li key={r.name} className="min-w-0">
                <div className="truncate text-xs">{r.name}</div>
                <div className="truncate font-mono text-[11px] text-muted-foreground" title={r.url}>
                  {r.url}
                </div>
              </li>
            ))}
          </ul>
        )}
      </Section>
    </>
  );
}

function FileRow({ file, onOpen }: { file: GitFileChange; onOpen: () => void }) {
  const { t } = useTranslation();
  const code = `${file.index}${file.work}`;
  const label = statusLabel(file, t);
  const path = file.origPath ? `${file.origPath} → ${file.path}` : file.path;
  return (
    <li className="min-w-0">
      <button
        type="button"
        onClick={onOpen}
        title={`${label} · ${path} · ${t("git.viewDiff")}`}
        className="flex w-full min-w-0 items-baseline gap-1.5 rounded-xs text-left hover:bg-accent hover:text-accent-foreground"
      >
        <span
          className={cn(
            "w-5 shrink-0 whitespace-pre font-mono text-[11px] leading-5",
            statusTone(file.index, file.work)
          )}
        >
          {code}
        </span>
        <span className="min-w-0 truncate font-mono text-[11.5px]">{file.path}</span>
      </button>
    </li>
  );
}

function Section({
  title,
  count,
  children,
}: {
  title: string;
  count?: number;
  children: ReactNode;
}) {
  return (
    <section className="border-t px-3 py-2">
      <div className="mb-1.5 flex items-center gap-1 text-[11px] tracking-wide text-muted-foreground">
        {title}
        {count != null && <span className="tabular-nums">{count}</span>}
      </div>
      {children}
    </section>
  );
}

function Hint({ children }: { children: ReactNode }) {
  return (
    <p className="px-3 py-4 text-xs leading-relaxed text-muted-foreground">{children}</p>
  );
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

function statusTone(index: string, work: string): string {
  const s = index + work;
  if (s.includes("U")) return "text-warning";
  if (s.includes("D")) return "text-destructive";
  if (s.includes("A") || s.includes("?")) return "text-success";
  if (s.includes("M") || s.includes("R") || s.includes("C") || s.includes("T"))
    return "text-warning";
  return "text-muted-foreground";
}

export function statusLabel(file: GitFileChange, t: (key: string) => string): string {
  const mark = file.index !== " " && file.index !== "?" ? file.index : file.work;
  switch (mark) {
    case "?":
      return t("git.status_untracked");
    case "A":
      return t("git.status_added");
    case "D":
      return t("git.status_deleted");
    case "R":
      return t("git.status_renamed");
    case "C":
      return t("git.status_copied");
    case "U":
      return t("git.status_unmerged");
    case "T":
      return t("git.status_typechange");
    default:
      return t("git.status_modified");
  }
}

function formatAgo(ts: number, t: (key: string, opts?: { n: number }) => string): string {
  if (!ts) return "";
  const sec = Math.max(0, (Date.now() - ts) / 1000);
  if (sec < 60) return t("git.justNow");
  if (sec < 3600) return t("git.minAgo", { n: Math.floor(sec / 60) });
  if (sec < 86400) return t("git.hourAgo", { n: Math.floor(sec / 3600) });
  if (sec < 86400 * 30) return t("git.dayAgo", { n: Math.floor(sec / 86400) });
  return new Date(ts).toLocaleDateString();
}
