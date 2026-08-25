import { Fragment, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { ChevronRight, FolderTree, List } from "lucide-react";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { baseOf, buildFileTree, dirOf, type TreeNode } from "@/lib/fileTree";

/**
 * 一组改动文件的展示，列表 / 目录树两种视图。
 *
 * 「修改」面板与 History 的提交详情共用：两处列的都是"一组改动文件"，
 * 只是来源不同（工作区 vs 某条提交）。所以这里只认一个中立的形状，
 * 谁调用谁负责把自己的数据翻译过来。
 */
export interface FileChangeItem {
  /** 仓库根相对路径，也当 key */
  path: string;
  /** 重命名 / 复制前的路径 */
  origPath?: string;
  /** 单字母状态：A M D R C T ?，决定颜色与提示文案 */
  status: string;
  /** 二进制文件或算不出来时为 null——那与"改了 0 行"是两回事 */
  added: number | null;
  deleted: number | null;
}

/**
 * 勾选。给了才画 checkbox 列——History 的提交详情不需要选，
 * 「修改」面板才要（选中的那些进下一次提交）。
 */
export interface FileSelection {
  isSelected: (path: string) => boolean;
  /** 目录行会把整棵子树的路径一次传进来 */
  toggle: (paths: string[], next: boolean) => void;
}

export type FileViewMode = "list" | "tree";

const MODE_KEY = "falcon.fileView";

export function loadFileViewMode(): FileViewMode {
  try {
    return localStorage.getItem(MODE_KEY) === "tree" ? "tree" : "list";
  } catch {
    return "list";
  }
}

export function saveFileViewMode(mode: FileViewMode) {
  try {
    localStorage.setItem(MODE_KEY, mode);
  } catch {
    // 隐私模式下写不进去，不影响本次会话
  }
}

/** 列表 / 目录树的切换按钮。两个面板的工具条各自摆放，所以单独导出 */
export function FileViewToggle({
  mode,
  onChange,
}: {
  mode: FileViewMode;
  onChange: (next: FileViewMode) => void;
}) {
  const { t } = useTranslation();
  return (
    <>
      <Button
        variant="ghost"
        size="icon-xs"
        className={cn("text-muted-foreground", mode === "list" && "bg-accent text-foreground")}
        aria-label={t("git.viewList")}
        title={t("git.viewList")}
        aria-pressed={mode === "list"}
        onClick={() => onChange("list")}
      >
        <List />
      </Button>
      <Button
        variant="ghost"
        size="icon-xs"
        className={cn("text-muted-foreground", mode === "tree" && "bg-accent text-foreground")}
        aria-label={t("git.viewTree")}
        title={t("git.viewTree")}
        aria-pressed={mode === "tree"}
        onClick={() => onChange("tree")}
      >
        <FolderTree />
      </Button>
    </>
  );
}

export function FileChangeView({
  files,
  mode,
  onOpen,
  selection,
  /** 目录树的根节点名（仓库目录名）。不给就不画根那一行 */
  rootName,
}: {
  files: FileChangeItem[];
  mode: FileViewMode;
  onOpen: (file: FileChangeItem) => void;
  selection?: FileSelection;
  rootName?: string;
}) {
  if (mode === "list") {
    return (
      <ul>
        {files.map((file) => (
          <FileRow
            key={rowKey(file)}
            file={file}
            depth={0}
            selection={selection}
            onOpen={() => onOpen(file)}
          />
        ))}
      </ul>
    );
  }
  return (
    <TreeView files={files} onOpen={onOpen} selection={selection} rootName={rootName} />
  );
}

/** 子树里全部文件的路径。目录行的 checkbox 一次切换它们 */
function subtreePaths(nodes: TreeNode<FileChangeItem>[]): string[] {
  const out: string[] = [];
  for (const node of nodes) {
    if (node.kind === "file") out.push(node.item.path);
    else out.push(...subtreePaths(node.children));
  }
  return out;
}

/** 一组路径的勾选合成态：全选 / 全不选 / 半选 */
function groupState(paths: string[], selection: FileSelection): boolean | "indeterminate" {
  let on = 0;
  for (const p of paths) if (selection.isSelected(p)) on++;
  if (on === 0) return false;
  return on === paths.length ? true : "indeterminate";
}

/**
 * 同一个路径可能出现两次（比如 `AD`：暂存了新增又在工作区删掉），
 * 所以 key 得带上状态。
 */
function rowKey(file: FileChangeItem): string {
  return `${file.status}:${file.path}`;
}

function TreeView({
  files,
  onOpen,
  selection,
  rootName,
}: {
  files: FileChangeItem[];
  onOpen: (file: FileChangeItem) => void;
  selection?: FileSelection;
  rootName?: string;
}) {
  const nodes = buildFileTree(files, (f) => f.path);
  // 默认全展开：改动文件通常就几十个，进来还要一层层点开才看得见反而更慢。
  // 这里存的是**折叠**的那些，于是新出现的目录天然是展开的
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  const toggle = (path: string) =>
    setCollapsed((cur) => {
      const next = new Set(cur);
      if (!next.delete(path)) next.add(path);
      return next;
    });

  const body = (
    <TreeLevel
      nodes={nodes}
      depth={rootName ? 1 : 0}
      collapsed={collapsed}
      onToggle={toggle}
      onOpen={onOpen}
      selection={selection}
    />
  );

  if (!rootName) return <ul>{body}</ul>;
  const all = files.map((f) => f.path);
  return (
    <ul>
      <DirRow
        name={rootName}
        count={files.length}
        depth={0}
        open={!collapsed.has("")}
        onToggle={() => toggle("")}
        selection={selection}
        paths={all}
      />
      {!collapsed.has("") && body}
    </ul>
  );
}

function TreeLevel({
  nodes,
  depth,
  collapsed,
  onToggle,
  onOpen,
  selection,
}: {
  nodes: TreeNode<FileChangeItem>[];
  depth: number;
  collapsed: Set<string>;
  onToggle: (path: string) => void;
  onOpen: (file: FileChangeItem) => void;
  selection?: FileSelection;
}): ReactNode {
  return nodes.map((node) =>
    node.kind === "file" ? (
      <FileRow
        key={rowKey(node.item)}
        file={node.item}
        depth={depth}
        nameOnly
        selection={selection}
        onOpen={() => onOpen(node.item)}
      />
    ) : (
      // Fragment 而不是嵌套 <li><ul>：目录与它的子项要平铺在同一个 ul 里，
      // 行的悬浮底色才能通栏（嵌套 ul 会被父 li 的 padding 框住）
      <Fragment key={node.path}>
        <DirRow
          name={node.name}
          count={node.fileCount}
          depth={depth}
          open={!collapsed.has(node.path)}
          onToggle={() => onToggle(node.path)}
          selection={selection}
          paths={subtreePaths(node.children)}
        />
        {!collapsed.has(node.path) && (
          <TreeLevel
            nodes={node.children}
            depth={depth + 1}
            collapsed={collapsed}
            onToggle={onToggle}
            onOpen={onOpen}
            selection={selection}
          />
        )}
      </Fragment>
    )
  );
}

/** 每层缩进多少像素。用 padding 而不是嵌套 ul，行的悬浮底色才能通栏 */
const INDENT = 12;

function DirRow({
  name,
  count,
  depth,
  open,
  onToggle,
  selection,
  paths,
}: {
  name: string;
  count: number;
  depth: number;
  open: boolean;
  onToggle: () => void;
  selection?: FileSelection;
  /** 这个目录下（含更深层）的全部文件路径 */
  paths: string[];
}) {
  const { t } = useTranslation();
  const state = selection ? groupState(paths, selection) : false;
  return (
    <li className="flex items-center pr-2 hover:bg-accent/50">
      <button
        type="button"
        onClick={onToggle}
        title={name}
        aria-expanded={open}
        style={{ paddingLeft: 12 + depth * INDENT }}
        className="flex h-6 min-w-0 flex-1 items-center gap-1 text-left"
      >
        <ChevronRight
          className={cn("size-3 shrink-0 text-muted-foreground transition-transform", open && "rotate-90")}
        />
        <span className="min-w-0 truncate text-[11.5px] font-medium">{name}</span>
        <span className="shrink-0 text-[11px] text-muted-foreground tabular-nums">
          {t("git.nFiles", { n: count })}
        </span>
      </button>
      {selection && (
        <Checkbox
          className="ml-2 size-3.5"
          checked={state}
          // 半选点一下应该变全选（"我要这一整个目录"），而不是全不选
          onCheckedChange={() => selection.toggle(paths, state !== true)}
          aria-label={t("changes.selectDir", { name })}
        />
      )}
    </li>
  );
}

function FileRow({
  file,
  depth,
  nameOnly,
  selection,
  onOpen,
}: {
  file: FileChangeItem;
  depth: number;
  /** 目录树里不重复显示目录（它就在上一层），列表视图才显示 */
  nameOnly?: boolean;
  selection?: FileSelection;
  onOpen: () => void;
}) {
  const { t } = useTranslation();
  const dir = nameOnly ? "" : dirOf(file.path);
  const label = statusText(file.status, t);
  const full = file.origPath ? `${file.origPath} → ${file.path}` : file.path;
  return (
    <li className="flex items-center pr-2 hover:bg-accent hover:text-accent-foreground">
      <button
        type="button"
        onClick={onOpen}
        title={`${label} · ${full} · ${t("git.viewDiff")}`}
        // 缩进要跳过目录行的那个折叠箭头（size-3 + gap-1），文件名才和上一层的目录名对齐
        style={{ paddingLeft: 12 + depth * INDENT + (nameOnly ? 16 : 0) }}
        className="flex h-6 min-w-0 flex-1 items-center gap-2 text-left"
      >
        <span className={cn("w-2.5 shrink-0 text-[11px]", statusTone(file.status))}>
          {file.status === "?" ? "+" : file.status}
        </span>
        <span className="min-w-0 truncate font-mono text-[11.5px]">{baseOf(file.path)}</span>
        {dir && (
          <span className="min-w-0 shrink truncate font-mono text-[11px] text-muted-foreground">
            {dir}
          </span>
        )}
        <span className="ml-auto shrink-0 font-mono text-[11px] tabular-nums">
          {file.added == null || file.deleted == null ? (
            <span className="text-muted-foreground">{t("git.binary")}</span>
          ) : (
            <>
              <span className="text-success">+{file.added}</span>{" "}
              <span className="text-destructive">−{file.deleted}</span>
            </>
          )}
        </span>
      </button>
      {selection && (
        <Checkbox
          className="ml-2 size-3.5"
          checked={selection.isSelected(file.path)}
          onCheckedChange={(next) => selection.toggle([file.path], next === true)}
          aria-label={t("changes.selectFile", { name: baseOf(file.path) })}
        />
      )}
    </li>
  );
}

export function statusTone(status: string): string {
  switch (status) {
    case "U":
      return "text-warning";
    case "D":
      return "text-destructive";
    case "A":
    case "?":
      return "text-success";
    default:
      return "text-warning";
  }
}

export function statusText(status: string, t: (key: string) => string): string {
  switch (status) {
    case "?":
      return t("git.status_untracked");
    case "A":
      return t("git.status_added");
    case "D":
      return t("git.status_deleted");
    case "R":
      return t("git.status_renamed");
    case "C":
      return t("git.status_copied");
    case "U":
      return t("git.status_unmerged");
    case "T":
      return t("git.status_typechange");
    default:
      return t("git.status_modified");
  }
}

/**
 * porcelain 的 XY 两列压成一个展示用字母。
 *
 * 优先看暂存列（X）：`MM` 是"暂存了一版又改了一版"，主要事实是它被改过；
 * `??` 两列都是问号，取哪个都一样。
 */
export function porcelainStatus(index: string, work: string): string {
  if (index === "?" || work === "?") return "?";
  if (index !== " " && index !== "") return index;
  return work.trim() || "M";
}
