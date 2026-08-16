import { useTranslation } from "react-i18next";
import type { Project, SessionWithProject } from "@mojito/shared";
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
    run(
      () => api.reattachSession(session.id),
      () =>
        useApp.getState().toast({
          kind: "success",
          title: t("toast.reattached"),
          body: t("toast.reattachedBody"),
        })
    );

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
    await Promise.all([st.refreshProjects(), st.refreshSessions()]);
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
        label: t("project.derive"),
        onSelect: () => store.openWorktreeForm(project.id),
      });
    }
    items.push({
      label: t("project.durability"),
      onSelect: () => store.openDrawer(project.id),
    });
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
    if (project) {
      items.push({
        label: t("project.durability"),
        onSelect: () => store.openDrawer(project.id),
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
    copyConn,
    projectMenuItems,
    sessionMenuItems,
  };
}
