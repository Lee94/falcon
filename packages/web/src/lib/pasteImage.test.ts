import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  imageFromClipboard,
  imagesFromDrop,
  quoteForPrompt,
  type DataTransferLike,
} from "./pasteImage.js";

/** node:test 里没有 File；这些函数只透传对象，用带 type 的桩即可 */
function fakeFile(type: string): File {
  return { type } as File;
}

function clipboard(opts: {
  text?: string;
  files?: string[];
}): DataTransferLike {
  return {
    items: (opts.files ?? []).map((type) => ({
      kind: "file",
      type,
      getAsFile: () => fakeFile(type),
    })),
    getData: (t: string) => (t === "text/plain" ? (opts.text ?? "") : ""),
  };
}

describe("imageFromClipboard", () => {
  it("treats a screenshot (file item, no text) as an image paste", () => {
    const file = imageFromClipboard(clipboard({ files: ["image/png"] }));
    assert.equal(file?.type, "image/png");
  });

  it("prefers text when both are present — Excel/web copies carry a bitmap too", () => {
    assert.equal(
      imageFromClipboard(clipboard({ text: "A1\tB1", files: ["image/png"] })),
      null
    );
  });

  it("ignores plain text and non-image files", () => {
    assert.equal(imageFromClipboard(clipboard({ text: "hello" })), null);
    assert.equal(imageFromClipboard(clipboard({ files: ["application/pdf"] })), null);
    assert.equal(imageFromClipboard(null), null);
  });
});

describe("imagesFromDrop", () => {
  it("keeps only image files, and keeps all of them", () => {
    const dt: DataTransferLike = {
      files: [fakeFile("image/png"), fakeFile("text/plain"), fakeFile("image/jpeg")],
    };
    assert.deepEqual(
      imagesFromDrop(dt).map((f) => f.type),
      ["image/png", "image/jpeg"]
    );
    assert.deepEqual(imagesFromDrop(null), []);
  });
});

describe("quoteForPrompt", () => {
  it("quotes only when the path contains whitespace", () => {
    assert.equal(quoteForPrompt("/home/u/.mojito/paste/img-1.png"), "/home/u/.mojito/paste/img-1.png");
    assert.equal(
      quoteForPrompt("C:\\Users\\a b\\.mojito\\paste\\i.png"),
      '"C:\\Users\\a b\\.mojito\\paste\\i.png"'
    );
  });
});
