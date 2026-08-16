import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useApp } from "@/store.js";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { buttonVariants } from "@/components/ui/button";
import { StatusMark } from "./StatusMark.js";

/**
 * 破坏性操作确认。
 *
 * 标题是动作 + 对象，正文说明具体后果，删除项目时列出会被一起终止的会话。
 * **默认焦点在取消上**（Radix AlertDialog 的默认行为）——Enter 是取消，
 * 要确认得显式 Tab 过去或点一下。
 */
export function ConfirmDialog() {
  const { t } = useTranslation();
  const spec = useApp((s) => s.confirm);
  const close = useApp((s) => s.closeConfirm);
  const [busy, setBusy] = useState(false);

  if (!spec) return null;

  const run = async () => {
    setBusy(true);
    try {
      await spec.onConfirm();
    } finally {
      setBusy(false);
      close();
    }
  };

  return (
    <AlertDialog
      open
      onOpenChange={(next) => {
        if (!next) close();
      }}
    >
      <AlertDialogContent
        className="max-h-[calc(100vh-5rem)] overflow-y-auto"
        onEscapeKeyDown={(e) => e.preventDefault()}
      >
        <AlertDialogHeader>
          <AlertDialogTitle>{spec.title}</AlertDialogTitle>
          <AlertDialogDescription>{spec.body}</AlertDialogDescription>
        </AlertDialogHeader>

        {spec.list && spec.list.length > 0 && (
          <div className="rounded-md border bg-background/60 px-3 py-2.5">
            {spec.list.map((item, i) => (
              <div
                key={`${item.name}-${i}`}
                className="flex h-6 items-center gap-2.5 text-xs"
              >
                {item.state && <StatusMark state={item.state} />}
                <span className="w-40 truncate">{item.name}</span>
                {item.meta && (
                  <span className="text-muted-foreground">{item.meta}</span>
                )}
              </div>
            ))}
          </div>
        )}

        {spec.footnote && (
          <p className="text-xs leading-relaxed text-muted-foreground">{spec.footnote}</p>
        )}

        <AlertDialogFooter>
          <AlertDialogCancel>{t("common.cancel")}</AlertDialogCancel>
          <AlertDialogAction
            className={buttonVariants({ variant: "destructive" })}
            disabled={busy}
            onClick={(e) => {
              // 关闭时机由 run() 自己决定，别让 Radix 抢在请求发出前拆掉对话框
              e.preventDefault();
              void run();
            }}
          >
            {spec.confirmLabel}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
