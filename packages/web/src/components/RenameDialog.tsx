import { useState } from "react";
import { useTranslation } from "react-i18next";
import { api } from "../api.js";
import { useApp } from "../store.js";
import { useSessionLabel } from "../lib/useSessionLabel.js";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { AppDialog } from "./common/AppDialog.js";
import { Field } from "./common/Field.js";

/**
 * 会话重命名。替换掉 prompt()——界面上不该再出现浏览器原生弹窗。
 *
 * 提交空串是合法操作，不是"没填"：会话默认没有名字，显示的是自动标题
 * （见 lib/sessionTitle.ts），清掉名字就是回到那个状态，这里是唯一的出口。
 */
export function RenameDialog() {
  const { t } = useTranslation();
  const sessionId = useApp((s) => s.renameFor);
  const session = useApp((s) => s.sessions.find((x) => x.id === s.renameFor));
  const close = useApp((s) => s.closeRename);
  const refreshSessions = useApp((s) => s.refreshSessions);
  const toast = useApp((s) => s.toast);
  const sessionLabel = useSessionLabel();
  const [draft, setDraft] = useState(session?.name ?? "");
  const [busy, setBusy] = useState(false);

  if (!sessionId || !session) return null;

  // 输入框空着时替用户显示的那行灰字：清掉名字之后这个会话会叫什么
  const auto = sessionLabel({ ...session, name: "" });

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    const name = draft.trim();
    setBusy(true);
    try {
      await api.renameSession(sessionId, name);
      toast(
        name
          ? { kind: "info", title: t("toast.renamed", { name }) }
          : { kind: "info", title: t("toast.renameCleared") }
      );
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
        <Field
          label={t("session.renameLabel")}
          htmlFor="rename-input"
          hint={t("session.renameHint", { auto })}
        >
          <Input
            id="rename-input"
            data-autofocus
            placeholder={auto}
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
          />
        </Field>
        <div className="mt-1 flex justify-end gap-2">
          <Button variant="outline" type="button" onClick={close}>
            {t("common.cancel")}
          </Button>
          <Button type="submit" disabled={busy}>
            {t("project.save")}
          </Button>
        </div>
      </form>
    </AppDialog>
  );
}
