import { useCallback } from "react";
import { useApp } from "../store.js";
import { sessionLabel } from "./sessionTitle.js";
import type { Session } from "@falcon/shared";

/**
 * lib/sessionTitle.ts 的 sessionLabel() 的 React 版：自己订阅 projects
 * （shell 兜底要用），projects 没变时函数引用是稳的——命令面板把它放进
 * useMemo 的依赖里，每次渲染换个函数就等于没 memo。
 */
export function useSessionLabel(): (session: Session) => string {
  const projects = useApp((s) => s.projects);
  return useCallback((session: Session) => sessionLabel(session, projects), [projects]);
}
