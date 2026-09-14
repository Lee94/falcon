import { useTranslation } from "react-i18next";
import { ArrowLeftRight, FileDiff, Folder, GitBranch, ListTodo } from "lucide-react";
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
  { id: "meegle", icon: ListTodo, label: "meegle.panel", shortcut: "toggleMeeglePanel" },
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
      // 不成岛：图标直接落在窗口底上，右边的 padding 由它自己带（骨架那层 pr-0）
      className="flex w-10 shrink-0 flex-col items-center gap-0.5 px-1.5 py-1.5 text-sidebar-foreground"
      aria-label={t("rightbar.label")}
    >
      {ITEMS.map((item) => {
        const on = visible && panel === item.id;
        const label = t(item.label);
        return (
          <button
            key={item.id}
            className={cn(
              "grid size-7 place-items-center rounded-lg text-muted-foreground outline-none transition-colors hover:bg-background/60 hover:text-foreground focus-visible:ring-1 focus-visible:ring-ring",
              on && "bg-tint text-tint-foreground"
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
