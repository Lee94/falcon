import assert from "node:assert/strict";
import test from "node:test";
import {
  MEEGLE_WORK_ITEM_MIME,
  hasMeegleWorkItemType,
  parseMeegleWorkItemDrag,
  writeMeegleWorkItemDrag,
} from "./meegleDrag.js";

function reader(types: string[], value: string) {
  return {
    types,
    getData: (type: string) => (type === MEEGLE_WORK_ITEM_MIME ? value : ""),
  } as Pick<DataTransfer, "types" | "getData">;
}

test("writes and parses the private work-item payload", () => {
  const values = new Map<string, string>();
  const transfer = {
    effectAllowed: "uninitialized",
    setData: (type: string, value: string) => values.set(type, value),
  } as Pick<DataTransfer, "effectAllowed" | "setData">;

  assert.equal(writeMeegleWorkItemDrag(transfer, { id: "123456", spaceKey: "space_key" }), true);
  assert.equal(transfer.effectAllowed, "copy");
  const parsed = parseMeegleWorkItemDrag(
    reader([MEEGLE_WORK_ITEM_MIME], values.get(MEEGLE_WORK_ITEM_MIME) ?? "")
  );
  assert.deepEqual(parsed, {
    version: 1,
    kind: "meegle-work-item",
    id: "123456",
    spaceKey: "space_key",
  });
});

test("ignores ordinary browser drags without the private MIME type", () => {
  const data = reader(["text/plain"], JSON.stringify({
    version: 1,
    kind: "meegle-work-item",
    id: "123",
    spaceKey: "space",
  }));
  assert.equal(hasMeegleWorkItemType(data), false);
  assert.equal(parseMeegleWorkItemDrag(data), null);
});

test("rejects malformed, wrong-version, and invalid identifier payloads", () => {
  const parse = (value: unknown) =>
    parseMeegleWorkItemDrag(
      reader([MEEGLE_WORK_ITEM_MIME], typeof value === "string" ? value : JSON.stringify(value))
    );
  assert.equal(parse("{"), null);
  assert.equal(parse({ version: 2, kind: "meegle-work-item", id: "123", spaceKey: "space" }), null);
  assert.equal(parse({ version: 1, kind: "meegle-work-item", id: "feat/x", spaceKey: "space" }), null);
  assert.equal(parse({ version: 1, kind: "meegle-work-item", id: "123", spaceKey: "../space" }), null);
});

test("does not write a payload when the item lacks safe identifiers", () => {
  let wrote = false;
  const transfer = {
    effectAllowed: "uninitialized",
    setData: () => {
      wrote = true;
    },
  } as Pick<DataTransfer, "effectAllowed" | "setData">;
  assert.equal(writeMeegleWorkItemDrag(transfer, { id: "", spaceKey: "space" }), false);
  assert.equal(wrote, false);
});
