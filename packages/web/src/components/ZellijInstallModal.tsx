import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { ChevronDown, ChevronRight, TriangleAlert } from "lucide-react";
import { canRetryInstall } from "@falcon/shared";
import type {
  HostZellijStatus,
  InstallServerMessage,
  NonDurableReason,
  ZellijInstallStage,
} from "@falcon/shared";
import { api } from "../api.js";
import { useApp } from "../store.js";
import { reasonText } from "../lib/reason.js";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { AppDialog } from "./common/AppDialog.js";
import { Field } from "./common/Field.js";

type Phase = "loading" | "ask" | "installing" | "failed";

/** Zellij 官方发行的 target 三元组，与后端 version.ts 保持一致 */
const TARGETS = [
  "x86_64-unknown-linux-musl",
  "aarch64-unknown-linux-musl",
  "x86_64-apple-darwin",
  "aarch64-apple-darwin",
  "x86_64-pc-windows-msvc",
] as const;

/** 只有这几种失败换个下载源才有用；其余的显示输入框只会误导 */
const URL_FIXABLE: NonDurableReason[] = [
  "download-failed",
  "extract-failed",
  "probe-failed",
];

/**
 * 主机级持久会话的授权 + 安装。
 *
 * 授权是"允许 falcon 往这台服务器写可执行文件"的决定，该问；安装是随后的执行
 * 过程，只报进度。授权按主机记，同一台机器只问一次。
 *
 * 失败态里能直接改下载地址再重试——后端刚刚已经用旧地址自动试过 3 次了，
 * 不给改地址的话那个「重试」按钮几乎注定再失败一轮。
 */
