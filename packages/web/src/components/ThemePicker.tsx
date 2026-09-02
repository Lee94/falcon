import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { ChevronsUpDown, ClipboardCopy, PencilLine } from "lucide-react";
import { useApp } from "../store.js";
import { cachedCatalog, findTheme, loadCatalog, type CatalogEntry } from "../lib/theme/catalog.js";
import {
  parseGhosttyTheme,
  parseThemeSetting,
  resolveThemeColors,
  serializeGhosttyTheme,
  sourceFromColors,
  type ThemeColors,
} from "../lib/theme/ghostty.js";
import { choiceOf, type ThemeChoice, type ThemeMode } from "../lib/theme/pref.js";
import { writeBrowserClipboard } from "../lib/osc52.js";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from "@/components/ui/command";
import { AppDialog } from "./common/AppDialog.js";
import { Field } from "./common/Field.js";

/** 高亮 / 编辑到应用整套主题之间的延迟：键盘连按时别每一下都重建终端渲染器 */
const PREVIEW_DELAY = 120;

/** 主题缩略：底色、字色、红、蓝四格，一眼能分出深浅与调性 */
export function ThemeSwatch({ colors, className }: { colors: ThemeColors; className?: string }) {
  return (
    <span
      aria-hidden
      className={cn(
        "grid size-4 shrink-0 grid-cols-2 grid-rows-2 overflow-hidden rounded-sm border border-foreground/20",
        className
      )}
    >
      <span style={{ background: colors.background }} />
      <span style={{ background: colors.foreground }} />
      <span style={{ background: colors.palette[1] }} />
      <span style={{ background: colors.palette[4] }} />
    </span>
  );
}

/**
 * 一个槽位的主题选择器：弹层里是可搜索的内置目录（Falcon 两套 + Ghostty 全部），
 * 键盘高亮到哪一项整个应用就先换成哪一项看效果，选定才落盘；底部进自定义编辑器。
 */
