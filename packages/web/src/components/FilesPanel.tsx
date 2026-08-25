import { Fragment, useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import {
  ChevronRight,
  File,
  FileCode,
  FileImage,
  FileText,
  FoldVertical,
  RefreshCw,
} from "lucide-react";
import type { WorkspaceEntry } from "@falcon/shared";
import { api } from "../api.js";
import { useApp, selectFocusProjectId } from "../store.js";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";

/**
 * 右侧「文件」面板：项目工作目录的文件树。
 *
 * 与「修改」面板的分工：那边列的是 git 眼里"这次动了什么"，这边列的是磁盘上
 * 真正有什么——未跟踪的、被 ignore 的、生成出来的，都在。所以它不能复用
 * FileChangeView：那套结构一次拿到全部路径再树化，而这里目录可能有几万项，
 * 只能按层懒加载。
 *
 * 只读：点文件新开一个查看 tab（同一文件再点则聚焦），不提供新建 / 重命名 / 删除。改文件是终端里的事。
 */

/** 一层目录的加载状态。folded 的层保留缓存，再展开时立刻出来 */
interface DirState {
  loading: boolean;
  entries?: WorkspaceEntry[];
  truncated?: boolean;
  error?: string;
}

/** 每层缩进多少像素，与 FileChangeView 保持一致 */
const INDENT = 12;

function useDirTree(projectId: string | null) {
  const [dirs, setDirs] = useState<Record<string, DirState>>({});
  // 根目录（"" ）恒在展开集合里：面板打开就该看见东西
  const [expanded, setExpanded] = useState<string[]>([""]);
  // 换项目与手动刷新都要作废在途请求，否则慢的那个回来会盖掉新内容
  const gen = useRef(0);

  useEffect(() => {
    gen.current++;
    setDirs({});
    setExpanded([""]);
  }, [projectId]);

  // 展开集合里还没有数据的层，挨个去拉。占位的 loading 先写进 dirs，
  // 于是这个 effect 因 dirs 变化重跑时不会把同一层拉第二遍
  useEffect(() => {
    if (!projectId) return;
    const my = gen.current;
    for (const path of expanded) {
      if (dirs[path]) continue;
      setDirs((d) => ({ ...d, [path]: { loading: true } }));
      api
        .listFiles(projectId, path)
        .then((res) => {
          if (gen.current !== my) return;
          setDirs((d) => ({
            ...d,
            [path]: { loading: false, entries: res.entries, truncated: res.truncated },
          }));
        })
        .catch((err) => {
          if (gen.current !== my) return;
          useApp.getState().handleApiError(err);
          setDirs((d) => ({ ...d, [path]: { loading: false, error: (err as Error).message } }));
        });
    }
  }, [projectId, expanded, dirs]);

  const toggle = useCallback((path: string) => {
    setExpanded((cur) => (cur.includes(path) ? cur.filter((p) => p !== path) : [...cur, path]));
  }, []);

  /** 重新拉当前展开的每一层。目录内容会被外面的终端改，面板自己不轮询 */
  const refresh = useCallback(() => {
    gen.current++;
    setDirs({});
  }, []);

  const collapseAll = useCallback(() => setExpanded([""]), []);

  return { dirs, expanded, toggle, refresh, collapseAll };
}

export function FilesPanel() {
  const { t } = useTranslation();
  const projectId = useApp(selectFocusProjectId);
  const project = useApp((s) => s.projects.find((p) => p.id === projectId));
  const openFile = useApp((s) => s.openFile);
  const active = useApp((s) => s.active);
  const { dirs, expanded, toggle, refresh, collapseAll } = useDirTree(projectId);

  const root = dirs[""];
  const activePath =
    active.kind === "file" && active.projectId === projectId ? active.path : null;

  return (
    <aside className="flex min-h-0 flex-1 flex-col border-l bg-sidebar text-sidebar-foreground">
      <div className="flex h-11 shrink-0 items-center gap-1 border-b pr-2 pl-3">
        <span className="min-w-0 flex-1 truncate text-[13px] font-semibold">
          {t("files.title")}
        </span>
        <Button
          variant="ghost"
          size="icon-xs"
          className="text-muted-foreground"
          aria-label={t("files.collapseAll")}
          title={t("files.collapseAll")}
          disabled={!projectId}
          onClick={collapseAll}
        >
          <FoldVertical />
        </Button>
        <Button
          variant="ghost"
          size="icon-xs"
          className="text-muted-foreground"
          aria-label={t("files.refresh")}
          title={t("files.refresh")}
          disabled={!projectId}
          onClick={refresh}
        >
          <RefreshCw className={cn(root?.loading && "animate-spin")} />
        </Button>
      </div>

      {!projectId ? (
        <Hint>{t("files.noProject")}</Hint>
      ) : root?.error ? (
        <Hint>
          {t("files.loadFailed")}
          <span className="mt-1 block font-mono text-[11px]">{root.error}</span>
        </Hint>
      ) : (
        <div className="min-h-0 flex-1 overflow-y-auto pb-2">
          <ul>
            <DirRow
              name={project?.name ?? "/"}
              depth={0}
              open={expanded.includes("")}
              onToggle={() => toggle("")}
            />
            {expanded.includes("") && (
              <Level
                path=""
                depth={1}
                dirs={dirs}
                expanded={expanded}
                activePath={activePath}
                onToggle={toggle}
                onOpen={(path) => projectId && openFile(projectId, path)}
              />
            )}
          </ul>
        </div>
      )}
    </aside>
  );
}

/** 一层目录的内容。目录行下面直接跟它自己的子层，平铺在同一个 ul 里 */
function Level({
  path,
  depth,
  dirs,
  expanded,
  activePath,
  onToggle,
  onOpen,
}: {
  path: string;
  depth: number;
  dirs: Record<string, DirState>;
  expanded: string[];
  activePath: string | null;
  onToggle: (path: string) => void;
  onOpen: (path: string) => void;
}): ReactNode {
  const { t } = useTranslation();
  const state = dirs[path];

  if (!state || state.loading) return <RowNote depth={depth}>{t("files.loading")}</RowNote>;
  if (state.error) return <RowNote depth={depth}>{state.error}</RowNote>;
  const entries = state.entries ?? [];
  if (entries.length === 0) return <RowNote depth={depth}>{t("files.empty")}</RowNote>;

  return (
    <>
      {entries.map((entry) =>
        entry.kind === "dir" ? (
          <Fragment key={entry.path}>
            <DirRow
              name={entry.name}
              depth={depth}
              open={expanded.includes(entry.path)}
              onToggle={() => onToggle(entry.path)}
            />
            {expanded.includes(entry.path) && (
              <Level
                path={entry.path}
                depth={depth + 1}
                dirs={dirs}
                expanded={expanded}
                activePath={activePath}
                onToggle={onToggle}
                onOpen={onOpen}
              />
            )}
          </Fragment>
        ) : (
          <FileRow
            key={entry.path}
            entry={entry}
            depth={depth}
            active={activePath === entry.path}
            onOpen={() => onOpen(entry.path)}
          />
        )
      )}
      {state.truncated && (
        <RowNote depth={depth}>
          {t("files.truncated", { n: entries.length })}
        </RowNote>
      )}
    </>
  );
}

function DirRow({
  name,
  depth,
  open,
  onToggle,
}: {
  name: string;
  depth: number;
  open: boolean;
  onToggle: () => void;
}) {
  return (
    <li>
      <button
        type="button"
        onClick={onToggle}
        title={name}
        aria-expanded={open}
        style={{ paddingLeft: 12 + depth * INDENT }}
        className="flex h-6 w-full min-w-0 items-center gap-1 pr-3 text-left hover:bg-accent/50"
      >
        <ChevronRight
          className={cn(
            "size-3 shrink-0 text-muted-foreground transition-transform",
            open && "rotate-90"
          )}
        />
        <span className="min-w-0 truncate text-[11.5px] font-medium">{name}</span>
      </button>
    </li>
  );
}

function FileRow({
  entry,
  depth,
  active,
  onOpen,
}: {
  entry: WorkspaceEntry;
  depth: number;
  active: boolean;
  onOpen: () => void;
}) {
  const { t } = useTranslation();
  const Icon = iconFor(entry.name);
  return (
    <li>
      <button
        type="button"
        onClick={onOpen}
        title={`${entry.path} · ${t("files.openFile")}`}
        // 缩进要跳过目录行那个折叠箭头（size-3 + gap-1），文件名才和同层目录名对齐
        style={{ paddingLeft: 12 + depth * INDENT + 16 }}
        className={cn(
          "flex h-6 w-full min-w-0 items-center gap-1.5 pr-3 text-left hover:bg-accent hover:text-accent-foreground",
          active && "bg-accent text-accent-foreground"
        )}
      >
        <Icon className="size-3 shrink-0 text-muted-foreground" />
        <span className="min-w-0 truncate font-mono text-[11.5px]">{entry.name}</span>
      </button>
    </li>
  );
}

/** 「正在读取…」「空目录」这类占位行，缩进跟着所在层走 */
function RowNote({ depth, children }: { depth: number; children: ReactNode }) {
  return (
    <li
      className="flex h-6 items-center pr-3 text-[11px] text-muted-foreground"
      style={{ paddingLeft: 12 + depth * INDENT + 16 }}
    >
      <span className="min-w-0 truncate">{children}</span>
    </li>
  );
}

function Hint({ children }: { children: ReactNode }) {
  return <p className="px-3 py-4 text-xs leading-relaxed text-muted-foreground">{children}</p>;
}

const IMAGE_EXT = new Set(["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "avif", "svg"]);
const TEXT_EXT = new Set(["md", "markdown", "txt", "log", "rst", "adoc"]);
const CODE_EXT = new Set([
  "ts", "tsx", "js", "jsx", "mjs", "cjs", "json", "jsonc", "css", "scss", "html", "vue",
  "py", "go", "rs", "java", "kt", "swift", "c", "h", "cc", "cpp", "hpp", "cs", "rb", "php",
  "sh", "bash", "zsh", "fish", "ps1", "sql", "yml", "yaml", "toml", "ini", "conf", "xml",
]);

/** 按扩展名选个图标。一排全是同一个图标时，文件名之外没有任何可扫读的信息 */
function iconFor(name: string): typeof File {
  const i = name.lastIndexOf(".");
  const ext = i <= 0 ? "" : name.slice(i + 1).toLowerCase();
  if (IMAGE_EXT.has(ext)) return FileImage;
  if (TEXT_EXT.has(ext)) return FileText;
  if (CODE_EXT.has(ext)) return FileCode;
  return File;
}
