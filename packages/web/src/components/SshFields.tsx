import { useTranslation } from "react-i18next";
import type { SshAuthMethod } from "@falcon/shared";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Field } from "./common/Field.js";

export interface SshFieldValues {
  host: string;
  port: number;
  username: string;
  authMethod: SshAuthMethod;
  keyPath: string;
  secret: string;
}

/** 主机表单和存量项目手写 SSH 共用的连接字段。 */
export function SshFields({
  value,
  onChange,
  hasSecret,
}: {
  value: SshFieldValues;
  onChange: (next: SshFieldValues) => void;
  hasSecret?: boolean;
}) {
  const { t } = useTranslation();
  const patch = (partial: Partial<SshFieldValues>) => onChange({ ...value, ...partial });
  const secretLabel =
    value.authMethod === "key" ? t("project.passphrase") : t("project.sshPassword");

  return (
    <>
      <div className="flex gap-2.5">
        <Field label={t("project.host")} htmlFor="ssh-host" className="flex-1">
          <Input
            id="ssh-host"
            className="font-mono"
            value={value.host}
            onChange={(e) => patch({ host: e.target.value })}
          />
        </Field>
        <Field label={t("project.port")} htmlFor="ssh-port" className="w-22">
          <Input
            id="ssh-port"
            className="font-mono"
            type="number"
            value={value.port}
            onChange={(e) => patch({ port: Number(e.target.value) })}
          />
        </Field>
      </div>
      <Field label={t("project.username")} htmlFor="ssh-user">
        <Input
          id="ssh-user"
          className="font-mono"
          value={value.username}
          onChange={(e) => patch({ username: e.target.value })}
        />
      </Field>
      <Field label={t("project.authMethod")}>
        <Select
          value={value.authMethod}
          onValueChange={(v) => patch({ authMethod: v as SshAuthMethod })}
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
      {value.authMethod === "key" && (
        <Field label={t("project.keyPath")} htmlFor="ssh-key">
          <Input
            id="ssh-key"
            className="font-mono"
            value={value.keyPath}
            onChange={(e) => patch({ keyPath: e.target.value })}
            placeholder="~/.ssh/id_ed25519"
          />
        </Field>
      )}
      {value.authMethod !== "agent" && (
        <Field
          label={secretLabel}
          htmlFor="ssh-secret"
          hint={hasSecret ? t("project.secretKept") : undefined}
        >
          <Input
            id="ssh-secret"
            type="password"
            value={value.secret}
            onChange={(e) => patch({ secret: e.target.value })}
          />
        </Field>
      )}
    </>
  );
}
