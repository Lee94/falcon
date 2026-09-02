import assert from "node:assert/strict";
import { beforeEach, describe, it } from "node:test";
import {
  acquireGpu,
  pickRenderer,
  readRendererOverride,
  resetAcquiredGpu,
  webgpuAvailable,
  type AdapterLike,
  type DeviceLike,
  type GpuEnv,
  type GpuLike,
} from "./gpu.js";

interface FakeGpuOptions {
  adapter?: "none" | "fallback" | "ok";
  device?: "ok" | "reject";
}

function fakeGpu(opts: FakeGpuOptions = {}) {
  let resolveLost: (info: GPUDeviceLostInfo) => void = () => undefined;
  const lost = new Promise<GPUDeviceLostInfo>((res) => {
    resolveLost = res;
  });
  const device: DeviceLike = { lost };
  const adapter: AdapterLike = {
    requestDevice: () =>
      opts.device === "reject" ? Promise.reject(new Error("nope")) : Promise.resolve(device),
    info: { isFallbackAdapter: opts.adapter === "fallback" },
  };
  const calls = { requestAdapter: 0 };
  const gpu: GpuLike = {
    requestAdapter: () => {
      calls.requestAdapter++;
      return Promise.resolve(opts.adapter === "none" ? null : adapter);
    },
    getPreferredCanvasFormat: () => "bgra8unorm",
  };
  const env: GpuEnv = { isSecureContext: true, gpu };
  return { env, calls, loseDevice: () => resolveLost({ reason: "unknown", message: "" } as GPUDeviceLostInfo) };
}

describe("webgpuAvailable", () => {
  it("非 secure context（http://局域网 IP）直接不可用", () => {
    assert.equal(webgpuAvailable({ isSecureContext: false, gpu: fakeGpu().env.gpu }), false);
    assert.equal(webgpuAvailable({ isSecureContext: true, gpu: undefined }), false);
    assert.equal(webgpuAvailable(fakeGpu().env), true);
  });
});

describe("acquireGpu", () => {
  beforeEach(() => resetAcquiredGpu());

  it("unsupported / no-adapter / fallback adapter / device-rejected 各归一个原因", async () => {
    assert.deepEqual(await acquireGpu({ isSecureContext: false, gpu: fakeGpu().env.gpu }), {
      ok: false,
      reason: "unsupported",
    });
    resetAcquiredGpu();
    assert.deepEqual(await acquireGpu(fakeGpu({ adapter: "none" }).env), { ok: false, reason: "no-adapter" });
    resetAcquiredGpu();
    assert.deepEqual(await acquireGpu(fakeGpu({ adapter: "fallback" }).env), { ok: false, reason: "no-adapter" });
    resetAcquiredGpu();
    assert.deepEqual(await acquireGpu(fakeGpu({ device: "reject" }).env), { ok: false, reason: "device-rejected" });
  });

  it("成功后是单例：第二次不再 requestAdapter", async () => {
    const f = fakeGpu();
    const a = await acquireGpu(f.env);
    const b = await acquireGpu(f.env);
    assert.equal(a.ok, true);
    assert.equal(b.ok && a.ok && a.gpu === b.gpu, true);
    assert.equal(f.calls.requestAdapter, 1);
    if (a.ok) assert.equal(a.gpu.format, "bgra8unorm");
  });

  it("失败不缓存：下一次会重试", async () => {
    const f = fakeGpu({ adapter: "none" });
    await acquireGpu(f.env);
    await acquireGpu(f.env);
    assert.equal(f.calls.requestAdapter, 2);
  });

  it("device lost 之后重新走 requestAdapter", async () => {
    const f = fakeGpu();
    const a = await acquireGpu(f.env);
    f.loseDevice();
    // lost 的 then 回调在微任务里跑
    await new Promise((r) => setTimeout(r, 0));
    const b = await acquireGpu(f.env);
    assert.equal(f.calls.requestAdapter, 2);
    assert.equal(a.ok && b.ok && a.gpu !== b.gpu, true);
  });
});

describe("pickRenderer", () => {
  it("请求 canvas 永远 canvas；请求 webgpu 看结果", () => {
    assert.deepEqual(pickRenderer("canvas", null), { kind: "canvas" });
    assert.deepEqual(pickRenderer("webgpu", null), { kind: "canvas", reason: "unsupported" });
    assert.deepEqual(pickRenderer("webgpu", { ok: false, reason: "no-adapter" }), {
      kind: "canvas",
      reason: "no-adapter",
    });
    const gpu = { device: {} as GPUDevice, format: "bgra8unorm" as const, lost: new Promise<GPUDeviceLostInfo>(() => undefined), shared: new Map() };
    assert.deepEqual(pickRenderer("webgpu", { ok: true, gpu }), { kind: "webgpu" });
  });
});

describe("readRendererOverride", () => {
  it("只认两个合法值，storage 缺失或抛异常都当没有", () => {
    assert.equal(readRendererOverride(undefined), null);
    assert.equal(readRendererOverride({ getItem: () => "canvas" }), "canvas");
    assert.equal(readRendererOverride({ getItem: () => "webgpu" }), "webgpu");
    assert.equal(readRendererOverride({ getItem: () => "dom" }), null);
    assert.equal(
      readRendererOverride({
        getItem: () => {
          throw new Error("SecurityError");
        },
      }),
      null
    );
  });
});
