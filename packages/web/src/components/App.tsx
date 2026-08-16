import { useEffect, useRef } from "react";
import { useApp, isPendingId, selectSidebarVisible } from "../store.js";
import { matchCommand, type Command } from "../lib/shortcuts.js";
import { useActions } from "../lib/useActions.js";
import { cn } from "@/lib/utils";
import { Login } from "./Login.js";
import { Sidebar } from "./Sidebar.js";
import { TabBar } from "./TabBar.js";
import { SessionOverview } from "./SessionOverview.js";
import { PendingPane, TerminalView } from "./TerminalView.js";
import { ProjectForm } from "./ProjectForm.js";
import { WorktreeForm } from "./WorktreeForm.js";
import { PasswordModal } from "./PasswordModal.js";
import { CommandPalette } from "./CommandPalette.js";
import { HostDrawer } from "./HostDrawer.js";
import { RenameDialog } from "./RenameDialog.js";
import { ZellijInstallModal } from "./ZellijInstallModal.js";
import { Menu } from "./common/Menu.js";
import { ConfirmDialog } from "./common/ConfirmDialog.js";
import { Toaster } from "@/components/ui/sonner";

/** ⌘T / ＋ 建在当前会话所属的项目里；没有当前会话就用第一个项目 */
function currentProjectId(): string | null {
  const s = useApp.getState();
  if (s.active.kind === "terminal") {
    const id = s.active.sessionId;
    const pendingEntry = s.pending.find((p) => p.id === id);
    if (pendingEntry) return pendingEntry.projectId;
    const session = s.sessions.find((x) => x.id === id);
    if (session) return session.projectId;
  }
  return s.projects[0]?.id ?? null;
}

export function App() {
  const authChecked = useApp((s) => s.authChecked);
  const auth = useApp((s) => s.auth);
  const tabs = useApp((s) => s.tabs);
  const active = useApp((s) => s.active);
  const sidebarVisible = useApp(selectSidebarVisible);
  const projectForm = useApp((s) => s.projectForm);
  const worktreeFor = useApp((s) => s.worktreeFor);
  const passwordOpen = useApp((s) => s.passwordOpen);
  const init = useApp((s) => s.init);
  const refreshSessions = useApp((s) => s.refreshSessions);
  const closeProjectForm = useApp((s) => s.closeProjectForm);
  const closeWorktreeForm = useApp((s) => s.closeWorktreeForm);
  const setPasswordOpen = useApp((s) => s.setPasswordOpen);
  const actions = useActions();
  const actionsRef = useRef(actions);
  actionsRef.current = actions;

  useEffect(() => {
    void init();
  }, [init]);

  const authed = auth && (!auth.required || auth.authenticated);

  useEffect(() => {
    if (!authed) return;
    const timer = setInterval(() => void refreshSessions(), 5000);
    return () => clearInterval(timer);
  }, [authed, refreshSessions]);

  // 窄屏临时收起侧栏；这是设计里唯一的"响应式"，不做移动端交互。
  // 只影响显示，不改用户偏好——显式开合会解除它。
  useEffect(() => {
    const mq = window.matchMedia("(max-width: 1023px)");
    const apply = () => useApp.getState().setSidebarAutoHidden(mq.matches);
    apply();
    mq.addEventListener("change", apply);
    // resize 兜底：部分环境（远程桌面、devtools 的设备模拟）不补发 mq 的 change
    window.addEventListener("resize", apply);
    return () => {
      mq.removeEventListener("change", apply);
      window.removeEventListener("resize", apply);
    };
  }, []);

  const runCommand = (cmd: Command) => {
    const s = useApp.getState();
    switch (cmd) {
      case "palette":
        s.setPalette(!s.paletteOpen);
        return;
      case "toggleSidebar":
        s.toggleSidebar();
        return;
      case "overview":
        s.showOverview();
        return;
      case "newTerminal": {
        const projectId = currentProjectId();
        if (projectId) void s.newTerminal(projectId);
        return;
      }
      case "closeTab":
        if (s.active.kind === "terminal") void s.closeTab(s.active.sessionId);
        return;
      case "reattach": {
        if (s.active.kind !== "terminal") return;
        const sessionId = s.active.sessionId;
        const session = s.sessions.find((x) => x.id === sessionId);
        if (session?.state === "unverified") void actionsRef.current.reattach(session);
        return;
      }
      case "nextTab":
        s.cycleTab(1);
        return;
      case "prevTab":
        s.cycleTab(-1);
        return;
      default: {
        const n = Number(cmd.slice(3));
        if (Number.isInteger(n)) s.focusTabAt(n - 1);
      }
    }
  };

  useEffect(() => {
    if (!authed) return;
    const onKey = (e: KeyboardEvent) => {
      const s = useApp.getState();

      // Esc 只关最上面那一层浮层；没有浮层时不拦截，Esc 归终端（vim 用户）。
      // 各浮层都把 Radix 自己的 Esc 关闭 preventDefault 掉了，唯一的分发点在这里。
      if (e.key === "Escape") {
        if (s.install) {
          const { projectId, thenCreate } = s.install;
          s.closeInstall();
          if (thenCreate) void s.createSessionNow(projectId);
        } else if (s.confirm) s.closeConfirm();
        else if (s.renameFor) s.closeRename();
        else if (s.worktreeFor) s.closeWorktreeForm();
        else if (s.projectForm) s.closeProjectForm();
        else if (s.passwordOpen) s.setPasswordOpen(false);
        else if (s.paletteOpen) s.setPalette(false);
        else if (s.drawerProjectId) s.closeDrawer();
        else if (s.menu) s.closeMenu();
        return;
      }

      const cmd = matchCommand(e);
      if (!cmd) return;
      e.preventDefault();
      runCommand(cmd);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [authed]);

  if (!authChecked) return null;
  if (auth && auth.required && !auth.authenticated) return <Login />;

  return (
    <div className="flex h-full min-w-[820px] flex-col overflow-hidden">
      <div className="flex min-h-0 flex-1">
        {sidebarVisible && <Sidebar />}
        <main className="flex min-w-0 flex-1 flex-col bg-background">
          <TabBar />
          <div className="relative min-h-0 flex-1">
            <div
              className={cn(
                "absolute inset-0 flex flex-col",
                active.kind === "overview" ? "" : "invisible"
              )}
            >
              <SessionOverview />
            </div>
            {/* 非活动 pane 只是 visibility:hidden，绝不卸载——
                xterm 实例和 WebSocket 一旦卸载就要重连重放，切 tab 会闪 */}
            {tabs.map((id) => {
              const isActive = active.kind === "terminal" && active.sessionId === id;
              return (
                <div
                  key={id}
                  className={cn(
                    "absolute inset-0 flex flex-col",
                    isActive ? "" : "invisible"
                  )}
                >
                  {isPendingId(id) ? (
                    <PendingPane pendingId={id} />
                  ) : (
                    <TerminalView sessionId={id} visible={isActive} />
                  )}
                </div>
              );
            })}
          </div>
        </main>
      </div>

      <Menu />
      <CommandPalette />
      <HostDrawer />
      <ConfirmDialog />
      <RenameDialog />
      <ZellijInstallModal />
      {projectForm && (
        <ProjectForm
          key={projectForm.edit?.id ?? "new"}
          existing={projectForm.edit}
          onClose={closeProjectForm}
        />
      )}
      {worktreeFor && (
        <WorktreeForm key={worktreeFor} sourceId={worktreeFor} onClose={closeWorktreeForm} />
      )}
      {passwordOpen && <PasswordModal onClose={() => setPasswordOpen(false)} />}
      <Toaster />
    </div>
  );
}
