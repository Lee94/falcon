import { useTranslation } from "react-i18next";
import type { Project } from "@falcon/shared";
import { memberBasename } from "../../lib/multiDerive.js";
import { selectMultiRepoDir, useApp } from "../../store.js";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

/**
 * 多仓库项目在 Git / 修改面板头部的成员切换器。非多仓库项目渲染 null，
 * 两个面板可以无条件挂上它。选择存在 store（multiRepo）——两个面板与
 * diff tab 必须看同一个成员，组件本地 state 必然漂移。
 */
export function MultiRepoSelect({ project }: { project: Project | null | undefined }) {
  const { t } = useTranslation();
  const multiRepo = useApp((s) => s.multiRepo);
  const setMultiRepo = useApp((s) => s.setMultiRepo);
  if (!project?.multi) return null;
  const value = selectMultiRepoDir({ multiRepo }, project) ?? "";
  return (
    <Select value={value} onValueChange={(v) => setMultiRepo(project.id, v)}>
      <SelectTrigger
        className="h-7 w-auto max-w-36 shrink-0 gap-1 px-2 font-mono text-[11px]"
        aria-label={t("multi.pickRepo")}
        title={value}
      >
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        {project.multi.repos.map((m) => (
          <SelectItem key={m.dir} value={m.dir} className="font-mono text-xs">
            {memberBasename(m.dir)}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}