export function ThemePicker({ slot }: { slot: ThemeMode }) {
  const { t } = useTranslation();
  const choice = useApp((s) => s.themes[slot]);
  const setThemeChoice = useApp((s) => s.setThemeChoice);
  const previewTheme = useApp((s) => s.previewTheme);
  const [open, setOpen] = useState(false);
  const [editing, setEditing] = useState(false);
  const [catalog, setCatalog] = useState<CatalogEntry[] | null>(cachedCatalog);
  const [loadFailed, setLoadFailed] = useState(false);
  const [highlighted, setHighlighted] = useState("");
  const previewTimer = useRef<number | null>(null);

  useEffect(() => {
    if (!open || catalog) return;
    let cancelled = false;
    setLoadFailed(false);
    loadCatalog().then(
      (entries) => {
        if (!cancelled) setCatalog(entries);
      },
      () => {
        if (!cancelled) setLoadFailed(true);
      }
    );
    return () => {
      cancelled = true;
    };
  }, [open, catalog]);

  // 弹层开着时按高亮项预览；关掉（含选定后）复原到真正的槽位主题
  useEffect(() => {
    if (!open) {
      if (previewTimer.current !== null) window.clearTimeout(previewTimer.current);
      previewTimer.current = null;
      previewTheme(null);
      return;
    }
    const entry = catalog && highlighted && !highlighted.startsWith("custom:") ? findTheme(catalog, highlighted) : null;
    const target = entry ? choiceOf(entry) : highlighted.startsWith("custom:") ? choice : null;
    if (!target) return;
    if (previewTimer.current !== null) window.clearTimeout(previewTimer.current);
    previewTimer.current = window.setTimeout(() => previewTheme(target), PREVIEW_DELAY);
    return () => {
      if (previewTimer.current !== null) window.clearTimeout(previewTimer.current);
    };
  }, [open, highlighted, catalog, choice, previewTheme]);

  const groups = useMemo(() => {
    if (!catalog) return [];
    const falcon = catalog.filter((e) => e.name.startsWith("Falcon "));
    const light = catalog.filter((e) => !e.name.startsWith("Falcon ") && e.appearance === "light");
    const dark = catalog.filter((e) => !e.name.startsWith("Falcon ") && e.appearance === "dark");
    const same = slot === "light" ? light : dark;
    const other = slot === "light" ? dark : light;
    return [
      { key: "falcon", heading: t("theme.groupFalcon"), entries: falcon },
      { key: slot, heading: t(slot === "light" ? "theme.groupLight" : "theme.groupDark"), entries: same },
      { key: slot === "light" ? "dark" : "light", heading: t(slot === "light" ? "theme.groupDark" : "theme.groupLight"), entries: other },
    ];
  }, [catalog, slot, t]);

  const currentValue = choice.kind === "custom" ? `custom:${choice.name}` : choice.name;

  const pick = (entry: CatalogEntry) => {
    setThemeChoice(slot, choiceOf(entry));
    setOpen(false);
  };

  return (
    <>
      <Popover
        open={open}
        onOpenChange={(next) => {
          setOpen(next);
          if (next) setHighlighted(currentValue);
        }}
      >
        <PopoverTrigger asChild>
          <Button
            variant="outline"
            size="sm"
            role="combobox"
            aria-expanded={open}
            aria-label={t("theme.pick")}
            className="w-64 justify-between font-normal"
          >
            <span className="flex min-w-0 items-center gap-2">
              <ThemeSwatch colors={choice.colors} />
              <span className="truncate">{choice.name}</span>
            </span>
            <ChevronsUpDown className="size-3.5 text-muted-foreground" />
          </Button>
        </PopoverTrigger>
        <PopoverContent align="end" className="w-80 p-0">
          <Command loop value={highlighted} onValueChange={setHighlighted}>
            <CommandInput placeholder={t("theme.search", { n: catalog?.length ?? 0 })} />
            <CommandList className="max-h-80">
              {!catalog && (
                <div className="py-6 text-center text-xs text-muted-foreground">
                  {loadFailed ? t("theme.loadFailed") : t("theme.loading")}
                </div>
              )}
              {catalog && <CommandEmpty>{t("theme.empty")}</CommandEmpty>}
              {choice.kind === "custom" && (
                <CommandGroup heading={t("theme.groupCurrent")}>
                  <CommandItem value={currentValue} onSelect={() => setEditing(true)}>
                    <ThemeSwatch colors={choice.colors} />
                    <span className="truncate">{choice.name}</span>
                    <PencilLine className="ml-auto size-3.5" />
                  </CommandItem>
                </CommandGroup>
              )}
              {groups.map((group) =>
                group.entries.length === 0 ? null : (
                  <CommandGroup key={group.key} heading={group.heading}>
                    {group.entries.map((entry) => (
                      <CommandItem key={entry.name} value={entry.name} onSelect={() => pick(entry)}>
                        <ThemeSwatch colors={entry.colors} />
                        <span className="truncate">{entry.name}</span>
                        {choice.kind === "builtin" && choice.name === entry.name && (
                          <span className="ml-auto text-[11px] text-muted-foreground">{t("theme.groupCurrent")}</span>
                        )}
                      </CommandItem>
                    ))}
                  </CommandGroup>
                )
              )}
            </CommandList>
            <div className="flex items-center gap-1 border-t p-1">
              <Button
                variant="ghost"
                size="xs"
                className="flex-1 justify-start font-normal"
                onClick={() => {
                  setOpen(false);
                  setEditing(true);
                }}
              >
                <PencilLine />
                {t("theme.customEntry")}
              </Button>
              <CopyGhosttyButton colors={choice.colors} />
            </div>
          </Command>
        </PopoverContent>
      </Popover>
      {editing && <ThemeEditor slot={slot} initial={choice} onClose={() => setEditing(false)} />}
    </>
  );
}

function CopyGhosttyButton({ colors }: { colors: ThemeColors }) {
  const { t } = useTranslation();
  const toast = useApp((s) => s.toast);
  return (
    <Button
      variant="ghost"
      size="xs"
      className="font-normal"
      title={t("theme.copyGhostty")}
      aria-label={t("theme.copyGhostty")}
      onClick={() =>
        void writeBrowserClipboard(serializeGhosttyTheme(colors)).then(
          () => toast({ kind: "success", title: t("theme.copied") }),
          () => toast({ kind: "warning", title: t("toast.copyFailed") })
        )
      }
    >
      <ClipboardCopy />
    </Button>
  );
}

/**
 * 粘贴 / 修改 Ghostty 主题文本。编辑过程中整个应用实时按文本预览，关掉复原。
 * `theme = 内置名` 以目录里那套为底再覆盖，与 Ghostty 读配置的顺序一致。
 */
