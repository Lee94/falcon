import assert from "node:assert/strict";
import { test } from "node:test";
import { parseDrill, serializeDrill, type Drill } from "./meegleDrill.js";

const stack: Drill[] = [
  { kind: "view", spaceKey: "space", viewId: "v1", label: "我的视图", multi: false, typeName: "需求" },
  { kind: "item", spaceKey: "space", id: "7105690993", title: "Broken chart", typeKey: "issue" },
];

test("a drilled-in position survives a round trip through storage", () => {
  assert.deepEqual(parseDrill(serializeDrill(stack)), stack);
});

test("nothing stored means the panel opens at its tab root", () => {
  assert.deepEqual(parseDrill(null), []);
  assert.deepEqual(parseDrill(""), []);
  assert.deepEqual(parseDrill("{]"), []);
  assert.deepEqual(parseDrill('{"kind":"item"}'), []);
});

test("layers missing the identifiers the CLI needs are dropped, along with everything above them", () => {
  const raw = JSON.stringify([
    { kind: "view", spaceKey: "space", viewId: "v1", label: "我的视图", multi: false },
    { kind: "item", spaceKey: "space", title: "no id" },
    { kind: "item", spaceKey: "space", id: "7105690993", title: "unreachable" },
  ]);
  assert.deepEqual(parseDrill(raw), [
    { kind: "view", spaceKey: "space", viewId: "v1", label: "我的视图", multi: false },
  ]);
  assert.deepEqual(parseDrill(JSON.stringify([{ kind: "view", viewId: "v1" }])), []);
  assert.deepEqual(parseDrill(JSON.stringify([{ kind: "other", spaceKey: "space", id: "1" }])), []);
});

test("a view that lost its name still opens, labelled by its id", () => {
  const [view] = parseDrill(JSON.stringify([{ kind: "view", spaceKey: "space", viewId: "v1" }]));
  assert.equal(view?.kind === "view" && view.label, "v1");
  assert.equal(view?.kind === "view" && view.multi, false);
});

test("an item keeps opening without a placeholder title — the detail fetch fills it in", () => {
  const [item] = parseDrill(JSON.stringify([{ kind: "item", spaceKey: "space", id: "710" }]));
  assert.equal(item?.kind === "item" && item.title, "");
});

test("a runaway stack is capped instead of growing in storage forever", () => {
  const deep = Array.from({ length: 12 }, (_, i) => ({
    kind: "item" as const,
    spaceKey: "space",
    id: `${i}`,
    title: `#${i}`,
  }));
  assert.equal(parseDrill(JSON.stringify(deep)).length, 8);
  assert.equal(JSON.parse(serializeDrill(deep)).length, 8);
});
