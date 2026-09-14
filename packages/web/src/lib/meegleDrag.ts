import {
  isValidMeegleKey,
  isValidMeegleWorkItemId,
  type MeegleWorkItem,
} from "@falcon/shared";

/** Falcon-private drag type: plain text/URLs must never trigger project derivation. */
export const MEEGLE_WORK_ITEM_MIME = "application/x-falcon-meegle-work-item+json";

export interface MeegleWorkItemDragPayload {
  version: 1;
  kind: "meegle-work-item";
  id: string;
  spaceKey: string;
}

type DragReader = Pick<DataTransfer, "types" | "getData">;
type DragWriter = Pick<DataTransfer, "setData" | "effectAllowed">;

export function canDragMeegleWorkItem(
  item: Pick<MeegleWorkItem, "id" | "spaceKey">
): boolean {
  return isValidMeegleWorkItemId(item.id) && isValidMeegleKey(item.spaceKey);
}

export function hasMeegleWorkItemType(data: Pick<DataTransfer, "types">): boolean {
  return Array.from(data.types).includes(MEEGLE_WORK_ITEM_MIME);
}

export function parseMeegleWorkItemDrag(data: DragReader): MeegleWorkItemDragPayload | null {
  if (!hasMeegleWorkItemType(data)) return null;
  const raw = data.getData(MEEGLE_WORK_ITEM_MIME);
  if (!raw || raw.length > 256) return null;
  try {
    const value: unknown = JSON.parse(raw);
    if (
      typeof value !== "object" ||
      value === null ||
      (value as Record<string, unknown>).version !== 1 ||
      (value as Record<string, unknown>).kind !== "meegle-work-item"
    ) {
      return null;
    }
    const { id, spaceKey } = value as Record<string, unknown>;
    if (!isValidMeegleWorkItemId(id) || !isValidMeegleKey(spaceKey)) return null;
    return { version: 1, kind: "meegle-work-item", id, spaceKey };
  } catch {
    return null;
  }
}

export function writeMeegleWorkItemDrag(
  data: DragWriter,
  item: Pick<MeegleWorkItem, "id" | "spaceKey">
): boolean {
  if (!canDragMeegleWorkItem(item)) return false;
  const payload: MeegleWorkItemDragPayload = {
    version: 1,
    kind: "meegle-work-item",
    id: item.id,
    spaceKey: item.spaceKey,
  };
  data.effectAllowed = "copy";
  data.setData(MEEGLE_WORK_ITEM_MIME, JSON.stringify(payload));
  return true;
}
