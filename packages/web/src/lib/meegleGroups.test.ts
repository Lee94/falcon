import test from "node:test";
import assert from "node:assert/strict";
import { filterMeegleItems, groupMeegleItems, meeglePage } from "./meegleGroups.js";

test("business/type/status groups preserve records and count each level", () => {
  const rows = [
    { id: "1", spaceKey: "a", name: "one", business: "B", typeKey: "story", status: "Open" },
    { id: "2", spaceKey: "b", name: "two", business: "B", typeKey: "story", status: "Done" },
    { id: "3", spaceKey: "a", name: "three", business: " ", typeKey: "story" },
    { id: "4", spaceKey: "a", name: "four", business: "null", typeKey: "story" },
  ];
  const groups = groupMeegleItems(rows);
  assert.deepEqual(groups.map(g => g.count), [2, 1, 1]);
  assert.equal(groups[0].children?.[0].count, 2);
  assert.deepEqual(groups[0].children?.[0].children?.map(g => g.label), ["Open", "Done"]);
  assert.equal(groups[1].label, undefined);
  assert.notEqual(groups[1].key, groups[2].key);
  assert.deepEqual(rows.map(r => r.id), ["1", "2", "3", "4"]);
});

test("100-item pages replace rather than accumulate and clamp bounds", () => {
  const rows = Array.from({ length: 201 }, (_, i) => i);
  assert.equal(meeglePage(rows, 1).items.length, 100);
  assert.deepEqual(meeglePage(rows, 2).items, rows.slice(100, 200));
  assert.deepEqual(meeglePage(rows, 99).items, [200]);
  assert.equal(meeglePage(rows, 3).hasMore, false);
  assert.equal(meeglePage(rows, 0).page, 1);
  assert.deepEqual(meeglePage([], 8).items, []);
});

test("filtering is case insensitive and only sees its supplied page, including business and todo fields", () => {
  const rows = Array.from({ length: 101 }, (_, i) => ({
    id: String(i), spaceKey: "a", typeKey: "story", name: `item ${i}`,
    business: i === 100 ? "Payments" : "Core", nodeName: "Review",
  }));
  assert.deepEqual(filterMeegleItems(meeglePage(rows, 1).items, "payments"), []);
  assert.equal(filterMeegleItems(meeglePage(rows, 2).items, " PAYMENTS ").length, 1);
  assert.equal(filterMeegleItems(rows, "review").length, 101);
  assert.equal(filterMeegleItems(rows, " "), rows);
});
