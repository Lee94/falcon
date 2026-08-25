import type { Project, SystemInfo } from "@falcon/shared";

/**
 * 主机身份色。
 *
 * 由 user@host:port 稳定哈希到一个 hue，同一台机器在侧栏、tab、抽屉里永远是
 * 同一条色带。本地项目**不给**色条——"有颜色 = 在别人的机器上"这个信号必须
 * 独占，才有警示价值。
 *
 * 只哈希色相；深浅交给 CSS 变量 --host-s / --host-l（styles.css 里明暗各一套），
 * 于是同一台机器在浅色下是深色条、在深色下是浅色条，色相不变、认得出来。
 */

/** 避开 100–150（绿，与运行中冲突）和 0–20（红，与已丢失冲突） */
const BANDS = [30, 50, 170, 190, 210, 230, 250, 270, 290, 310, 330];

export function hostHue(key: string): number {
  let h = 2166136261; // FNV-1a
  for (let i = 0; i < key.length; i++) {
    h ^= key.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return BANDS[Math.abs(h) % BANDS.length]!;
}

export function hostKey(project?: Project | null): string {
  if (!project?.ssh) return "";
  const { username, host, port } = project.ssh;
  return `${username}@${host}:${port}`;
}

/**
 * 项目的身份色。alpha 传入时返回同色调的低透明底色（如 header 叠加）。
 * 本地项目返回 transparent / 中性色。
 */
export function hostBar(project?: Project | null, alpha?: string): string {
  if (!project || project.type !== "ssh" || !project.ssh) {
    return alpha ? "transparent" : "var(--border)";
  }
  const h = hostHue(hostKey(project));
  const base = `${h} var(--host-s) var(--host-l)`;
  return alpha ? `hsl(${base} / ${alpha})` : `hsl(${base})`;
}

/** SSH 项目才有色条；本地项目返回 undefined，调用方据此不渲染色条 */
export function sshBar(project?: Project | null): string | undefined {
  return project?.type === "ssh" ? hostBar(project) : undefined;
}

/** 已保存主机的身份色，和引用它的项目同一条色带 */
export function hostBarFromSsh(ssh: { username: string; host: string; port: number }): string {
  const h = hostHue(sshConn(ssh));
  return `hsl(${h} var(--host-s) var(--host-l))`;
}

/** user@host:port。主机列表和项目下拉共用，避免两处拼法漂移 */
export function sshConn(ssh: { username: string; host: string; port: number }): string {
  return `${ssh.username}@${ssh.host}:${ssh.port}`;
}

/** 连接串：SSH 为 user@host:port，本地为「本机 · 平台」 */
export function connLabel(
  project: Project | undefined | null,
  system: SystemInfo | null,
  localWord: string
): string {
  if (!project) return "";
  if (project.type === "ssh" && project.ssh) {
    return sshConn(project.ssh);
  }
  return system?.platform ? `${localWord} · ${system.platform}` : localWord;
}

/** 侧栏/总览里那一列短标签：SSH 显示主机名，本地显示「本机」 */
export function hostLabel(project: Project | undefined | null, localWord: string): string {
  if (!project) return "";
  return project.type === "ssh" && project.ssh ? project.ssh.host : localWord;
}
