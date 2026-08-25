import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { basename, dirname, filterFiles, scorePath } from "./fileSearch.js";

describe("basename / dirname", () => {
  it("splits on the last slash", () => {
    assert.equal(basename("src/lib/a.ts"), "a.ts");
    assert.equal(dirname("src/lib/a.ts"), "src/lib");
    assert.equal(basename("README.md"), "README.md");
    assert.equal(dirname("README.md"), "");
  });
});

describe("filterFiles", () => {
  const paths = [
    "README.md",
    "src/index.ts",
    "src/lib/shortcuts.ts",
    "packages/web/src/store.ts",
    "packages/server/src/files.ts",
  ];

  it("empty query returns the first N paths", () => {
    assert.deepEqual(filterFiles(paths, "", 2), ["README.md", "src/index.ts"]);
  });

  it("ranks basename prefix above a path substring", () => {
    const hit = filterFiles(paths, "file");
    assert.equal(hit[0], "packages/server/src/files.ts");
  });

  it("fuzzy subsequence on the filename", () => {
    const hit = filterFiles(paths, "scts");
    assert.ok(hit.includes("src/lib/shortcuts.ts"));
  });

  it("drops non-matches", () => {
    assert.deepEqual(filterFiles(paths, "zzz"), []);
  });
});

describe("scorePath", () => {
  it("exact basename wins over a path substring", () => {
    assert.equal(scorePath("src/a.ts", "a.ts"), 0);
    assert.ok((scorePath("a.ts", "a.ts") as number) < (scorePath("src/a.ts.bak", "a.ts") as number));
  });
});
