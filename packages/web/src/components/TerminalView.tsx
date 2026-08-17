import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { WebLinksAddon } from "@xterm/addon-web-links";
import type { DeadReason, ServerMessage, SessionState } from "@mojito/shared";
import { api } from "../api.js";
import { useApp } from "../store.js";
import { connLabel } from "../lib/hostColor.js";
import { chord, matchCommand } from "../lib/shortcuts.js";
import { MAPLE_FONT_FAMILY, NERD_FONT_FAMILY, resolveTermTheme, termFontStack } from "../lib/term.js";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Banner } from "./common/Banner.js";

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
  const theme = useApp((s) => s.theme);
  const termPref = useApp((s) => s.term);
  const palette = resolveTermTheme(termPref.themeId, theme);

  const [view, setView] = useState<ViewState>({
    session: "active",
    reconnectAttempt: 0,
    wsClosed: false,
    attachError: null,
  });

  useEffect(() => {
    const pref = useApp.getState().term;
    const term = new Terminal({
      fontFamily: termFontStack(pref),
      fontSize: pref.fontSize,
      lineHeight: pref.lineHeight,
      cursorStyle: pref.cursorStyle,
      cursorBlink: pref.cursorBlink,
      scrollback: 5000,
      // unicode.activeVersion 是 proposed API；不打开会在设 11 时直接抛
      allowProposedApi: true,
      // 默认 Unicode 6 把大量 CJK / emoji / 图标当成 1 格，后一个字符会盖掉右半
      rescaleOverlappingGlyphs: true,
      // 这个 effect 不跟偏好重建（重建 = 断 WS + 重放历史），初值直接读，
      // 之后的切换交给下面那个 effect 就地改 options
      theme: resolveTermTheme(pref.themeId, useApp.getState().theme),
    });
    // 全局快捷键命中时把按键交给应用层：xterm 不处理，事件照样冒泡到 window。
    // Ctrl+C / Ctrl+R / Ctrl+W / Esc 不在快捷键表里，因此行为与原生终端一致。
    term.attachCustomKeyEventHandler(
      (e) => e.type !== "keydown" || matchCommand(e) === null
    );
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.loadAddon(new WebLinksAddon());
    term.loadAddon(new Unicode11Addon());
    term.unicode.activeVersion = "11";
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

    // Maple 还在下时回放已经进 atlas 了；字库到了之后碰一下字号，逼 xterm 丢掉旧字形。
    // 已经加载完就不必再动——立刻改字号会撞上 Viewport 还没挂 dimensions。
    const faces = [`${pref.fontSize}px "${MAPLE_FONT_FAMILY}"`, `${pref.fontSize}px "${NERD_FONT_FAMILY}"`];
    if (faces.some((spec) => !document.fonts.check(spec))) {
      void Promise.all(faces.map((spec) => document.fonts.load(spec))).then(() => {
        requestAnimationFrame(() => {
          if (disposed || term.rows <= 0) return;
          rebuildTermAtlas(term, useApp.getState().term.fontSize);
          if (containerRef.current && containerRef.current.offsetHeight > 0) fit.fit();
        });
      });
    }

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

  // 换字体 / 字号 / 主题时就地改 options，已经打印出来的内容一并重绘，不断 WS
  useEffect(() => {
    const term = termRef.current;
    if (!term) return;
    term.options.fontFamily = termFontStack(termPref);
    term.options.fontSize = termPref.fontSize;
    term.options.lineHeight = termPref.lineHeight;
    term.options.cursorStyle = termPref.cursorStyle;
    term.options.cursorBlink = termPref.cursorBlink;
    term.options.theme = palette;
    if (visible && containerRef.current && containerRef.current.offsetHeight > 0) {
      fitRef.current?.fit();
    }
  }, [termPref, palette, visible]);

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
          "terminal-host min-h-0 flex-1 py-1.5 pl-2 transition-opacity",
          view.session !== "active" && "opacity-55"
        )}
        style={{ backgroundColor: palette.background ?? undefined }}
      >
        <div ref={containerRef} className="h-full" />
      </div>
    </>
  );
}

/** 同值改 fontFamily 不会清 atlas；字号微扰一次即可。renderer 没就绪时 swallow。 */
function rebuildTermAtlas(term: Terminal, fontSize: number): void {
  term.options.fontSize = fontSize + 0.01;
  term.options.fontSize = fontSize;
  try {
    term.refresh(0, term.rows - 1);
  } catch {
    // Viewport 有时还没挂上 dimensions
  }
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
