/** 一个已附着的底层终端（本地 PTY 或 SSH channel）。 */
export interface Backend {
  write(data: string): void;
  resize(cols: number, rows: number): void;
  /** 仅断开本端附着，不终止底层（用于持久会话链路重建前的清理） */
  destroy(): void;
  /**
   * tty 前台进程名。只有本地 POSIX PTY 提供（node-pty 原生能力）；
   * 持久会话不走这里（外层 PTY 的前台永远是 Zellij 客户端），问 Zellij。
   */
  processName?(): string | undefined;
}

export interface BackendCallbacks {
  onData(data: string): void;
  /** 底层附着结束（shell 退出 / 链路关闭）。持久会话由上层判断是链路问题还是真退出。 */
  onExit(): void;
}

export interface AttachResult {
  backend: Backend;
  durable: boolean;
  /** 接回持久会话时用 dump-screen 抓回的历史，用于重建 Scrollback */
  capturedHistory?: string;
}

/** dump-screen 输出为 \n 结尾的行，回放进 xterm 需要 \r\n */
export function normalizeCaptured(text: string): string {
  return text.replace(/\r?\n/g, "\r\n");
}

/** 接回持久会话时，宿主机上的 Zellij session 已经不在了 */
export class SessionGoneError extends Error {
  constructor() {
    super("Zellij 会话不存在");
  }
}
