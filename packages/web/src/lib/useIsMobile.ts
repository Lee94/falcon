import { useSyncExternalStore } from "react";

/**
 * 移动壳的断点。767 而不是复用 1023（那只是自动收侧栏，桌面布局仍成立）：
 * 换成移动壳意味着 TabBar / 侧栏 / 右栏整个不渲染，只留终端和键位条。
 *
 * 跨过断点时 TerminalView 会随布局树卸载重挂（WS 重连 + replay 恢复画面），
 * 与切换终端引擎同一条已接受的代价——真机上不会来回横跳，只有开发者
 * 拖窗口 / 开 devtools 设备模拟才会。
 */
const QUERY = "(max-width: 767px)";

const mq =
  typeof window !== "undefined" && window.matchMedia ? window.matchMedia(QUERY) : null;

function subscribe(onChange: () => void): () => void {
  if (!mq) return () => undefined;
  mq.addEventListener("change", onChange);
  // resize 兜底：部分环境（远程桌面、devtools 设备模拟）不补发 mq 的 change
  window.addEventListener("resize", onChange);
  return () => {
    mq.removeEventListener("change", onChange);
    window.removeEventListener("resize", onChange);
  };
}

function snapshot(): boolean {
  return mq?.matches ?? false;
}

export function useIsMobile(): boolean {
  return useSyncExternalStore(subscribe, snapshot);
}
