import type { ReactNode } from "react";
import { StatusMark, type MarkState } from "./StatusMark.js";
import { cn } from "@/lib/utils";

const TONES = {
  info: "bg-primary/10 border-primary/25",
  warn: "bg-warning/10 border-warning/40",
  error: "bg-destructive/10 border-destructive/40",
} as const;

/**
 * Stage 顶部的持续性提示。只用于**需要用户动作**的会话级异常状态
 * （待接回 / 已丢失 / attach 失败），瞬时反馈走 toast。
 */
export function Banner({
  tone,
  state,
  title,
  body,
  actions,
}: {
  tone: keyof typeof TONES;
  state: MarkState;
  title: string;
  body?: string;
  actions?: ReactNode;
}) {
  return (
    <div className={cn("flex items-center gap-3 border-b px-3.5 py-2.5", TONES[tone])}>
      <StatusMark state={state} size="lg" />
      <span className="text-[13px]">{title}</span>
      {body && (
        <span className="min-w-0 truncate text-xs text-muted-foreground" title={body}>
          {body}
        </span>
      )}
      {actions && <span className="ml-auto flex shrink-0 gap-2">{actions}</span>}
    </div>
  );
}
