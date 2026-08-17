/**
 * 明暗主题。
 *
 * 三档偏好：跟随系统 / 浅色 / 深色。落到 <html> 上就两种：有没有 `.dark`。
 * class 由 index.html 的内联脚本先写一次（首帧就是对的，不闪白），这里只负责
 * 后续的切换与"跟随系统"时的实时响应——两处必须用同一个 key 和同一套逻辑。
 */

export type ThemePref = "system" | "light" | "dark";
export type ThemeMode = "light" | "dark";

export const THEME_KEY = "mojito.theme";

const darkQuery =
  typeof window !== "undefined" && window.matchMedia
    ? window.matchMedia("(prefers-color-scheme: dark)")
    : null;

export function loadThemePref(): ThemePref {
  try {
    const raw = localStorage.getItem(THEME_KEY);
    if (raw === "light" || raw === "dark" || raw === "system") return raw;
  } catch {
    // 隐私模式下读不到，跟随系统即可
  }
  return "system";
}

export function saveThemePref(pref: ThemePref): void {
  try {
    localStorage.setItem(THEME_KEY, pref);
  } catch {
    // 写不进去就只在本次页面生效，不影响使用
  }
}

export function resolveTheme(pref: ThemePref): ThemeMode {
  if (pref !== "system") return pref;
  return darkQuery?.matches ? "dark" : "light";
}

/**
 * 落到 DOM。只切这一个 class：底色、语义色、以及浏览器原生滚动条 / 表单控件
 * 认的 color-scheme，全都挂在样式表的 `:root` / `.dark` 上，不在这里重复一份。
 */
export function applyTheme(mode: ThemeMode): void {
  document.documentElement.classList.toggle("dark", mode === "dark");
}

/** 仅在偏好为 system 时有意义：系统切换明暗时回调 */
export function watchSystemTheme(onChange: (mode: ThemeMode) => void): () => void {
  if (!darkQuery) return () => undefined;
  const handler = (e: MediaQueryListEvent) => onChange(e.matches ? "dark" : "light");
  darkQuery.addEventListener("change", handler);
  return () => darkQuery.removeEventListener("change", handler);
}

