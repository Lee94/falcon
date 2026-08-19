import { useEffect, useRef, useState, type MouseEvent } from "react";
import { useTranslation } from "react-i18next";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { WebLinksAddon } from "@xterm/addon-web-links";
import type { DeadReason, ServerMessage, SessionState } from "@mojito/shared";
import { PASTE_IMAGE_MAX_BYTES } from "@mojito/shared";
import { api } from "../api.js";
import { useApp } from "../store.js";
import { connLabel } from "../lib/hostColor.js";
import { chord, matchCommand } from "../lib/shortcuts.js";
import {
  MAPLE_FONT_FAMILY,
  NERD_FONT_FAMILY,
  resolveTermTheme,
  termColorHint,
  termFontStack,
} from "../lib/term.js";
import { isUsableTermSize } from "../lib/termFit.js";
import { osc52ClipboardText, writeBrowserClipboard } from "../lib/osc52.js";
import { imageFromClipboard, imagesFromDrop, quoteForPrompt } from "../lib/pasteImage.js";
import { useActions } from "../lib/useActions.js";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Banner } from "./common/Banner.js";
import { openContextMenu } from "./common/Menu.js";

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
  const applySizeRef = useRef<(rebuild?: boolean) => boolean>(() => false);
  const sendAppearanceRef = useRef<() => void>(() => undefined);
  const refreshSessions = useApp((s) => s.refreshSessions);
  const session = useApp((s) => s.sessions.find((x) => x.id === sessionId));
  const project = useApp((s) => s.projects.find((p) => p.id === session?.projectId));
  const newTerminal = useApp((s) => s.newTerminal);
  const dropTab = useApp((s) => s.dropTab);
  const toast = useApp((s) => s.toast);
  const theme = useApp((s) => s.theme);
  const termPref = useApp((s) => s.term);
  const actions = useActions();
  const palette = resolveTermTheme(termPref.themeId, theme);

  const [view, setView] = useState<ViewState>({
    session: "active",
    reconnectAttempt: 0,
    wsClosed: false,
    attachError: null,
  });

  /**
   * 图片上传到会话宿主机，把落盘路径粘进终端输入——Claude Code 等 TUI
   * 认输入框里的图片路径（拖文件进原生终端就是同一个机制）。
   * 只依赖 props 与稳定的 ref，effect 里捕获到哪个渲染的实例都一样。
   */
  const uploadImages = async (blobs: Blob[]) => {
    const paths: string[] = [];
    for (const blob of blobs) {
      if (blob.size > PASTE_IMAGE_MAX_BYTES) {
        toast({
          kind: "warning",
          title: t("term.pasteImageTooLarge", { max: PASTE_IMAGE_MAX_BYTES / 1024 / 1024 }),
        });
        continue;
      }
      try {
        const res = await api.pasteImage(sessionId, blob);
        paths.push(quoteForPrompt(res.path));
      } catch (err) {
        toast({
          kind: "warning",
          title: t("term.pasteImageFailed"),
          body: (err as Error).message,
        });
      }
    }
    if (paths.length > 0) termRef.current?.paste(paths.join(" "));
  };

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
    // Claude Code / vim 选中即复制走 OSC 52；xterm.js 默认丢弃。只写不读。
    term.parser.registerOscHandler(52, (data) => {
      const text = osc52ClipboardText(data);
      if (text !== undefined) void writeBrowserClipboard(text).catch(() => undefined);
      return true;
    });
    term.open(containerRef.current!);
    termRef.current = term;
    fitRef.current = fit;

    const proto = location.protocol === "https:" ? "wss" : "ws";
    const ws = new WebSocket(`${proto}://${location.host}/ws/sessions/${sessionId}`);
    /** 卸载后这条 socket 的收尾事件不许再改状态——否则自己关掉的连接会被当成"后端断了" */
    let disposed = false;
    /** 还没量出真实格子之前禁止把 80×24 默认值发给 PTY */
    let measured = false;

    const sendResize = () => {
      if (!measured || ws.readyState !== WebSocket.OPEN) return;
      ws.send(JSON.stringify({ type: "resize", cols: term.cols, rows: term.rows }));
    };

    const sendAppearance = () => {
      if (ws.readyState !== WebSocket.OPEN) return;
      const { theme: ui, term: pref } = useApp.getState();
      ws.send(JSON.stringify({ type: "appearance", ...termColorHint(pref.themeId, ui) }));
    };
    sendAppearanceRef.current = sendAppearance;

    const applySize = (rebuild = false): boolean => {
      if (disposed) return false;
      const box = containerRef.current;
      if (rebuild && term.rows > 0) {
        rebuildTermAtlas(term, useApp.getState().term.fontSize);
      }
      const proposed = fit.proposeDimensions();
      if (
        !isUsableTermSize(proposed, {
          width: box?.offsetWidth ?? 0,
          height: box?.offsetHeight ?? 0,
        })
      ) {
        return false;
      }
      fit.fit();
      measured = true;
      sendResize();
      return true;
    };
    applySizeRef.current = applySize;

    ws.onopen = () => {
      sendResize();
      sendAppearance();
    };
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
          // dump-screen 是整屏快照，必须清掉断线前的 VT 状态再写
          term.reset();
          term.write(msg.data);
          break;
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
          useApp.getState().applySessionState(sessionId, msg.state, msg.deadReason);
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
      applySize();
    });
    ro.observe(containerRef.current!);

    // 终端自己的选区（没被 TUI 鼠标协议吃掉时）松手即复制，对齐 iTerm。
    const host = containerRef.current!;
    const copySelection = () => {
      const text = term.getSelection();
      if (text) void writeBrowserClipboard(text).catch(() => undefined);
    };
    host.addEventListener("mouseup", copySelection);

    // 截图等纯图片粘贴在 capture 阶段截下来走上传；带文本的粘贴不拦，
    // 照旧冒到 xterm 的 textarea 走文本粘贴。拖图片文件进终端同理。
    const onPasteCapture = (e: ClipboardEvent) => {
      const file = imageFromClipboard(e.clipboardData);
      if (!file) return;
      e.preventDefault();
      e.stopPropagation();
      void uploadImages([file]);
    };
    const onDragOver = (e: DragEvent) => {
      if (e.dataTransfer?.types.includes("Files")) e.preventDefault();
    };
    const onDrop = (e: DragEvent) => {
      if (!e.dataTransfer?.types.includes("Files")) return;
      // 非图片文件也要拦下默认行为，否则浏览器会直接导航到那个文件
      e.preventDefault();
      const files = imagesFromDrop(e.dataTransfer);
      if (files.length > 0) void uploadImages(files);
    };
    host.addEventListener("paste", onPasteCapture, true);
    host.addEventListener("dragover", onDragOver);
    host.addEventListener("drop", onDrop);

    // 刷新后 check() 常已是 true（字库在缓存 / main 里预热过），但 canvas 第一帧
    // 仍可能按 fallback 量格子。必须等 fonts.ready 再重建 atlas，不能跳过。
    const faces = [`${pref.fontSize}px "${MAPLE_FONT_FAMILY}"`, `${pref.fontSize}px "${NERD_FONT_FAMILY}"`];
    const afterFonts = () => {
      requestAnimationFrame(() => {
        requestAnimationFrame(() => {
          if (disposed) return;
          applySize(true);
        });
      });
    };
    void Promise.all([document.fonts.ready, ...faces.map((spec) => document.fonts.load(spec))]).then(
      afterFonts
    );
    document.fonts.addEventListener("loadingdone", afterFonts);

    // renderer / flex 高度经常晚一拍；fonts.ready 已 resolved 时也要再试几帧
    let frames = 0;
    const bump = () => {
      if (disposed || frames++ >= 12) return;
      if (!applySize()) requestAnimationFrame(bump);
    };
    requestAnimationFrame(bump);

    return () => {
      disposed = true;
      document.fonts.removeEventListener("loadingdone", afterFonts);
      host.removeEventListener("mouseup", copySelection);
      host.removeEventListener("paste", onPasteCapture, true);
      host.removeEventListener("dragover", onDragOver);
      host.removeEventListener("drop", onDrop);
      ro.disconnect();
      ws.close();
      term.dispose();
      termRef.current = null;
      applySizeRef.current = () => false;
      sendAppearanceRef.current = () => undefined;
      // fit addon 跟着 term 一起作废，留着会让 visible 分支对已 dispose 的实例调 fit()
      fitRef.current = null;
    };
  }, [sessionId, refreshSessions]);

  useEffect(() => {
    if (visible) {
      applySizeRef.current();
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
    // 改 options 本身会重测格子；不要在这里 nudge 字号——刚 open 时 Viewport 可能还没挂
    if (visible) applySizeRef.current();
    sendAppearanceRef.current();
  }, [termPref, palette, visible]);

  const manualReattach = async () => {
    setView((v) => ({ ...v, attachError: null }));
    try {
      const row = await api.reattachSession(sessionId);
      useApp.getState().applySessionState(row.id, row.state, row.deadReason);
      if (row.state !== "active") {
        throw new Error(t("session.attachFailed"));
      }
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

  const focusTerm = () => {
    queueMicrotask(() => termRef.current?.focus());
  };

  const onTermContextMenu = (e: MouseEvent) => {
    const term = termRef.current;
    if (!term) return;
    const selected = term.getSelection();
    const canPaste = view.session === "active" && !view.wsClosed;
    openContextMenu(
      e,
      actions.termMenuItems({
        hasSelection: selected.length > 0,
        canPaste,
        onCopy: () => {
          void navigator.clipboard.writeText(selected).catch(() => {
            toast({ kind: "warning", title: t("toast.copyFailed") });
          });
          focusTerm();
        },
        onPaste: () => {
          void (async () => {
            try {
              // 与快捷键粘贴同一条规则：有文本贴文本，纯图片才走上传。
              // Firefox 老版本没有 read()，静默退回文本路径。
              const items = navigator.clipboard.read
                ? await navigator.clipboard.read().catch(() => null)
                : null;
              const hasText = items?.some((i) => i.types.includes("text/plain")) ?? false;
              if (items && !hasText) {
                for (const item of items) {
                  const type = item.types.find((x) => x.startsWith("image/"));
                  if (type) {
                    await uploadImages([await item.getType(type)]);
                    return;
                  }
                }
              }
              const text = await navigator.clipboard.readText();
              if (text) termRef.current?.paste(text);
            } catch {
              toast({ kind: "warning", title: t("term.pasteFailed") });
            } finally {
              focusTerm();
            }
          })();
        },
        onClear: () => {
          termRef.current?.clear();
          focusTerm();
        },
      })
    );
  };

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
        onContextMenu={onTermContextMenu}
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
