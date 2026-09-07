import { strict as assert } from "node:assert";
import test from "node:test";
import {
  classify,
  collapseRemovePaths,
  extOf,
  imageMimeOf,
  mimeOf,
  indexCommand,
  INDEX_SKIP_DIRS,
  listCommand,
  mkdirCommand,
  parseEntries,
  parseIndexLines,
  parseRead,
  readCommand,
  relativizeIndexLine,
  relSegments,
  removeCommand,
  renameCommand,
  resolveInside,
  sortEntries,
  validateEntryName,
} from "./files.js";

test("relSegments 拒绝越界路径", () => {
  assert.deepEqual(relSegments(undefined), []);
  assert.deepEqual(relSegments(""), []);
  assert.deepEqual(relSegments("src/git/path.ts"), ["src", "git", "path.ts"]);
  // 多余的分隔符是无害的，直接吃掉
  assert.deepEqual(relSegments("/src//a/"), ["src", "a"]);
  for (const bad of ["..", "a/../../etc", "./x", "a/\u0000b", "a/\nb"]) {
    assert.throws(() => relSegments(bad), /路径不合法/, bad);
  }
});

test("resolveInside 按宿主机规则拼路径", () => {
  assert.equal(resolveInside("posix", "/home/u/repo", "src/a.ts"), "/home/u/repo/src/a.ts");
  assert.equal(resolveInside("posix", "/home/u/repo", ""), "/home/u/repo");
  assert.equal(resolveInside("windows", "C:\\code\\repo", "src/a.ts"), "C:\\code\\repo\\src\\a.ts");
  // 前端一律用 /，Windows 远端也是——分隔符只在这一步落成平台形式
  assert.equal(resolveInside("windows", "C:/code/repo", "a"), "C:\\code\\repo\\a");
  assert.throws(() => resolveInside("posix", "/home/u/repo", "../other"), /路径不合法/);
});

