import type { FastifyInstance } from "fastify";
import type { WebSocket } from "ws";
import type {
  ClientMessage,
  InstallServerMessage,
  ServerMessage,
} from "@mojito/shared";
import type { Auth } from "./auth.js";
import type { Db } from "./db.js";
import { InstallError } from "./zellij/install.js";
import type { SessionManager, Viewer } from "./sessions/manager.js";

export function registerWs(
  app: FastifyInstance,
  deps: { auth: Auth; manager: SessionManager; db: Db }
) {
  const { auth, manager, db } = deps;

  /**
   * Zellij 安装通道。
   *
   * 安装是主机级操作、可能耗时数十秒（宿主机自己下载 14 MiB 压缩包再解压），
   * 塞进创建会话的 REST 请求里会让前端只能干等。这里推分阶段状态，
   * 并支持中途取消——取消只影响本次，不写入拒绝状态：取消按钮出现在进度条上，
   * 语境是"我不想等这次传输"，而不是"我撤销授权"。
   *
   * 每次连上来都当作用户显式发起的安装（首次或重试），因此一律 fresh：
   * 上一次的失败判定是缓存在 SshLink 上的，不清掉的话重试只会秒回同一个错误。
   */
  app.get("/ws/install/:projectId", { websocket: true }, (socket: WebSocket, req) => {
    if (!auth.isAuthenticated(req)) {
      socket.close(4401, "unauthorized");
      return;
    }
    const { projectId } = req.params as { projectId: string };
    const project = db.getProject(projectId);
    const send = (msg: InstallServerMessage) => {
      if (socket.readyState === socket.OPEN) socket.send(JSON.stringify(msg));
    };

    if (!project) {
      send({ type: "failed", reason: "verify-failed", detail: "项目不存在" });
      socket.close();
      return;
    }

    const controller = new AbortController();
    socket.on("message", (raw) => {
      try {
        if (JSON.parse(raw.toString())?.type === "cancel") controller.abort();
      } catch {
        // 忽略无法解析的消息
      }
    });
    socket.on("close", () => controller.abort());

    // 后端自动重试的轮次，失败时一并告诉用户——"已经替你试过 3 次"
    // 与"一次都没试"是完全不同的处境，前者说明该去查网络而不是傻点重试。
    let attempts = 1;

    void manager
      .prepare(
        project,
        (stage, attempt, command) => {
          attempts = attempt;
          send({ type: "stage", stage, attempt, command });
        },
        controller.signal,
        { fresh: true }
      )
      .then((state) => {
        if (state.durable) {
          send({ type: "done" });
        } else {
          send({
            type: "failed",
            reason: state.reason ?? "verify-failed",
            detail: state.detail,
            attempts,
          });
        }
      })
      .catch((err) => {
        send({
          type: "failed",
          reason: err instanceof InstallError ? err.reason : "verify-failed",
          detail: err instanceof InstallError ? err.detail : (err as Error).message,
          attempts,
        });
      })
      .finally(() => socket.close());
  });

  app.get("/ws/sessions/:id", { websocket: true }, (socket: WebSocket, req) => {
    if (!auth.isAuthenticated(req)) {
      socket.close(4401, "unauthorized");
      return;
    }
    const { id } = req.params as { id: string };

    const viewer: Viewer = {
      send(msg: ServerMessage) {
        if (socket.readyState === socket.OPEN) {
          socket.send(JSON.stringify(msg));
        }
      },
    };

    socket.on("message", (raw) => {
      let msg: ClientMessage;
      try {
        msg = JSON.parse(raw.toString());
      } catch {
        return;
      }
      if (msg.type === "input" && typeof msg.data === "string") {
        manager.input(id, msg.data);
      } else if (
        msg.type === "resize" &&
        Number.isInteger(msg.cols) &&
        Number.isInteger(msg.rows) &&
        msg.cols > 0 &&
        msg.rows > 0
      ) {
        manager.resize(id, msg.cols, msg.rows);
      }
    });

    socket.on("close", () => {
      // Detach：仅断开视图，会话继续运行
      manager.removeViewer(id, viewer);
    });

    void manager.addViewer(id, viewer);
  });
}
