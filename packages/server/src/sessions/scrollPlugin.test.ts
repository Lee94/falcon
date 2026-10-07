import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { describe, it } from "node:test";
import {
  posixPermissionsCommand,
  posixPluginDigestCommand,
  posixPluginInstallCommand,
  targetOs,
} from "./scrollPlugin.js";

const sh = (cmd: string, input?: string) =>
  execFileSync("/bin/sh", ["-c", cmd], { input, encoding: "utf8" });

describe("targetOs", () => {
  it("按 zellij target 分出 permissions.kdl 的位置口径", () => {
    assert.equal(targetOs("aarch64-apple-darwin"), "darwin");
    assert.equal(targetOs("x86_64-unknown-linux-musl"), "linux");
    assert.equal(targetOs("x86_64-pc-windows-msvc"), null);
  });
});

// 下面几条真跑一遍生成的 sh 命令：引号嵌套最容易错，看字符串看不出来
describe("POSIX 部署命令", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "falcon-scroll-"));
  const plugin = path.join(dir, "with space", "falcon-scroll.wasm");

  it("没装过时摘要为空；装完摘要对得上、本体完整、不留 .partial", () => {
    assert.equal(sh(posixPluginDigestCommand(plugin)).trim(), "");
    sh(posixPluginInstallCommand(plugin, "abc123"), "wasm-bytes");
    assert.equal(fs.readFileSync(plugin, "utf8"), "wasm-bytes");
    assert.equal(sh(posixPluginDigestCommand(plugin)).trim(), "abc123");
    assert.ok(!fs.existsSync(`${plugin}.partial`));
  });

  it("预授权只追加一次，并保留文件里原有的条目", () => {
    const perm = path.join(dir, "cache", "zellij", "permissions.kdl");
    fs.mkdirSync(path.dirname(perm), { recursive: true });
    fs.writeFileSync(perm, '"/other.wasm" {\n    ReadCliPipes\n}'); // 末尾故意没有换行
    sh(posixPermissionsCommand(perm, plugin));
    sh(posixPermissionsCommand(perm, plugin));
    const text = fs.readFileSync(perm, "utf8");
    assert.equal(text.split(`"${plugin}" {`).length - 1, 1);
    assert.ok(text.startsWith('"/other.wasm" {\n    ReadCliPipes\n}\n'));
  });

  it("文件不存在时连目录一起建", () => {
    const perm = path.join(dir, "fresh", "deep", "permissions.kdl");
    sh(posixPermissionsCommand(perm, plugin));
    assert.ok(fs.readFileSync(perm, "utf8").includes(`"${plugin}" {\n    ReadApplicationState\n`));
  });
});
