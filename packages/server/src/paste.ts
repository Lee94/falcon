/**
 * 粘贴图片：把浏览器剪贴板里的图片写到会话宿主机上，返回绝对路径。
 * 前端随后把路径粘进终端输入——Claude Code 等 TUI 认"输入框里的图片路径"。
 *
 * 落脚点统一是 mojito 根目录下的 paste/（本地 = --data-dir，远端 = ~/.mojito），
 * 与 Zellij 布局同根，卸载时一并带走。SSH 侧不走 SFTP（理由同 fs.ts）：
 * POSIX 用 `cat > file` 收 stdin 原始字节；Windows 的 exec 通道对二进制
 * 不可靠，改收 base64 再在 PowerShell 里解码。
 */
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { joinPath } from "./git/path.js";
import {
  encodePowerShell,
  quotePosix,
  quotePowerShell,
  type HostKind,
} from "./zellij/host.js";

/** 超过这个岁数的旧图在下一次粘贴时顺手删掉；正常粘贴当场就被读走了 */
const MAX_AGE_HOURS = 24;

const IMAGE_EXTENSIONS: Record<string, string> = {
  "image/png": "png",
  "image/jpeg": "jpg",
  "image/gif": "gif",
  "image/webp": "webp",
};

/** Content-Type → 扩展名；认不出的类型返回 null（不落盘来路不明的字节） */
export function imageExt(contentType: string | undefined): string | null {
  if (!contentType) return null;
  const mime = contentType.split(";")[0].trim().toLowerCase();
  return IMAGE_EXTENSIONS[mime] ?? null;
}

/** root 是宿主机上的 mojito 根目录（remoteRoot 或本地 dataDir） */
export function pasteDir(kind: HostKind, root: string): string {
  return joinPath(kind, root, "paste");
}

export function pasteFileName(ext: string): string {
  return `img-${crypto.randomUUID().slice(0, 8)}.${ext}`;
}

/**
 * POSIX：建目录、清旧图、stdin 原始字节写入。
 * find 缺 -mmin（极老的 busybox）时静默跳过清理，`;` 保证 cat 照常跑；
 * 整条命令的退出码就是 cat 的——mkdir 失败时重定向也会失败，同样非零。
 */
export function posixWriteCommand(dir: string, filePath: string): string {
  const d = quotePosix(dir);
  return (
    `d=${d}; mkdir -p "$d" && ` +
    `find "$d" -maxdepth 1 -type f -mmin +${MAX_AGE_HOURS * 60} -delete 2>/dev/null; ` +
    `cat > ${quotePosix(filePath)}`
  );
}

/**
 * Windows：stdin 收 base64 文本（OpenSSH for Windows 的 exec 通道会按代码页
 * 改写字节，原始二进制过不去），PowerShell 解码后 WriteAllBytes。
 * 写失败抛异常 → -EncodedCommand 退 1，stderr 里有信息。
 */
export function windowsWriteCommand(dir: string, filePath: string): string {
  return encodePowerShell(
    [
      `$d = ${quotePowerShell(dir)}`,
      `New-Item -ItemType Directory -Force -Path $d | Out-Null`,
      `Get-ChildItem -LiteralPath $d -File -EA SilentlyContinue | ` +
        `Where-Object { $_.LastWriteTime -lt (Get-Date).AddHours(-${MAX_AGE_HOURS}) } | ` +
        `Remove-Item -Force -EA SilentlyContinue`,
      `$b = [Convert]::FromBase64String([Console]::In.ReadToEnd())`,
      `[IO.File]::WriteAllBytes(${quotePowerShell(filePath)}, $b)`,
    ].join("; ")
  );
}

/** 本地会话：直接 node:fs 写进 <dataDir>/paste，清理逻辑与远端一致 */
export async function writeLocalPasteFile(
  dataDir: string,
  ext: string,
  data: Buffer
): Promise<string> {
  const dir = path.join(dataDir, "paste");
  await fs.promises.mkdir(dir, { recursive: true });

  const cutoff = Date.now() - MAX_AGE_HOURS * 3600 * 1000;
  const names = await fs.promises.readdir(dir).catch(() => [] as string[]);
  for (const name of names) {
    const full = path.join(dir, name);
    try {
      const stat = await fs.promises.stat(full);
      if (stat.isFile() && stat.mtimeMs < cutoff) await fs.promises.unlink(full);
    } catch {
      // 清理是顺手的事，失败不挡写入
    }
  }

  const file = path.join(dir, pasteFileName(ext));
  await fs.promises.writeFile(file, data);
  return file;
}
