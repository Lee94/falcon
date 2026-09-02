import { useEffect, useRef, useState, type MouseEvent } from "react";
import { useTranslation } from "react-i18next";
import type { DeadReason, ServerMessage, SessionState } from "@falcon/shared";
import { PASTE_IMAGE_MAX_BYTES, TERM_FRAME_OUTPUT, TERM_FRAME_REPLAY } from "@falcon/shared";
import { api } from "../api.js";
import { useApp } from "../store.js";
import { connLabel } from "../lib/hostColor.js";
import { chord, matchCommand } from "../lib/shortcuts.js";
import {
  MAPLE_FONT_FAMILY,
  NERD_FONT_FAMILY,
  resolveTermTheme,
  termColorHint,
} from "../lib/term.js";
import {
  createTermAdapter,
  type RendererInfo,
  type TermAdapter,
  type WebGpuFailReason,
} from "../lib/termAdapter.js";
import {
  mouseReportCoord,
  registerTermInput,
  repairMouseReport,
  takeStickyCtrl,
} from "../lib/termInput.js";
import { writeBrowserClipboard } from "../lib/osc52.js";
import { imageFromClipboard, imagesFromDrop, quoteForPrompt } from "../lib/pasteImage.js";
import { useActions } from "../lib/useActions.js";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Banner } from "./common/Banner.js";
import { openContextMenu } from "./common/Menu.js";

const GPU_REASON_KEY: Record<WebGpuFailReason, string> = {
  unsupported: "term.gpuReasonUnsupported",
  "no-adapter": "term.gpuReasonNoAdapter",
  "device-rejected": "term.gpuReasonDeviceRejected",
  "no-context": "term.gpuReasonNoContext",
  lost: "term.gpuReasonLost",
};

/** 每页面只提示一次：N 个 rio tab 同时丢 GPU 设备不能刷一屏 toast */
let gpuFallbackNotified = false;

function notifyRendererFallback(info: RendererInfo, t: (key: string, opts?: Record<string, string>) => string): void {
  console.info(`rio: renderer ${info.active} (requested ${info.requested}, cause ${info.cause}${info.reason ? `, reason ${info.reason}` : ""})`);
  if (info.requested !== "webgpu" || info.active === "webgpu" || gpuFallbackNotified) return;
  gpuFallbackNotified = true;
  const title =
    info.cause === "device-lost"
      ? t("term.rendererLost")
      : t("term.rendererFallback", { reason: t(GPU_REASON_KEY[info.reason ?? "unsupported"]) });
  useApp.getState().toast({ kind: "info", title });
}

interface ViewState {
  session: SessionState;
  deadReason?: DeadReason;
  reconnectAttempt: number;
  wsClosed: boolean;
  /** 与后端 WS 的自动重连轮次；0 = 没在重连 */
  wsRetry: number;
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
  const termRef = useRef<TermAdapter | null>(null);
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
  // 引擎切换必须整体重建（终端实例 + WS 重连拿服务端 replay 恢复内容），
  // 单独订阅避免其余偏好变化也触发重建
  const termEngine = useApp((s) => s.term.engine);
  const actions = useActions();
  const palette = resolveTermTheme(termPref.themeId, theme);

  const [view, setView] = useState<ViewState>({
    session: "active",
    reconnectAttempt: 0,
    wsClosed: false,
    wsRetry: 0,
    attachError: null,
  });
  /** 断线横幅上的「立即重连」，由主 effect 填充 */
  const retryNowRef = useRef<() => void>(() => undefined);

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
    /** 卸载后这条 socket 的收尾事件不许再改状态——否则自己关掉的连接会被当成"后端断了" */
    let disposed = false;
    /** 还没量出真实格子之前禁止把 80×24 默认值发给 PTY */
    let measured = false;
    /** 断线自动重连：connect() 每次换新 socket，旧 socket 迟到的事件按 ws !== sock 丢弃 */
    let ws: WebSocket | null = null;
    let retries = 0;
    let retryTimer: number | null = null;
    /** 最近一次有效 SGR 鼠标报文的坐标，触摸惯性期的 NaN 报文靠它修 */
    let lastMouseCoord: string | null = null;

