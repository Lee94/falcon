/**
 * Zellij 版本锁定与发行产物映射。
 *
 * 锁定版本而非跟随 latest：测试矩阵封闭、可回归，升级是一次有意的发版行为。
 * 更新版本请用 `pnpm update-zellij <version>`，不要手改这里的常量。
 *
 * 不做完整性校验（无哈希常量）——走默认 GitHub 源时 HTTPS 已保证来源与完整性；
 * 配置了自定义 base URL 的用户会在 UI 上看到明确警告。
 */

export const ZELLIJ_VERSION = "0.44.3";

/** 官方发行地址；每台主机可在 zellij_hosts 表中覆盖 */
export const DEFAULT_BASE_URL = "https://github.com/zellij-org/zellij/releases/download";

/**
 * 用 no-web 变体：不需要 Zellij 自带的 web server，
 * 省约 8 MiB 体积，也少一个我们不需要的监听端口。
 */
const VARIANT = "no-web";

/** Zellij 官方发行的 target 三元组。没有 linux-gnu、freebsd、aarch64-windows。 */
export type ZellijTarget =
  | "x86_64-unknown-linux-musl"
  | "aarch64-unknown-linux-musl"
  | "x86_64-apple-darwin"
  | "aarch64-apple-darwin"
  | "x86_64-pc-windows-msvc";

export interface TargetInfo {
  target: ZellijTarget;
  /** Windows 为 zip，其余为 tar.gz */
  archive: "tar.gz" | "zip";
  windows: boolean;
}

export function targetInfo(target: ZellijTarget): TargetInfo {
  const windows = target === "x86_64-pc-windows-msvc";
  return { target, archive: windows ? "zip" : "tar.gz", windows };
}

/** 发行包文件名，如 zellij-no-web-x86_64-unknown-linux-musl.tar.gz */
export function assetName(target: ZellijTarget): string {
  const { archive } = targetInfo(target);
  return `zellij-${VARIANT}-${target}.${archive}`;
}

export function downloadUrl(target: ZellijTarget, baseUrl = DEFAULT_BASE_URL): string {
  return `${baseUrl.replace(/\/+$/, "")}/v${ZELLIJ_VERSION}/${assetName(target)}`;
}

/**
 * `uname -sm` 输出 → target。无匹配返回 null（调用方降级为非持久并标注架构不支持）。
 */
export function targetFromUname(uname: string): ZellijTarget | null {
  const [os, machine] = uname.trim().split(/\s+/);
  if (!os || !machine) return null;
  const arm = machine === "aarch64" || machine === "arm64";
  const x64 = machine === "x86_64" || machine === "amd64";

  if (os === "Linux") {
    if (x64) return "x86_64-unknown-linux-musl";
    if (arm) return "aarch64-unknown-linux-musl";
    return null;
  }
  if (os === "Darwin") {
    if (x64) return "x86_64-apple-darwin";
    if (arm) return "aarch64-apple-darwin";
    return null;
  }
  return null;
}

/**
 * Windows 的 %PROCESSOR_ARCHITECTURE% → target。
 * ARM64 也映射到 x86_64：Zellij 没有 Windows ARM64 产物（PR #5090/#5258 提了四个月未合），
 * 只能靠 Win11 的 x64 模拟跑，行不行由随后的 --version 握手裁决。
 */
export function targetFromWindowsArch(arch: string): ZellijTarget | null {
  const a = arch.trim().toUpperCase();
  if (a === "AMD64" || a === "ARM64" || a === "X86_64") return "x86_64-pc-windows-msvc";
  return null;
}

/** 后端所在机器（本地会话）的 target */
export function localTarget(): ZellijTarget | null {
  const { platform, arch } = process;
  if (platform === "win32") {
    // x64 与 arm64 都用 x64 产物，理由同 targetFromWindowsArch
    return arch === "x64" || arch === "arm64" ? "x86_64-pc-windows-msvc" : null;
  }
  if (platform === "linux") {
    if (arch === "x64") return "x86_64-unknown-linux-musl";
    if (arch === "arm64") return "aarch64-unknown-linux-musl";
    return null;
  }
  if (platform === "darwin") {
    if (arch === "x64") return "x86_64-apple-darwin";
    if (arch === "arm64") return "aarch64-apple-darwin";
    return null;
  }
  return null;
}

/** 二进制文件名（不含目录） */
export function binaryName(target: ZellijTarget): string {
  return targetInfo(target).windows
    ? `zellij-${ZELLIJ_VERSION}.exe`
    : `zellij-${ZELLIJ_VERSION}`;
}
