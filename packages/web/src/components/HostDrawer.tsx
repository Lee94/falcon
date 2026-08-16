import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import type { HostZellijStatus } from "@mojito/shared";
import { api } from "../api.js";
import { useApp } from "../store.js";
import { connLabel, sshBar } from "../lib/hostColor.js";
import { reasonText } from "../lib/reason.js";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetFooter,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";

interface Fact {
  k: string;
  v: string;
  tone?: "ok" | "warn" | "bad" | "dim";
  mono?: boolean;
}

const TONES = {
  ok: "text-success",
  warn: "text-warning",
  bad: "text-destructive",
  dim: "text-muted-foreground",
} as const;

/**
 * 主机级「持久会话设置」。
 *
 * 这是曾经点过「不安装」之后唯一的回头路：`authorized: false` 会让创建会话时的
 * 弹窗永不再现，没有这个抽屉，那台主机就只能永远开非持久会话。
 */
export function HostDrawer() {
  const { t } = useTranslation();
  const projectId = useApp((s) => s.drawerProjectId);
  const project = useApp((s) => s.projects.find((p) => p.id === projectId));
  const system = useApp((s) => s.system);
  const failure = useApp((s) => (projectId ? s.installFailures[projectId] : undefined));
  const close = useApp((s) => s.closeDrawer);
  const openInstall = useApp((s) => s.openInstall);
  const toast = useApp((s) => s.toast);

  const [status, setStatus] = useState<HostZellijStatus | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setStatus(null);
    setError(null);
    if (!projectId || project?.type !== "ssh") return;
    let cancelled = false;
    void api
      .hostStatus(projectId)
      .then((s) => !cancelled && setStatus(s))
      .catch((err) => !cancelled && setError((err as Error).message));
    return () => {
      cancelled = true;
    };
  }, [projectId, project?.type]);

  if (!projectId || !project) return null;

  const isSsh = project.type === "ssh";
  const ready = Boolean(status?.authorized === true && status.installedVersion);

  const facts: Fact[] = [];
  if (isSsh) {
    facts.push({
      k: t("drawer.authState"),
      v:
        status?.authorized === true
          ? t("drawer.authYes")
          : status?.authorized === false
            ? t("drawer.authNo")
            : t("drawer.authUnknown"),
      tone: status?.authorized === true ? "ok" : "warn",
    });
    facts.push({
      k: t("drawer.zellij"),
      v: status?.installedVersion ?? t("drawer.notInstalled"),
      mono: true,
      tone: status?.installedVersion ? undefined : "dim",
    });
    facts.push({
      k: t("drawer.baseUrl"),
      v: status?.baseUrl || status?.defaultBaseUrl || "",
      mono: true,
      tone: "dim",
    });
    if (status && status.verifiedDurable !== null) {
      facts.push({
        k: t("drawer.verified"),
        v: status.verifiedDurable ? t("drawer.verifiedOk") : t("drawer.verifiedNo"),
        tone: status.verifiedDurable ? "ok" : "warn",
      });
    }
    facts.push({
      k: t("drawer.lastFailure"),
      v: failure
        ? `${reasonText(t, failure.reason)} · ${t("zellij.autoRetried", { n: failure.attempts })}`
        : t("drawer.none"),
      tone: failure ? "bad" : "dim",
    });
  } else {
    facts.push({ k: t("drawer.platform"), v: system?.platform ?? "", mono: true });
    facts.push({
      k: t("drawer.durability"),
      v:
        system?.localDurable === true
          ? t("session.durable")
          : system?.localDurable === false
            ? t("session.nonDurable")
            : t("drawer.none"),
      tone: system?.localDurable === true ? "ok" : "warn",
    });
    if (system?.localDurableReason) {
      facts.push({
        k: t("drawer.lastFailure"),
        v: reasonText(t, system.localDurableReason),
        tone: "bad",
      });
    }
  }

  const revoke = async () => {
    try {
      await api.setHostAuthorization(projectId, { authorized: false });
      close();
      toast({
        kind: "warning",
        title: t("drawer.revoked"),
        body: t("drawer.revokedBody"),
      });
    } catch (err) {
      toast({ kind: "danger", title: t("toast.failed"), body: (err as Error).message });
    }
  };

  return (
    <Sheet
      open
      onOpenChange={(next) => {
        if (!next) close();
      }}
    >
      <SheetContent
        side="right"
        className="w-100 gap-0 sm:max-w-none"
        onEscapeKeyDown={(e) => e.preventDefault()}
      >
        <SheetHeader className="flex-row items-center gap-2.5 border-b">
          <span
            className="h-4 w-[3px] shrink-0 rounded-full"
            style={{ background: sshBar(project) ?? "var(--border)" }}
          />
          <SheetTitle className="text-[15px]">{t("drawer.title")}</SheetTitle>
          <SheetDescription className="sr-only">
            {isSsh ? t("drawer.hostScope") : t("drawer.noteLocal")}
          </SheetDescription>
        </SheetHeader>

        <div className="flex-1 overflow-y-auto p-4">
          <div className="font-mono text-[13px] break-all">
            {connLabel(project, system, t("project.typeLocalShort"))}
          </div>
          <div className="mb-4.5 text-xs text-muted-foreground">
            {isSsh ? t("drawer.hostScope") : t("drawer.noteLocal")}
          </div>

          {error && (
            <p className="mb-3 text-[13px] text-destructive">
              {t("drawer.loadFailed")}：{error}
            </p>
          )}

          {facts.map((fact) => (
            <div key={fact.k} className="flex gap-3 border-b py-2">
              <span className="w-24 shrink-0 text-xs text-muted-foreground">{fact.k}</span>
              <span
                className={cn(
                  "min-w-0 flex-1 text-[12.5px] break-all",
                  fact.tone && TONES[fact.tone],
                  fact.mono && "font-mono"
                )}
                title={fact.v}
              >
                {fact.v}
              </span>
            </div>
          ))}

          {isSsh && (
            <p className="mt-4 text-[12.5px] leading-relaxed text-muted-foreground">
              {ready ? t("drawer.noteReady") : t("drawer.noteNotReady")}
            </p>
          )}
        </div>

        <SheetFooter className="flex-row gap-2 border-t">
          {isSsh ? (
            <>
              <Button
                className="flex-1"
                onClick={() => openInstall({ projectId, thenCreate: false })}
              >
                {ready ? t("drawer.recheck") : t("drawer.install")}
              </Button>
              {ready ? (
                <Button variant="secondary" onClick={() => void revoke()}>
                  {t("drawer.revoke")}
                </Button>
              ) : (
                <Button
                  variant="secondary"
                  onClick={() => openInstall({ projectId, thenCreate: false })}
                >
                  {t("drawer.recheck")}
                </Button>
              )}
            </>
          ) : (
            <Button variant="secondary" className="flex-1" onClick={close}>
              {t("common.close")}
            </Button>
          )}
        </SheetFooter>
      </SheetContent>
    </Sheet>
  );
}
