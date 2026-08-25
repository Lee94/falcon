import type { Project, SshHost } from "@falcon/shared";
import type { ProjectHead } from "../store.js";
import { hostBarFromSsh, sshBar, sshConn } from "./hostColor.js";

/**
 * 「服务器 → 文件夹 → 检出」的分组视图模型。侧栏与移动端切换面板共用：
 * 两边必须看到同一棵树，分组规则只能有一份。纯函数，零 I/O。
 */

/** 第一层：本机、已保存主机、以及没有绑定主机的存量 SSH */
export interface ServerGroup {
  key: string;
  kind: "local" | "host" | "legacy";
  name: string;
  conn?: string;
  bar?: string;
  host?: SshHost;
  folders: FolderGroup[];
}

/** 第二层：一个源项目（文件夹）。第三层永远是当前检出 + 附属 worktree */
export interface FolderGroup {
  project: Project;
  worktrees: Project[];
}

export function folderKey(projectId: string): string {
  return `p:${projectId}`;
}

export function checkoutLabel(project: Project, head?: ProjectHead): string {
  if (project.worktree) return project.worktree.branch;
  // 多仓库容器没有单一 HEAD，返回空串让调用方给「N 个仓库」这类多仓库专用标签
  if (project.multi) return "";
  return head?.branch ?? head?.sha ?? project.name;
}

export function groupServers(
  projects: Project[],
  hosts: SshHost[],
  localName: string,
  showArchived: boolean
): ServerGroup[] {
  // 存档的附属项目默认不占侧栏；开关一开就在原来的位置出现
  const shown = showArchived
    ? projects
    : projects.filter((p) => !p.worktree?.archivedAt);
  const sources = shown.filter((p) => !p.worktree);
  const sourceIds = new Set(sources.map((p) => p.id));
  const kidsBySource = new Map<string, Project[]>();
  const orphans: Project[] = [];
  for (const p of shown) {
    const src = p.worktree?.sourceProjectId;
    if (!src) continue;
    if (sourceIds.has(src)) {
      const list = kidsBySource.get(src) ?? [];
      list.push(p);
      kidsBySource.set(src, list);
    } else {
      orphans.push(p);
    }
  }

  const foldersOf = (match: (p: Project) => boolean): FolderGroup[] => [
    ...sources.filter(match).map((project) => ({
      project,
      worktrees: kidsBySource.get(project.id) ?? [],
    })),
    ...orphans.filter(match).map((project) => ({ project, worktrees: [] })),
  ];

  const servers: ServerGroup[] = [
    {
      key: "s:local",
      kind: "local",
      name: localName,
      folders: foldersOf((p) => p.type === "local"),
    },
  ];

  for (const host of hosts) {
    servers.push({
      key: `s:host:${host.id}`,
      kind: "host",
      name: host.name,
      conn: sshConn(host),
      bar: hostBarFromSsh(host),
      host,
      folders: foldersOf((p) => p.type === "ssh" && p.hostId === host.id),
    });
  }

  const seen = new Set<string>();
  for (const p of shown) {
    if (p.type !== "ssh" || p.hostId) continue;
    const conn = p.ssh ? sshConn(p.ssh) : "ssh";
    if (seen.has(conn)) continue;
    seen.add(conn);
    servers.push({
      key: `s:legacy:${conn}`,
      kind: "legacy",
      name: p.ssh?.host ?? conn,
      conn,
      bar: sshBar(p),
      folders: foldersOf(
        (x) => x.type === "ssh" && !x.hostId && (x.ssh ? sshConn(x.ssh) : "ssh") === conn
      ),
    });
  }

  return servers;
}
