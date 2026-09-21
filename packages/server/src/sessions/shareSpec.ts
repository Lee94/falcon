import type { PublicShareInput, ShareOrigin } from "@falcon/shared";
import { isValidPort, parseHost, parsePort } from "./forwardSpec.js";

export interface NormalizedShare {
  name?: string;
  origin: ShareOrigin;
  destHost: string;
  destPort: number;
  enabled: boolean;
}

const NAME_MAX = 80;

export function validateShareInput(
  input: Partial<PublicShareInput> | null | undefined,
  projectType: "local" | "ssh"
): { ok: true; value: NormalizedShare } | { ok: false; error: string } {
  if (!input || typeof input !== "object") return { ok: false, error: "缺少发布配置" };

  const origin = input.origin;
  if (origin !== "local" && origin !== "remote") {
    return { ok: false, error: "发布目标必须是 local 或 remote" };
  }
  if (origin === "remote" && projectType !== "ssh") {
    return { ok: false, error: "只有 SSH 项目能发布远端端口" };
  }

  const destPort = parsePort(input.destPort);
  if (destPort == null || !isValidPort(destPort)) {
    return { ok: false, error: "目标端口无效" };
  }

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
      origin,
      destHost,
      destPort,
      enabled: input.enabled !== false,
    },
  };
}

export function shareDestKey(origin: ShareOrigin, destHost: string, destPort: number): string {
  return `${origin}\0${destHost}\0${destPort}`;
}
