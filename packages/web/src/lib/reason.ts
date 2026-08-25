import type { NonDurableReason, WorktreeFailure } from "@falcon/shared";

type TFunc = (key: string, options?: Record<string, unknown>) => string;

/** 把 WorktreeFailure 翻成给人看的一句话。key 约定与 zellij.reason_* 一致 */
export function worktreeReasonText(t: TFunc, reason?: WorktreeFailure): string {
  if (!reason) return t("common.error");
  return t(`worktree.reason_${reason.replace(/-/g, "_")}`);
}

/** 把 NonDurableReason 翻成给人看的一句话 */
export function reasonText(t: TFunc, reason?: NonDurableReason): string {
  if (!reason) return t("zellij.reason_not_authorized");
  return t(`zellij.reason_${reason.replace(/-/g, "_")}`);
}

/** 持久性徽标的 hover 文案：先说保住了什么，再说丢了什么 */
export function durabilityHint(
  t: TFunc,
  durable: boolean,
  reason?: NonDurableReason,
  zellijVersion?: string
): string {
  if (durable) {
    return zellijVersion
      ? t("session.durableHintZellij", { version: zellijVersion })
      : t("session.durableHint");
  }
  return t("session.nonDurableHint", { reason: reasonText(t, reason) });
}
