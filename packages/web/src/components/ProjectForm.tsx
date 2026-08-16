import { useState } from "react";
import { useTranslation } from "react-i18next";
import type { Project, ProjectInput, ProjectType, SshAuthMethod } from "@mojito/shared";
import { api } from "../api.js";
import { useApp } from "../store.js";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { AppDialog } from "./common/AppDialog.js";
import { Field, Segmented } from "./common/Field.js";

export function ProjectForm({
  existing,
  onClose,
}: {
  existing: Project | null;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const refreshProjects = useApp((s) => s.refreshProjects);

  const [type, setType] = useState<ProjectType>(existing?.type ?? "local");
  const [name, setName] = useState(existing?.name ?? "");
  const [workingDir, setWorkingDir] = useState(existing?.workingDir ?? "");
  const [shell, setShell] = useState(existing?.shell ?? "");
  const [host, setHost] = useState(existing?.ssh?.host ?? "");
  const [port, setPort] = useState(existing?.ssh?.port ?? 22);
  const [username, setUsername] = useState(existing?.ssh?.username ?? "");
  const [authMethod, setAuthMethod] = useState<SshAuthMethod>(
    existing?.ssh?.authMethod ?? "key"
  );
  const [keyPath, setKeyPath] = useState(existing?.ssh?.keyPath ?? "");
  const [secret, setSecret] = useState("");
  const [pathHint, setPathHint] = useState<{ ok: boolean; text: string } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const validatePath = async () => {
    try {
      const res = await api.validatePath(workingDir);
      setPathHint(
        res.ok
          ? { ok: true, text: t("project.pathOk") }
          : { ok: false, text: res.error ?? "?" }
      );
    } catch (err) {
      setPathHint({ ok: false, text: (err as Error).message });
    }
  };

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    const input: ProjectInput = {
      name,
      type,
      workingDir: workingDir || undefined,
      shell: shell || undefined,
      ssh:
        type === "ssh"
          ? {
              host,
              port,
              username,
              authMethod,
              keyPath: authMethod === "key" ? keyPath : undefined,
              secret: secret || undefined,
            }
          : undefined,
    };
    try {
      if (existing) {
        await api.updateProject(existing.id, input);
      } else {
        await api.createProject(input);
      }
      await refreshProjects();
      onClose();
    } catch (err) {
      setError((err as Error).message);
    } finally {
      setBusy(false);
    }
  };

  const secretLabel =
    authMethod === "key" ? t("project.passphrase") : t("project.sshPassword");
  const title = existing ? t("project.editTitle") : t("project.createTitle");

  return (
    <AppDialog title={title} onClose={onClose} lockOverlay>
      <form onSubmit={submit} className="grid gap-3">
        {!existing && (
          <Segmented
            value={type}
            label={t("project.name")}
            onChange={setType}
            options={[
              { value: "local", label: t("project.typeLocal") },
              { value: "ssh", label: t("project.typeSsh") },
            ]}
          />
        )}

        <Field label={t("project.name")} htmlFor="project-name">
          <Input
            id="project-name"
            value={name}
            data-autofocus
            onChange={(e) => setName(e.target.value)}
          />
        </Field>

        {type === "local" ? (
          <Field
            label={t("project.workingDir")}
            htmlFor="project-dir"
            hint={pathHint?.text}
            tone={pathHint ? (pathHint.ok ? "ok" : "err") : undefined}
          >
            <div className="flex gap-2.5">
              <Input
                id="project-dir"
                className="font-mono"
                value={workingDir}
                onChange={(e) => {
                  setWorkingDir(e.target.value);
                  setPathHint(null);
                }}
                placeholder="D:\code\my-project"
              />
              <Button
                variant="outline"
                type="button"
                onClick={validatePath}
                disabled={!workingDir}
              >
                {t("project.validate")}
              </Button>
            </div>
          </Field>
        ) : (
          <>
            <div className="flex gap-2.5">
              <Field label={t("project.host")} htmlFor="ssh-host" className="flex-1">
                <Input
                  id="ssh-host"
                  className="font-mono"
                  value={host}
                  onChange={(e) => setHost(e.target.value)}
                />
              </Field>
              <Field label={t("project.port")} htmlFor="ssh-port" className="w-22">
                <Input
                  id="ssh-port"
                  className="font-mono"
                  type="number"
                  value={port}
                  onChange={(e) => setPort(Number(e.target.value))}
                />
              </Field>
            </div>
            <Field label={t("project.username")} htmlFor="ssh-user">
              <Input
                id="ssh-user"
                className="font-mono"
                value={username}
                onChange={(e) => setUsername(e.target.value)}
              />
            </Field>
            <Field label={t("project.authMethod")}>
              <Select
                value={authMethod}
                onValueChange={(v) => setAuthMethod(v as SshAuthMethod)}
              >
                <SelectTrigger className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="key">{t("project.authKey")}</SelectItem>
                  <SelectItem value="password">{t("project.authPassword")}</SelectItem>
                  <SelectItem value="agent">{t("project.authAgent")}</SelectItem>
                </SelectContent>
              </Select>
            </Field>
            {authMethod === "key" && (
              <Field label={t("project.keyPath")} htmlFor="ssh-key">
                <Input
                  id="ssh-key"
                  className="font-mono"
                  value={keyPath}
                  onChange={(e) => setKeyPath(e.target.value)}
                  placeholder="~/.ssh/id_ed25519"
                />
              </Field>
            )}
            {authMethod !== "agent" && (
              <Field
                label={secretLabel}
                htmlFor="ssh-secret"
                hint={existing?.ssh?.hasSecret ? t("project.secretKept") : undefined}
              >
                <Input
                  id="ssh-secret"
                  type="password"
                  value={secret}
                  onChange={(e) => setSecret(e.target.value)}
                />
              </Field>
            )}
            <Field label={t("project.remoteDir")} htmlFor="ssh-dir">
              <Input
                id="ssh-dir"
                className="font-mono"
                value={workingDir}
                onChange={(e) => setWorkingDir(e.target.value)}
                placeholder="/home/user/project"
              />
            </Field>
          </>
        )}

        <Field label={t("project.shell")} htmlFor="project-shell">
          <Input
            id="project-shell"
            className="font-mono"
            value={shell}
            onChange={(e) => setShell(e.target.value)}
            placeholder={type === "local" ? "pwsh.exe / zsh" : "zsh"}
          />
        </Field>

        {error && <p className="text-[13px] text-destructive">{error}</p>}

        <div className="mt-1 flex justify-end gap-2">
          <Button variant="outline" type="button" onClick={onClose}>
            {t("common.cancel")}
          </Button>
          <Button type="submit" disabled={busy || !name}>
            {existing ? t("project.save") : t("project.create")}
          </Button>
        </div>
      </form>
    </AppDialog>
  );
}
