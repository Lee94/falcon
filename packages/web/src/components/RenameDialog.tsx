import { useState } from "react";
import { useTranslation } from "react-i18next";
import { api } from "../api.js";
import { useApp } from "../store.js";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { AppDialog } from "./common/AppDialog.js";
import { Field } from "./common/Field.js";

/** 会话重命名。替换掉 prompt()——界面上不该再出现浏览器原生弹窗。 */
export function RenameDialog() {
  const { t } = useTranslation();
  const sessionId = useApp((s) => s.renameFor);
  const session = useApp((s) => s.sessions.find((x) => x.id === s.renameFor));
  const close = useApp((s) => s.closeRename);
  const refreshSessions = useApp((s) => s.refreshSessions);
  const toast = useApp((s) => s.toast);
  const [draft, setDraft] = useState(session?.name ?? "");
  const [busy, setBusy] = useState(false);

  if (!sessionId || !session) return null;

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    const name = draft.trim();
    if (!name) return;
    setBusy(true);
    try {
      await api.renameSession(sessionId, name);
      toast({ kind: "info", title: t("toast.renamed", { name }) });
      close();
    } catch (err) {
      toast({ kind: "danger", title: t("toast.failed"), body: (err as Error).message });
    } finally {
      setBusy(false);
      await refreshSessions();
    }
  };

  return (
    <AppDialog title={t("session.renameTitle")} onClose={close} lockOverlay>
      <form onSubmit={submit} className="grid gap-3">
        <Field label={t("session.renameLabel")} htmlFor="rename-input">
          <Input
            id="rename-input"
            data-autofocus
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
          />
        </Field>
        <div className="mt-1 flex justify-end gap-2">
          <Button variant="outline" type="button" onClick={close}>
            {t("common.cancel")}
          </Button>
          <Button type="submit" disabled={busy || !draft.trim()}>
            {t("project.save")}
          </Button>
        </div>
      </form>
    </AppDialog>
  );
}