test("listCommand：posix 逐条判目录并 stat，windows 走 EncodedCommand", () => {
  const posix = listCommand("posix", "/home/u/re po");
  assert.match(posix, /ls -1A "\$d"/);
  assert.match(posix, /'\/home\/u\/re po'/);
  assert.match(posix, /stat -c %Y/);
  assert.match(posix, /stat -f %m/);
  assert.match(posix, /printf '%s %s %s %s/);

  const win = listCommand("windows", "C:\\code\\repo");
  assert.match(win, /^powershell/i);
  assert.match(win, /-EncodedCommand /);
  const script = Buffer.from(win.split(" ").pop()!, "base64").toString("utf16le");
  assert.match(script, /Get-ChildItem -LiteralPath \$d -Force/);
  assert.match(script, /PSIsContainer/);
  assert.match(script, /LastWriteTimeUtc/);
});

test("parseEntries 认 d/f + size + mtime，忽略杂行", () => {
  const entries = parseEntries(
    "d 0 1700000000 src\r\nf 123 1700000001 README.md\nf\n\nnoise\nd 0 1 .github\n",
    "packages"
  );
  assert.deepEqual(entries, [
    { name: "src", path: "packages/src", kind: "dir", mtime: 1700000000 },
    { name: "README.md", path: "packages/README.md", kind: "file", size: 123, mtime: 1700000001 },
    { name: ".github", path: "packages/.github", kind: "dir", mtime: 1 },
  ]);
  // 根目录下不带前缀；名字可以含空格
  const spaced = parseEntries("f 4 0 a b.txt\n", "");
  assert.equal(spaced[0]!.path, "a b.txt");
  assert.equal(spaced[0]!.size, 4);
});

test("relativizeIndexLine 收成工作目录相对路径", () => {
  assert.equal(relativizeIndexLine("/home/u/repo/src/a.ts", "/home/u/repo", "posix"), "src/a.ts");
  assert.equal(relativizeIndexLine("/home/u/repo", "/home/u/repo", "posix"), null);
  assert.equal(relativizeIndexLine("/etc/passwd", "/home/u/repo", "posix"), null);
  assert.equal(
    relativizeIndexLine("C:\\code\\repo\\src\\a.ts", "C:\\code\\repo", "windows"),
    "src/a.ts"
  );
  // Windows 大小写不敏感
  assert.equal(
    relativizeIndexLine("c:\\code\\repo\\b.ts", "C:\\code\\repo", "windows"),
    "b.ts"
  );
});

test("parseIndexLines 丢掉越界行，跳过 node_modules 段", () => {
  const { paths, truncated } = parseIndexLines(
    [
      "/home/u/repo/src/a.ts",
      "/home/u/repo/node_modules/x/index.js",
      "/etc/passwd",
      "/home/u/repo/README.md",
      "__TRUNCATED__",
    ].join("\n"),
    "/home/u/repo",
    "posix"
  );
  assert.deepEqual(paths, ["src/a.ts", "README.md"]);
  assert.equal(truncated, true);
});

test("indexCommand：posix 用 find prune，windows 走 EncodedCommand", () => {
  const posix = indexCommand("posix", "/home/u/re po");
  assert.match(posix, /find "\$d"/);
  assert.match(posix, /-name 'node_modules'/);
  assert.match(posix, /__TRUNCATED__/);

  const win = indexCommand("windows", "C:\\code\\repo");
  assert.match(win, /-EncodedCommand /);
  const script = Buffer.from(win.split(" ").pop()!, "base64").toString("utf16le");
  assert.match(script, /Get-ChildItem -LiteralPath \$p/);
  assert.match(script, /node_modules/);
  assert.ok(INDEX_SKIP_DIRS.includes("node_modules"));
});

test("sortEntries 目录在前，点开头的不沉底", () => {
  const sorted = sortEntries([
    { name: "b.ts", path: "b.ts", kind: "file" },
    { name: "node_modules", path: "node_modules", kind: "dir" },
    { name: ".github", path: ".github", kind: "dir" },
    { name: ".env", path: ".env", kind: "file" },
  ]);
  assert.deepEqual(
    sorted.map((e) => e.name),
    [".github", "node_modules", ".env", "b.ts"]
  );
});

test("readCommand 带上限，windows 循环读到 cap", () => {
  const posix = readCommand("posix", "/repo/a.ts", 1024);
  assert.match(posix, /wc -c < "\$f"/);
  assert.match(posix, /head -c 1024 "\$f" \| \$b64/);
  // base64 不一定存在，得有替补
  assert.match(posix, /openssl base64/);

  const script = Buffer.from(
    readCommand("windows", "C:\\repo\\a.ts", 1024).split(" ").pop()!,
    "base64"
  ).toString("utf16le");
  assert.match(script, /\$ms\.Length -lt 1024/);
  assert.match(script, /ToBase64String/);
});

test("parseRead：首行是真实大小，其余拼回 base64", () => {
  const body = Buffer.from("hello world").toString("base64");
  const res = parseRead(`11\n${body.slice(0, 4)}\n${body.slice(4)}\n`);
  assert.equal(res.size, 11);
  assert.equal(res.bytes.toString("utf8"), "hello world");
  assert.throws(() => parseRead("not-a-number\nzzz"), /读不到文件大小/);
});

test("classify：图片按扩展名，NUL 判二进制，其余当文本", () => {
  // 图片不读字节：readWorkspaceFile 对图片按 cap 0 读，这里收到的就是空 Buffer
  const png = classify("logo.png", 3, Buffer.alloc(0));
  assert.equal(png.kind, "image");
  assert.equal(png.kind === "image" && png.mime, "image/png");
  assert.equal(png.kind === "image" && png.size, 3);

  const bin = classify("a.out", 4, Buffer.from([1, 0, 2, 3]));
  assert.equal(bin.kind, "binary");

  const text = classify("README.md", 5, Buffer.from("hello"));
  assert.equal(text.kind, "text");
  assert.equal(text.kind === "text" && text.truncated, false);

  // 取回来的字节比真实大小少 = 被 cap 截断了
  const cut = classify("big.log", 999_999, Buffer.from("head"));
  assert.equal(cut.kind === "text" && cut.truncated, true);

  // 超过原始字节上限的图片浏览器取不到，只报大小
  const huge = classify("big.png", 99_000_000, Buffer.alloc(0));
  assert.equal(huge.kind, "too-large");
  // 上限内的大图（超过文本上限也无妨）照常是图片
  const big = classify("photo.jpg", 5 * 1024 * 1024, Buffer.alloc(0));
  assert.equal(big.kind, "image");
});

test("mimeOf：按扩展名给 Content-Type，认不出是 octet-stream", () => {
  assert.equal(mimeOf("index.html"), "text/html; charset=utf-8");
  assert.equal(mimeOf("a.HTM"), "text/html; charset=utf-8");
  assert.equal(mimeOf("style.css"), "text/css; charset=utf-8");
  assert.equal(mimeOf("app.mjs"), "text/javascript; charset=utf-8");
  assert.equal(mimeOf("logo.svg"), "image/svg+xml");
  assert.equal(mimeOf("font.woff2"), "font/woff2");
  assert.equal(mimeOf("a.out"), "application/octet-stream");
  assert.equal(mimeOf("Makefile"), "application/octet-stream");
  // 只有 image/* 才走图片预览；html / svg 的区分就在这里
  assert.equal(imageMimeOf("logo.svg"), "image/svg+xml");
  assert.equal(imageMimeOf("index.html"), undefined);
});

test("readCommand cap 0 是合法的空读：只取大小", () => {
  const posix = readCommand("posix", "/w/a.png", 0);
  assert.match(posix, /head -c 0 /);
  const win = readCommand("windows", "C:\\w\\a.png", 0);
  assert.ok(win.length > 0);
  // 空 base64 解出空字节，大小照常
  const r = parseRead("1234\n\n");
  assert.equal(r.size, 1234);
  assert.equal(r.bytes.length, 0);
});

test("extOf 忽略点开头的无扩展名文件", () => {
  assert.equal(extOf("a.tar.gz"), "gz");
  assert.equal(extOf(".gitignore"), "");
  assert.equal(extOf("Makefile"), "");
});

test("validateEntryName 挡分隔符、控制字符与 Windows 保留字符", () => {
  validateEntryName("posix", "a:b?.txt");
  for (const bad of ["", ".", "..", "a/b", "a\\b", "a\nb", "a\u0000b"]) {
    assert.throws(() => validateEntryName("posix", bad), /文件名/, JSON.stringify(bad));
  }
  assert.throws(() => validateEntryName("windows", "a:b.txt"), /Windows/);
});

test("collapseRemovePaths 丢掉空串、收进祖先", () => {
  assert.deepEqual(collapseRemovePaths(["", "src", "src/a.ts", "README.md", "src/"]), [
    "README.md",
    "src",
  ]);
  assert.deepEqual(collapseRemovePaths([]), []);
});

test("mkdirCommand：非递归要上级在，递归是 mkdir -p", () => {
  const posix = mkdirCommand("posix", "/home/u/repo/src", "/home/u/repo", false);
  assert.match(posix, /mkdir -- "\$p"/);
  assert.match(posix, /ENOENT/);
  assert.doesNotMatch(posix, /mkdir -p/);

  const rec = mkdirCommand("posix", "/home/u/repo/a/b", "/home/u/repo/a", true);
  assert.match(rec, /mkdir -p -- "\$p"/);

  const win = mkdirCommand("windows", "C:\\code\\repo\\src", "C:\\code\\repo", false);
  const script = Buffer.from(win.split(" ").pop()!, "base64").toString("utf16le");
  assert.match(script, /New-Item -ItemType Directory -LiteralPath \$p/);
  assert.doesNotMatch(script, /-Force/);
});

test("renameCommand 已存在则 EEXIST，缺源则 ENOENT", () => {
  const posix = renameCommand("posix", "/r/a", "/r/b");
  assert.match(posix, /mv -- "\$s" "\$d"/);
  assert.match(posix, /ENOENT/);
  assert.match(posix, /EEXIST/);

  const script = Buffer.from(
    renameCommand("windows", "C:\\r\\a", "C:\\r\\b").split(" ").pop()!,
    "base64"
  ).toString("utf16le");
  assert.match(script, /Move-Item -LiteralPath \$s/);
});

test("removeCommand 走 rm -rf / Directory.Delete，不跟符号链接", () => {
  const posix = removeCommand("posix", "/home/u/repo/src");
  assert.match(posix, /rm -rf -- "\$p"/);
  assert.match(posix, /'\/home\/u\/repo\/src'/);

  const script = Buffer.from(
    removeCommand("windows", "C:\\code\\repo\\src").split(" ").pop()!,
    "base64"
  ).toString("utf16le");
  assert.match(script, /\[IO\.Directory\]::Delete\(\$p, \$true\)/);
  assert.match(script, /\[IO\.File\]::Delete\(\$p\)/);
  assert.doesNotMatch(script, /Remove-Item/);
});
