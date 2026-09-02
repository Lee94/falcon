/**
 * 终端引擎微基准：xterm.js（WebGL addon）vs rio（自研 WebGPU）vs rio（rioterm 自带 canvas）。
 *
 * 走的是生产代码路径（createTermAdapter），所以只能在 vite dev 页面里跑：
 *   1. pnpm dev:web，浏览器打开 http://localhost:5173/（要 localhost 或 HTTPS，否则没有 WebGPU）
 *   2. 把本文件整个粘进 DevTools 控制台回车，然后：
 *        await benchTermEngines()                       // 三项默认全跑
 *        await benchTermEngines({ perTicks: [10, 20, 40], floods: false })
 *   3. 想看低端机，DevTools → Performance → CPU 4× slowdown 再跑一遍
 *
 * 三项测试（220×50，系统等宽字体 13px，3 轮取中位）：
 *   - log：10.4MB / 10 万行 ASCII 日志一次灌入，到"最后一帧画完"的墙钟时间（解析 + 一次渲染）
 *   - cjk：5.9MB / 5 万行中文，同上
 *   - tui：每个 rAF tick 写 perTick 帧 220×50 的全屏彩色重绘（每帧约 23KB），60 tick。
 *     ticksMs 是 60 个 tick 的墙钟（满帧 ≈ 1000ms），totalMs 是写完后再等引擎把积压画完。
 *     xterm 的 write 是异步的，会用推迟解析来保住 tick，所以要看 totalMs − ticksMs 的积压；
 *     rio 的 write 是同步的，画面永远是最新的，压力大时直接体现在 ticksMs 上。
 *
 * 2026-09-02 在 Mac mini（Apple Silicon，Chrome，dpr 2）的结果见 docs/adr/0005-rio-webgpu-renderer.md。
 */
