import type { PublicShareInput } from "@falcon/shared";
import { isValidPort, parseHost, parsePort } from "./forwardSpec.js";

/** 挂在哪台机器上不在这里校验：那是创建时一次性的事，由 ShareManager 查主机表 */
export interface NormalizedShare {
  name?: string;
  destHost: string;
  destPort: number;
  enabled: boolean;
}

const NAME_MAX = 80;

export function validateShareInput(
  input: Partial<PublicShareInput> | null | undefined
): { ok: true; value: NormalizedShare } | { ok: false; error: string } {
  if (!input || typeof input !== "object") return { ok: false, error: "缺少发布配置" };

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
      destHost,
      destPort,
      enabled: input.enabled !== false,
    },
  };
}
