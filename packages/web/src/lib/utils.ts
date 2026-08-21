import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

/**
 * setInterval + 页面可见性门控：不可见时跳过（用户切去别的标签页时不该
 * 继续打后端，SSH 项目上每一轮都是真实网络往返），重新可见时立刻补一次。
 * 返回清理函数。
 */
export function pollWhileVisible(fn: () => void, ms: number): () => void {
  const tick = () => {
    if (!document.hidden) fn();
  };
  const timer = window.setInterval(tick, ms);
  document.addEventListener("visibilitychange", tick);
  return () => {
    clearInterval(timer);
    document.removeEventListener("visibilitychange", tick);
  };
}
