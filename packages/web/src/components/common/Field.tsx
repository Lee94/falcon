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

/** 二选一切换。语义上是 radiogroup，不是 tab —— 它切的是同一份表单的填法。 */
export function Segmented<T extends string>({
  value,
  options,
  onChange,
  label,
}: {
  value: T;
  options: { value: T; label: string }[];
  onChange: (next: T) => void;
  label?: string;
}) {
  return (
    <div role="radiogroup" aria-label={label} className="flex overflow-hidden rounded-md border">
      {options.map((option) => {
        const on = option.value === value;
        return (
          <button
            key={option.value}
            type="button"
            role="radio"
            aria-checked={on}
            className={cn(
              "h-8 flex-1 whitespace-nowrap px-3 text-[13px] outline-none transition-colors focus-visible:ring-[3px] focus-visible:ring-ring/50 focus-visible:ring-inset",
              on
                ? "bg-primary text-primary-foreground"
                : "text-muted-foreground hover:bg-accent hover:text-accent-foreground"
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
