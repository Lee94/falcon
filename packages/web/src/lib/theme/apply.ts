/**
 * 主题落到 DOM。只有这一个文件碰 document / matchMedia。
 *
 * 全部 token 以自定义属性写在 <html> 的内联 style 上，压过样式表 `:root` 里的
 * 兜底值；`.dark` 只按主题的实际深浅切（不按明暗模式——给浅色槽位选了深底主题，
 * 界面就该按深色算），它带动 color-scheme（原生滚动条 / 表单控件）、Tailwind 的
 * `dark:` 变体与 sonner 的明暗。
 *
 * index.html 的内联脚本在首帧之前只写 --background / --foreground 与 .dark
 * （那时 body 还是空的，别的看不见），这里 React 起来后再整套落一遍。
 */

import type { ResolvedTheme } from "./derive.js";

let appliedKeys: string[] = [];

export function applyThemeToDom(theme: ResolvedTheme): void {
  const root = document.documentElement;
  const style = root.style;
  // 上一套主题写过、这一套没有的键要清掉，否则残留的值会一直压着样式表
  for (const key of appliedKeys) {
    if (!(key in theme.vars)) style.removeProperty(key);
  }
  for (const [key, value] of Object.entries(theme.vars)) style.setProperty(key, value);
  appliedKeys = Object.keys(theme.vars);

  root.classList.toggle("dark", theme.appearance === "dark");

  // 安装后的标题栏 / 状态栏颜色跟底色走
  let meta = document.querySelector('meta[name="theme-color"]');
  if (!meta) {
    meta = document.createElement("meta");
    meta.setAttribute("name", "theme-color");
    document.head.appendChild(meta);
  }
  meta.setAttribute("content", theme.colors.background);
}

const darkQuery =
  typeof window !== "undefined" && window.matchMedia
    ? window.matchMedia("(prefers-color-scheme: dark)")
    : null;

export function systemPrefersDark(): boolean {
  return darkQuery?.matches ?? false;
}

/** 系统切换明暗时回调；只有明暗模式是"跟随系统"时调用方才该响应 */
export function watchSystemTheme(onChange: (dark: boolean) => void): () => void {
  if (!darkQuery) return () => undefined;
  const handler = (e: MediaQueryListEvent) => onChange(e.matches);
  darkQuery.addEventListener("change", handler);
  return () => darkQuery.removeEventListener("change", handler);
}
