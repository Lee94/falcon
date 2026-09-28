import { useState } from "react";
import { useTranslation } from "react-i18next";
import { api } from "../api.js";
import { loginNext } from "../lib/loginNext.js";
import { useApp } from "../store.js";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Field } from "./common/Field.js";

export function Login() {
  const { t } = useTranslation();
  const [password, setPassword] = useState("");
  const [failed, setFailed] = useState(false);
  const [busy, setBusy] = useState(false);
  const init = useApp((s) => s.init);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setFailed(false);
    try {
      await api.login(password);
      // 从 px0 入口被送来登录的（多半是原生客户端交给系统浏览器的链接）：直接回去
      const next = loginNext(window.location.search);
      if (next) {
        window.location.replace(next);
        return;
      }
      await init();
    } catch {
      setFailed(true);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="flex h-full items-center justify-center bg-app">
      <Card className="w-85">
        <CardHeader>
          <CardTitle className="text-lg">{t("login.title")}</CardTitle>
        </CardHeader>
        <CardContent>
          <form onSubmit={submit} className="grid gap-3">
            <Field label={t("login.password")} htmlFor="login-password">
              <Input
                id="login-password"
                type="password"
                value={password}
                autoFocus
                onChange={(e) => setPassword(e.target.value)}
              />
            </Field>
            {failed && <p className="text-[13px] text-destructive">{t("login.failed")}</p>}
            <Button type="submit" className="mt-1" disabled={busy || !password}>
              {t("login.submit")}
            </Button>
          </form>
        </CardContent>
      </Card>
    </div>
  );
}
