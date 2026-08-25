import { useTranslation } from "react-i18next";
import { CircleAlert, CircleDot, CircleX, LoaderCircle, type LucideIcon } from "lucide-react";
import type { SessionState } from "@falcon/shared";
import { cn } from "@/lib/utils";

/** creating 是纯前端状态：REST 还没返回，tab 已经出现了 */
export type MarkState = SessionState | "creating";

const ICONS: Record<MarkState, LucideIcon> = {
  active: CircleDot,
  unverified: CircleAlert,
  dead: CircleX,
  creating: LoaderCircle,
};

const TONES: Record<MarkState, string> = {
  active: "text-success",
  unverified: "text-warning animate-breathe",
  dead: "text-destructive",
  creating: "text-muted-foreground animate-spin",
};

export function useStateLabel() {
  const { t } = useTranslation();
  return (state: MarkState) => t(`session.state_${state}`);
}

/**
 * 状态记号：形状 + 颜色双编码。灰度截图里也能分出运行中 / 待接回 / 已丢失。
 * 呼吸动画只给 unverified —— 它承载"系统在等重连"的信息，不是装饰。
 */
export function StatusMark({
  state,
  size = "sm",
  label,
  className,
}: {
  state: MarkState;
  size?: "sm" | "lg";
  label?: string;
  className?: string;
}) {
  const stateLabel = useStateLabel();
  const Glyph = ICONS[state];
  const text = label ?? stateLabel(state);
  return (
    <span
      role="img"
      aria-label={text}
      title={text}
      className={cn("flex shrink-0", TONES[state], className)}
    >
      <Glyph className={size === "lg" ? "size-3.5" : "size-3"} strokeWidth={2} />
    </span>
  );
}
