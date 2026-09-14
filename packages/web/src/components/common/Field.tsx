import type { ReactNode } from "react";
import { Label } from "@/components/ui/label";
import { cn } from "@/lib/utils";

/** 表单一行：标签 + 控件 + 提示。提示区带 ok / err 两个语气。 */
export function Field({
  label,
  htmlFor,
  hint,
  tone,
  className,
  children,
}: {
  label?: string;
  htmlFor?: string;
  hint?: ReactNode;
  tone?: "ok" | "err";
  className?: string;
  children: ReactNode;
}) {
  return (
    <div className={cn("grid gap-1.5", className)}>
      {label && (
        <Label htmlFor={htmlFor} className="text-xs text-muted-foreground">
          {label}
        </Label>
      )}
      {children}
      {hint && (
        <p
          className={cn(
            "text-xs leading-relaxed",
            tone === "ok"
              ? "text-success"
              : tone === "err"
                ? "text-destructive"
                : "text-muted-foreground"
          )}
        >
          {hint}
        </p>
      )}
    </div>
  );
}

/**
 * 二选一切换。语义上是 radiogroup，不是 tab —— 它切的是同一份表单的填法。
 *
 * 形态是「凹槽里浮起一块」：槽用 .sunken，选中项直接用 .island，和整个界面的
 * 岛是同一套材质与曲率，只是尺度小了一号。
 */
export function Segmented<T extends string>({
  value,
  options,
  onChange,
  label,
  dense,
}: {
  value: T;
  options: { value: T; label: string }[];
  onChange: (next: T) => void;
  label?: string;
  /** 侧边栏那种窄容器里用，和同处的 h-7 输入框对齐 */
  dense?: boolean;
}) {
  return (
    <div role="radiogroup" aria-label={label} className="sunken flex gap-0.5 p-0.5">
      {options.map((option) => {
        const on = option.value === value;
        return (
          <button
            key={option.value}
            type="button"
            role="radio"
            aria-checked={on}
            className={cn(
              "flex-1 rounded-md whitespace-nowrap outline-none transition-colors focus-visible:ring-1 focus-visible:ring-ring",
              dense ? "h-6 px-2 text-xs" : "h-7 px-3 text-[13px]",
              on
                ? "island rounded-md font-medium text-foreground"
                : "text-muted-foreground hover:text-foreground"
            )}
            onClick={() => onChange(option.value)}
          >
            {option.label}
          </button>
        );
      })}
    </div>
  );
}
