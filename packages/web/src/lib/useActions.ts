import { useTranslation } from "react-i18next";
import type { Project, SessionWithProject, SshHost } from "@falcon/shared";
import { WORKTREE_ARCHIVE_TTL_DAYS } from "@falcon/shared";
import { api } from "../api.js";
import { useApp, type MenuItemSpec } from "../store.js";
import { connLabel } from "./hostColor.js";
import { chord } from "./shortcuts.js";

type TFunc = (key: string, options?: Record<string, unknown>) => string;

/** 相对空闲时间；绝对时间戳放在 title 上 */
export function idleText(t: TFunc, lastActiveAt: number): string {
  const min = Math.floor((Date.now() - lastActiveAt) / 60000);
  if (min < 1) return t("overview.idleNow");
  if (min < 60) return t("overview.idleMin", { n: min });
  const hours = Math.floor(min / 60);
  if (hours < 24) return t("overview.idleHour", { n: hours });
  return t("overview.idleDay", { n: Math.floor(hours / 24) });
}

export function absoluteTime(ms: number): string {
  return new Date(ms).toLocaleString();
}

/**
 * 会话 / 项目上的动作，以及两个溢出菜单的内容。
 * 侧栏、总览、命令面板共用同一份，避免同一个操作在三处各写一遍。
 */
