import { useTranslation } from "react-i18next";
import { TerminalIcon } from "lucide-react";
import { useApp } from "../store.js";
import { Button } from "@/components/ui/button";

/** 侧栏选中了项目、但这个项目下还没有终端时的主区 */
export function ProjectEmpty() {
  const { t } = useTranslation();
  const projectId = useApp((s) => s.selectedProjectId);
  const project = useApp((s) => s.projects.find((p) => p.id === s.selectedProjectId));
  const newTerminal = useApp((s) => s.newTerminal);

  return (
    <div className="island absolute inset-0 z-10 flex flex-col items-center justify-center gap-2.5 px-6 text-center text-muted-foreground">
      <TerminalIcon className="size-7 stroke-[1.5] text-muted-foreground/50" />
      <span className="text-[15px] text-foreground">
        {t("sidebar.emptyProjectTitle")}
        {project ? ` · ${project.name}` : ""}
      </span>
      <span className="max-w-115 text-[12.5px] leading-relaxed">
        {t("sidebar.emptyProjectBody")}
      </span>
      {projectId && (
        <Button className="mt-1.5" onClick={() => void newTerminal(projectId)}>
          {t("sidebar.newTerminal")}
        </Button>
      )}
    </div>
  );
}
