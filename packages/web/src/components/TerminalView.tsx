import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebLinksAddon } from "@xterm/addon-web-links";
import type { DeadReason, ServerMessage, SessionState } from "@mojito/shared";
import { api } from "../api.js";
import { useApp } from "../store.js";
import { connLabel } from "../lib/hostColor.js";
import { chord, matchCommand } from "../lib/shortcuts.js";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Banner } from "./common/Banner.js";

/**
 * xterm 只认具体颜色，读不了 CSS 变量，所以主题色在这里写死一次。
 * 取的是 shadcn neutral 暗色的 --background / --foreground 的 sRGB 值。
 */
const TERM_THEME = {
  background: "#0a0a0a",
  foreground: "#fafafa",
  cursor: "#fafafa",
  selectionBackground: "#ffffff33",
};

interface ViewState {
  session: SessionState;
  deadReason?: DeadReason;
  reconnectAttempt: number;
  wsClosed: boolean;
  attachError: string | null;
}

export function TerminalView({
  sessionId,
  visible,
}: {
  sessionId: string;
  visible: boolean;
}) {
  const { t } = useTranslation();
  const containerRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const refreshSessions = useApp((s) => s.refreshSessions);
  const session = useApp((s) => s.sessions.find((x) => x.id === sessionId));
  const project = useApp((s) => s.projects.find((p) => p.id === session?.projectId));
  const newTerminal = useApp((s) => s.newTerminal);
  const dropTab = useApp((s) => s.dropTab);

  const [view, setView] = useState<ViewState>({
    session: "active",
    reconnectAttempt: 0,
    wsClosed: false,
    attachError: null,
  });

  useEffect(() => {
    const term = new Terminal({
      fontFamily: '"Cascadia Mono", "JetBrains Mono", "SF Mono", Consolas, monospace',
      fontSize: 13,
      scrollback: 5000,
      theme: TERM_THEME,
    });
    // 全局快捷键命中时把按键交给应用层：xterm 不处理，事件照样冒泡到 window。
    // Ctrl+C / Ctrl+R / Ctrl+W / Esc 不在快捷键表里，因此行为与原生终端一致。
    term.attachCustomKeyEventHandler(
      (e) => e.type !== "keydown" || matchCommand(e) === null
    );
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.loadAddon(new WebLinksAddon());
    term.open(containerRef.current!);
    fit.fit();
    termRef.current = term;
    fitRef.current = fit;

    const proto = location.protocol === "https:" ? "wss" : "ws";
    const ws = new WebSocket(`${proto}://${location.host}/ws/sessions/${sessionId}`);
    /** 卸载后这条 socket 的收尾事件不许再改状态——否则自己关掉的连接会被当成"后端断了" */
    let disposed = false;

    const sendResize = () => {
      if (ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify({ type: "resize", cols: term.cols, rows: term.rows }));
      }
    };

    ws.onopen = () => sendResize();
    ws.onclose = () => {
      if (!disposed) setView((v) => ({ ...v, wsClosed: true }));
    };
    ws.onmessage = (ev) => {
      if (disposed) return;
      let msg: ServerMessage;
      try {
        msg = JSON.parse(ev.data as string);
      } catch {
        return;
      }
      switch (msg.type) {
        case "replay":
        case "output":
          term.write(msg.data);
          break;
        case "state":
          setView((v) => ({
            ...v,
            session: msg.state,
            deadReason: msg.deadReason,
            reconnectAttempt: msg.state === "active" ? 0 : v.reconnectAttempt,
            attachError: msg.state === "active" ? null : v.attachError,
          }));
          if (msg.state !== "active") void refreshSessions();
          break;
        case "reconnecting":
          setView((v) => ({ ...v, reconnectAttempt: msg.attempt }));
          break;
        case "error":
          setView((v) => ({ ...v, attachError: msg.message }));
          break;
      }
    };

    term.onData((data) => {
      if (ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify({ type: "input", data }));
      }
    });
    term.onResize(() => sendResize());

    const ro = new ResizeObserver(() => {
      if (containerRef.current && containerRef.current.offsetHeight > 0) {
        fit.fit();
      }
    });
    ro.observe(containerRef.current!);

    return () => {
      disposed = true;
      ro.disconnect();
      ws.close();
      term.dispose();
      termRef.current = null;
      // fit addon 跟着 term 一起作废，留着会让 visible 分支对已 dispose 的实例调 fit()
      fitRef.current = null;
    };
  }, [sessionId, refreshSessions]);

  useEffect(() => {
    if (visible) {
      fitRef.current?.fit();
      termRef.current?.focus();
    }
  }, [visible]);

  const manualReattach = async () => {
    setView((v) => ({ ...v, attachError: null }));
    try {
      await api.reattachSession(sessionId);
      void refreshSessions();
    } catch (err) {
      setView((v) => ({ ...v, attachError: (err as Error).message }));
    }
  };

  const banner = (() => {
    if (view.wsClosed) {
      return (
        <Banner
          tone="error"
          state="dead"
          title={t("session.connClosed")}
          body={t("session.connClosedBody")}
          actions={
            <Button variant="outline" size="sm" onClick={() => location.reload()}>
              {t("session.reload")}
            </Button>
          }
        />
      );
    }
    if (view.session === "dead") {
      const reason = view.deadReason
        ? t(`session.deadReason_${view.deadReason.replace(/-/g, "_")}`)
        : "";
      return (
        <Banner
          tone="error"
          state="dead"
          title={t("session.deadTitle")}
          body={t("session.deadBody", { reason })}
          actions={
            <>
              {project && (
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => void newTerminal(project.id)}
                >
                  {t("session.deadActionNew")}
                </Button>
              )}
              <Button
                variant="outline"
                size="sm"
                onClick={() => {
                  void api.clearSession(sessionId).catch(() => undefined);
                  dropTab(sessionId);
                  void refreshSessions();
                }}
              >
                {t("session.clearRecord")}
              </Button>
            </>
          }
        />
      );
    }
    if (view.session === "unverified") {
      return (
        <Banner
          tone="warn"
          state="unverified"
          title={
            view.reconnectAttempt > 0
              ? t("session.reconnecting", { attempt: view.reconnectAttempt })
              : t("session.unverifiedTitle")
          }
          body={
            view.attachError
              ? `${t("session.attachFailed")}: ${view.attachError}`
              : t("session.unverifiedBody")
          }
          actions={
            <Button variant="warning" size="sm" onClick={() => void manualReattach()}>
              {t("session.reattach")} {chord("reattach")}
            </Button>
          }
        />
      );
    }
    if (view.attachError) {
      return (
        <Banner
          tone="error"
          state="dead"
          title={t("session.attachFailed")}
          body={view.attachError}
          actions={
            <Button variant="outline" size="sm" onClick={() => void manualReattach()}>
              {t("session.retry")}
            </Button>
          }
        />
      );
    }
    return null;
  })();

  return (
    <>
      {banner}
      {/* 输入被禁用这件事要看得见 */}
      <div
        className={cn(
          "terminal-host min-h-0 flex-1 bg-background py-1.5 pl-2 transition-opacity",
          view.session !== "active" && "opacity-55"
        )}
      >
        <div ref={containerRef} className="h-full" />
      </div>
    </>
  );
}

