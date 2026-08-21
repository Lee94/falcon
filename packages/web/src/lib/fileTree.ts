/**
 * 把一串文件路径拼成目录树。纯函数，不碰 DOM。
 *
 * 「修改」面板与 History 的提交详情共用它——两处列的都是"一组改动文件"，
 * 只是来源不同。
 */

/** 树里的一个文件。调用方带什么额外字段（增删行数、状态）都行，泛型透传 */
export interface TreeFile<T> {
  kind: "file";
  /** 展示名：路径最后一段 */
  name: string;
  /** 仓库根相对的完整路径，也当 React key 用 */
  path: string;
  item: T;
}

export interface TreeDir<T> {
  kind: "dir";
  /**
   * 展示名。单子目录链会被压成一格，所以这里可能是 `server/src` 而不是 `src`
   * ——见 compact 的注释。
   */
  name: string;
  /** 这一层的完整路径前缀，当 React key 与展开状态的键用 */
  path: string;
  children: TreeNode<T>[];
  /** 子树里的文件总数（含更深层） */
  fileCount: number;
}

export type TreeNode<T> = TreeFile<T> | TreeDir<T>;

interface Draft<T> {
  dirs: Map<string, Draft<T>>;
  files: { name: string; path: string; item: T }[];
}

/**
 * 建树。
 *
 * **单子目录链会被压缩成一格**：`packages/server/src/git/x.ts` 里的
 * `packages` 只有一个子目录、`server` 也只有一个，于是显示成
 * `packages` → `server/src` → `git`，而不是每层一格缩进。一个只有几个
 * 改动文件的仓库，不压缩的话大半个面板都在画空目录的缩进。
 *
 * 顺序：目录在前、文件在后，各自按名字排（localeCompare，中文路径才不会
 * 按码位乱序）。git 给的顺序本身是按路径排的，但树化之后必须重排。
 */
export function buildFileTree<T>(
  items: T[],
  pathOf: (item: T) => string
): TreeNode<T>[] {
  const root: Draft<T> = { dirs: new Map(), files: [] };

  for (const item of items) {
    const path = pathOf(item);
    const parts = path.split("/").filter(Boolean);
    if (parts.length === 0) continue;
    const name = parts[parts.length - 1]!;
    let cur = root;
    for (const seg of parts.slice(0, -1)) {
      let next = cur.dirs.get(seg);
      if (!next) {
        next = { dirs: new Map(), files: [] };
        cur.dirs.set(seg, next);
      }
      cur = next;
    }
    cur.files.push({ name, path, item });
  }

  return finish(root, "");
}

function finish<T>(draft: Draft<T>, prefix: string): TreeNode<T>[] {
  const dirs: TreeDir<T>[] = [];
  for (const [name, sub] of draft.dirs) {
    dirs.push(compact(name, sub, prefix ? `${prefix}/${name}` : name));
  }
  dirs.sort((a, b) => a.name.localeCompare(b.name));
  const files: TreeFile<T>[] = draft.files
    .map((f) => ({ kind: "file" as const, name: f.name, path: f.path, item: f.item }))
    .sort((a, b) => a.name.localeCompare(b.name));
  return [...dirs, ...files];
}

/** 只有一个子目录、且本层没有文件时，把两层并成一格：`server` + `src` → `server/src` */
function compact<T>(name: string, draft: Draft<T>, path: string): TreeDir<T> {
  let label = name;
  let cur = draft;
  let full = path;
  while (cur.files.length === 0 && cur.dirs.size === 1) {
    const [childName, child] = [...cur.dirs.entries()][0]!;
    label = `${label}/${childName}`;
    full = `${full}/${childName}`;
    cur = child;
  }
  const children = finish(cur, full);
  return {
    kind: "dir",
    name: label,
    path: full,
    children,
    fileCount: countFiles(children),
  };
}

function countFiles<T>(nodes: TreeNode<T>[]): number {
  let n = 0;
  for (const node of nodes) n += node.kind === "file" ? 1 : node.fileCount;
  return n;
}

/**
 * 列表视图里每个文件显示的目录（文件名右边那截灰字）。
 * 顶层文件没有目录，返回空串。
 */
export function dirOf(path: string): string {
  const i = path.lastIndexOf("/");
  return i < 0 ? "" : path.slice(0, i);
}

/** 路径最后一段 */
export function baseOf(path: string): string {
  const i = path.lastIndexOf("/");
  return i < 0 ? path : path.slice(i + 1);
}
