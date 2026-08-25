import { Fragment } from "react";
import { useTranslation } from "react-i18next";
import { Folder, Folders, GitBranch, Monitor, Plus, Server, Settings } from "lucide-react";
import type { Project, SessionWithProject } from "@falcon/shared";
import { useApp } from "../store.js";
import { checkoutLabel, groupServers, type ServerGroup } from "../lib/projectTree.js";
import { cn } from "@/lib/utils";
import { StatusMark, useStateLabel } from "./common/StatusMark.js";

/**
 * 移动端切换面板：底部抽屉，按「服务器 → 项目 → 会话」列出全部会话。
 * 分组逻辑与侧栏共用 groupServers——两边必须看到同一棵树。
 *
 * 整行都是命中区。点「待接回」的会话就是接回：打开后 TerminalView 建 WS，
 * 首个 resize 会触发服务端的懒惰接回（见 TerminalView 里 sendResize 的注释）。
 */
export function MobileSwitcher({ onClose }: { onClose: () => void }) {
  const { t } = useTranslation();
  const projects = useApp((s) => s.projects);
  const hosts = useApp((s) => s.hosts);
  const sessions = useApp((s) => s.sessions);
  const heads = useApp((s) => s.heads);
  const showArchived = useApp((s) => s.showArchived);
  const active = useApp((s) => s.active);
  const selectedProjectId = useApp((s) => s.selectedProjectId);
  const openSession = useApp((s) => s.openSession);
  const newTerminal = useApp((s) => s.newTerminal);
  const openSettings = useApp((s) => s.openSettings);
  const stateLabel = useStateLabel();

  // 空服务器组不占抽屉：移动端只做切换，建项目这类管理动作留给桌面端
  const servers = groupServers(
    projects,
    hosts,
    t("project.typeLocalShort"),
    showArchived
  ).filter((g) => g.folders.length > 0);
  const activeId = active.kind === "terminal" ? active.sessionId : null;
  const activeSession = activeId ? sessions.find((s) => s.id === activeId) : undefined;

  /** 抽屉底部「新建终端」落在哪个项目：与顶栏 ＋ 同一套优先级 */
  const targetProject =
    projects.find((p) => p.id === (selectedProjectId ?? activeSession?.projectId)) ??
    projects[0];

  const open = (id: string) => {
    openSession(id);
    onClose();
  };
  const create = (projectId: string) => {
    onClose();
    void newTerminal(projectId);
  };

  return (
    <div className="fixed inset-0 z-50" role="dialog" aria-modal="true" aria-label={t("mobile.switchTitle")}>
      <div className="absolute inset-0 bg-black/50" onClick={onClose} />
      <div
        className="absolute inset-x-0 bottom-0 flex max-h-[85%] flex-col rounded-t-xl border-t bg-popover text-popover-foreground shadow-lg"
        style={{ paddingBottom: "max(0.75rem, env(safe-area-inset-bottom))" }}
      >
        <div className="grid shrink-0 place-items-center pt-2 pb-1">
          <div className="h-1 w-9 rounded-full bg-muted-foreground/40" />
        </div>
        <div className="flex h-8 shrink-0 items-center px-4 text-[13px] font-medium text-muted-foreground">
          {t("mobile.switchTitle")}
        </div>

        <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain">
          {servers.length === 0 && (
            <p className="px-4 py-3 text-xs leading-relaxed text-muted-foreground">
              {t("sidebar.empty")}
            </p>
          )}
          {servers.map((server) => (
            <ServerSection
              key={server.key}
              server={server}
              sessions={sessions}
              heads={heads}
              activeId={activeId}
              stateLabel={stateLabel}
              onOpen={open}
              onCreate={create}
            />
          ))}
        </div>

        <div className="shrink-0 border-t pt-1">
          {targetProject && (
            <button
              className="flex h-12 w-full items-center gap-2.5 px-4 text-left text-[13px] outline-none active:bg-accent/50 focus-visible:ring-[3px] focus-visible:ring-ring/50"
              onClick={() => create(targetProject.id)}
            >
              <Plus className="size-4 shrink-0 text-muted-foreground" />
              <span className="truncate">
                {t("palette.newTerminalIn", { name: targetProject.name })}
              </span>
            </button>
          )}
          <button
            className="flex h-12 w-full items-center gap-2.5 px-4 text-left text-[13px] outline-none active:bg-accent/50 focus-visible:ring-[3px] focus-visible:ring-ring/50"
            onClick={() => {
              onClose();
              openSettings();
            }}
          >
            <Settings className="size-4 shrink-0 text-muted-foreground" />
            <span>{t("sidebar.settingsTitle")}</span>
          </button>
        </div>
      </div>
    </div>
  );
}

