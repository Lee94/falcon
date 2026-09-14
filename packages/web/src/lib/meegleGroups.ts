import { MEEGLE_PAGE_SIZE, type MeegleTodoItem, type MeegleWorkItem } from "@falcon/shared";

export interface ItemGroup<T> {
  key: string;
  label?: string;
  count: number;
  children?: ItemGroup<T>[];
  items?: T[];
}

/** Group only the supplied page. Missing values never collide with real labels. */
export function groupMeegleItems<T extends MeegleWorkItem>(items: T[]): ItemGroup<T>[] {
  const group = (rows: T[], level: number): ItemGroup<T>[] => {
    const buckets = new Map<string, { label?: string; rows: T[] }>();
    for (const row of rows) {
      const label = (level === 0 ? row.business
        : level === 1 ? row.typeName || row.typeKey : row.status)?.trim() || undefined;
      const identity = level === 1 ? row.typeKey || label : label;
      const key = JSON.stringify(identity ?? null);
      const bucket = buckets.get(key) ?? { label, rows: [] };
      bucket.rows.push(row);
      buckets.set(key, bucket);
    }
    return [...buckets].map(([key, { label, rows }]) => ({
      key, label, count: rows.length,
      ...(level === 2 ? { items: rows } : { children: group(rows, level + 1) }),
    }));
  };
  return group(items, 0);
}

/** Local paging is confined to the returned search/recent snapshot, not remote totals. */
export function meeglePage<T>(items: T[], requested: number) {
  const pages = Math.max(1, Math.ceil(items.length / MEEGLE_PAGE_SIZE));
  const page = Math.max(1, Math.min(pages, Math.floor(requested) || 1));
  return {
    page, pages,
    items: items.slice((page - 1) * MEEGLE_PAGE_SIZE, page * MEEGLE_PAGE_SIZE),
    hasMore: page < pages,
  };
}

export function filterMeegleItems<T extends MeegleWorkItem & Partial<MeegleTodoItem>>(
  items: T[], filter: string,
): T[] {
  const q = filter.trim().toLowerCase();
  if (!q) return items;
  return items.filter(it =>
    [it.name, it.id, it.spaceName, it.typeName, it.nodeName, it.stateName, it.status,
      it.business]
      .filter((s): s is string => Boolean(s))
      .some(s => s.toLowerCase().includes(q))
  );
}
