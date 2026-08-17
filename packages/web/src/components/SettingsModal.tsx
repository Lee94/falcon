import { useMemo, useState, type FormEvent, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { Info, Palette, Search, Server, Shield, type LucideIcon } from "lucide-react";
import type { ITheme } from "@xterm/xterm";
import type { SshAuthMethod, SshHost } from "@mojito/shared";
import { api } from "../api.js";
import { useApp, type SettingsTab } from "../store.js";
import { sshConn } from "../lib/hostColor.js";
import {
  TERM_FONT_IDS,
  TERM_FONT_SIZE_MAX,
  TERM_FONT_SIZE_MIN,
  TERM_LINE_HEIGHT_MAX,
  TERM_LINE_HEIGHT_MIN,
  DEFAULT_TERM_PREF,
  TERM_THEME_GROUPS,
  TERM_THEME_LABELS,
  clampFontSize,
  clampLineHeight,
  resolveTermTheme,
  termFontStack,
  type TermCursorStyle,
  type TermFontId,
  type TermPref,
  type TermThemeId,
} from "../lib/term.js";
import { useActions } from "../lib/useActions.js";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectLabel,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { Dialog, DialogContent, DialogTitle } from "@/components/ui/dialog";
import { ThemeChoice } from "./common/ThemeToggle.js";
import { Field, Segmented } from "./common/Field.js";

interface TabDef {
  id: SettingsTab;
  group: "general" | "connection";
  icon: LucideIcon;
}

const TABS: TabDef[] = [
  { id: "appearance", group: "general", icon: Palette },
  { id: "account", group: "general", icon: Shield },
  { id: "about", group: "general", icon: Info },
  { id: "hosts", group: "connection", icon: Server },
];

const GROUPS: TabDef["group"][] = ["general", "connection"];

/**
 * 设置是覆盖层，不是一页。左侧按模块切，右侧是当前模块的项——
 * 主题、密码、远端主机、关于，以前散落在会话总览底部。
 */
export function SettingsModal() {
  const { t } = useTranslation();
  const tab = useApp((s) => s.settingsTab);
  const setTab = useApp((s) => s.setSettingsTab);
  const close = useApp((s) => s.closeSettings);
  const [query, setQuery] = useState("");

  const needle = query.trim().toLowerCase();
  const visible = useMemo(() => {
    if (!needle) return TABS;
    return TABS.filter((item) => {
      const label = t(`settings.tab_${item.id}`).toLowerCase();
      const group = t(`settings.group_${item.group}`).toLowerCase();
      const keys = t(`settings.tab_${item.id}_keys`).toLowerCase();
      return label.includes(needle) || group.includes(needle) || keys.includes(needle);
    });
  }, [needle, t]);

  const visibleIds = new Set(visible.map((item) => item.id));
  const active = visibleIds.has(tab) ? tab : (visible[0]?.id ?? tab);

  return (
    <Dialog
      open
      onOpenChange={(next) => {
        if (!next) close();
      }}
    >
      <DialogContent
        aria-describedby={undefined}
        className="flex h-[calc(100vh-4rem)] w-[min(90rem,calc(100vw-4rem))] max-w-none flex-row gap-0 overflow-hidden p-0 sm:max-w-none"
        onEscapeKeyDown={(e) => e.preventDefault()}
        onOpenAutoFocus={(e) => {
          const node = e.currentTarget as HTMLElement | null;
          const wanted = node?.querySelector<HTMLElement>("[data-autofocus]");
          if (!wanted) return;
          e.preventDefault();
          wanted.focus();
        }}
      >
        <DialogTitle className="sr-only">{t("settings.title")}</DialogTitle>
        <nav
          aria-label={t("settings.title")}
          className="flex w-56 shrink-0 flex-col border-r bg-sidebar px-3 py-4 text-sidebar-foreground"
        >
          <div className="relative mb-4">
            <Search className="pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-muted-foreground" />
            <Input
              value={query}
              data-autofocus
              placeholder={t("settings.search")}
              aria-label={t("settings.search")}
              className="h-8 bg-background pl-8"
              onChange={(e) => setQuery(e.target.value)}
            />
          </div>
          <div role="tablist" aria-orientation="vertical" className="min-h-0 flex-1 overflow-y-auto">
            {visible.length === 0 && (
              <p className="px-2 py-3 text-xs text-muted-foreground">{t("settings.empty")}</p>
            )}
            {GROUPS.map((group) => {
              const items = visible.filter((item) => item.group === group);
              if (items.length === 0) return null;
              return (
                <div key={group} className="mb-3">
                  <div className="px-2 pt-1 pb-1.5 text-[11px] tracking-wide text-muted-foreground">
                    {t(`settings.group_${group}`)}
                  </div>
                  {items.map((item) => {
                    const Icon = item.icon;
                    const on = item.id === active;
                    return (
                      <button
                        key={item.id}
                        type="button"
                        role="tab"
                        aria-selected={on}
                        className={cn(
                          "flex h-8 w-full items-center gap-2 rounded-md px-2 text-[13px] outline-none focus-visible:ring-[3px] focus-visible:ring-ring/50",
                          on
                            ? "bg-accent font-medium text-accent-foreground"
                            : "text-muted-foreground hover:bg-sidebar-accent hover:text-sidebar-accent-foreground"
                        )}
                        onClick={() => setTab(item.id)}
                      >
                        <Icon className="size-3.5 shrink-0" />
                        {t(`settings.tab_${item.id}`)}
                      </button>
                    );
                  })}
                </div>
              );
            })}
          </div>
        </nav>
        <div
          role="tabpanel"
          className="min-w-0 flex-1 overflow-y-auto px-8 pt-8 pb-10 pr-12"
        >
          {active === "appearance" && <AppearancePane />}
          {active === "account" && <AccountPane />}
          {active === "hosts" && <HostsPane />}
          {active === "about" && <AboutPane />}
        </div>
      </DialogContent>
    </Dialog>
  );
}

function AppearancePane() {
  const { t } = useTranslation();
  const term = useApp((s) => s.term);
  const setTerm = useApp((s) => s.setTerm);
  const resetTerm = useApp((s) => s.resetTerm);

  return (
    <>
      <SettingSection title={t("settings.appearanceTitle")}>
        <SettingRow label={t("theme.label")} hint={t("settings.themeHint")}>
          <ThemeChoice />
        </SettingRow>
      </SettingSection>
      <SettingSection title={t("settings.terminalTitle")} description={t("settings.terminalHint")}>
        <SettingRow label={t("settings.termFont")} hint={t("settings.termFontHint")}>
          <Select
            value={term.fontId}
            onValueChange={(id) => setTerm({ fontId: id as TermFontId })}
          >
            <SelectTrigger size="sm" className="w-64">
              <SelectValue />
            </SelectTrigger>
            <SelectContent align="end" position="popper" className="w-64">
              {TERM_FONT_IDS.map((id) => (
                <SelectItem key={id} value={id}>
                  {t(`term.font_${id.replace(/-/g, "_")}`)}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </SettingRow>
        {term.fontId === "custom" && (
          <SettingRow label={t("settings.termCustomFont")}>
            <Input
              value={term.customFamily}
              placeholder={t("settings.termCustomFontPlaceholder")}
              className="h-8 w-64"
              onChange={(e) => setTerm({ customFamily: e.target.value })}
            />
          </SettingRow>
        )}
        <SettingRow label={t("settings.termFontSize")}>
          <RangeValue
            min={TERM_FONT_SIZE_MIN}
            max={TERM_FONT_SIZE_MAX}
            step={1}
            value={term.fontSize}
            display={`${term.fontSize}`}
            label={t("settings.termFontSize")}
            onChange={(n) => setTerm({ fontSize: clampFontSize(n) })}
          />
        </SettingRow>
        <SettingRow label={t("settings.termLineHeight")}>
          <RangeValue
            min={TERM_LINE_HEIGHT_MIN}
            max={TERM_LINE_HEIGHT_MAX}
            step={0.05}
            value={term.lineHeight}
            display={term.lineHeight.toFixed(2)}
            label={t("settings.termLineHeight")}
            onChange={(n) => setTerm({ lineHeight: clampLineHeight(n) })}
          />
        </SettingRow>
        <SettingRow label={t("settings.termCursor")}>
          <Segmented<TermCursorStyle>
            label={t("settings.termCursor")}
            value={term.cursorStyle}
            onChange={(cursorStyle) => setTerm({ cursorStyle })}
            options={(["block", "bar", "underline"] as const).map((value) => ({
              value,
              label: t(`term.cursor_${value}`),
            }))}
          />
        </SettingRow>
        <SettingRow label={t("settings.termCursorBlink")}>
          <Checkbox
            checked={term.cursorBlink}
            onCheckedChange={(checked) => setTerm({ cursorBlink: checked === true })}
            aria-label={t("settings.termCursorBlink")}
          />
        </SettingRow>
        <SettingRow label={t("settings.termTheme")} hint={t("settings.termThemeHint")}>
          <Select
            value={term.themeId}
            onValueChange={(id) => setTerm({ themeId: id as TermThemeId })}
          >
            <SelectTrigger size="sm" className="w-64">
              <SelectValue />
            </SelectTrigger>
            <SelectContent align="end" position="popper" className="w-64">
              {TERM_THEME_GROUPS.map((group) => (
                <SelectGroup key={group.id}>
                  <SelectLabel>{t(`term.themeGroup_${group.id}`)}</SelectLabel>
                  {group.themes.map((id) => (
                    <SelectItem key={id} value={id}>
                      <span className="flex items-center gap-2">
                        <ThemeSwatch id={id} />
                        {id === "match" ? t("term.theme_match") : TERM_THEME_LABELS[id]}
                      </span>
                    </SelectItem>
                  ))}
                </SelectGroup>
              ))}
            </SelectContent>
          </Select>
        </SettingRow>
        <div className="pt-4">
          <div className="mb-2 flex items-center justify-between gap-3">
            <div className="text-sm">{t("settings.termPreview")}</div>
            <Button
              variant="outline"
              size="xs"
              disabled={termIsDefault(term)}
              onClick={() => resetTerm()}
            >
              {t("settings.termReset")}
            </Button>
          </div>
          <TermPreview />
        </div>
      </SettingSection>
    </>
  );
}

const PREVIEW_ANSI = {
  red: "#cc0000",
  green: "#4e9a06",
  yellow: "#c4a000",
  blue: "#3465a4",
  magenta: "#75507b",
  cyan: "#06989a",
  brightBlack: "#555753",
};

function ansi(theme: ITheme, key: keyof typeof PREVIEW_ANSI): string {
  return theme[key] ?? PREVIEW_ANSI[key];
}

function ThemeSwatch({ id }: { id: TermThemeId }) {
  const ui = useApp((s) => s.theme);
  const theme = resolveTermTheme(id, ui);
  return (
    <span
      aria-hidden
      className="inline-flex size-3.5 shrink-0 overflow-hidden rounded-sm border border-black/15 dark:border-white/20"
    >
      <span className="h-full w-1/2" style={{ background: theme.background }} />
      <span className="h-full w-1/2" style={{ background: theme.foreground }} />
    </span>
  );
}

function TermPreview() {
  const term = useApp((s) => s.term);
  const ui = useApp((s) => s.theme);
  const theme = resolveTermTheme(term.themeId, ui);
  return (
    <div
      className="overflow-hidden rounded-md border"
      style={{ background: theme.background, color: theme.foreground }}
    >
      <pre
        className="m-0 overflow-x-auto p-3"
        style={{
          fontFamily: termFontStack(term),
          fontSize: term.fontSize,
          lineHeight: term.lineHeight,
        }}
      >
        <span style={{ color: ansi(theme, "green") }}>fay</span>
        <span style={{ color: ansi(theme, "brightBlack") }}>@</span>
        <span style={{ color: ansi(theme, "blue") }}>host</span>
        {" "}
        <span style={{ color: ansi(theme, "cyan") }}>~/mojito</span>
        {"\n"}
        <span style={{ color: ansi(theme, "magenta") }}>$</span>
        {" git status\n"}
        <span style={{ color: ansi(theme, "yellow") }}>On branch</span>
        {" main\n"}
        {"nothing to commit, working tree clean\n"}
        <span style={{ color: ansi(theme, "magenta") }}>$</span>
        {" echo 你好 · "}
        <span style={{ color: ansi(theme, "green") }}>{"\ue718"}</span>
        {" node\n"}
        <span style={{ color: ansi(theme, "magenta") }}>$</span>
        {" "}
        <span style={{ color: ansi(theme, "red") }}>{"\uf111"}</span>
        {" "}
        <span style={{ color: ansi(theme, "green") }}>{"\uf00c"}</span>
      </pre>
    </div>
  );
}

function termIsDefault(term: TermPref): boolean {
  return (
    term.fontId === DEFAULT_TERM_PREF.fontId &&
    term.customFamily === DEFAULT_TERM_PREF.customFamily &&
    term.fontSize === DEFAULT_TERM_PREF.fontSize &&
    term.lineHeight === DEFAULT_TERM_PREF.lineHeight &&
    term.themeId === DEFAULT_TERM_PREF.themeId &&
    term.cursorStyle === DEFAULT_TERM_PREF.cursorStyle &&
    term.cursorBlink === DEFAULT_TERM_PREF.cursorBlink
  );
}

function RangeValue({
  min,
  max,
  step,
  value,
  display,
  label,
  onChange,
}: {
  min: number;
  max: number;
  step: number;
  value: number;
  display: string;
  label: string;
  onChange: (n: number) => void;
}) {
  return (
    <div className="flex items-center gap-2">
      <input
        type="range"
        min={min}
        max={max}
        step={step}
        value={value}
        aria-label={label}
        className="h-8 w-28 accent-primary"
        onChange={(e) => onChange(Number(e.target.value))}
      />
      <span className="w-9 text-right font-mono text-xs tabular-nums text-muted-foreground">
        {display}
      </span>
    </div>
  );
}

function AccountPane() {
  const { t } = useTranslation();
  const auth = useApp((s) => s.auth);
  const refreshAuth = useApp((s) => s.refreshAuth);
  const toast = useApp((s) => s.toast);
  const [current, setCurrent] = useState("");
  const [next, setNext] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await api.setPassword(next, current || undefined);
      await refreshAuth();
      setCurrent("");
      setNext("");
      toast({ kind: "success", title: t("password.saved") });
    } catch (err) {
      setError((err as Error).message);
    } finally {
      setBusy(false);
    }
  };

  const logout = async () => {
    await api.logout();
    await refreshAuth();
  };

  return (
    <>
      <SettingSection
        title={t("settings.accountTitle")}
        description={t("password.hint")}
      >
        <SettingRow label={t("settings.passwordStatus")}>
          <span className="text-sm text-muted-foreground">
            {auth?.passwordSet ? t("settings.passwordSet") : t("settings.passwordUnset")}
          </span>
        </SettingRow>
        <form onSubmit={submit} className="grid max-w-md gap-3 pt-4">
          {auth?.passwordSet && (
            <Field label={t("password.current")} htmlFor="pw-current">
              <Input
                id="pw-current"
                type="password"
                value={current}
                onChange={(e) => setCurrent(e.target.value)}
              />
            </Field>
          )}
          <Field label={t("password.next")} htmlFor="pw-next">
            <Input
              id="pw-next"
              type="password"
              value={next}
              onChange={(e) => setNext(e.target.value)}
            />
          </Field>
          {error && <p className="text-[13px] text-destructive">{error}</p>}
          <div>
            <Button type="submit" size="sm" disabled={busy || next.length < 6}>
              {t("password.submit")}
            </Button>
          </div>
        </form>
      </SettingSection>
      {auth?.required && auth.authenticated && (
        <SettingSection title={t("common.logout")}>
          <SettingRow label={t("common.logout")} hint={t("settings.logoutHint")}>
            <Button variant="outline" size="sm" onClick={() => void logout()}>
              {t("common.logout")}
            </Button>
          </SettingRow>
        </SettingSection>
      )}
    </>
  );
}

function HostsPane() {
  const { t } = useTranslation();
  const hosts = useApp((s) => s.hosts);
  const openHostForm = useApp((s) => s.openHostForm);

  return (
    <SettingSection title={t("host.title")} description={t("host.hint")}>
      <HostList hosts={hosts} onAdd={() => openHostForm(null)} />
    </SettingSection>
  );
}

function AboutPane() {
  const { t } = useTranslation();
  const system = useApp((s) => s.system);
  return (
    <SettingSection title={t("settings.aboutTitle")}>
      <SettingRow label={t("overview.version")}>
        <span className="font-mono text-sm text-muted-foreground">
          {system ? `v${system.version}` : "—"}
        </span>
      </SettingRow>
      <SettingRow label={t("overview.platform")}>
        <span className="font-mono text-sm text-muted-foreground">
          {system?.platform ?? "—"}
        </span>
      </SettingRow>
    </SettingSection>
  );
}

function SettingSection({
  title,
  description,
  children,
}: {
  title: string;
  description?: string;
  children: ReactNode;
}) {
  return (
    <section className="mb-8 last:mb-0">
      <h2 className="text-base font-semibold tracking-tight">{title}</h2>
      {description && (
        <p className="mt-1 mb-3 text-xs leading-relaxed text-muted-foreground">{description}</p>
      )}
      <div className={description ? undefined : "mt-3"}>{children}</div>
    </section>
  );
}

function SettingRow({
  label,
  hint,
  children,
}: {
  label: string;
  hint?: string;
  children: ReactNode;
}) {
  return (
    <div className="flex items-center justify-between gap-6 border-b py-4 last:border-b-0">
      <div className="min-w-0">
        <div className="text-sm">{label}</div>
        {hint && <p className="mt-0.5 text-xs text-muted-foreground">{hint}</p>}
      </div>
      <div className="shrink-0">{children}</div>
    </div>
  );
}

function authLabel(method: SshAuthMethod): "project.authKey" | "project.authPassword" | "project.authAgent" {
  return method === "key"
    ? "project.authKey"
    : method === "password"
      ? "project.authPassword"
      : "project.authAgent";
}

function HostList({ hosts, onAdd }: { hosts: SshHost[]; onAdd: () => void }) {
  const { t } = useTranslation();
  const openHostForm = useApp((s) => s.openHostForm);
  const toast = useApp((s) => s.toast);
  const actions = useActions();
  const [testingId, setTestingId] = useState<string | null>(null);

  const test = async (host: SshHost) => {
    setTestingId(host.id);
    try {
      const res = await api.testHost(host.id);
      if (res.ok) {
        toast({
          kind: "success",
          title: t("host.testOk"),
          body: t("host.testOkBody", {
            kind: t(`host.testKind_${res.kind}`),
            home: res.home,
          }),
        });
      } else {
        toast({ kind: "danger", title: t("host.testFail"), body: res.error });
      }
    } catch (err) {
      toast({ kind: "danger", title: t("host.testFail"), body: (err as Error).message });
    } finally {
      setTestingId(null);
    }
  };

  return (
    <div>
      {hosts.length === 0 ? (
        <div className="mb-3 rounded-lg border bg-card px-4 py-6 text-center text-xs text-muted-foreground">
          {t("host.empty")}
        </div>
      ) : (
        <div className="mb-3 overflow-x-auto rounded-lg border bg-card">
          <Table>
            <TableHeader>
              <TableRow className="hover:bg-transparent">
                <TableHead>{t("host.name")}</TableHead>
                <TableHead>{t("host.conn")}</TableHead>
                <TableHead>{t("host.auth")}</TableHead>
                <TableHead />
              </TableRow>
            </TableHeader>
            <TableBody>
              {hosts.map((host) => (
                <TableRow key={host.id}>
                  <TableCell className="font-medium">{host.name}</TableCell>
                  <TableCell className="font-mono text-xs text-muted-foreground">
                    {sshConn(host)}
                  </TableCell>
                  <TableCell className="text-xs text-muted-foreground">
                    {t(authLabel(host.authMethod))}
                    <span className="text-muted-foreground/40"> · </span>
                    {host.projectCount > 0
                      ? t("host.usedBy", { n: host.projectCount })
                      : t("host.unused")}
                  </TableCell>
                  <TableCell>
                    <span className="flex justify-end gap-1.5">
                      <Button
                        variant="ghost"
                        size="sm"
                        disabled={testingId === host.id}
                        onClick={() => void test(host)}
                      >
                        {testingId === host.id ? t("host.testing") : t("host.test")}
                      </Button>
                      <Button variant="ghost" size="sm" onClick={() => openHostForm(host)}>
                        {t("host.edit")}
                      </Button>
                      <Button variant="ghost" size="sm" onClick={() => actions.deleteHost(host)}>
                        {t("host.delete")}
                      </Button>
                    </span>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      )}
      <Button variant="outline" size="sm" onClick={onAdd}>
        {t("host.add")}
      </Button>
    </div>
  );
}
