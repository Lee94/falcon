import { useState, type ReactNode } from "react";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { cn } from "@/lib/utils";

/**
 * 对话框基座。焦点陷阱、还原焦点、role/aria-modal 全由 Radix 负责，这里只补两条
 * 产品级取舍：
 *
 * 1. **Esc 不在这里处理**（onEscapeKeyDown 一律 preventDefault）。全局 Esc 由 App
 *    统一分发，保证多层浮层时只关最上面那一层；让 Radix 也关一次会连关两层。
 * 2. lockOverlay：表单类对话框点遮罩**不关闭**，改为轻微抖动。填了一半 SSH 配置
 *    手滑点到遮罩就全丢，是不可接受的代价。
 */
export function AppDialog({
  title,
  description,
  children,
  footer,
  onClose,
  wide,
  lockOverlay,
  hideTitle,
  className,
}: {
  title: string;
  description?: ReactNode;
  children?: ReactNode;
  footer?: ReactNode;
  onClose: () => void;
  wide?: boolean;
  lockOverlay?: boolean;
  /** 标题只留给读屏器（对话框自己在正文里另有标题时用） */
  hideTitle?: boolean;
  className?: string;
}) {
  const [shake, setShake] = useState(false);

  const bump = () => {
    setShake(true);
    setTimeout(() => setShake(false), 220);
  };

  return (
    <Dialog
      open
      onOpenChange={(next) => {
        if (!next) onClose();
      }}
    >
      <DialogContent
        showCloseButton={!lockOverlay}
        aria-describedby={description ? undefined : ""}
        className={cn(
          "max-h-[calc(100vh-5rem)] gap-4 overflow-y-auto",
          wide ? "sm:max-w-xl" : "sm:max-w-md",
          shake && "animate-shake",
          className
        )}
        onEscapeKeyDown={(e) => e.preventDefault()}
        onOpenAutoFocus={(e) => {
          // 打开时把焦点放在 [data-autofocus] 上；没有就交回给 Radix 的默认行为。
          // 这里取 currentTarget 而不是 ref：shadcn 的包装组件是按 React 19 的
          // 「ref 即 props」写的，在 React 18 下给它们传 ref 会静默失效。
          const node = e.currentTarget as HTMLElement | null;
          const wanted = node?.querySelector<HTMLElement>("[data-autofocus]");
          if (!wanted) return;
          e.preventDefault();
          wanted.focus();
        }}
        onPointerDownOutside={(e) => {
          if (!lockOverlay) return;
          e.preventDefault();
          bump();
        }}
        onInteractOutside={(e) => {
          if (lockOverlay) e.preventDefault();
        }}
      >
        <DialogHeader className={hideTitle ? "sr-only" : undefined}>
          <DialogTitle>{title}</DialogTitle>
          {description && <DialogDescription>{description}</DialogDescription>}
        </DialogHeader>
        {children}
        {footer && (
          <div className="flex items-center justify-end gap-2 pt-1">{footer}</div>
        )}
      </DialogContent>
    </Dialog>
  );
}
