import type { Session } from "@falcon/shared";

/**
 * 会话在界面上显示成什么。
 *
 * 优先级：手起的名字 > 前台命令（后端探的自动标题）> agent 的 CLI 名。
 * 三样都没有就返回 null——**这时不要编一个占位名出来**，调用方各自兜底：
 * 窗口标题栏右边紧跟着完整工作目录，什么都不显示才对；侧栏只有一个文本槽，
 * 落到 shellLabel()。
 *
 * agent 兜底取 CLI 名小写（claude / codex / grok），与前台命令同形：CLI 正跑着时
 * 探到的前台命令本来就是同一个词，它退出回到 shell 也不会跳成另一种写法。
 */
export function sessionTitle(
  session: Pick<Session, "name" | "title" | "agent">
): string | null {
  const name = session.name.trim();
  if (name) return name;
  const title = session.title?.trim();
  if (title) return title;
  return session.agent ?? null;
}

/**
 * 会话在**列表**里显示成什么：自动标题，空闲 shell 落到 shell 的命令名。
 *
 * 侧栏、会话总览、命令面板、移动端切换面板、各种确认框共用一份——同一个会话
 * 在这些地方必须叫同一个名字，否则用户没法把它们对应起来。React 里用
 * useSessionLabel()，store 内部（没有 hook）直接调这个。
 *
 * 窗口标题栏不走这里：它右边就是完整工作目录，空闲会话该让标题那一格空着
 * （见 WorkCanvas 的 TermPaneInfo）。
 */
export function sessionLabel(
  session: Pick<Session, "name" | "title" | "agent" | "projectId">,
  projects: readonly { id: string; shell?: string }[]
): string {
  return (
    sessionTitle(session) ??
    shellLabel(projects.find((p) => p.id === session.projectId)?.shell)
  );
}

/**
 * 空闲会话在侧栏顶上的那一格：shell 的命令名（zsh / bash / powershell）。
 *
 * 用命令名而不是"终端 3"这类标签，是为了跟前台命令同一形态——同一行里这一格
 * 要么是 `pnpm dev`，要么是 `zsh`，读的人不用分辨"这是名字还是在跑的东西"。
 *
 * 路径两种分隔符都切：project.shell 可能是远端 Windows 上的 `C:\\...\\pwsh.exe`，
 * 而这里不能用 node:path（同 server/git/path.ts 的理由：后端平台 ≠ 宿主机平台）。
 */
export function shellLabel(shell?: string | null): string {
  const leaf = (shell ?? "").split(/[/\\]/).pop() ?? "";
  const bare = leaf.replace(/\.(exe|cmd|bat)$/i, "");
  return bare || "shell";
}
