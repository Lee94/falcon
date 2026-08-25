import { useState } from "react";
import { useTranslation } from "react-i18next";
import type { SshHost, SshHostInput } from "@falcon/shared";
import { api } from "../api.js";
import { useApp } from "../store.js";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { AppDialog } from "./common/AppDialog.js";
import { Field } from "./common/Field.js";
import { SshFields, type SshFieldValues } from "./SshFields.js";

export function HostForm({
  existing,
  onSaved,
  onClose,
}: {
  existing: SshHost | null;
  onSaved?: (host: SshHost) => void;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const refreshHosts = useApp((s) => s.refreshHosts);
  const refreshProjects = useApp((s) => s.refreshProjects);
  const toast = useApp((s) => s.toast);

  const [name, setName] = useState(existing?.name ?? "");
  const [ssh, setSsh] = useState<SshFieldValues>({
    host: existing?.host ?? "",
    port: existing?.port ?? 22,
    username: existing?.username ?? "",
    authMethod: existing?.authMethod ?? "key",
    keyPath: existing?.keyPath ?? "",
    secret: "",
  });
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [probe, setProbe] = useState<{ ok: boolean; text: string } | null>(null);
  const [probing, setProbing] = useState(false);

  const testConn = async () => {
    setProbing(true);
    setProbe(null);
    setError(null);
    try {
      const res = await api.testHostDraft({
        name: name.trim() || "test",
        host: ssh.host,
        port: ssh.port,
        username: ssh.username,
        authMethod: ssh.authMethod,
        keyPath: ssh.authMethod === "key" ? ssh.keyPath : undefined,
        secret: ssh.secret || undefined,
        hostId: existing?.id,
      });
      setProbe(
        res.ok
          ? {
              ok: true,
              text: `${t("host.testOk")} · ${t(`host.testKind_${res.kind}`)} · ${res.home}`,
            }
          : { ok: false, text: res.error }
      );
    } catch (err) {
      setProbe({ ok: false, text: (err as Error).message });
    } finally {
      setProbing(false);
    }
  };

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    const input: SshHostInput = {
      name,
      host: ssh.host,
      port: ssh.port,
      username: ssh.username,
      authMethod: ssh.authMethod,
      keyPath: ssh.authMethod === "key" ? ssh.keyPath : undefined,
      secret: ssh.secret || undefined,
    };
    try {
      const saved = existing
        ? await api.updateHost(existing.id, input)
        : await api.createHost(input);
      await refreshHosts();
      if (existing) await refreshProjects();
      toast({
        kind: "success",
        title: existing
          ? t("host.saved", { name: saved.name })
          : t("host.created", { name: saved.name }),
      });
      onSaved?.(saved);
      onClose();
    } catch (err) {
      setError((err as Error).message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <AppDialog
      title={existing ? t("host.editTitle") : t("host.createTitle")}
      onClose={onClose}
      lockOverlay
    >
      <form onSubmit={submit} className="grid gap-3">
        <Field label={t("host.name")} htmlFor="host-name">
          <Input
            id="host-name"
            value={name}
            data-autofocus
            onChange={(e) => setName(e.target.value)}
            placeholder={t("host.namePlaceholder")}
          />
        </Field>
        <SshFields
          value={ssh}
          onChange={(next) => {
            setSsh(next);
            setProbe(null);
          }}
          hasSecret={existing?.hasSecret}
        />
        {error && <p className="text-[13px] text-destructive">{error}</p>}
        {probe && (
          <p className={probe.ok ? "text-[13px] text-success" : "text-[13px] text-destructive"}>
            {probe.text}
          </p>
        )}
        <div className="mt-1 flex items-center gap-2">
          <Button
            variant="outline"
            type="button"
            disabled={probing || !ssh.host.trim() || !ssh.username.trim()}
            onClick={() => void testConn()}
          >
            {probing ? t("host.testing") : t("host.test")}
          </Button>
          <span className="flex-1" />
          <Button variant="outline" type="button" onClick={onClose}>
            {t("common.cancel")}
          </Button>
          <Button type="submit" disabled={busy || !name.trim()}>
            {existing ? t("host.save") : t("host.create")}
          </Button>
        </div>
      </form>
    </AppDialog>
  );
}
