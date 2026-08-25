import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { FileText } from "lucide-react";
import { api } from "../api.js";
import { useApp, selectFocusProjectId } from "../store.js";
import { basename, dirname, filterFiles } from "../lib/fileSearch.js";
import { Dialog, DialogContent, DialogTitle } from "@/components/ui/dialog";
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
  CommandShortcut,
} from "@/components/ui/command";

/**
 * ⌘P Quick Open。搜当前项目工作目录里的文件，选中后新开一个查看 tab。
 *
 * 跟命令面板同一套 cmdk 壳，shouldFilter=false：打分是这个产品自己的
 * （文件名优先于路径），交给 cmdk 会把路径里的斜杠当普通字符搅乱排序。
 */
export function FileQuickOpen() {
  const { t } = useTranslation();
  const open = useApp((s) => s.quickOpen);
  const setQuickOpen = useApp((s) => s.setQuickOpen);
  const openFile = useApp((s) => s.openFile);
  const focusId = useApp(selectFocusProjectId);
  const firstId = useApp((s) => s.projects[0]?.id ?? null);
  const projectId = focusId ?? firstId;
  const [query, setQuery] = useState("");
  const [paths, setPaths] = useState<string[] | null>(null);
  const [truncated, setTruncated] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (open) setQuery("");
  }, [open]);

  useEffect(() => {
    if (!open || !projectId) {
      setPaths(null);
      setError(null);
      setTruncated(false);
      return;
    }
    let cancelled = false;
    setPaths(null);
    setError(null);
    api
      .indexFiles(projectId)
      .then((res) => {
        if (cancelled) return;
        setPaths(res.paths);
        setTruncated(res.truncated);
      })
      .catch((err) => {
        if (cancelled) return;
        useApp.getState().handleApiError(err);
        setError((err as Error).message);
      });
    return () => {
      cancelled = true;
    };
  }, [open, projectId]);

  const hits = useMemo(() => filterFiles(paths ?? [], query), [paths, query]);

  if (!open) return null;

  const pick = (path: string) => {
    if (!projectId) return;
    setQuickOpen(false);
    openFile(projectId, path);
  };

  return (
    <Dialog
      open
      onOpenChange={(next) => {
        if (!next) setQuickOpen(false);
      }}
    >
      <DialogContent
        showCloseButton={false}
        aria-describedby={undefined}
        className="overflow-hidden p-0 sm:max-w-xl"
        onEscapeKeyDown={(e) => e.preventDefault()}
      >
        <DialogTitle className="sr-only">{t("quickOpen.title")}</DialogTitle>
        <Command shouldFilter={false} loop>
          <CommandInput
            value={query}
            onValueChange={setQuery}
            placeholder={t("quickOpen.placeholder")}
          />
          <CommandList className="max-h-88">
            {!projectId ? (
              <CommandEmpty>{t("quickOpen.noProject")}</CommandEmpty>
            ) : error ? (
              <CommandEmpty>
                {t("quickOpen.failed")}
                <span className="mt-1 block font-mono text-[11px]">{error}</span>
              </CommandEmpty>
            ) : paths == null ? (
              <CommandEmpty>{t("quickOpen.loading")}</CommandEmpty>
            ) : hits.length === 0 ? (
              <CommandEmpty>{t("quickOpen.empty")}</CommandEmpty>
            ) : (
              <CommandGroup>
                {hits.map((path) => (
                  <CommandItem key={path} value={path} onSelect={() => pick(path)}>
                    <FileText />
                    <span className="min-w-0 flex-1 truncate">{basename(path)}</span>
                    {dirname(path) && (
                      <CommandShortcut className="max-w-50 truncate font-mono font-normal">
                        {dirname(path)}
                      </CommandShortcut>
                    )}
                  </CommandItem>
                ))}
              </CommandGroup>
            )}
            {truncated && paths && (
              <p className="px-3 py-2 text-[11px] text-muted-foreground">
                {t("quickOpen.truncated", { n: paths.length })}
              </p>
            )}
          </CommandList>
        </Command>
      </DialogContent>
    </Dialog>
  );
}
