/**
 * 中转（端口转发 + 公网发布）的纯函数层：同端口互斥与旧规则迁移。零 I/O。
 *
 * 同端口可以存多条通道，同时只能一条生效。「同端口」按监听真正落在哪台机器上算：
 * - 本地转发（ssh -L）的监听在 falcon 后端本机，不管挂在哪台 SSH Host 上都抢
 *   同一张端口表——本机 5432 → A 与本机 5432 → B 就是这条规则要解决的「切换目标」；
 * - 远端转发（ssh -R）的监听在远端，只和同一台主机上的远端转发冲突；
 * - 公网发布不占固定端口（本机桥是 listen(0)），「同端口」指发布的是同一个目标
 *   端口：同一台机器上同一个服务发两条 Quick Tunnel 只会多一个没人用的 URL。
 * 只比端口不比监听地址：界面上只能填端口，127.0.0.1 与 0.0.0.0 的同号端口在
 * 系统层面本来就互斥。
 */

import type { ForwardKind } from "@falcon/shared";

/** 公网发布挂在本机时 host_id 为 null，槽位里用这个占位 */
const LOCAL = "local";

export interface ForwardSlotRow {
  id: string;
  host_id: string;
  kind: string;
  bind_port: number;
  enabled: number;
  created_at: number;
}

export interface ShareSlotRow {
  id: string;
  host_id: string | null;
  dest_port: number;
  enabled: number;
  created_at: number;
}

export function forwardSlot(row: Pick<ForwardSlotRow, "host_id" | "kind" | "bind_port">): string {
  return (row.kind as ForwardKind) === "local"
    ? `local\0${row.bind_port}`
    : `remote\0${row.host_id}\0${row.bind_port}`;
}

export function shareSlot(row: Pick<ShareSlotRow, "host_id" | "dest_port">): string {
  return `${row.host_id ?? LOCAL}\0${row.dest_port}`;
}

/**
 * 让 target 生效时要停掉的规则：与它同槽位、此刻 enabled 的其它行。
 * target 自己不在返回值里，不论它现在是否 enabled。
 */
export function displacedBy<T extends { id: string; enabled: number }>(
  rows: readonly T[],
  target: T,
  slot: (row: T) => string
): string[] {
  const key = slot(target);
  return rows.filter((r) => r.id !== target.id && r.enabled === 1 && slot(r) === key).map((r) => r.id);
}

/**
 * 一组规则里违反「同槽位只有一条 enabled」的那些，按创建先后留最早的一条。
 * 只给迁移用：旧的按项目挂的规则并到一台主机上以后，可能撞出同端口的一对。
 */
export function excessEnabled<T extends { id: string; enabled: number; created_at: number }>(
  rows: readonly T[],
  slot: (row: T) => string
): string[] {
  const kept = new Set<string>();
  const excess: string[] = [];
  const enabled = rows.filter((r) => r.enabled === 1).sort((a, b) => a.created_at - b.created_at);
  for (const row of enabled) {
    const key = slot(row);
    if (kept.has(key)) excess.push(row.id);
    else kept.add(key);
  }
  return excess;
}

export interface LegacyProjectConn {
  host_id: string | null;
  ssh_host: string | null;
  ssh_port: number | null;
  ssh_username: string | null;
}

export interface HostConn {
  id: string;
  host: string;
  port: number;
  username: string;
}

/**
 * 旧的按项目挂的规则迁到哪台已保存主机上。
 *
 * 先认项目上记的 host_id（主机还在才算）；存量项目没绑主机时按连接三元组
 * （host / port / username）找一台一模一样的——隧道走的就是这组凭据，换成别的
 * 用户登录，远端监听的权限与可达地址都可能不同。都找不到返回 null，由调用方丢弃
 * 并告警：没有落点的规则在新模型里无处可挂。
 */
export function legacyForwardHost(
  project: LegacyProjectConn,
  hosts: readonly HostConn[]
): string | null {
  if (project.host_id && hosts.some((h) => h.id === project.host_id)) return project.host_id;
  if (!project.ssh_host || !project.ssh_username) return null;
  const port = project.ssh_port ?? 22;
  const match = hosts.find(
    (h) => h.host === project.ssh_host && h.port === port && h.username === project.ssh_username
  );
  return match?.id ?? null;
}
