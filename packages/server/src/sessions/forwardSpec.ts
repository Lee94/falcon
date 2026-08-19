import type { ForwardKind, PortForwardInput } from "@mojito/shared";

export interface NormalizedForward {
  name?: string;
  kind: ForwardKind;
  bindHost: string;
  bindPort: number;
  destHost: string;
  destPort: number;
  enabled: boolean;
}

const DEFAULT_HOST = "127.0.0.1";
const NAME_MAX = 80;

export function parsePort(value: unknown): number | null {
  if (typeof value === "number" && Number.isInteger(value)) return value;
  if (typeof value === "string" && value.trim() !== "") {
    const n = Number(value);
    if (Number.isInteger(n)) return n;
  }
  return null;
}

export function isValidPort(port: number): boolean {
  return Number.isInteger(port) && port >= 1 && port <= 65535;
}

/**
 * 监听 / 目标主机。空串回退到 127.0.0.1——端口转发默认只绑本机回环，
 * 避免一不小心把远端服务暴露到局域网。
 */
export function parseHost(value: unknown, fallback = DEFAULT_HOST): string | null {
  if (value == null || value === "") return fallback;
  if (typeof value !== "string") return null;
  const host = value.trim();
  if (!host) return fallback;
  if (host.length > 253 || /\s/.test(host)) return null;
  return host;
}

export function validateForwardInput(
  input: Partial<PortForwardInput> | null | undefined
): { ok: true; value: NormalizedForward } | { ok: false; error: string } {
  if (!input || typeof input !== "object") return { ok: false, error: "缺少转发配置" };

  const kind = input.kind;
  if (kind !== "local" && kind !== "remote") {
    return { ok: false, error: "转发方向必须是 local 或 remote" };
  }

  const bindPort = parsePort(input.bindPort);
  if (bindPort == null || !isValidPort(bindPort)) {
    return { ok: false, error: "监听端口无效" };
  }
  const destPort = parsePort(input.destPort);
  if (destPort == null || !isValidPort(destPort)) {
    return { ok: false, error: "目标端口无效" };
  }

  const bindHost = parseHost(input.bindHost);
  if (!bindHost) return { ok: false, error: "监听地址无效" };
  const destHost = parseHost(input.destHost);
  if (!destHost) return { ok: false, error: "目标地址无效" };

  let name: string | undefined;
  if (input.name != null && input.name !== "") {
    if (typeof input.name !== "string") return { ok: false, error: "名称无效" };
    name = input.name.trim();
    if (!name) name = undefined;
    else if (name.length > NAME_MAX) return { ok: false, error: `名称最多 ${NAME_MAX} 个字符` };
  }

  return {
    ok: true,
    value: {
      name,
      kind,
      bindHost,
      bindPort,
      destHost,
      destPort,
      enabled: input.enabled !== false,
    },
  };
}

/** 同一条链路上，同方向同监听地址端口不能重复。 */
export function forwardBindKey(kind: ForwardKind, bindHost: string, bindPort: number): string {
  return `${kind}\0${bindHost}\0${bindPort}`;
}

export function formatForwardEndpoint(host: string, port: number): string {
  return host.includes(":") ? `[${host}]:${port}` : `${host}:${port}`;
}
