/**
 * docker 操作的结构化错误。形状与 git/error.ts 的 WorktreeError 一致：
 * reason 是 shared 里的闭集，前端按它渲染具体说明。
 */

import type { DockerUnavailableReason } from "@falcon/shared";

export class DockerError extends Error {
  constructor(
    readonly reason: DockerUnavailableReason,
    message: string,
    readonly detail?: string
  ) {
    super(message);
  }
}

const FAILURE_TEXT: Record<DockerUnavailableReason, string> = {
  "docker-missing": "宿主机上没有 docker，或 docker 不在 PATH 中",
  "docker-permission": "当前用户无权访问 Docker（常见于未加入 docker 组、不能访问 docker.sock）",
  "docker-daemon": "Docker 引擎没有在跑，或连不上它的 API",
  "compose-missing": "宿主机上没有 Docker Compose（docker compose 插件或 docker-compose）",
  "no-working-dir": "项目没有指定工作目录，无从查找 compose 文件",
  "link-failed": "命令没能在宿主机上跑起来",
  "command-failed": "docker 命令执行失败",
};

export function dockerFailureText(reason: DockerUnavailableReason): string {
  return FAILURE_TEXT[reason];
}
