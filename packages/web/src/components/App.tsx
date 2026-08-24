import { lazy, Suspense, useEffect, useRef } from "react";
import { useApp, isPendingId, selectRightVisible, selectSidebarVisible } from "../store.js";
import { matchCommand, type Command } from "../lib/shortcuts.js";
import { useActions } from "../lib/useActions.js";
import { useIsMobile } from "../lib/useIsMobile.js";
import { cn, pollWhileVisible } from "@/lib/utils";
import { Login } from "./Login.js";
import { MobileShell } from "./MobileShell.js";
import { Sidebar } from "./Sidebar.js";
import { RightBar } from "./RightBar.js";
import { GitPanel } from "./GitPanel.js";
import { ChangesPanel } from "./ChangesPanel.js";
import { FilesPanel } from "./FilesPanel.js";
import { ForwardPanel } from "./ForwardPanel.js";
import { TabBar } from "./TabBar.js";
import { SessionOverview } from "./SessionOverview.js";
import { ProjectEmpty } from "./ProjectEmpty.js";
import { RenameDialog } from "./RenameDialog.js";
import { Menu } from "./common/Menu.js";
import { ConfirmDialog } from "./common/ConfirmDialog.js";
import { Toaster } from "@/components/ui/sonner";

// 浮层与重组件按需加载：首屏（登录页 / 总览）不需要 xterm、cmdk、表单和
// 设置页，切出去能把入口 chunk 砍掉一半以上。都是本地静态资源，首次打开
// 时的加载只有几毫秒，fallback 给 null 就够了。
const TerminalView = lazy(() =>
  import("./TerminalView.js").then((m) => ({ default: m.TerminalView }))
);
const PendingPane = lazy(() =>
  import("./TerminalView.js").then((m) => ({ default: m.PendingPane }))
);
const GitDiffView = lazy(() =>
  import("./GitDiffView.js").then((m) => ({ default: m.GitDiffView }))
);
const FileView = lazy(() => import("./FileView.js").then((m) => ({ default: m.FileView })));
const ProjectForm = lazy(() =>
  import("./ProjectForm.js").then((m) => ({ default: m.ProjectForm }))
);
const HostForm = lazy(() => import("./HostForm.js").then((m) => ({ default: m.HostForm })));
const WorktreeForm = lazy(() =>
  import("./WorktreeForm.js").then((m) => ({ default: m.WorktreeForm }))
);
const SettingsModal = lazy(() =>
  import("./SettingsModal.js").then((m) => ({ default: m.SettingsModal }))
);
const CommandPalette = lazy(() =>
  import("./CommandPalette.js").then((m) => ({ default: m.CommandPalette }))
);
const ZellijInstallModal = lazy(() =>
  import("./ZellijInstallModal.js").then((m) => ({ default: m.ZellijInstallModal }))
);

