import { lazy, Suspense, useEffect, useRef } from "react";
import { useApp, selectRightVisible, selectSidebarVisible } from "../store.js";
import { matchCommand, type Command } from "../lib/shortcuts.js";
import { useActions } from "../lib/useActions.js";
import { useIsMobile } from "../lib/useIsMobile.js";
import { api } from "../api.js";
import { cn, pollWhileVisible } from "@/lib/utils";
import { Login } from "./Login.js";
import { MobileShell } from "./MobileShell.js";
import { Sidebar } from "./Sidebar.js";
import { RightBar } from "./RightBar.js";
import { ResizableSlot } from "./common/ResizeHandle.js";
import { GitPanel } from "./GitPanel.js";
import { ChangesPanel } from "./ChangesPanel.js";
import { FilesPanel } from "./FilesPanel.js";
import { ForwardPanel } from "./ForwardPanel.js";
import { MeeglePanel } from "./MeeglePanel.js";
import { WorkCanvas } from "./WorkCanvas.js";
import { SessionOverview } from "./SessionOverview.js";
import { ProjectEmpty } from "./ProjectEmpty.js";
import { RenameDialog } from "./RenameDialog.js";
import { Menu } from "./common/Menu.js";
import { ConfirmDialog } from "./common/ConfirmDialog.js";
import { AskpassDialog } from "./AskpassDialog.js";
import { Toaster } from "@/components/ui/sonner";

// 浮层与重组件按需加载：首屏（登录页 / 总览）不需要 xterm、cmdk、表单和
// 设置页，切出去能把入口 chunk 砍掉一半以上。都是本地静态资源，首次打开
// 时的加载只有几毫秒，fallback 给 null 就够了。
// （文件 / 差异视图现在是画布里的窗口，它们的懒加载在 WorkCanvas 里）
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
const FileQuickOpen = lazy(() =>
  import("./FileQuickOpen.js").then((m) => ({ default: m.FileQuickOpen }))
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
  const quickOpen = useApp((s) => s.quickOpen);
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

  useEffect(() => {
    if (!authed) return;
    const pull = () => {
      void api
        .pendingAskpass()
        .then((list) => {
          const push = useApp.getState().pushAskpass;
          for (const p of list) push(p);
        })
        .catch(() => {});
    };
    pull();
    return pollWhileVisible(pull, 1500);
  }, [authed]);

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
      case "quickOpen":
        s.setQuickOpen(!s.quickOpen);
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
      case "toggleMeeglePanel":
        s.toggleRightPanel("meegle");
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
          const { projectId, thenCreate, agent } = s.install;
          s.closeInstall();
          if (thenCreate) void s.createSessionNow(projectId, { agent });
        } else if (s.confirm) s.closeConfirm();
        else if (s.renameFor) s.closeRename();
        else if (s.worktreeFor) s.closeWorktreeForm();
        else if (s.hostForm) s.closeHostForm();
        else if (s.projectForm) s.closeProjectForm();
        else if (s.paletteOpen) s.setPalette(false);
        else if (s.quickOpen) s.setQuickOpen(false);
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
      /*
       * 浮动岛骨架（docs/adr/0011）：窗口底铺 --app，侧栏 / 主区 / 右面板是浮在上面
       * 的圆角岛，之间只有 GAP 那道缝——没有一条分栏边框。右侧活动栏不成岛，图标
       * 直接落在窗口底上，所以这里右边不留 padding，由它自己带。
       */
      <div className="flex min-h-0 flex-1 gap-1.5 bg-app p-1.5 pr-0">
        {sidebarVisible && (
          <ResizableSlot side="left">
            <Sidebar />
          </ResizableSlot>
        )}
        {/*
          * 没有顶部 tab 栏：工作区就是画布上的列，开哪些窗口、谁在哪一列由排布决定，
          * 新建入口在侧栏（每个 checkout 一行 ＋）。
          */}
        <main className="flex min-w-0 flex-1 flex-col">
          {/* 这层只管定位：内容岛由各个视图自己出（画布是一列一座岛） */}
          <div className="relative min-h-0 flex-1">
            {/* 总览没有终端那种"卸载=重连"的成本，切走直接卸载，省掉后台轮询时的整表重渲 */}
            {active.kind === "overview" && (
              <div className="island absolute inset-0 z-10 flex flex-col overflow-hidden">
                <SessionOverview />
              </div>
            )}
            {active.kind === "project" && <ProjectEmpty />}
            <WorkCanvas />
          </div>
        </main>
        {rightVisible && (
          <ResizableSlot side="right">
            {rightPanel === "files" && <FilesPanel />}
            {rightPanel === "changes" && <ChangesPanel />}
            {rightPanel === "git" && <GitPanel />}
            {rightPanel === "forward" && <ForwardPanel />}
            {rightPanel === "meegle" && <MeeglePanel />}
          </ResizableSlot>
        )}
        <RightBar />
      </div>
      )}

      <Menu />
      <ConfirmDialog />
      <AskpassDialog />
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
        {quickOpen && <FileQuickOpen />}
      </Suspense>
      <Toaster />
    </div>
  );
}