function ServerSection({
  server,
  sessions,
  heads,
  activeId,
  stateLabel,
  onOpen,
  onCreate,
}: {
  server: ServerGroup;
  sessions: SessionWithProject[];
  heads: Record<string, { branch?: string; sha?: string }>;
  activeId: string | null;
  stateLabel: (state: SessionWithProject["state"]) => string;
  onOpen: (id: string) => void;
  onCreate: (projectId: string) => void;
}) {
  const { t } = useTranslation();
  const Icon = server.kind === "local" ? Monitor : Server;
  return (
    <div className="pb-1">
      <div className="flex h-8 items-center gap-1.5 px-4 pt-1 text-xs text-muted-foreground">
        {/* 主机身份色条只给 SSH——"有颜色 = 在别人的机器上"的信号必须独占 */}
        {server.bar && (
          <span className="h-3.5 w-[3px] shrink-0 rounded-full" style={{ background: server.bar }} />
        )}
        <Icon className="size-3.5 shrink-0" />
        <span className="truncate font-medium">{server.name}</span>
        <span className="flex-1" />
        {server.conn && <span className="truncate text-[11px]">{server.conn}</span>}
      </div>
      {server.folders.map((folder) => (
        <Fragment key={folder.project.id}>
          <ProjectBlock
            project={folder.project}
            label={folder.project.name}
            meta={
              folder.project.multi
                ? t("multi.repoCount", { n: folder.project.multi.repos.length })
                : checkoutLabel(folder.project, heads[folder.project.id])
            }
            depth={0}
            sessions={sessions}
            activeId={activeId}
            stateLabel={stateLabel}
            onOpen={onOpen}
            onCreate={onCreate}
          />
          {folder.worktrees.map((wt) => (
            <ProjectBlock
              key={wt.id}
              project={wt}
              label={checkoutLabel(wt)}
              meta={wt.name}
              worktree
              depth={1}
              sessions={sessions}
              activeId={activeId}
              stateLabel={stateLabel}
              onOpen={onOpen}
              onCreate={onCreate}
            />
          ))}
        </Fragment>
      ))}
    </div>
  );
}

function ProjectBlock({
  project,
  label,
  meta,
  worktree,
  depth,
  sessions,
  activeId,
  stateLabel,
  onOpen,
  onCreate,
}: {
  project: Project;
  label: string;
  meta?: string;
  worktree?: boolean;
  depth: number;
  sessions: SessionWithProject[];
  activeId: string | null;
  stateLabel: (state: SessionWithProject["state"]) => string;
  onOpen: (id: string) => void;
  onCreate: (projectId: string) => void;
}) {
  const { t } = useTranslation();
  const mine = sessions.filter((s) => s.projectId === project.id);
  const RowIcon = worktree ? GitBranch : project.multi ? Folders : Folder;

  return (
    <div>
      <div
        className="flex h-9 items-center gap-2 pr-4"
        style={{ paddingLeft: 16 + depth * 16 }}
        title={project.workingDir ?? project.name}
      >
        <RowIcon className="size-3.5 shrink-0 text-muted-foreground" />
        <span className="min-w-0 flex-1 truncate text-[13px] font-medium">{label}</span>
        {meta && (
          <span className="max-w-32 truncate font-mono text-[11px] text-muted-foreground">
            {meta}
          </span>
        )}
      </div>
      {mine.length === 0 ? (
        // 没有会话的项目给一条"新建"占位行——不然它在移动端就是死胡同
        <button
          className="mx-2 flex h-12 w-[calc(100%-1rem)] items-center gap-2 rounded-md px-2 text-left text-[13px] text-muted-foreground outline-none active:bg-accent/50 focus-visible:ring-[3px] focus-visible:ring-ring/50"
          style={{ paddingLeft: 16 + depth * 16 }}
          onClick={() => onCreate(project.id)}
        >
          <Plus className="size-3.5 shrink-0" />
          <span>{t("sidebar.newTerminal")}</span>
        </button>
      ) : (
        mine.map((s) => (
          <button
            key={s.id}
            className={cn(
              "mx-2 flex h-12 w-[calc(100%-1rem)] items-center gap-2 rounded-md px-2 text-left outline-none focus-visible:ring-[3px] focus-visible:ring-ring/50",
              s.id === activeId ? "bg-accent" : "active:bg-accent/50"
            )}
            style={{ paddingLeft: 16 + depth * 16 }}
            onClick={() => onOpen(s.id)}
          >
            <span className="min-w-0 flex-1 truncate text-[13px]">{s.name}</span>
            {/* 运行中不摆状态；待接回 / 已丢失点整行即打开（顺带触发懒惰接回） */}
            {s.state !== "active" && (
              <span
                className={cn(
                  "flex shrink-0 items-center gap-1.5 text-xs",
                  s.state === "unverified" ? "text-warning" : "text-destructive"
                )}
              >
                <StatusMark state={s.state} />
                {stateLabel(s.state)}
              </span>
            )}
          </button>
        ))
      )}
    </div>
  );
}