/** ⌘T / ＋：侧栏选中的项目优先，否则当前会话所属项目，再否则第一个项目 */
function currentProjectId(): string | null {
  const s = useApp.getState();
  if (s.selectedProjectId) return s.selectedProjectId;
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
  const rightVisible = useApp(selectRightVisible);
  const rightPanel = useApp((s) => s.rightPanel);
  const projectForm = useApp((s) => s.projectForm);
  const hostForm = useApp((s) => s.hostForm);
  const worktreeFor = useApp((s) => s.worktreeFor);
  const settingsOpen = useApp((s) => s.settingsOpen);
  const installOpen = useApp((s) => s.install != null);
  const paletteOpen = useApp((s) => s.paletteOpen);
  const init = useApp((s) => s.init);
  const refreshSessions = useApp((s) => s.refreshSessions);
  const closeProjectForm = useApp((s) => s.closeProjectForm);
  const closeHostForm = useApp((s) => s.closeHostForm);
  const closeWorktreeForm = useApp((s) => s.closeWorktreeForm);
  const actions = useActions();
  const actionsRef = useRef(actions);
  actionsRef.current = actions;
  const isMobile = useIsMobile();

  useEffect(() => {
    void init();
  }, [init]);

  // 整页关掉浏览器自带右键菜单；自定义菜单各自用 openContextMenu 开。
  useEffect(() => {
    const block = (e: Event) => e.preventDefault();
    document.addEventListener("contextmenu", block);
    return () => document.removeEventListener("contextmenu", block);
  }, []);

  const authed = auth && (!auth.required || auth.authenticated);

  useEffect(() => {
    if (!authed) return;
    // 页面不可见时停掉轮询，回到前台立刻补一次
    return pollWhileVisible(() => void refreshSessions(), 5000);
  }, [authed, refreshSessions]);

  // 窄屏临时收起侧栏；这是设计里唯一的"响应式"，不做移动端交互。
  // 只影响显示，不改用户偏好——显式开合会解除它。
  useEffect(() => {
    const mq = window.matchMedia("(max-width: 1023px)");
    const apply = () => {
      // resize 每帧都进来，值没变就别打扰 store（130 个 selector 会全跑一遍）
      const s = useApp.getState();
      if (s.sidebarAutoHidden !== mq.matches) s.setSidebarAutoHidden(mq.matches);
    };
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
      case "toggleGitPanel":
        s.toggleRightPanel("git");
        return;
      case "toggleChangesPanel":
        s.toggleRightPanel("changes");
        return;
      case "toggleForwardPanel":
        s.toggleRightPanel("forward");
        return;
      case "toggleFilesPanel":
        s.toggleRightPanel("files");
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
        else if (s.active.kind === "diff") s.closeDiff();
        else if (s.active.kind === "file") s.closeFile();
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
        else if (s.hostForm) s.closeHostForm();
        else if (s.projectForm) s.closeProjectForm();
        else if (s.paletteOpen) s.setPalette(false);
        else if (s.settingsOpen) s.closeSettings();
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
    // 移动端只换主区骨架；浮层（菜单 / 确认 / 表单 / 设置 / toast）两端共用
    <div className={cn("flex h-full flex-col overflow-hidden", !isMobile && "min-w-[820px]")}>
      {isMobile ? (
        <MobileShell />
      ) : (
      <div className="flex min-h-0 flex-1">
        {sidebarVisible && <Sidebar />}
        <main className="flex min-w-0 flex-1 flex-col bg-background">
          <TabBar />
          <div className="relative min-h-0 flex-1">
            {/* 总览没有终端那种"卸载=重连"的成本，切走直接卸载，省掉后台轮询时的整表重渲 */}
            {active.kind === "overview" && (
              <div className="absolute inset-0 flex flex-col">
                <SessionOverview />
              </div>
            )}
            {active.kind === "project" && <ProjectEmpty />}
            {/* 差异视图没有 xterm 那种重连成本，切走即卸载，切回来重拉一份新的 */}
            {active.kind === "diff" && (
              <div className="absolute inset-0 flex flex-col">
                <Suspense fallback={null}>
                  <GitDiffView />
                </Suspense>
              </div>
            )}
            {/* 查看 tab 与差异 tab 同理：切走即卸载，切回来重读一次文件 */}
            {active.kind === "file" && (
              <div className="absolute inset-0 flex flex-col">
                <Suspense fallback={null}>
                  <FileView />
                </Suspense>
              </div>
            )}
            {/* 非活动 pane 只是移出视口，绝不卸载——
                xterm 实例和 WebSocket 一旦卸载就要重连重放，切 tab 会闪。
                用 translate 而不是 visibility:hidden：xterm 靠 IntersectionObserver
                的几何相交判定是否暂停渲染，hidden 不改几何、后台 tab 会照常全速刷
                DOM；移出视口（被根节点 overflow-hidden 裁掉）才会真正暂停，切回时
                xterm 自动做一次全量刷新。transform 不影响布局尺寸，fit 测量不受影响 */}
            {tabs.map((id) => {
              const isActive = active.kind === "terminal" && active.sessionId === id;
              return (
                <div
                  key={id}
                  className={cn(
                    "absolute inset-0 flex flex-col",
                    isActive ? "" : "invisible -translate-x-[200%]"
                  )}
                >
                  <Suspense fallback={null}>
                    {isPendingId(id) ? (
                      <PendingPane pendingId={id} />
                    ) : (
                      <TerminalView sessionId={id} visible={isActive} />
                    )}
                  </Suspense>
                </div>
              );
            })}
          </div>
        </main>
        {rightVisible && rightPanel === "files" && <FilesPanel />}
        {rightVisible && rightPanel === "changes" && <ChangesPanel />}
        {rightVisible && rightPanel === "git" && <GitPanel />}
        {rightVisible && rightPanel === "forward" && <ForwardPanel />}
        <RightBar />
      </div>
      )}

      <Menu />
      <ConfirmDialog />
      <RenameDialog />
      <Suspense fallback={null}>
        {settingsOpen && <SettingsModal />}
        {installOpen && <ZellijInstallModal />}
        {projectForm && (
          <ProjectForm
            key={`project-${projectForm.edit?.id ?? "new"}`}
            existing={projectForm.edit}
            preset={projectForm.preset}
            onClose={closeProjectForm}
          />
        )}
        {hostForm && (
          <HostForm
            key={`host-${hostForm.edit?.id ?? "new"}`}
            existing={hostForm.edit}
            onSaved={hostForm.onSaved}
            onClose={closeHostForm}
          />
        )}
        {worktreeFor && (
          <WorktreeForm key={worktreeFor} sourceId={worktreeFor} onClose={closeWorktreeForm} />
        )}
        {paletteOpen && <CommandPalette />}
      </Suspense>
      <Toaster />
    </div>
  );
}