    // 引擎、初始外观都在这里定死；之后的外观切换交给下面那个 effect 走
    // applyAppearance（xterm 就地改 options，rio 内部重建），不断 WS
    const adapter = createTermAdapter({
      pref,
      theme: resolveTermTheme(pref.themeId, useApp.getState().theme),
      // 与服务端 zellij scroll_buffer_size（10000）匹配：replay 重建时前端
      // 缓冲会被整体替换，设得比远端小就白白丢历史
      scrollback: 10000,
      hooks: {
        onData: (data) => {
          if (ws?.readyState !== WebSocket.OPEN) return;
          // 触摸惯性滚动的 NaN 坐标鼠标报文在这里修（见 repairMouseReport 注释），
          // 修不了的直接丢——发出去就是往 shell 里打乱码
          const repaired = repairMouseReport(data, lastMouseCoord);
          if (repaired === null) return;
          lastMouseCoord = mouseReportCoord(repaired) ?? lastMouseCoord;
          // 移动端键位条的粘滞 Ctrl 在这里落地：点亮后下一个字符转控制字节。
          // 桌面端 Ctrl 永远不点亮，等于恒等变换
          ws.send(JSON.stringify({ type: "input", data: takeStickyCtrl(repaired) }));
        },
        onResize: () => sendResize(),
        // rio 引擎异步就绪（wasm 首次加载 / 换肤重建）；好了再补量一次尺寸
        onReady: () => {
          applySize();
        },
        isGlobalKey: (e) => matchCommand(e) !== null,
        onEngineError: (message) =>
          useApp.getState().toast({ kind: "warning", title: t("term.engineFailed"), body: message }),
        onRenderer: (info) => notifyRendererFallback(info, t),
      },
    });
    adapter.open(containerRef.current!);
    termRef.current = adapter;

    // 移动端键位条从这里把 Esc / 方向键等序列注入本会话（见 lib/termInput.ts）
    const unregisterInput = registerTermInput(sessionId, {
      send: (data) => {
        if (ws?.readyState === WebSocket.OPEN) {
          ws.send(JSON.stringify({ type: "input", data }));
        }
      },
      appCursorKeys: () => adapter.appCursorKeys(),
    });

    const proto = location.protocol === "https:" ? "wss" : "ws";
    const wsUrl = `${proto}://${location.host}/ws/sessions/${sessionId}`;

    /** 这条 socket 上最后一次发出的尺寸；拖窗时 ResizeObserver 逐帧触发，
     *  格子数没变就不打扰服务端（服务端每条 resize 都有落库逻辑）。
     *  换新 socket 必须强发一次：服务端拿它当持久会话的懒惰接回信号。 */
    let sentCols = -1;
    let sentRows = -1;
    const sendResize = (force = false) => {
      if (!measured || ws?.readyState !== WebSocket.OPEN) return;
      if (!force && adapter.cols === sentCols && adapter.rows === sentRows) return;
      sentCols = adapter.cols;
      sentRows = adapter.rows;
      ws.send(JSON.stringify({ type: "resize", cols: adapter.cols, rows: adapter.rows }));
    };

    const sendAppearance = () => {
      if (ws?.readyState !== WebSocket.OPEN) return;
      const { theme: ui, term: pref } = useApp.getState();
      ws.send(JSON.stringify({ type: "appearance", ...termColorHint(pref.themeId, ui) }));
    };
    sendAppearanceRef.current = sendAppearance;

    const applySize = (rebuild = false): boolean => {
      if (disposed) return false;
      if (rebuild) adapter.refreshMetrics();
      if (!adapter.fit()) return false;
      measured = true;
      sendResize();
      return true;
    };
    applySizeRef.current = applySize;

    const scheduleRetry = () => {
      if (disposed || retryTimer != null) return;
      // 1s 起步翻倍、15s 封顶：不打爆刚起来的后端，也不让用户干等太久
      const delay = Math.min(15_000, 1000 * 2 ** Math.min(retries, 4));
      retries++;
      setView((v) => ({ ...v, wsRetry: retries }));
      retryTimer = window.setTimeout(() => {
        retryTimer = null;
        connect();
      }, delay);
    };