export function useActions() {
  const { t } = useTranslation() as { t: TFunc };

  const fail = (err: unknown) => {
    const store = useApp.getState();
    store.handleApiError(err);
    store.toast({
      kind: "danger",
      title: t("toast.failed"),
      body: (err as Error).message,
    });
  };

  const run = async (fn: () => Promise<unknown>, done?: () => void) => {
    try {
      await fn();
      done?.();
    } catch (err) {
      fail(err);
    }
    await useApp.getState().refreshSessions();
  };

  const reattach = (session: SessionWithProject) =>
    run(async () => {
      const row = await api.reattachSession(session.id);
      useApp.getState().applySessionState(row.id, row.state, row.deadReason);
      if (row.state !== "active") {
        throw new Error(t("session.attachFailed"));
      }
      useApp.getState().toast({
        kind: "success",
        title: t("toast.reattached"),
        body: t("toast.reattachedBody"),
      });
    });

  const terminate = (session: SessionWithProject) => {
    useApp.getState().askConfirm({
      title: t("session.terminateTitle", { name: session.name }),
      body: t("session.terminateBody"),
      confirmLabel: t("session.terminateConfirm"),
      onConfirm: () =>
        run(
          () => api.terminateSession(session.id),
          () =>
            useApp
              .getState()
              .toast({ kind: "danger", title: t("toast.terminated", { name: session.name }) })
        ),
    });
  };

  const terminateMany = (ids: string[]) => {
    const store = useApp.getState();
    const picked = store.sessions.filter((s) => ids.includes(s.id));
    store.askConfirm({
      title: t("session.terminateManyTitle", { n: picked.length }),
      body: t("session.terminateManyBody"),
      list: picked.map((s) => ({
        name: s.name,
        state: s.state,
        meta: idleText(t, s.lastActiveAt),
      })),
      confirmLabel: t("session.terminateManyConfirm"),
      onConfirm: () =>
        run(
          async () => {
            for (const s of picked) await api.terminateSession(s.id);
          },
          () => {
            const st = useApp.getState();
            st.setSelected([]);
            st.toast({ kind: "danger", title: t("toast.terminatedMany", { n: picked.length }) });
          }
        ),
    });
  };

  /** 清除只删一条记录，没有副作用——不该和终止用同一套确认心智 */
  const clearDead = (session: SessionWithProject) =>
    run(() => api.clearSession(session.id), () => useApp.getState().dropTab(session.id));

  const clearAllDead = async () => {
    const store = useApp.getState();
    const dead = store.sessions.filter((s) => s.state === "dead");
    if (dead.length === 0) {
      store.toast({ kind: "info", title: t("overview.noDead") });
      return;
    }
    await run(
      async () => {
        for (const s of dead) await api.clearSession(s.id);
      },
      () => {
        const st = useApp.getState();
        dead.forEach((s) => st.dropTab(s.id));
        st.toast({
          kind: "info",
          title: t("overview.clearedDead", { n: dead.length }),
          body: t("overview.clearedDeadBody"),
        });
      }
    );
  };

  /**
   * 目录没删干净时把残留路径原样告诉用户——项目记录已经删了，他自己收拾得了，
   * 前提是知道该去哪儿收拾。sticky 是因为这条比"已删除"重要得多。
   */
  const reportLeftovers = (warnings?: string[]) => {
    if (!warnings?.length) return;
    useApp.getState().toast({
      kind: "warning",
      sticky: true,
      title: t("worktree.leftoverTitle"),
      body: warnings.join("\n"),
    });
  };

  const finishDelete = async () => {
    const st = useApp.getState();
    await Promise.all([st.refreshProjects(), st.refreshSessions(), st.refreshHosts()]);
  };

  const deleteProject = (project: Project) => {
    const store = useApp.getState();
    // 附属项目连坐：会话数与确认框都要把它们算进去
    const kids = store.projects.filter((p) => p.worktree?.sourceProjectId === project.id);
    const doomed = new Set([project.id, ...kids.map((p) => p.id)]);
    const live = store.sessions.filter((s) => doomed.has(s.projectId) && s.state !== "dead");
    store.askConfirm({
      title: t("project.deleteTitle", { name: project.name }),
      body:
        live.length > 0
          ? t("project.deleteBody", { n: live.length })
          : t("project.deleteBodyEmpty"),
      list: [
        ...kids.map((p) => ({ name: p.workingDir ?? p.name, meta: p.worktree?.branch })),
        ...live.map((s) => ({
          name: s.name,
          state: s.state,
          meta: idleText(t, s.lastActiveAt),
        })),
      ],
      footnote: [
        kids.length > 0 ? t("worktree.cascadeFootnote", { n: kids.length }) : null,
        project.type === "ssh" ? t("project.deleteFootnote") : null,
      ]
        .filter(Boolean)
        .join(" ") || undefined,
      confirmLabel: t("project.deleteConfirm"),
      onConfirm: async () => {
        try {
          const res = await api.deleteProject(project.id, true);
          useApp.getState().toast({
            kind: "danger",
            title: t("project.deleted", { name: project.name }),
            body: t("project.deletedBody", { n: live.length }),
          });
          reportLeftovers(res.warnings);
        } catch (err) {
          fail(err);
        }
        await finishDelete();
      },
    });
  };

  /**
   * 删除附属项目：先问一次工作区状态，把会丢的东西摆到台面上。
   *
   * 预检失败**不阻断**删除，只是确认框改口说"无法确认里面有没有未保存的东西"——
   * 把"读不到"和"是干净的"混成一个答案才是真会让人丢东西的做法。
   */
  const deleteWorktreeProject = async (project: Project) => {
    const store = useApp.getState();
    const live = store.sessions.filter(
      (s) => s.projectId === project.id && s.state !== "dead"
    );
    const dir = project.workingDir ?? "";
    const branch = project.worktree?.branch ?? "";
    const st = await api.worktreeStatus(project.id).catch(() => null);
    const clean = st != null && !st.error && st.dirtyCount === 0 && st.ignoredCount === 0;

    store.askConfirm({
      title: t("worktree.deleteTitle", { name: project.name }),
      body: [
        clean
          ? t("worktree.deleteBodyClean", { dir, branch })
          : t("worktree.deleteBodyDirty", { dir, branch }),
        live.length > 0 ? t("worktree.deleteBodySessions", { n: live.length }) : null,
      ]
        .filter(Boolean)
        .join(" "),
      list: [
        ...live.map((s) => ({
          name: s.name,
          state: s.state,
          meta: idleText(t, s.lastActiveAt),
        })),
        ...(st?.dirtySample ?? []).map((f) => ({ name: f, meta: t("worktree.dirtyFile") })),
      ],
      footnote:
        [
          // 多仓库派生行：说明整目录下 N 棵 worktree 一并移除（分支照例保留）
          project.multi
            ? t("multi.deleteFootnote", { n: project.multi.repos.length })
            : null,
          st == null || st.error ? t("worktree.deleteStatusUnknown") : null,
          st && !st.present ? t("worktree.deleteMissing") : null,
          st && st.dirtyCount > st.dirtySample.length
            ? t("worktree.deleteDirtyMore", { n: st.dirtyCount - st.dirtySample.length })
            : null,
          // 被忽略的文件必须单说：status --porcelain 里没有它们，但 .env、
          // 本地数据库会跟着一起消失，而 .env 通常是全世界唯一一份
          st && st.ignoredCount > 0
            ? t("worktree.deleteDirtyIgnored", { n: st.ignoredCount })
            : null,
          st?.ahead ? t("worktree.deleteAhead", { n: st.ahead }) : null,
        ]
          .filter(Boolean)
          .join(" · ") || undefined,
      confirmLabel: t("worktree.deleteConfirm"),
      onConfirm: async () => {
        try {
          const res = await api.deleteProject(project.id, true);
          useApp.getState().toast({
            kind: "danger",
            title: t("worktree.deleted", { name: project.name }),
            body: dir,
          });
          reportLeftovers(res.warnings);
        } catch (err) {
          fail(err);
        }
        await finishDelete();
      },
    });
  };

  /**
   * 存档附属项目：隐藏而不是删除。目录与分支原样保留，到期后端自动清理；
   * 会话与删除一样连坐，所以确认框把会被终止的会话摆出来。
   */
  const archiveWorktreeProject = (project: Project) => {
    const store = useApp.getState();
    const live = store.sessions.filter(
      (s) => s.projectId === project.id && s.state !== "dead"
    );
    store.askConfirm({
      title: t("worktree.archiveTitle", { name: project.name }),
      body: [
        t("worktree.archiveBody", {
          days: WORKTREE_ARCHIVE_TTL_DAYS,
          branch: project.worktree?.branch ?? "",
        }),
        live.length > 0 ? t("worktree.deleteBodySessions", { n: live.length }) : null,
      ]
        .filter(Boolean)
        .join(" "),
      list: live.map((s) => ({
        name: s.name,
        state: s.state,
        meta: idleText(t, s.lastActiveAt),
      })),
      footnote: t("worktree.archiveFootnote", { dir: project.workingDir ?? "" }),
      confirmLabel: t("worktree.archiveConfirm"),
      onConfirm: async () => {
        try {
          await api.archiveProject(project.id);
          const st = useApp.getState();
          // 项目从侧栏消失，选中态落回源项目，别让主区停在一个看不见的项目上
          if (st.selectedProjectId === project.id) {
            const srcId = project.worktree?.sourceProjectId;
            if (srcId && st.projects.some((p) => p.id === srcId)) st.selectProject(srcId);
            else st.showOverview();
          }
          st.toast({
            kind: "info",
            title: t("worktree.archivedToast", { name: project.name }),
            body: t("worktree.archivedToastBody", { days: WORKTREE_ARCHIVE_TTL_DAYS }),
            actionLabel: t("worktree.restore"),
            onAction: () => void restoreWorktreeProject(project),
          });
        } catch (err) {
          fail(err);
        }
        await finishDelete();
      },
    });
  };

  /** 恢复只是清掉存档标记，没有副作用——不需要确认框 */
  const restoreWorktreeProject = async (project: Project) => {
    try {
      await api.restoreProject(project.id);
      useApp.getState().toast({
        kind: "success",
        title: t("worktree.restored", { name: project.name }),
      });
    } catch (err) {
      fail(err);
    }
    await useApp.getState().refreshProjects();
  };

  const deleteHost = (host: SshHost) => {
    const store = useApp.getState();
    if (host.projectCount > 0) {
      store.toast({
        kind: "warning",
        title: t("host.deleteInUse", { n: host.projectCount }),
      });
      return;
    }
    store.askConfirm({
      title: t("host.deleteTitle", { name: host.name }),
      body: t("host.deleteBody"),
      confirmLabel: t("host.delete"),
      onConfirm: async () => {
        try {
          await api.deleteHost(host.id);
          await store.refreshHosts();
          store.toast({ kind: "success", title: t("host.deleted", { name: host.name }) });
        } catch (err) {
          fail(err);
        }
      },
    });
  };

  const hostMenuItems = (host: SshHost): MenuItemSpec[] => [
    {
      label: t("host.addProject"),
      onSelect: () => useApp.getState().openProjectForm(null, { type: "ssh", hostId: host.id }),
    },
    {
      // 多仓库项目的成员必须同宿主机，所以入口挂在主机上：位置随主机定死
      label: t("multi.newProject"),
      onSelect: () =>
        useApp.getState().openProjectForm(null, { type: "ssh", hostId: host.id, multi: true }),
    },
    {
      label: t("host.edit"),
      separated: true,
      onSelect: () => useApp.getState().openHostForm(host),
    },
    {
      label: t("host.delete"),
      separated: true,
      danger: true,
      onSelect: () => deleteHost(host),
    },
  ];

  /** 侧栏第一层：本机 / 未绑定主机的存量 SSH。已保存主机走 hostMenuItems。 */
  const serverMenuItems = (kind: "local" | "legacy"): MenuItemSpec[] => [
    {
      label: t("sidebar.newProject"),
      onSelect: () =>
        useApp.getState().openProjectForm(null, { type: kind === "local" ? "local" : "ssh" }),
    },
    // legacy（未绑定主机的存量 SSH）不给：多仓库 v1 不支持手写 ssh 字段
    ...(kind === "local"
      ? [
          {
            label: t("multi.newProject"),
            onSelect: () =>
              useApp.getState().openProjectForm(null, { type: "local" as const, multi: true }),
          },
        ]
      : []),
  ];

  const termMenuItems = (opts: {
    hasSelection: boolean;
    canPaste: boolean;
    onCopy: () => void;
    onPaste: () => void;
    onClear: () => void;
  }): MenuItemSpec[] => [
    {
      label: t("term.copy"),
      disabled: !opts.hasSelection,
      onSelect: opts.onCopy,
    },
    {
      label: t("term.paste"),
      disabled: !opts.canPaste,
      onSelect: opts.onPaste,
    },
    {
      label: t("term.clear"),
      separated: true,
      onSelect: opts.onClear,
    },
  ];

  const copyConn = async (project: Project) => {
    const store = useApp.getState();
    const text = connLabel(project, store.system, t("project.typeLocalShort"));
    try {
      await navigator.clipboard.writeText(text);
      store.toast({ kind: "info", title: t("toast.copied"), body: text });
    } catch {
      store.toast({ kind: "warning", title: t("toast.copyFailed"), body: text });
    }
  };

  const projectMenuItems = (project: Project): MenuItemSpec[] => {
    const store = useApp.getState();
    // 已存档：只剩恢复与删除。其余动作（开终端、编辑）都以"项目还在服役"为前提
    if (project.worktree?.archivedAt) {
      return [
        {
          label: t("worktree.restore"),
          onSelect: () => void restoreWorktreeProject(project),
        },
        {
          label: t("worktree.deleteNow"),
          separated: true,
          danger: true,
          onSelect: () => void deleteWorktreeProject(project),
        },
      ];
    }
    const items: MenuItemSpec[] = [
      {
        label: t("sidebar.newTerminal"),
        kbd: chord("newTerminal"),
        onSelect: () => void store.newTerminal(project.id),
      },
      {
        label: t("project.filterInOverview"),
        onSelect: () => {
          store.setProjectFilter(project.id);
          store.setFilter("all");
          store.showOverview();
        },
      },
      {
        label: t("project.edit"),
        separated: true,
        onSelect: () => store.openProjectForm(project),
      },
    ];
    // 附属项目不能再派生：树永远只有两级，删除级联也就不必递归
    if (!project.worktree) {
      items.push({
        // 多仓库容器同一个入口，WorktreeForm 内部按 project.multi 分流成批量派生
        label: project.multi ? t("multi.derive") : t("project.derive"),
        onSelect: () => store.openWorktreeForm(project.id),
      });
      // 开关是全局的，但入口挂在有存档的源项目上——不然藏起来的东西无处发现。
      // 开着的时候恒显示，用户才关得回去
      const archivedKids = store.projects.filter(
        (p) => p.worktree?.sourceProjectId === project.id && p.worktree.archivedAt
      );
      if (archivedKids.length > 0 || store.showArchived) {
        items.push({
          label: t("worktree.showArchived", { n: archivedKids.length }),
          checked: store.showArchived,
          onSelect: () => store.toggleShowArchived(),
        });
      }
    } else {
      items.push({
        label: t("worktree.archive"),
        separated: true,
        onSelect: () => archiveWorktreeProject(project),
      });
    }
    items.push({
      label: project.worktree ? t("worktree.deleteConfirm") : t("project.delete"),
      separated: true,
      danger: true,
      onSelect: () =>
        project.worktree ? void deleteWorktreeProject(project) : deleteProject(project),
    });
    return items;
  };

  const sessionMenuItems = (session: SessionWithProject): MenuItemSpec[] => {
    const store = useApp.getState();
    const project = store.projects.find((p) => p.id === session.projectId);
    const items: MenuItemSpec[] = [];
    if (session.state === "unverified") {
      items.push({
        label: t("session.reattachMenu"),
        kbd: chord("reattach"),
        onSelect: () => void reattach(session),
      });
    }
    if (session.state !== "dead") {
      items.push({ label: t("session.open"), onSelect: () => store.openSession(session.id) });
    }
    if (project) {
      items.push({
        label: t("session.copyConn"),
        onSelect: () => void copyConn(project),
      });
      items.push({
        label: t("session.duplicate"),
        onSelect: () => void store.newTerminal(project.id),
      });
    }
    if (session.state !== "dead") {
      items.push({
        label: t("session.rename"),
        separated: true,
        onSelect: () => store.openRename(session.id),
      });
    }
    if (session.state === "dead") {
      items.push({
        label: t("session.clearRecord"),
        separated: true,
        onSelect: () => void clearDead(session),
      });
    } else {
      items.push({
        label: t("session.terminate"),
        separated: true,
        danger: true,
        onSelect: () => terminate(session),
      });
    }
    return items;
  };

  return {
    reattach,
    terminate,
    terminateMany,
    clearDead,
    clearAllDead,
    deleteProject,
    deleteWorktreeProject,
    archiveWorktreeProject,
    restoreWorktreeProject,
    deleteHost,
    copyConn,
    projectMenuItems,
    hostMenuItems,
    serverMenuItems,
    sessionMenuItems,
    termMenuItems,
  };
}
