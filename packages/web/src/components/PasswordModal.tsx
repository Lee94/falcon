import { useState } from "react";
import { useTranslation } from "react-i18next";
import { api } from "../api.js";
import { useApp } from "../store.js";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { AppDialog } from "./common/AppDialog.js";
import { Field } from "./common/Field.js";

export function PasswordModal({ onClose }: { onClose: () => void }) {
  const { t } = useTranslation();
  const auth = useApp((s) => s.auth);
  const refreshAuth = useApp((s) => s.refreshAuth);
  const [current, setCurrent] = useState("");
  const [next, setNext] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await api.setPassword(next, current || undefined);
      await refreshAuth();
      onClose();
    } catch (err) {
      setError((err as Error).message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <AppDialog title={t("password.title")} onClose={onClose} lockOverlay>
      <form onSubmit={submit} className="grid gap-3">
        {auth?.passwordSet && (
          <Field label={t("password.current")} htmlFor="pw-current">
            <Input
              id="pw-current"
              type="password"
              value={current}
              onChange={(e) => setCurrent(e.target.value)}
            />
          </Field>
        )}
        <Field label={t("password.next")} htmlFor="pw-next" hint={t("password.hint")}>
          <Input
            id="pw-next"
            type="password"
            value={next}
            data-autofocus
            onChange={(e) => setNext(e.target.value)}
          />
        </Field>
        {error && <p className="text-[13px] text-destructive">{error}</p>}
        <div className="mt-1 flex justify-end gap-2">
          <Button variant="outline" type="button" onClick={onClose}>
            {t("common.cancel")}
          </Button>
          <Button type="submit" disabled={busy || next.length < 6}>
            {t("password.submit")}
          </Button>
        </div>
      </form>
    </AppDialog>
  );
}
