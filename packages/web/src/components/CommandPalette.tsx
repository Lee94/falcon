import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  CircleAlert,
  CircleDot,
  CircleX,
  Download,
  ArrowLeftRight,
  FileDiff,
  Folder,
  GitBranch,
  LayoutDashboard,
  PanelLeft,
  Plus,
  Settings,
  ShieldCheck,
  TerminalIcon,
  X,
  type LucideIcon,
} from "lucide-react";
import { api } from "../api.js";
import { useApp, selectRightVisible, selectSidebarVisible } from "../store.js";
import { hostLabel } from "../lib/hostColor.js";
import { chord } from "../lib/shortcuts.js";
import { useActions } from "../lib/useActions.js";
import { useInstall } from "../lib/useInstall.js";
import { THEME_ICONS, THEME_PREFS } from "./common/ThemeToggle.js";
import { Dialog, DialogContent, DialogTitle } from "@/components/ui/dialog";
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
  CommandShortcut,
} from "@/components/ui/command";

interface PaletteItem {
  key: string;
  label: string;
  meta?: string;
  icon: LucideIcon;
  tone?: string;
  run: () => void;
}

interface PaletteGroup {
  label: string;
  items: PaletteItem[];
}

/**
 * ⌘K 命令面板。前缀语法：@ 会话、# 项目、> 命令。
 * 所有功能都能纯键盘走完——这是"键盘操作接近于零"的解药。
 *
 * cmdk 只用来做列表与键盘遍历（shouldFilter=false）：前缀语义是这个产品自己的，
 * 交给 cmdk 的模糊打分会把 `@` / `#` / `>` 当普通字符去匹配。
 */