    const connect = () => {
      const sock = new WebSocket(wsUrl);
      // 终端数据走二进制帧（1 字节类型 + UTF-8 载荷），Uint8Array 直接喂
      // xterm.write，绕开 JSON 转义与解析；控制消息仍是 JSON 文本帧
      sock.binaryType = "arraybuffer";
      ws = sock;
      sock.onopen = () => {
        if (disposed || ws !== sock) return;
        const reconnected = retries > 0;
        retries = 0;
        setView((v) => ({ ...v, wsClosed: false, wsRetry: 0 }));
        // resize 顺带触发服务端对持久会话的懒惰接回（ensureAttached）
        sendResize(true);
        sendAppearance();
        // 断开期间错过的会话状态变化不等 5s 轮询，立刻补一次
        if (reconnected) void refreshSessions();
      };
      sock.onclose = (ev) => {
        if (disposed || ws !== sock) return;
        // 4401 = 认证没了（如后端重启丢掉内存 token）。重试只会无限 4401，
        // 刷新认证状态让 App 落到登录页
        if (ev.code === 4401) {
          setView((v) => ({ ...v, wsClosed: true, wsRetry: 0 }));
          void useApp.getState().refreshAuth();
          return;
        }
        setView((v) => ({ ...v, wsClosed: true }));
        scheduleRetry();
      };
      sock.onmessage = (ev) => {
        if (disposed || ws !== sock) return;
        if (ev.data instanceof ArrayBuffer) {
          const frame = new Uint8Array(ev.data);
          if (frame.length < 1) return;
          if (frame[0] === TERM_FRAME_REPLAY) {
            // 回放是整份快照，必须清掉断线前的 VT 状态再写
            adapter.reset();
          } else if (frame[0] !== TERM_FRAME_OUTPUT) {
            return;
          }
          adapter.write(frame.subarray(1));
          return;
        }
        let msg: ServerMessage;
        try {
          msg = JSON.parse(ev.data as string);
        } catch {
          return;
        }
        switch (msg.type) {
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
    };

    /** 「立即重连」按钮与浏览器 online 事件都不必等退避定时器 */
    const retryNow = () => {
      if (
        disposed ||
        !ws ||
        ws.readyState === WebSocket.OPEN ||
        ws.readyState === WebSocket.CONNECTING
      ) {
        return;
      }
      if (retryTimer != null) {
        clearTimeout(retryTimer);
        retryTimer = null;
      }
      retries++;
      setView((v) => ({ ...v, wsRetry: retries }));
      connect();
    };
    retryNowRef.current = retryNow;
    window.addEventListener("online", retryNow);
    // 后台标签页的定时器会被浏览器节流到分钟级；切回来时立刻补一次
    const onVisible = () => {
      if (document.visibilityState === "visible") retryNow();
    };
    document.addEventListener("visibilitychange", onVisible);

    connect();

    // rAF 合并：applySize 里的 proposeDimensions/fit 会强制同步回流，
    // 拖窗时逐帧 × N 个 tab 会叠出可感知的掉帧
    let sizeRaf: number | null = null;
    const ro = new ResizeObserver(() => {
      if (sizeRaf != null) return;
      sizeRaf = requestAnimationFrame(() => {
        sizeRaf = null;
        applySize();
      });
    });
    ro.observe(containerRef.current!);

    // 终端自己的选区（没被 TUI 鼠标协议吃掉时）松手即复制，对齐 iTerm。
    const host = containerRef.current!;
    const copySelection = () => {
      const text = adapter.getSelection();
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
      unregisterInput();
      window.removeEventListener("online", retryNow);
      document.removeEventListener("visibilitychange", onVisible);
      if (retryTimer != null) clearTimeout(retryTimer);
      retryNowRef.current = () => undefined;
      document.fonts.removeEventListener("loadingdone", afterFonts);
      host.removeEventListener("mouseup", copySelection);
      host.removeEventListener("paste", onPasteCapture, true);
      host.removeEventListener("dragover", onDragOver);
      host.removeEventListener("drop", onDrop);
      if (sizeRaf != null) cancelAnimationFrame(sizeRaf);
      ro.disconnect();
      ws?.close();
      adapter.dispose();
      termRef.current = null;
      applySizeRef.current = () => false;
      sendAppearanceRef.current = () => undefined;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sessionId, refreshSessions, termEngine]);

  useEffect(() => {
    if (visible) {
      applySizeRef.current();
      termRef.current?.focus();
    }
  }, [visible]);

  // 换字体 / 字号 / 主题时热改外观（xterm 就地改 options 并重绘已打印内容；
  // rio 在适配器内部重建实例），不断 WS。没变化时适配器自行去重
  useEffect(() => {
    const term = termRef.current;
    if (!term) return;
    term.applyAppearance(termPref, palette);
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
          title={
            view.wsRetry > 0
              ? t("session.connReconnecting", { attempt: view.wsRetry })
              : t("session.connClosed")
          }
          body={t("session.connClosedBody")}
          actions={
            <>
              <Button variant="outline" size="sm" onClick={() => retryNowRef.current()}>
                {t("session.retryNow")}
              </Button>
              <Button variant="ghost" size="sm" onClick={() => location.reload()}>
                {t("session.reload")}
              </Button>
            </>
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
