import { useTranslation } from "react-i18next";
import { ArrowLeftRight, FileDiff, Folder, GitBranch } from "lucide-react";
import { useApp, selectRightVisible, type RightPanelId } from "../store.js";
import { chord, type Command } from "../lib/shortcuts.js";
import { cn } from "@/lib/utils";

const ITEMS: {
  id: RightPanelId;
  icon: typeof GitBranch;
  label: string;
  shortcut: Command;
}[] = [
  { id: "files", icon: Folder, label: "files.panel", shortcut: "toggleFilesPanel" },
  { id: "changes", icon: FileDiff, label: "changes.panel", shortcut: "toggleChangesPanel" },
  { id: "git", icon: GitBranch, label: "git.panel", shortcut: "toggleGitPanel" },
  { id: "forward", icon: ArrowLeftRight, label: "forward.panel", shortcut: "toggleForwardPanel" },
];

/**
 * 右侧活动栏。点同一格关面板，点另一格切过去。
 */
export function RightBar() {
  const { t } = useTranslation();
  const visible = useApp(selectRightVisible);
  const panel = useApp((s) => s.rightPanel);
  const toggleRightPanel = useApp((s) => s.toggleRightPanel);

  return (
    <aside
      className="flex w-9 shrink-0 flex-col items-center border-l bg-sidebar py-1.5 text-sidebar-foreground"
      aria-label={t("rightbar.label")}
    >
      {ITEMS.map((item) => {
        const on = visible && panel === item.id;
        const label = t(item.label);
        return (
          <button
            key={item.id}
            className={cn(
              "grid size-7 place-items-center rounded-md text-muted-foreground outline-none hover:bg-accent/50 hover:text-foreground focus-visible:ring-[3px] focus-visible:ring-ring/50",
              on && "bg-accent text-foreground"
            )}
            aria-label={label}
            aria-pressed={on}
            title={`${label} · ${chord(item.shortcut)}`}
            onClick={() => toggleRightPanel(item.id)}
          >
            <item.icon className="size-3.5" />
          </button>
        );
      })}
    </aside>
  );
}