window.benchTermEngines = async function benchTermEngines(opts = {}) {
  const { perTicks = [5, 20, 40], floods = true, rounds = 3, kinds = ["xterm", "rio-webgpu", "rio-canvas"] } = opts;
  const [{ createTermAdapter }, { resolveTermTheme, DEFAULT_TERM_PREF }] = await Promise.all([
    import("/src/lib/termAdapter.ts"),
    import("/src/lib/term.ts"),
  ]);
  const enc = new TextEncoder();
  const raf = () => new Promise((r) => requestAnimationFrame(r));
  const median = (xs) => {
    const s = [...xs].sort((a, b) => a - b);
    return s[Math.floor(s.length / 2)];
  };
  const COLS = 220;
  const ROWS = 50;
  const hex = (n) => ((n * 2654435761) >>> 0).toString(16).padStart(8, "0");

  const buildLog = (n) => {
    const parts = [];
    for (let i = 0; i < n; i++) {
      parts.push(
        `2026-09-02T12:${String(i % 60).padStart(2, "0")}:${String((i * 7) % 60).padStart(2, "0")}.${String(i % 1000).padStart(3, "0")}Z INFO  worker-${i % 32} request id=${hex(i)} path=/api/v1/items/${i} status=200 latency=${i % 97}ms\n`
      );
    }
    return enc.encode(parts.join(""));
  };
  const buildCjk = (n) => {
    const parts = [];
    for (let i = 0; i < n; i++) {
      parts.push(`第${i}行 中文日志测试 数据处理完成 耗时 ${i % 50} 毫秒 用户 张三 操作 更新配置 结果 成功 ${hex(i)}\n`);
    }
    return enc.encode(parts.join(""));
  };
  const buildFrames = (n) => {
    const frames = [];
    for (let f = 0; f < n; f++) {
      let s = "\x1b[H";
      for (let r = 0; r < ROWS; r++) {
        s += `\x1b[${r + 1};1H`;
        for (let seg = 0; seg < COLS / 10; seg++) {
          const color = ((seg * 7 + f * 13 + r * 3) % 216) + 16;
          s += `\x1b[38;5;${color}m`;
          s += "0123456789abcdef"[(f + r + seg) % 16].repeat(10);
        }
        s += "\x1b[0m";
      }
      frames.push(enc.encode(s));
    }
    return frames;
  };
  const logBytes = floods ? buildLog(100000) : null;
  const cjkBytes = floods ? buildCjk(50000) : null;
  const frames = buildFrames(20);
  const chunks = (bytes, size = 65536) => {
    const out = [];
    for (let i = 0; i < bytes.length; i += size) out.push(bytes.subarray(i, Math.min(bytes.length, i + size)));
    return out;
  };

  const theme = resolveTermTheme("one-dark", "dark");
  async function makeAdapter(kind) {
    const engine = kind === "xterm" ? "xterm" : "rio";
    // 走调试逃生口选 rio 的渲染器
    if (kind === "rio-canvas") localStorage.setItem("falcon.rio.renderer", "canvas");
    else if (kind === "rio-webgpu") localStorage.setItem("falcon.rio.renderer", "webgpu");
    const host = document.createElement("div");
    Object.assign(host.style, { position: "fixed", left: "0", top: "0", width: "1760px", height: "760px", zIndex: "99999", background: "#000" });
    document.body.appendChild(host);
    const pref = { ...DEFAULT_TERM_PREF, engine, fontId: "system", fontSize: 13, lineHeight: 1, cursorBlink: false };
    let readyResolve;
    const ready = new Promise((r) => (readyResolve = r));
    let rendererInfo = null;
    const adapter = createTermAdapter({
      pref,
      theme,
      scrollback: 10000,
      hooks: { onData() {}, onResize() {}, onReady() { readyResolve(); }, isGlobalKey: () => false, onRenderer(i) { rendererInfo = i; } },
    });
    adapter.open(host);
    if (engine === "xterm") readyResolve();
    await ready;
    // 把格子凑到 220×50：按实际 cell 尺寸反推容器
    for (let i = 0; i < 6; i++) {
      let ok = false;
      for (let k = 0; k < 30 && !(ok = adapter.fit()); k++) await raf();
      if (!ok) throw new Error(`${kind}: fit failed`);
      if (adapter.cols === COLS && adapter.rows === ROWS) break;
      host.style.width = `${Math.ceil((host.offsetWidth / adapter.cols) * COLS) + 1}px`;
      host.style.height = `${Math.ceil((host.offsetHeight / adapter.rows) * ROWS) + 1}px`;
    }
    localStorage.removeItem("falcon.rio.renderer");
    // xterm 的 write 异步：空 SGR 的回调等于"之前的都解析完了"；再等两个 rAF 让渲染器画出来
    const drain = async () => {
      if (engine === "xterm") await new Promise((r) => adapter.term.write("\x1b[0m", r));
      await raf();
      await raf();
    };
    return { adapter, host, drain, kind, info: () => rendererInfo, cols: adapter.cols, rows: adapter.rows };
  }
  const destroy = (t) => {
    t.adapter.dispose();
    t.host.remove();
  };

  async function flood(t, bytes) {
    t.adapter.reset();
    await t.drain();
    const t0 = performance.now();
    for (const c of chunks(bytes)) t.adapter.write(c);
    await t.drain();
    return performance.now() - t0;
  }
  async function tui(t, ticks, perTick) {
    t.adapter.reset();
    await t.drain();
    let f = 0;
    const t0 = performance.now();
    for (let i = 0; i < ticks; i++) {
      for (let k = 0; k < perTick; k++) t.adapter.write(frames[f++ % frames.length]);
      await raf();
    }
    const ticksMs = performance.now() - t0;
    await t.drain();
    return { ticksMs, totalMs: performance.now() - t0 };
  }

  const results = {};
  for (const kind of kinds) {
    const t = await makeAdapter(kind);
    const r = { grid: `${t.cols}x${t.rows}`, renderer: t.info()?.active ?? "webgl" };
    if (floods) {
      const logs = [];
      for (let i = 0; i < rounds; i++) logs.push(await flood(t, logBytes));
      r.logMs = Math.round(median(logs));
      const cjks = [];
      for (let i = 0; i < rounds; i++) cjks.push(await flood(t, cjkBytes));
      r.cjkMs = Math.round(median(cjks));
    }
    for (const perTick of perTicks) {
      const runs = [];
      for (let i = 0; i < rounds; i++) runs.push(await tui(t, 60, perTick));
      r[`tui${perTick}`] = {
        ticksMs: Math.round(median(runs.map((x) => x.ticksMs))),
        totalMs: Math.round(median(runs.map((x) => x.totalMs))),
      };
    }
    destroy(t);
    results[kind] = r;
    await raf();
  }
  const meta = { logMB: logBytes ? Math.round((logBytes.length / 1048576) * 10) / 10 : null, cjkMB: cjkBytes ? Math.round((cjkBytes.length / 1048576) * 10) / 10 : null, frameKB: Math.round(frames[0].length / 1024) };
  console.table(results);
  return { meta, results };
};
