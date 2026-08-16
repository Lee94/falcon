import { useTranslation } from "react-i18next";
import { Monitor, Moon, Sun, type LucideIcon } from "lucide-react";
import { useApp } from "../../store.js";
import type { ThemePref } from "../../lib/theme.js";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { menuAnchor } from "./Menu.js";

export const THEME_PREFS: ThemePref[] = ["system", "light", "dark"];

export const THEME_ICONS: Record<ThemePref, LucideIcon> = {
  system: Monitor,
  light: Sun,
  dark: Moon,
};

/** 侧栏底部那个图标按钮：图标显示的是**偏好**，跟随系统时永远是显示器图标 */
export function ThemeButton() {
  const { t } = useTranslation();
  const themePref = useApp((s) => s.themePref);
  const openMenu = useApp((s) => s.openMenu);
  const setTheme = useApp((s) => s.setTheme);
  const Icon = THEME_ICONS[themePref];

  return (
    <Button
      variant="ghost"
      size="icon-sm"
      className="text-muted-foreground"
      aria-label={t("theme.label")}
      title={`${t("theme.label")} · ${t(`theme.${themePref}`)}`}
      onClick={(e) =>
        openMenu({
          ...menuAnchor(e),
          items: THEME_PREFS.map((pref) => ({
            label: t(`theme.${pref}`),
            checked: pref === themePref,
            onSelect: () => setTheme(pref),
          })),
        })
      }
    >
      <Icon />
    </Button>
  );
}

/**
 * 设置页里的三段选择器。一眼能看见三个档位分别是什么、现在停在哪一档——
 * 循环切换的单按钮做不到这件事。
 */
export function ThemeChoice() {
  const { t } = useTranslation();
  const themePref = useApp((s) => s.themePref);
  const setTheme = useApp((s) => s.setTheme);

  return (
    <div
      role="radiogroup"
      aria-label={t("theme.label")}
      className="flex gap-0.5 rounded-md border p-0.5"
    >
      {THEME_PREFS.map((pref) => {
        const Icon = THEME_ICONS[pref];
        const on = pref === themePref;
        return (
          <Button
            key={pref}
            role="radio"
            aria-checked={on}
            variant="ghost"
            size="xs"
            className={cn(
              "font-normal text-muted-foreground",
              on && "bg-accent text-accent-foreground"
            )}
            onClick={() => setTheme(pref)}
          >
            <Icon />
            {t(`theme.${pref}`)}
          </Button>
        );
      })}
    </div>
  );
}
