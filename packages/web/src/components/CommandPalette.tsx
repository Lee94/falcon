import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  CircleAlert,
  CircleDot,
  CircleX,
  PanelLeft,
  Plus,
  Settings,
  ShieldCheck,
  TerminalIcon,
  X,
  type LucideIcon,
} from "lucide-react";
import { api } from "../api.js";
import { useApp, selectSidebarVisible } from "../store.js";
import { hostLabel } from "../lib/hostColor.js";
import { chord } from "../lib/shortcuts.js";
import { useActions } from "../lib/useActions.js";
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
  const actions = useActions();
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

    const projectItems: PaletteItem[] = projects.map((project) => ({
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
      .filter((p) => p.type === "ssh")
      .forEach((project) =>
        actionItems.push({
          key: `>${t("project.durability")} ${project.name} ${project.ssh?.host ?? ""}`,
          label: t("palette.enableDurable", { host: project.ssh?.host ?? project.name }),
          meta: project.name,
          icon: ShieldCheck,
          run: () => store.openDrawer(project.id),
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
      icon: Settings,
      run: () => store.showOverview(),
    });
    actionItems.push({
      key: `>${t("palette.toggleSidebarOn")} ${t("palette.toggleSidebarOff")}`,
      label: sidebarVisible ? t("palette.toggleSidebarOn") : t("palette.toggleSidebarOff"),
      meta: chord("toggleSidebar"),
      icon: PanelLeft,
      run: () => store.toggleSidebar(),
    });
    actionItems.push({
      key: `>${t("palette.setPassword")}`,
      label: t("palette.setPassword"),
      icon: Settings,
      run: () => store.setPasswordOpen(true),
    });
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
  }, [open, sessions, projects, active, auth, sidebarVisible, t]);

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