function ThemeEditor({
  slot,
  initial,
  onClose,
}: {
  slot: ThemeMode;
  initial: ThemeChoice;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const setThemeChoice = useApp((s) => s.setThemeChoice);
  const previewTheme = useApp((s) => s.previewTheme);
  const [name, setName] = useState(initial.kind === "custom" ? initial.name : "");
  const [text, setText] = useState(() =>
    initial.kind === "custom" ? serializeGhosttyTheme(initial.colors) : `theme = ${initial.name}\n`
  );
  const [catalog, setCatalog] = useState<CatalogEntry[] | null>(cachedCatalog);

  useEffect(() => {
    if (catalog) return;
    let cancelled = false;
    loadCatalog().then(
      (entries) => {
        if (!cancelled) setCatalog(entries);
      },
      () => undefined
    );
    return () => {
      cancelled = true;
    };
  }, [catalog]);

  const parsed = useMemo(() => {
    const src = parseGhosttyTheme(text);
    let baseName: string | undefined;
    if (src.theme) {
      const setting = parseThemeSetting(src.theme);
      baseName = setting.single ?? setting[slot] ?? setting.light ?? setting.dark;
    }
    const base = baseName && catalog ? findTheme(catalog, baseName) : undefined;
    const unknownBase = baseName && catalog && !base ? baseName : undefined;
    const recognized = src.recognized + (base ? 22 : 0);
    const colors = recognized > 0 ? resolveThemeColors(src, base ? sourceFromColors(base.colors) : undefined) : null;
    return { colors, recognized, unknownBase, baseName: base?.name };
  }, [text, slot, catalog]);

  useEffect(() => {
    if (!parsed.colors) return;
    const colors = parsed.colors;
    const timer = window.setTimeout(
      () => previewTheme({ name: name || t("theme.editorTitle"), kind: "custom", colors }),
      PREVIEW_DELAY
    );
    return () => window.clearTimeout(timer);
  }, [parsed.colors, name, previewTheme, t]);

  useEffect(() => () => previewTheme(null), [previewTheme]);

  const apply = () => {
    if (!parsed.colors) return;
    const label = name.trim() || parsed.baseName || t("theme.editorTitle");
    setThemeChoice(slot, { name: label.slice(0, 80), kind: "custom", colors: parsed.colors });
    onClose();
  };

  return (
    <AppDialog
      title={t("theme.editorTitle")}
      description={t("theme.editorHint")}
      onClose={onClose}
      wide
      lockOverlay
      footer={
        <>
          {parsed.colors && <CopyGhosttyButton colors={parsed.colors} />}
          <Button variant="outline" size="sm" onClick={onClose}>
            {t("common.cancel")}
          </Button>
          <Button size="sm" disabled={!parsed.colors} onClick={apply}>
            {t("theme.editorApply")}
          </Button>
        </>
      }
    >
      <div className="grid gap-3">
        <Field label={t("theme.editorName")} htmlFor="theme-name">
          <Input
            id="theme-name"
            value={name}
            placeholder={parsed.baseName ?? t("theme.editorNamePlaceholder")}
            onChange={(e) => setName(e.target.value)}
          />
        </Field>
        <Field
          label={t("theme.editorText")}
          htmlFor="theme-text"
          tone={parsed.colors ? (parsed.unknownBase ? undefined : "ok") : "err"}
          hint={
            parsed.unknownBase
              ? t("theme.editorUnknownBase", { name: parsed.unknownBase })
              : parsed.colors
                ? t("theme.editorRecognized", { n: parsed.recognized })
                : t("theme.editorNothing")
          }
        >
          <textarea
            id="theme-text"
            data-autofocus
            value={text}
            spellCheck={false}
            rows={14}
            className="w-full resize-y rounded-md border border-input bg-transparent px-3 py-2 font-mono text-xs leading-5 outline-none focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50 dark:bg-input/30"
            onChange={(e) => setText(e.target.value)}
          />
        </Field>
        {parsed.colors && (
          <div className="flex items-center gap-2 text-xs text-muted-foreground">
            <ThemeSwatch colors={parsed.colors} />
            <span className="flex gap-1">
              {parsed.colors.palette.map((c, i) => (
                <span key={i} className="size-3 rounded-sm border border-foreground/20" style={{ background: c }} />
              ))}
            </span>
          </div>
        )}
      </div>
    </AppDialog>
  );
}
