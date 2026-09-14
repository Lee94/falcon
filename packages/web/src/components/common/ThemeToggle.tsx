import { useTranslation } from "react-i18next";
import { Monitor, Moon, Sun, type LucideIcon } from "lucide-react";
import { useApp } from "../../store.js";
import type { ThemePref } from "../../lib/theme/pref.js";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";

export const THEME_PREFS: ThemePref[] = ["system", "light", "dark"];

export const THEME_ICONS: Record<ThemePref, LucideIcon> = {
  system: Monitor,
  light: Sun,
  dark: Moon,
};

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
      className="sunken flex gap-0.5 p-0.5"
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
              "font-normal text-muted-foreground hover:bg-transparent hover:text-foreground",
              on && "island rounded-md font-medium text-foreground"
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