export function CommandPalette() {
  const { t } = useTranslation();
  const open = useApp((s) => s.paletteOpen);
  const setPalette = useApp((s) => s.setPalette);
  const sessions = useApp((s) => s.sessions);
  const projects = useApp((s) => s.projects);
  const active = useApp((s) => s.active);
  const auth = useApp((s) => s.auth);
  const sidebarVisible = useApp(selectSidebarVisible);
  const rightVisible = useApp(selectRightVisible);
  const rightPanel = useApp((s) => s.rightPanel);
  const themePref = useApp((s) => s.themePref);
  const actions = useActions();
  const { canInstall, standalone, promptInstall } = useInstall();
  const [query, setQuery] = useState("");

  useEffect(() => {
    if (open) setQuery("");
  }, [open]);

  const groups = useMemo<PaletteGroup[]>(() => {
    if (!open) return [];
    const store = useApp.getState();
    const localWord = t("project.typeLocalShort");
    const stateIcon = (state: string): [LucideIcon, string] =>
      state === "unverified"
        ? [CircleAlert, "text-warning"]
        : state === "dead"
          ? [CircleX, "text-destructive"]
          : [CircleDot, "text-success"];

    const sessionItems: PaletteItem[] = sessions.map((session) => {
      const project = projects.find((p) => p.id === session.projectId);
      const [icon, tone] = stateIcon(session.state);
      return {
        key: `@${session.name} ${session.projectName} ${hostLabel(project, localWord)}`,
        label: session.name,
        meta: `${session.projectName} · ${hostLabel(project, localWord)}`,
        icon,
        tone,
        run: () => store.openSession(session.id),
      };
    });

    // 存档的项目不能开终端（到期会连目录一起删），不进面板
    const projectItems: PaletteItem[] = projects
      .filter((p) => !p.worktree?.archivedAt)
      .map((project) => ({
        key: `#${project.name} ${t("sidebar.newTerminal")}`,
        label: t("palette.newTerminalIn", { name: project.name }),
        meta: hostLabel(project, localWord),
        icon: TerminalIcon,
        run: () => void store.newTerminal(project.id),
      }));
    projectItems.push({
      key: `#${t("palette.newProject")}`,
      label: t("palette.newProject"),
      icon: Plus,
      run: () => store.openProjectForm(null),
    });

    const actionItems: PaletteItem[] = [];
    sessions
      .filter((s) => s.state === "unverified")
      .forEach((session) =>
        actionItems.push({
          key: `>${t("session.reattach")} ${session.name}`,
          label: t("palette.reattachOne", { name: session.name }),
          meta: chord("reattach"),
          icon: CircleAlert,
          tone: "text-warning",
          run: () => void actions.reattach(session),
        })
      );
    projects
      .filter((p) => p.type === "ssh" && !p.worktree?.archivedAt)
      .forEach((project) =>
        actionItems.push({
          key: `>${t("palette.enableDurable", { host: project.ssh?.host ?? project.name })} ${project.name}`,
          label: t("palette.enableDurable", { host: project.ssh?.host ?? project.name }),
          meta: project.name,
          icon: ShieldCheck,
          run: () => store.openInstall({ projectId: project.id, thenCreate: false }),
        })
      );
    if (active.kind === "terminal") {
      const current = sessions.find((s) => s.id === active.sessionId);
      if (current && current.state !== "dead") {
        actionItems.push({
          key: `>${t("palette.terminateCurrent")}`,
          label: t("palette.terminateCurrent"),
          icon: CircleX,
          tone: "text-destructive",
          run: () => actions.terminate(current),
        });
      }
    }
    actionItems.push({
      key: `>${t("palette.overview")}`,
      label: t("palette.overview"),
      meta: chord("overview"),
      icon: LayoutDashboard,
      run: () => store.showOverview(),
    });
    actionItems.push({
      key: `>${t("palette.openSettings")}`,
      label: t("palette.openSettings"),
      icon: Settings,
      run: () => store.openSettings(),
    });
    actionItems.push({
      key: `>${t("palette.toggleSidebarOn")} ${t("palette.toggleSidebarOff")}`,
      label: sidebarVisible ? t("palette.toggleSidebarOn") : t("palette.toggleSidebarOff"),
      meta: chord("toggleSidebar"),
      icon: PanelLeft,
      run: () => store.toggleSidebar(),
    });
    const filesOn = rightVisible && rightPanel === "files";
    const changesOn = rightVisible && rightPanel === "changes";
    const gitOn = rightVisible && rightPanel === "git";
    const forwardOn = rightVisible && rightPanel === "forward";
    actionItems.push({
      key: `>${t("palette.toggleFilesOn")} ${t("palette.toggleFilesOff")}`,
      label: filesOn ? t("palette.toggleFilesOn") : t("palette.toggleFilesOff"),
      meta: chord("toggleFilesPanel"),
      icon: Folder,
      run: () => store.toggleRightPanel("files"),
    });
    actionItems.push({
      key: `>${t("palette.toggleChangesOn")} ${t("palette.toggleChangesOff")}`,
      label: changesOn ? t("palette.toggleChangesOn") : t("palette.toggleChangesOff"),
      meta: chord("toggleChangesPanel"),
      icon: FileDiff,
      run: () => store.toggleRightPanel("changes"),
    });
    actionItems.push({
      key: `>${t("palette.toggleGitOn")} ${t("palette.toggleGitOff")}`,
      label: gitOn ? t("palette.toggleGitOn") : t("palette.toggleGitOff"),
      meta: chord("toggleGitPanel"),
      icon: GitBranch,
      run: () => store.toggleRightPanel("git"),
    });
    actionItems.push({
      key: `>${t("palette.toggleForwardOn")} ${t("palette.toggleForwardOff")}`,
      label: forwardOn ? t("palette.toggleForwardOn") : t("palette.toggleForwardOff"),
      meta: chord("toggleForwardPanel"),
      icon: ArrowLeftRight,
      run: () => store.toggleRightPanel("forward"),
    });
    THEME_PREFS.filter((pref) => pref !== store.themePref).forEach((pref) =>
      actionItems.push({
        key: `>${t("theme.label")} ${t(`theme.${pref}`)} theme`,
        label: t("palette.theme", { name: t(`theme.${pref}`) }),
        icon: THEME_ICONS[pref],
        run: () => store.setTheme(pref),
      })
    );
    actionItems.push({
      key: `>${t("palette.setPassword")}`,
      label: t("palette.setPassword"),
      icon: Settings,
      run: () => store.openSettings("account"),
    });
    actionItems.push({
      key: `>${t("palette.addHost")}`,
      label: t("palette.addHost"),
      icon: Plus,
      run: () => store.openHostForm(null),
    });
    if (canInstall && !standalone) {
      actionItems.push({
        key: `>${t("palette.installApp")}`,
        label: t("palette.installApp"),
        icon: Download,
        run: () => {
          void promptInstall();
        },
      });
    }
    if (auth?.required && auth.authenticated) {
      actionItems.push({
        key: `>${t("palette.logout")}`,
        label: t("palette.logout"),
        icon: X,
        run: () => {
          void api.logout().then(() => store.refreshAuth());
        },
      });
    }

    return [
      { label: t("palette.groupSessions"), items: sessionItems },
      { label: t("palette.groupProjects"), items: projectItems },
      { label: t("palette.groupActions"), items: actionItems },
    ];
    // actions 每次渲染都是新对象，纳入依赖会让 memo 失效；它只读 store，不用跟
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, sessions, projects, active, auth, sidebarVisible, rightVisible, rightPanel, themePref, canInstall, standalone, promptInstall, t]);

  const prefix = query.charAt(0);
  const needle = query.replace(/^[>@#]/, "").trim().toLowerCase();
  const visible = groups
    .filter((g) =>
      prefix === "@"
        ? g.label === t("palette.groupSessions")
        : prefix === "#"
          ? g.label === t("palette.groupProjects")
          : prefix === ">"
            ? g.label === t("palette.groupActions")
            : true
    )
    .map((g) => ({
      label: g.label,
      items: g.items.filter(
        (i) =>
          !needle ||
          `${i.label} ${i.meta ?? ""} ${i.key}`.toLowerCase().includes(needle)
      ),
    }))
    .filter((g) => g.items.length > 0);

  if (!open) return null;

  const pick = (item: PaletteItem) => {
    setPalette(false);
    item.run();
  };

  return (
    <Dialog
      open
      onOpenChange={(next) => {
        if (!next) setPalette(false);
      }}
    >
      <DialogContent
        showCloseButton={false}
        aria-describedby={undefined}
        className="overflow-hidden p-0 sm:max-w-xl"
        onEscapeKeyDown={(e) => e.preventDefault()}
      >
        <DialogTitle className="sr-only">{t("palette.title")}</DialogTitle>
        <Command shouldFilter={false} loop>
          <CommandInput
            value={query}
            onValueChange={setQuery}
            placeholder={t("palette.placeholder")}
          />
          <CommandList className="max-h-88">
            {visible.length === 0 && <CommandEmpty>{t("palette.empty")}</CommandEmpty>}
            {visible.map((group) => (
              <CommandGroup key={group.label} heading={group.label}>
                {group.items.map((item) => (
                  <CommandItem key={item.key} value={item.key} onSelect={() => pick(item)}>
                    <item.icon className={item.tone} />
                    <span className="min-w-0 flex-1 truncate">{item.label}</span>
                    {item.meta && (
                      <CommandShortcut className="font-mono">{item.meta}</CommandShortcut>
                    )}
                  </CommandItem>
                ))}
              </CommandGroup>
            ))}
          </CommandList>
        </Command>
      </DialogContent>
    </Dialog>
  );
}
