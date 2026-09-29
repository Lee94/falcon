import { useEffect, useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { ImagePlus } from "lucide-react";
import {
  APP_ICON_IDS,
  builtinIconUrl,
  customIconUrl,
  type AppIconChoice,
  type AppIconState,
} from "@falcon/shared";
import { api } from "../api.js";
import { applyAppIconLinks, normalizeIconImage } from "../lib/appIcon.js";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";

/**
 * 设置 › 外观 › 应用图标（ADR 0018）。内置几套 + 一格自定义；选择是服务端级的，
 * 换了之后本页标签页图标立刻跟着换，别的设备下次加载换。
 */
export function AppIconPicker() {
  const { t } = useTranslation();
  const [state, setState] = useState<AppIconState | null>(null);
  const [busy, setBusy] = useState(false);
  const fileRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    let alive = true;
    api
      .appIcon()
      .then((s) => alive && setState(s))
      .catch((err: Error) => toast.error(t("appIcon.loadFailed"), { description: err.message }));
    return () => {
      alive = false;
    };
  }, [t]);

  const run = async (task: () => Promise<AppIconState>, failKey: string) => {
    setBusy(true);
    try {
      const next = await task();
      setState(next);
      applyAppIconLinks(next);
    } catch (err) {
      toast.error(t(failKey), { description: (err as Error).message });
    } finally {
      setBusy(false);
    }
  };

  const choose = (id: AppIconChoice) => {
    if (!state || busy || state.selected === id) return;
    void run(() => api.setAppIcon(id), "appIcon.saveFailed");
  };

  const upload = async (file: File) => {
    let png: Blob;
    try {
      png = await normalizeIconImage(file);
    } catch {
      toast.error(t("appIcon.decodeFailed"), { description: file.name });
      return;
    }
    await run(() => api.uploadAppIcon(png), "appIcon.uploadFailed");
  };

  const pick = () => fileRef.current?.click();

  return (
    <div className="pt-1">
      <div
        role="radiogroup"
        aria-label={t("appIcon.title")}
        aria-busy={busy || !state}
        className="grid grid-cols-[repeat(auto-fill,minmax(6rem,1fr))] gap-2"
      >
        {APP_ICON_IDS.map((id) => (
          <Tile
            key={id}
            on={state?.selected === id}
            disabled={!state || busy}
            label={t(`appIcon.name_${id.replace(/-/g, "_")}`)}
            onSelect={() => choose(id)}
          >
            <img src={builtinIconUrl(id, "icon-192.png")} alt="" className="size-14" draggable={false} />
          </Tile>
        ))}
        {state?.custom ? (
          <Tile
            on={state.selected === "custom"}
            disabled={busy}
            label={t("appIcon.custom")}
            onSelect={() => choose("custom")}
          >
            <img
              src={customIconUrl(state.custom)}
              alt=""
              className="size-14 rounded-[0.8rem] object-contain"
              draggable={false}
            />
          </Tile>
        ) : (
          <button
            type="button"
            disabled={!state || busy}
            onClick={pick}
            className="flex flex-col items-center gap-2 rounded-lg px-2 pt-3 pb-2 text-xs text-muted-foreground outline-none transition-colors hover:bg-accent/50 hover:text-foreground focus-visible:ring-1 focus-visible:ring-ring disabled:opacity-50"
          >
            <span className="sunken flex size-14 items-center justify-center">
              <ImagePlus className="size-5" />
            </span>
            {t("appIcon.custom")}
          </button>
        )}
      </div>
      <div className="mt-3 flex items-center justify-between gap-3">
        <p className="text-xs leading-relaxed text-muted-foreground">{t("appIcon.customHint")}</p>
        <div className="flex shrink-0 gap-2">
          {state?.custom && (
            <Button
              variant="outline"
              size="xs"
              disabled={busy}
              onClick={() => void run(() => api.removeCustomAppIcon(), "appIcon.removeFailed")}
            >
              {t("appIcon.remove")}
            </Button>
          )}
          <Button variant="outline" size="xs" disabled={!state || busy} onClick={pick}>
            {t(state?.custom ? "appIcon.replace" : "appIcon.upload")}
          </Button>
        </div>
      </div>
      <input
        ref={fileRef}
        type="file"
        accept="image/*"
        hidden
        onChange={(e) => {
          const file = e.target.files?.[0];
          e.target.value = "";
          if (file) void upload(file);
        }}
      />
    </div>
  );
}

function Tile({
  on,
  disabled,
  label,
  onSelect,
  children,
}: {
  on: boolean;
  disabled: boolean;
  label: string;
  onSelect: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      role="radio"
      aria-checked={on}
      disabled={disabled}
      onClick={onSelect}
      className={cn(
        "flex flex-col items-center gap-2 rounded-lg px-2 pt-3 pb-2 text-xs outline-none transition-colors focus-visible:ring-1 focus-visible:ring-ring disabled:cursor-default",
        on ? "bg-tint font-medium text-tint-foreground" : "text-muted-foreground hover:bg-accent/50 hover:text-foreground"
      )}
    >
      {children}
      <span className="max-w-full truncate">{label}</span>
    </button>
  );
}
