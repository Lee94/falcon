import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { baseOf, buildFileTree, dirOf, type TreeNode } from "./fileTree.js";

const tree = (paths: string[]) => buildFileTree(paths, (p) => p);

/** 把树压成 `dir(name)[...]` / `file(name)` 的字符串，断言起来比嵌套对象好读 */
function shape(nodes: TreeNode<string>[]): string {
  return nodes
    .map((n) => (n.kind === "file" ? n.name : `${n.name}[${shape(n.children)}]`))
    .join(" ");
}

describe("buildFileTree", () => {
  it("compacts a single-child directory chain into one row", () => {
    // packages 只有一个子目录、server 也只有一个 —— 三层缩进什么信息都没多给
    assert.equal(
      shape(tree(["packages/server/src/git/command.ts", "packages/server/src/routes.ts"])),
      "packages/server/src[git[command.ts] routes.ts]"
    );
  });

  it("stops compacting where the tree actually branches", () => {
    assert.equal(
      shape(tree(["packages/server/src/a.ts", "packages/web/src/b.ts"])),
      "packages[server/src[a.ts] web/src[b.ts]]"
    );
  });

  it("does not compact past a directory that holds files of its own", () => {
    assert.equal(shape(tree(["a/b/c.ts", "a/top.ts"])), "a[b[c.ts] top.ts]");
  });

  it("puts directories before files and sorts each group by name", () => {
    assert.equal(shape(tree(["z.ts", "a.ts", "dir/x.ts"])), "dir[x.ts] a.ts z.ts");
  });

  it("counts files across the whole subtree, not just the immediate level", () => {
    const nodes = tree(["a/b/1.ts", "a/b/2.ts", "a/c/3.ts"]);
    const a = nodes[0];
    assert.equal(a?.kind, "dir");
    assert.equal(a.kind === "dir" && a.fileCount, 3);
  });

  it("keeps the full path on every node so it can key React and open a diff", () => {
    const nodes = tree(["packages/web/src/api.ts"]);
    const top = nodes[0];
    assert.equal(top?.kind === "dir" && top.path, "packages/web/src");
    assert.equal(
      top?.kind === "dir" && top.children[0]?.path,
      "packages/web/src/api.ts"
    );
  });

  it("handles a root-level file and an empty input", () => {
    assert.equal(shape(tree(["README.md"])), "README.md");
    assert.deepEqual(tree([]), []);
  });
});

describe("dirOf / baseOf", () => {
  it("splits a nested path", () => {
    assert.equal(dirOf("packages/web/src/api.ts"), "packages/web/src");
    assert.equal(baseOf("packages/web/src/api.ts"), "api.ts");
  });

  it("gives an empty directory for a root-level file", () => {
    assert.equal(dirOf("README.md"), "");
    assert.equal(baseOf("README.md"), "README.md");
  });
});
