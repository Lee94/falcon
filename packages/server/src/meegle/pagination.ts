import { MEEGLE_PAGE_SIZE, type MeeglePage } from "@falcon/shared";
import { CLI_PAGE_SIZE } from "./command.js";

/**
 * CLI 固定 50 条，REST 按 100 条组装；提前结束不再取后一页。
 * hasMore 必须来自最后一个实际取到的 CLI 页，不能用合并后条数猜。
 */
export async function logicalPage<T>(
  page: number,
  load: (cliPage: number) => Promise<MeeglePage<T>>
): Promise<MeeglePage<T>> {
  const count = MEEGLE_PAGE_SIZE / CLI_PAGE_SIZE;
  const items: T[] = [];
  let total: number | undefined;
  let hasMore = false;
  for (let offset = 1; offset <= count; offset++) {
    const result = await load((page - 1) * count + offset);
    items.push(...result.items);
    total = result.total ?? total;
    hasMore = result.hasMore;
    if (!hasMore) break;
  }
  return { items, page, hasMore, total };
}
