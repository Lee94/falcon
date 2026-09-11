import assert from "node:assert/strict";
import { test } from "node:test";
import { writeClipboardText } from "./clipboard.js";

class FakeItem {
  constructor(readonly data: Record<string, Blob | Promise<Blob> | string>) {}
}
const Item = FakeItem as unknown as typeof ClipboardItem;

test("clipboard write starts synchronously while detail loading remains pending", async () => {
  let resolve!: (value: string) => void;
  const content = new Promise<string>((done) => { resolve = done; });
  let started = false;
  let copied = "";
  const operation = writeClipboardText(() => content, {
    async write(items) {
      started = true;
      const item = items[0] as unknown as FakeItem;
      copied = await (await item.data["text/plain"] as Blob).text();
    },
    async writeText() { assert.fail("must use deferred ClipboardItem"); },
  }, Item);
  assert.equal(started, true);
  resolve("full Markdown");
  await operation;
  assert.equal(copied, "full Markdown");
});

test("a failed detail request rejects copying instead of copying a partial fallback", async () => {
  await assert.rejects(writeClipboardText(async () => { throw new Error("detail failed"); }, {
    async write(items) {
      await (items[0] as unknown as FakeItem).data["text/plain"];
    },
    async writeText() { assert.fail("must not copy fallback"); },
  }, Item), /detail failed/);
});

test("older clipboard APIs use writeText and propagate permission failures", async () => {
  let copied = "";
  const clipboard = {
    async write() { assert.fail("unsupported"); },
    async writeText(text: string) { copied = text; },
  };
  await writeClipboardText(async () => "complete", clipboard, undefined);
  assert.equal(copied, "complete");
  await assert.rejects(writeClipboardText(async () => "complete", {
    ...clipboard,
    async writeText() { throw new Error("permission"); },
  }, undefined), /permission/);
});