export function ZellijInstallModal() {
  const { t } = useTranslation();
  const spec = useApp((s) => s.install);
  const project = useApp((s) => s.projects.find((p) => p.id === spec?.projectId));
  const sessions = useApp((s) => s.sessions);
  const closeInstall = useApp((s) => s.closeInstall);
  const createSessionNow = useApp((s) => s.createSessionNow);
  const newTerminal = useApp((s) => s.newTerminal);
  const toast = useApp((s) => s.toast);

  const [phase, setPhase] = useState<Phase>("loading");
  const [status, setStatus] = useState<HostZellijStatus | null>(null);
  const [baseUrl, setBaseUrl] = useState("");
  const [savedUrl, setSavedUrl] = useState("");
  const [stage, setStage] = useState<ZellijInstallStage>("probing");
  const [commands, setCommands] = useState<string[]>([]);
  const [failure, setFailure] = useState<NonDurableReason | undefined>();
  const [detail, setDetail] = useState<string | undefined>();
  /** 后端本轮自动重试到第几次；>1 时说明"还在转"是因为在重试，而不是卡住了 */
  const [attempt, setAttempt] = useState(1);
  const [showDetail, setShowDetail] = useState(false);
  const [showManual, setShowManual] = useState(false);
  const [copied, setCopied] = useState(false);
  const [target, setTarget] = useState<(typeof TARGETS)[number]>(TARGETS[0]);
  const wsRef = useRef<WebSocket | null>(null);
  const projectId = spec?.projectId;

  useEffect(() => {
    if (!projectId) return;
    let cancelled = false;
    setPhase("loading");
    setShowDetail(false);
    setShowManual(false);
    void api
      .hostStatus(projectId)
      .then((s) => {
        if (cancelled) return;
        setStatus(s);
        setBaseUrl(s.baseUrl ?? "");
        setSavedUrl(s.baseUrl ?? "");
        // 已授权就直接进安装，不再打扰用户
        if (s.authorized === true) startInstall(projectId);
        else setPhase("ask");
      })
      .catch(() => {
        if (!cancelled) skip("verify-failed");
      });
    return () => {
      cancelled = true;
      wsRef.current?.close();
      wsRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [projectId]);

  if (!spec || !project) return null;
  const host = project.ssh?.host ?? project.name;

  /** 装完/放弃之后回到用户本来想干的事：从「新建终端」进来的就继续建会话 */
  function finish(create: boolean) {
    const id = spec!.projectId;
    closeInstall();
    if (create && spec!.thenCreate) void createSessionNow(id);
  }

  function skip(_reason?: NonDurableReason) {
    finish(true);
  }

  function startInstall(id: string) {
    wsRef.current?.close();
    setPhase("installing");
    setStage("probing");
    setAttempt(1);
    setFailure(undefined);
    setDetail(undefined);
    setCommands([]);
    const proto = location.protocol === "https:" ? "wss" : "ws";
    const ws = new WebSocket(`${proto}://${location.host}/ws/install/${id}`);
    wsRef.current = ws;
    // 后端报过结果之后再来的 error 事件只是通道收尾，不能拿它盖掉真正的失败原因
    let settled = false;
    ws.onmessage = (ev) => {
      let msg: InstallServerMessage;
      try {
        msg = JSON.parse(ev.data as string);
      } catch {
        return;
      }
      if (msg.type === "stage") {
        setStage(msg.stage);
        setAttempt(msg.attempt);
        if (msg.command) {
          const command = msg.command;
          setCommands((prev) => (prev[prev.length - 1] === command ? prev : [...prev, command]));
        }
      } else if (msg.type === "done") {
        settled = true;
        onInstalled(id);
      } else if (msg.type === "failed") {
        settled = true;
        setFailure(msg.reason);
        setDetail(msg.detail);
        setAttempt(msg.attempts ?? 1);
        setPhase("failed");
      }
    };
    ws.onerror = () => {
      // 连不上后端：这条通道自己就断了，跟远端装没装成无关，同样给重试按钮
      if (settled) return;
      settled = true;
      setFailure("probe-failed");
      setDetail(t("zellij.channelError"));
      setPhase("failed");
    };
  }

  /**
   * 装好之后已经起来的会话**不会**变持久——它们是裸 shell，外面没有 Zellij 包着。
   * 不说清楚的话用户会以为问题解决了，下次断线才发现工作没保住。
   */
  function onInstalled(id: string) {
    const live = sessions.filter((s) => s.projectId === id && s.state !== "dead").length;
    const willCreate = spec!.thenCreate;
    finish(true);
    toast({
      kind: "success",
      sticky: live > 0,
      title: t("zellij.readyTitle", { host }),
      body: live > 0 ? t("zellij.readyBody", { n: live }) : undefined,
      actionLabel: willCreate ? undefined : t("sidebar.newTerminal"),
      onAction: willCreate ? undefined : () => void newTerminal(id),
      dismissLabel: willCreate ? undefined : t("zellij.later"),
    });
  }

  async function allow() {
    await api.setHostAuthorization(spec!.projectId, {
      authorized: true,
      baseUrl: baseUrl.trim(),
    });
    setSavedUrl(baseUrl.trim());
    startInstall(spec!.projectId);
  }

  /** 显式拒绝会写库；措辞里把可逆性说出来，反悔的入口在命令面板 */
  async function deny() {
    await api.setHostAuthorization(spec!.projectId, { authorized: false });
    toast({
      kind: "warning",
      title: t("zellij.skippedTitle"),
      body: t("zellij.skippedBody"),
    });
    skip("not-authorized");
  }

  /** 取消只影响本次，不写入拒绝状态——点的是进度条上的取消，不是撤销授权 */
  function cancelInstall() {
    wsRef.current?.send(JSON.stringify({ type: "cancel" }));
    wsRef.current?.close();
    skip("cancelled");
  }

  async function retry() {
    const url = baseUrl.trim();
    if (url !== savedUrl.trim()) {
      // 换地址要先落库：后端据此作废旧的安装记录，重试才不会用回老源
      await api.setHostAuthorization(spec!.projectId, { baseUrl: url });
      setSavedUrl(url);
    }
    startInstall(spec!.projectId);
  }

  const version = status?.requiredVersion ?? "";
  const base = (baseUrl.trim() || status?.defaultBaseUrl || "").replace(/\/+$/, "");
  const windows = target === "x86_64-pc-windows-msvc";
  const asset = `zellij-no-web-${target}.${windows ? "zip" : "tar.gz"}`;
  const assetUrl = `${base}/v${version}/${asset}`;
  const manualCmd = windows
    ? [
        `New-Item -ItemType Directory -Force ~\\.falcon\\bin | Out-Null`,
        `curl.exe -fsSL "${assetUrl}" -o "$env:TEMP\\zellij.zip"`,
        `tar.exe -xf "$env:TEMP\\zellij.zip" -C ~\\.falcon\\bin`,
        `Move-Item -Force ~\\.falcon\\bin\\zellij.exe ~\\.falcon\\bin\\zellij-${version}.exe`,
      ].join("\n")
    : [
        `mkdir -p ~/.falcon/bin`,
        `curl -L "${assetUrl}" | tar xz -C ~/.falcon/bin`,
        `mv ~/.falcon/bin/zellij ~/.falcon/bin/zellij-${version}`,
        `chmod +x ~/.falcon/bin/zellij-${version}`,
      ].join("\n");

  const copyManual = async () => {
    try {
      await navigator.clipboard.writeText(manualCmd);
      setCopied(true);
      setTimeout(() => setCopied(false), 1600);
    } catch {
      toast({ kind: "warning", title: t("toast.copyFailed") });
    }
  };

  if (phase === "loading") return null;

  const urlEdited = baseUrl.trim() !== savedUrl.trim();
  const showUrlFix = failure ? URL_FIXABLE.includes(failure) : false;
  const title =
    phase === "failed"
      ? t("zellij.failTitle", { host })
      : t("zellij.enableTitle", { host });

  return (
    <AppDialog
      title={title}
      onClose={() => skip()}
      wide
      lockOverlay={phase === "installing"}
    >
      {phase === "ask" && status && (
        <>
          <p className="text-sm leading-relaxed text-muted-foreground">
            {t("zellij.enableBody", { host, version: status.requiredVersion })}
          </p>
          <Field
            label={t("zellij.sourceLabel")}
            htmlFor="zellij-url"
            /* 不做完整性校验，自定义地址等于把执行权交给该地址的控制者 */
            hint={<span className="text-warning">{t("zellij.sourceWarning")}</span>}
          >
            <Input
              id="zellij-url"
              className="font-mono text-xs"
              value={baseUrl}
              placeholder={status.defaultBaseUrl}
              onChange={(e) => setBaseUrl(e.target.value)}
            />
          </Field>
          <p className="text-xs text-muted-foreground">{t("zellij.authNote")}</p>
          <div className="flex justify-end gap-2">
            <Button variant="outline" onClick={() => void deny()}>
              {t("zellij.authDeny")}
            </Button>
            <Button data-autofocus onClick={() => void allow()}>
              {t("zellij.authAllow")}
            </Button>
          </div>
        </>
      )}

      {phase === "installing" && (
        <>
          {/* 宿主机自己下载，后端拿不到字节数，因此只有阶段没有百分比 */}
          <p className="text-sm text-muted-foreground">{t(`zellij.stage_${stage}`)}</p>
          {attempt > 1 && (
            <p className="text-xs text-muted-foreground">
              {t("zellij.autoRetrying", { n: attempt })}
            </p>
          )}
          {commands.length > 0 && (
            <CommandLog label={t("zellij.commandLog")} commands={commands} running />
          )}
          <div className="flex justify-end">
            <Button variant="outline" data-autofocus onClick={cancelInstall}>
              {t("zellij.installCancel")}
            </Button>
          </div>
        </>
      )}

      {phase === "failed" && (
        <>
          <div className="flex gap-2.5 rounded-md border border-warning/40 bg-warning/10 px-3 py-2.5">
            <TriangleAlert className="size-4 shrink-0 text-warning" />
            <div>
              <div className="text-[13px]">{reasonText(t, failure)}</div>
              {attempt > 1 && (
                <div className="mt-0.5 text-xs leading-relaxed text-muted-foreground">
                  {t("zellij.autoRetried", { n: attempt })}
                </div>
              )}
            </div>
          </div>

          {commands.length > 0 && (
            <CommandLog label={t("zellij.commandLog")} commands={commands} />
          )}

          {showUrlFix && (
            <Field label={t("zellij.sourceLabel")} htmlFor="zellij-url-retry">
              <Input
                id="zellij-url-retry"
                className="font-mono text-xs"
                value={baseUrl}
                placeholder={status?.defaultBaseUrl}
                onChange={(e) => setBaseUrl(e.target.value)}
              />
            </Field>
          )}

          {detail && (
            <Disclosure
              open={showDetail}
              onOpenChange={setShowDetail}
              label={t("zellij.detailToggle")}
            >
              <CodeBlock code={detail} />
            </Disclosure>
          )}

          <Disclosure
            open={showManual}
            onOpenChange={setShowManual}
            label={t("zellij.manualToggle")}
          >
            <Field label={t("zellij.manualTarget")} className="mb-2">
              <Select
                value={target}
                onValueChange={(v) => setTarget(v as (typeof TARGETS)[number])}
              >
                <SelectTrigger className="w-full font-mono text-xs">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {TARGETS.map((tg) => (
                    <SelectItem key={tg} value={tg} className="font-mono text-xs">
                      {tg}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </Field>
            <CodeBlock
              code={manualCmd}
              action={
                <Button variant="secondary" size="xs" onClick={() => void copyManual()}>
                  {copied ? t("zellij.copied") : t("zellij.copy")}
                </Button>
              }
              after={
                <Button
                  variant="link"
                  size="xs"
                  className="px-0"
                  onClick={() => startInstall(spec.projectId)}
                >
                  {t("zellij.recheck")}
                </Button>
              }
            />
          </Disclosure>

          <div className="flex items-center gap-2">
            <Button variant="ghost" onClick={() => skip(failure)}>
              {t("zellij.installSkip")}
            </Button>
            <span className="flex-1" />
            {canRetryInstall(failure) && (
              <Button data-autofocus onClick={() => void retry()}>
                {urlEdited ? t("zellij.installRetryNewUrl") : t("zellij.installRetry")}
              </Button>
            )}
          </div>
        </>
      )}
    </AppDialog>
  );
}

function Disclosure({
  open,
  onOpenChange,
  label,
  children,
}: {
  open: boolean;
  onOpenChange: (next: boolean) => void;
  label: string;
  children: React.ReactNode;
}) {
  return (
    <Collapsible open={open} onOpenChange={onOpenChange}>
      <CollapsibleTrigger className="flex w-full items-center gap-1.5 py-1.5 text-left text-[12.5px] text-muted-foreground outline-none hover:text-foreground focus-visible:ring-[3px] focus-visible:ring-ring/50">
        {open ? <ChevronDown className="size-3" /> : <ChevronRight className="size-3" />}
        {label}
      </CollapsibleTrigger>
      <CollapsibleContent>{children}</CollapsibleContent>
    </Collapsible>
  );
}

/** 安装过程中远端实际跑过的命令。最新一条是正在执行或刚失败的那条。 */
function CommandLog({
  label,
  commands,
  running,
}: {
  label: string;
  commands: string[];
  running?: boolean;
}) {
  const end = useRef<HTMLDivElement>(null);
  useEffect(() => {
    end.current?.scrollIntoView({ block: "nearest" });
  }, [commands.length]);

  return (
    <div className="grid gap-1.5">
      <p className="text-xs text-muted-foreground">{label}</p>
      <div className="max-h-52 overflow-y-auto rounded-md border bg-background px-3 py-2 font-mono text-[11.5px] leading-relaxed">
        {commands.map((cmd, i) => {
          const last = i === commands.length - 1;
          return (
            <pre
              key={`${i}-${cmd.slice(0, 24)}`}
              className={cn(
                "m-0 whitespace-pre-wrap break-all",
                !last && "mb-2",
                last ? "text-foreground" : "text-muted-foreground/70"
              )}
            >
              <span className="select-none text-muted-foreground">$ </span>
              {cmd}
              {last && running ? " …" : ""}
            </pre>
          );
        })}
        <div ref={end} />
      </div>
    </div>
  );
}

/** 可复制的命令块。内网 / 无出网远端用户唯一的出路，得能直接执行。 */
function CodeBlock({
  code,
  action,
  after,
}: {
  code: string;
  action?: React.ReactNode;
  after?: React.ReactNode;
}) {
  return (
    <div className="relative rounded-md border bg-background px-3 py-2.5">
      {action && <div className="absolute top-2 right-2">{action}</div>}
      <pre className="m-0 font-mono text-[11.5px] leading-relaxed break-all whitespace-pre-wrap text-muted-foreground">
        {code}
      </pre>
      {after && <div className="mt-2">{after}</div>}
    </div>
  );
}
