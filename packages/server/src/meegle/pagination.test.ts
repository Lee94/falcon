import { strict as assert } from "node:assert";
import test from "node:test";
import { logicalPage } from "./pagination.js";
import { normalizeMultiViewItems, normalizeTodo, normalizeViewItems } from "./command.js";

test("logical page 2 requests physical 3 and 4; final CLI hasMore wins even for a short page", async () => {
  const calls: number[] = [];
  const result = await logicalPage(2, async (page) => {
    calls.push(page);
    return { items: [page], page, hasMore: true, total: 500 };
  });
  assert.deepEqual(calls, [3, 4]);
  assert.deepEqual(result, { items: [3, 4], page: 2, hasMore: true, total: 500 });
});

for (const total of [0, 1, 49, 50, 51, 99, 100, 101, 150, 200]) {
  test(`todo logical paging boundaries: ${total} rows`, async () => {
    const calls: number[] = [];
    const result = await logicalPage(1, async (page) => {
      calls.push(page);
      const count = Math.max(0, Math.min(50, total - (page - 1) * 50));
      return normalizeTodo({ total, list: Array.from({ length: count }, (_, i) => ({
        work_item_info: { work_item_id: String((page - 1) * 50 + i + 1) },
      })) }, page);
    });
    assert.equal(result.items.length, Math.min(total, 100));
    assert.equal(result.hasMore, total > 100);
    assert.deepEqual(calls, total <= 50 ? [1] : [1, 2]);
  });
}

test("view and multi-view logical pages preserve last pagination flag and total", async () => {
  for (const normalize of [
    (page: number) => normalizeViewItems({
      work_item_list: [{ work_item_attribute: { work_item_id: String(page) } }],
      pagination: { has_more: page === 1, total: 2 },
    }, null, page),
    (page: number) => normalizeMultiViewItems({
      data: [{ work_item_id: String(page) }],
      pagination: { has_more: page === 1, total: 2 },
    }, page),
  ]) {
    const result = await logicalPage(1, async (page) => normalize(page));
    assert.deepEqual(result.items.map((i) => i.id), ["1", "2"]);
    assert.equal(result.hasMore, false);
    assert.equal(result.total, 2);
  }
});

test("an empty terminal CLI page ends unknown-total todo pages; errors are not hidden", async () => {
  const result = await logicalPage(1, async (page) => normalizeTodo({
    list: page === 1 ? Array.from({ length: 50 }, (_, i) => ({ work_item_info: { work_item_id: String(i) } })) : [],
  }, page));
  assert.equal(result.items.length, 50);
  assert.equal(result.hasMore, false);
  await assert.rejects(logicalPage(1, async () => { throw new Error("CLI unavailable"); }), /CLI unavailable/);
});
