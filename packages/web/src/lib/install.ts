/**
 * Chrome「安装应用」：捕获 beforeinstallprompt，供设置 / 命令面板主动调起。
 *
 * 可安装条件本身由 manifest + 图标 + 安全源（HTTPS / localhost）决定；
 * 这里只负责浏览器已经认定可安装之后的交互，不缓存任何资源。
 */

export type InstallOutcome = "accepted" | "dismissed" | "unavailable";

export interface BeforeInstallPromptLike {
  preventDefault(): void;
  prompt(): Promise<void>;
  userChoice: Promise<{ outcome: "accepted" | "dismissed" }>;
}

export type StandaloneWindow = {
  matchMedia?: (query: string) => { matches: boolean };
  navigator?: { standalone?: boolean };
};

const STANDALONE_MODES = [
  "(display-mode: standalone)",
  "(display-mode: minimal-ui)",
  "(display-mode: window-controls-overlay)",
  "(display-mode: fullscreen)",
] as const;

export function isStandalone(win: StandaloneWindow = globalThis as StandaloneWindow): boolean {
  if (win.navigator?.standalone === true) return true;
  return STANDALONE_MODES.some((query) => win.matchMedia?.(query).matches === true);
}

const INSTALLABLE_DISPLAY = new Set([
  "standalone",
  "fullscreen",
  "minimal-ui",
  "window-controls-overlay",
]);

/** Chrome 桌面安装清单的最低字段。用来锁住 manifest 不被改丢。 */
export function meetsChromeInstallManifest(value: unknown): boolean {
  if (!value || typeof value !== "object") return false;
  const manifest = value as Record<string, unknown>;
  const named =
    (typeof manifest.name === "string" && manifest.name.length > 0) ||
    (typeof manifest.short_name === "string" && manifest.short_name.length > 0);
  if (!named) return false;
  if (typeof manifest.start_url !== "string" || manifest.start_url.length === 0) return false;
  if (!INSTALLABLE_DISPLAY.has(manifest.display as string)) return false;
  if (manifest.prefer_related_applications === true) return false;

  const sizes = new Set<string>();
  const icons = Array.isArray(manifest.icons) ? manifest.icons : [];
  for (const icon of icons) {
    if (!icon || typeof icon !== "object") continue;
    const entry = icon as Record<string, unknown>;
    if (typeof entry.src !== "string" || entry.src.length === 0) continue;
    const purpose = typeof entry.purpose === "string" ? entry.purpose : "any";
    if (!purpose.split(/\s+/).includes("any")) continue;
    if (typeof entry.sizes !== "string") continue;
    for (const size of entry.sizes.split(/\s+/)) sizes.add(size);
  }
  return sizes.has("192x192") && sizes.has("512x512");
}

type Listener = () => void;

const listeners = new Set<Listener>();
let deferred: BeforeInstallPromptLike | null = null;

function emit(): void {
  for (const listener of listeners) listener();
}

export function canInstall(): boolean {
  return deferred !== null;
}

export function subscribeInstall(listener: Listener): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function handleBeforeInstallPrompt(event: BeforeInstallPromptLike): void {
  event.preventDefault();
  deferred = event;
  emit();
}

export function handleAppInstalled(): void {
  deferred = null;
  emit();
}

/** 测试用：清掉已捕获的事件，避免用例互相污染 */
export function resetInstallPrompt(): void {
  deferred = null;
}

export async function promptInstall(): Promise<InstallOutcome> {
  if (!deferred) return "unavailable";
  const event = deferred;
  deferred = null;
  emit();
  await event.prompt();
  const { outcome } = await event.userChoice;
  emit();
  return outcome;
}

export function initInstallListeners(
  target: Pick<EventTarget, "addEventListener" | "removeEventListener"> = window
): () => void {
  const onPrompt = (event: Event) => {
    handleBeforeInstallPrompt(event as unknown as BeforeInstallPromptLike);
  };
  const onInstalled = () => handleAppInstalled();
  target.addEventListener("beforeinstallprompt", onPrompt);
  target.addEventListener("appinstalled", onInstalled);
  return () => {
    target.removeEventListener("beforeinstallprompt", onPrompt);
    target.removeEventListener("appinstalled", onInstalled);
  };
}
