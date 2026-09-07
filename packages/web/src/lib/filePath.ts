/**
 * 文件面板的路径运算：把工作目录相对路径和宿主机绝对路径互相翻译，
 * 以及从文件夹上传的 webkitRelativePath 里抽出要 mkdir 的中间层。
 *
 * 前端不知道宿主机是 POSIX 还是 Windows，只能从 workingDir 的样子猜分隔符
 * （含反斜杠或盘符 → Windows）。相对路径在协议里一律 `/`。
 */

/** 工作目录相对路径 → 宿主机上显示用的绝对路径 */
export function joinHostPath(root: string | undefined, rel: string): string {
  if (!root) return rel ? rel : "/";
  const sep = hostSep(root);
  const base = root.replace(/[\\/]+$/, "");
  if (!rel) return base;
  return `${base}${sep}${rel.replaceAll("/", sep)}`;
}

/**
 * 路径栏里用户敲进去的字符串 → 工作目录相对路径。
 *
 * - 空 / 只有分隔符 → 工作目录本身（`""`）
 * - 以 workingDir 为前缀的绝对路径 → 去掉前缀
 * - 看起来像相对路径 → 原样（分隔符收成 `/`）
 * - 绝对路径但不在工作目录里、或含 `..` → null（拒绝越界）
 */
export function parseNavPath(input: string, root: string | undefined): string | null {
  const trimmed = input.trim();
  if (!trimmed || trimmed === "/" || trimmed === "\\") return "";
  const windows = isWindowsRoot(root);
  const normalized = (windows ? trimmed.replaceAll("/", "\\") : trimmed.replaceAll("\\", "/")).replace(
    /[\\/]+$/,
    ""
  );

  if (root) {
    const base = root.replace(/[\\/]+$/, "");
    const baseCmp = windows ? base.toLowerCase() : base;
    const pathCmp = windows ? normalized.toLowerCase() : normalized;
    if (pathCmp === baseCmp) return "";
    const prefix = `${baseCmp}${windows ? "\\" : "/"}`;
    if (pathCmp.startsWith(prefix)) {
      const rest = normalized.slice(base.length + 1);
      return toRel(rest, windows);
    }
    // 绝对路径但不在工作目录里
    if (isAbsoluteInput(normalized, windows)) return null;
  } else if (isAbsoluteInput(normalized, false) || isAbsoluteInput(normalized, true)) {
    // 没有 workingDir 可对照时，把开头的 / 当成工作目录根（`/src` → src）
    if (normalized.startsWith("/")) return toRel(normalized.slice(1), false);
    if (/^[A-Za-z]:[\\/]/.test(normalized)) return null;
  }

  return toRel(normalized.replace(/^[\\/]+/, ""), windows);
}

export function parentRel(rel: string): string | null {
  if (!rel) return null;
  const i = rel.lastIndexOf("/");
  return i < 0 ? "" : rel.slice(0, i);
}

export function relDir(rel: string): string {
  const i = rel.lastIndexOf("/");
  return i < 0 ? "" : rel.slice(0, i);
}

export function isHiddenName(name: string): boolean {
  return name.startsWith(".");
}

/**
 * 文件夹上传：每个文件的 webkitRelativePath 形如 `src/lib/a.ts`，
 * 中间层 `src`、`src/lib` 得先 mkdir。返回去重后最深的那一批——mkdir -p
 * 一次就能带上祖先。
 */
export function deepestUploadDirs(cwd: string, relativePaths: string[]): string[] {
  const dirs = new Set<string>();
  for (const rel of relativePaths) {
    const segs = rel.split("/").filter(Boolean);
    segs.pop();
    let acc = cwd;
    for (const s of segs) {
      acc = acc ? `${acc}/${s}` : s;
      dirs.add(acc);
    }
  }
  const all = [...dirs].sort();
  return all.filter((d) => !all.some((other) => other.startsWith(`${d}/`)));
}

export function formatMtime(sec: number | undefined): string {
  if (sec == null || !Number.isFinite(sec)) return "—";
  const d = new Date(sec * 1000);
  if (Number.isNaN(d.getTime())) return "—";
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

export function formatSize(n: number | undefined): string {
  if (n == null || !Number.isFinite(n)) return "—";
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

function hostSep(root: string): "\\" | "/" {
  return isWindowsRoot(root) ? "\\" : "/";
}

function isWindowsRoot(root: string | undefined): boolean {
  if (!root) return false;
  return /^[A-Za-z]:[\\/]/.test(root) || (root.includes("\\") && !root.includes("/"));
}

function isAbsoluteInput(path: string, windows: boolean): boolean {
  if (windows) return /^[A-Za-z]:[\\/]/.test(path) || path.startsWith("\\\\");
  return path.startsWith("/");
}

function toRel(path: string, windows: boolean): string | null {
  const segs = path.split(windows ? /[\\/]+/ : "/").filter((s) => s.length > 0);
  for (const s of segs) {
    if (s === "." || s === "..") return null;
  }
  return segs.join("/");
}
