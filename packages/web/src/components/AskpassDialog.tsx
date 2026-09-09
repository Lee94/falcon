import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { api } from "../api.js";
import { useApp } from "../store.js";
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
import { Input } from "@/components/ui/input";

/**
 * sudo askpass：agent 管道里跑 sudo 时没有 tty，密码弹在这里。
 * 人在终端里自己敲 sudo 不走这条（包装看到 stdin 是 tty 就原样透传）。
 */
export function AskpassDialog() {
  const { t } = useTranslation();
  const spec = useApp((s) => s.askpass[0] ?? null);
  const shift = useApp((s) => s.shiftAskpass);
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    setPassword("");
    setBusy(false);
  }, [spec?.id]);

  if (!spec) return null;

  const submit = async () => {
    setBusy(true);
    try {
      await api.answerAskpass(spec.id, password);
    } catch {
      // 请求已过期 / 取消过：对话框照关
    } finally {
      setBusy(false);
      shift();
    }
  };

  const cancel = async () => {
    try {
      await api.cancelAskpass(spec.id);
    } catch {
      //
    }
    shift();
  };

  return (
    <AlertDialog open>
      <AlertDialogContent
        onEscapeKeyDown={(e) => {
          e.preventDefault();
          void cancel();
        }}
        onOpenAutoFocus={(e) => {
          e.preventDefault();
          inputRef.current?.focus();
        }}
      >
        <AlertDialogHeader>
          <AlertDialogTitle>{t("askpass.title")}</AlertDialogTitle>
          <AlertDialogDescription>{spec.prompt}</AlertDialogDescription>
        </AlertDialogHeader>
        <form
          onSubmit={(e) => {
            e.preventDefault();
            if (!busy) void submit();
          }}
        >
          <Input
            ref={inputRef}
            type="password"
            autoComplete="off"
            placeholder={t("askpass.placeholder")}
            value={password}
            onChange={(e) => setPassword(e.target.value)}
          />
        </form>
        <AlertDialogFooter>
          <AlertDialogCancel
            onClick={(e) => {
              e.preventDefault();
              void cancel();
            }}
          >
            {t("common.cancel")}
          </AlertDialogCancel>
          <AlertDialogAction
            className={buttonVariants()}
            disabled={busy || !password}
            onClick={(e) => {
              e.preventDefault();
              void submit();
            }}
          >
            {t("askpass.submit")}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
