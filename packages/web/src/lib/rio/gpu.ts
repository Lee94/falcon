/**
 * WebGPU 设备获取与渲染器选择。
 *
 * 一个页面只要一个 GPUDevice：pipeline 与 bind group layout 只依赖 canvas format
 * （同一页面恒定），全部放在设备上按 key 共享；每个终端各自 configure 自己的 canvas。
 * device.lost 一旦 settle 就清掉单例，下一次 acquireGpu 重新走 requestAdapter——
 * 同一个 adapter 只能 requestDevice 一次，重试必须从 adapter 起。
 *
 * "没有 WebGPU"最常见的原因不是老浏览器，是非 secure context：这个项目常用
 * http://<局域网 IP>:4923 访问，那时 navigator.gpu 直接是 undefined，所以先查
 * isSecureContext 而不是等 requestAdapter 返回 null。
 */

export type RioRendererKind = "webgpu" | "canvas";

export type WebGpuFailReason =
  /** navigator.gpu 不存在（含非 secure context） */
  | "unsupported"
  /** requestAdapter() 返回 null 或只有软件回退适配器 */
  | "no-adapter"
  /** requestDevice() reject */
  | "device-rejected"
  /** canvas.getContext("webgpu") 返回 null */
  | "no-context"
  /** device.lost 触发且原地重建失败 */
  | "lost";

/** 真 GPU / GPUAdapter / GPUDevice 的最小子集，测试里手搓假对象即可 */
export interface DeviceLike {
  readonly lost: Promise<GPUDeviceLostInfo>;
}
export interface AdapterLike {
  requestDevice(): Promise<DeviceLike>;
  /** 新规范把 isFallbackAdapter 挪进了 info；两处都看 */
  readonly info?: { readonly isFallbackAdapter?: boolean };
  readonly isFallbackAdapter?: boolean;
}
export interface GpuLike {
  requestAdapter(): Promise<AdapterLike | null>;
  getPreferredCanvasFormat(): GPUTextureFormat;
}

export interface GpuEnv {
  isSecureContext: boolean;
  gpu: GpuLike | undefined;
}

export function browserGpuEnv(): GpuEnv {
  return {
    isSecureContext: globalThis.isSecureContext ?? false,
    gpu: (globalThis.navigator as Navigator | undefined)?.gpu,
  };
}

export function webgpuAvailable(env: GpuEnv): boolean {
  return env.isSecureContext && env.gpu !== undefined;
}

export interface GpuHandle {
  readonly device: GPUDevice;
  readonly format: GPUTextureFormat;
  readonly lost: Promise<GPUDeviceLostInfo>;
  /** 每设备共享对象（pipeline / layout）按 key 懒建；换设备整张表作废 */
  readonly shared: Map<string, unknown>;
}

export type GpuResult = { ok: true; gpu: GpuHandle } | { ok: false; reason: WebGpuFailReason };

let current: GpuHandle | null = null;
let pending: Promise<GpuResult> | null = null;

function isFallback(adapter: AdapterLike): boolean {
  return adapter.info?.isFallbackAdapter === true || adapter.isFallbackAdapter === true;
}

async function acquire(env: GpuEnv): Promise<GpuResult> {
  if (!webgpuAvailable(env)) return { ok: false, reason: "unsupported" };
  const gpu = env.gpu!;
  let adapter: AdapterLike | null;
  try {
    adapter = await gpu.requestAdapter();
  } catch {
    adapter = null;
  }
  // SwiftShader 之类的软件适配器比 canvas 2D 还慢，当不可用
  if (!adapter || isFallback(adapter)) return { ok: false, reason: "no-adapter" };
  let device: DeviceLike;
  try {
    device = await adapter.requestDevice();
  } catch {
    return { ok: false, reason: "device-rejected" };
  }
  const handle: GpuHandle = {
    device: device as GPUDevice,
    format: gpu.getPreferredCanvasFormat(),
    lost: device.lost,
    shared: new Map(),
  };
  void device.lost.then(
    () => {
      if (current === handle) {
        current = null;
        pending = null;
      }
    },
    () => undefined
  );
  current = handle;
  return { ok: true, gpu: handle };
}

/** 页面级单例。失败不缓存（下次打开终端再试一次，代价可忽略）。 */
export function acquireGpu(env: GpuEnv = browserGpuEnv()): Promise<GpuResult> {
  if (current) return Promise.resolve({ ok: true, gpu: current });
  pending ??= acquire(env).then((res) => {
    if (!res.ok) pending = null;
    return res;
  });
  return pending;
}

/** 测试用：丢掉单例 */
export function resetAcquiredGpu(): void {
  current = null;
  pending = null;
}

export function getShared<T>(gpu: GpuHandle, key: string, create: () => T): T {
  const hit = gpu.shared.get(key);
  if (hit !== undefined) return hit as T;
  const made = create();
  gpu.shared.set(key, made);
  return made;
}

export function pickRenderer(
  requested: RioRendererKind,
  gpu: GpuResult | null
): { kind: RioRendererKind; reason?: WebGpuFailReason } {
  if (requested === "canvas") return { kind: "canvas" };
  if (!gpu) return { kind: "canvas", reason: "unsupported" };
  if (!gpu.ok) return { kind: "canvas", reason: gpu.reason };
  return { kind: "webgpu" };
}

/**
 * 调试逃生口（无 UI）：`localStorage["falcon.rio.renderer"] = "canvas" | "webgpu"`
 * 覆盖自动选择。WebGPU 花屏但不抛异常时，用户不必整个切回 xterm。
 */
export const RIO_RENDERER_OVERRIDE_KEY = "falcon.rio.renderer";

export function readRendererOverride(
  storage: { getItem(key: string): string | null } | undefined
): RioRendererKind | null {
  try {
    const v = storage?.getItem(RIO_RENDERER_OVERRIDE_KEY);
    return v === "canvas" || v === "webgpu" ? v : null;
  } catch {
    return null;
  }
}
