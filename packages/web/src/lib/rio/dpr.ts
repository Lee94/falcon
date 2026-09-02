/**
 * devicePixelRatio 变化监听：窗口在 Retina 与外接屏之间拖动、浏览器 ⌘+/- 缩放。
 *
 * 浏览器没有 dpr 事件，惯用法是对 `(resolution: Ndppx)` 建 MediaQueryList，
 * dpr 一变它就从 matches 变成不 matches 触发 change；因为查询里写死了旧值，
 * 触发后必须按新 dpr 重新武装一次。matchMedia 与 dpr 读取都注入，便于 node 测试。
 */

export interface MediaQueryLike {
  addEventListener(type: "change", listener: () => void): void;
  removeEventListener(type: "change", listener: () => void): void;
}

export interface DprEnv {
  matchMedia(query: string): MediaQueryLike;
  devicePixelRatio(): number;
}

export function dprQuery(dpr: number): string {
  return `(resolution: ${dpr}dppx)`;
}

export function browserDprEnv(): DprEnv {
  return {
    matchMedia: (query) => window.matchMedia(query),
    devicePixelRatio: () => window.devicePixelRatio || 1,
  };
}

/** 返回停止函数。onChange 在重新武装之后调用，回调里同步换渲染器也不会漏掉下一次变化。 */
export function watchDpr(onChange: (dpr: number) => void, env: DprEnv = browserDprEnv()): () => void {
  let stopped = false;
  let mql: MediaQueryLike | null = null;
  let handler: (() => void) | null = null;

  const arm = () => {
    const dpr = env.devicePixelRatio();
    const query = env.matchMedia(dprQuery(dpr));
    const onFire = () => {
      query.removeEventListener("change", onFire);
      if (stopped) return;
      const next = env.devicePixelRatio();
      arm();
      onChange(next);
    };
    query.addEventListener("change", onFire);
    mql = query;
    handler = onFire;
  };
  arm();

  return () => {
    stopped = true;
    if (mql && handler) mql.removeEventListener("change", handler);
    mql = null;
    handler = null;
  };
}