/** 会话还没建起来时占住 tab：立刻有反馈，而不是等 REST 返回才出现 */
export function PendingPane({ pendingId }: { pendingId: string }) {
  const { t } = useTranslation();
  const entry = useApp((s) => s.pending.find((p) => p.id === pendingId));
  const project = useApp((s) => s.projects.find((p) => p.id === entry?.projectId));
  const system = useApp((s) => s.system);
  const retryPending = useApp((s) => s.retryPending);
  const dropTab = useApp((s) => s.dropTab);
  if (!entry) return null;

  const conn = connLabel(project, system, t("project.typeLocalShort"));

  return (
    <>
      {entry.error ? (
        <Banner
          tone="error"
          state="dead"
          title={t("session.createFailedTitle")}
          body={entry.error}
          actions={
            <>
              <Button
                variant="outline"
                size="sm"
                onClick={() => void retryPending(pendingId)}
              >
                {t("session.retry")}
              </Button>
              <Button variant="ghost" size="sm" onClick={() => dropTab(pendingId)}>
                {t("common.cancel")}
              </Button>
            </>
          }
        />
      ) : (
        <Banner
          tone="info"
          state="creating"
          title={t("session.creatingTitle")}
          body={t("session.creatingBody", { conn })}
          actions={
            <Button variant="ghost" size="sm" onClick={() => dropTab(pendingId)}>
              {t("common.cancel")}
            </Button>
          }
        />
      )}
      <div className="grid min-h-0 flex-1 place-items-center font-mono text-[13px] text-muted-foreground">
        {conn}
      </div>
    </>
  );
}
