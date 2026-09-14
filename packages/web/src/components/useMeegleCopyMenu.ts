import type { HTMLAttributes } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import type { MeegleWorkItem } from "@falcon/shared";
import { api } from "../api.js";
import { useApp } from "../store.js";
import { formatMeegleContext } from "../lib/meegleContext.js";
import { meegleDisplayKey } from "../lib/meegleKey.js";
import { writeClipboardText } from "../lib/clipboard.js";
import { openContextMenu } from "./common/Menu.js";

type Target = Pick<MeegleWorkItem, "id" | "spaceKey">;

export function useMeegleCopyMenu(onUnavailable?: (err: unknown) => void) {
  const { t } = useTranslation();
  const copyKey = async (item: Target) => {
    try {
      await writeClipboardText(async () => {
        try {
          return meegleDisplayKey(await api.meegleWorkItem(item.spaceKey, item.id));
        } catch (err) {
          useApp.getState().handleApiError(err);
          onUnavailable?.(err);
          throw err;
        }
      });
      toast.success(t("meegle.copyKeyDone"));
    } catch {
      toast.error(t("meegle.copyFailed"));
    }
  };
  const copyContext = async (item: Target) => {
    const notification = toast.loading(t("meegle.copyContextLoading"));
    try {
      await writeClipboardText(async () => {
        try {
          // 明确获取最新详情；复制出的获取时间不冒充旧缓存的采集时间。
          const detail = await api.meegleWorkItem(item.spaceKey, item.id, true);
          return formatMeegleContext(detail, new Date().toISOString(), (key) => t(key));
        } catch (err) {
          useApp.getState().handleApiError(err);
          onUnavailable?.(err);
          throw err;
        }
      });
      toast.success(t("meegle.copyContextDone"), { id: notification });
    } catch {
      toast.error(t("meegle.copyContextFailed"), { id: notification });
    }
  };
  return (item: Target | null): HTMLAttributes<HTMLElement> => {
    if (!item) return {};
    const items = [
      { label: t("meegle.copyKey"), onSelect: () => void copyKey(item) },
      { label: t("meegle.copyContext"), onSelect: () => void copyContext(item) },
    ];
    return {
      onContextMenu: (event) => openContextMenu(event, items),
      onKeyDown: (event) => {
        if (event.key !== "ContextMenu" && !(event.shiftKey && event.key === "F10")) return;
        const rect = event.currentTarget.getBoundingClientRect();
        openContextMenu({
          clientX: rect.left,
          clientY: rect.bottom,
          preventDefault: () => event.preventDefault(),
          stopPropagation: () => event.stopPropagation(),
        }, items);
      },
    };
  };
}
