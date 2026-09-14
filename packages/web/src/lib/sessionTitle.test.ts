import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { sessionLabel, sessionTitle, shellLabel } from "./sessionTitle.js";

describe("sessionTitle", () => {
  it("prefers the hand-given name over everything", () => {
    assert.equal(
      sessionTitle({ name: "打包机", title: "pnpm build", agent: "claude" }),
      "打包机"
    );
  });

  it("falls back to the foreground command, then to the agent's CLI name", () => {
    assert.equal(sessionTitle({ name: "", title: "pnpm dev", agent: "claude" }), "pnpm dev");
    assert.equal(sessionTitle({ name: "", title: undefined, agent: "codex" }), "codex");
  });

  it("returns null for an idle unnamed shell — callers pick their own fallback", () => {
    assert.equal(sessionTitle({ name: "", title: undefined, agent: undefined }), null);
    // 全空白的名字 / 标题等同于没有，不能让一行空白顶掉后面的兜底
    assert.equal(sessionTitle({ name: "   ", title: "  ", agent: undefined }), null);
  });
});

describe("shellLabel", () => {
  it("takes the command name off either separator", () => {
    assert.equal(shellLabel("/bin/zsh"), "zsh");
    assert.equal(shellLabel("C:\\Program Files\\PowerShell\\7\\pwsh.exe"), "pwsh");
    assert.equal(shellLabel("bash"), "bash");
  });

  it("falls back when the project leaves the shell to the platform default", () => {
    assert.equal(shellLabel(undefined), "shell");
    assert.equal(shellLabel(null), "shell");
    assert.equal(shellLabel("/bin/"), "shell");
  });
});

describe("sessionLabel", () => {
  const projects = [
    { id: "p1", shell: "/bin/fish" },
    { id: "p2" },
  ];

  it("falls back to the project's shell for an idle unnamed session", () => {
    const idle = { name: "", title: undefined, agent: undefined, projectId: "p1" };
    assert.equal(sessionLabel(idle, projects), "fish");
    // 项目没配 shell（交给平台默认）时至少给个稳定的词，不能是空行
    assert.equal(sessionLabel({ ...idle, projectId: "p2" }, projects), "shell");
    assert.equal(sessionLabel({ ...idle, projectId: "gone" }, projects), "shell");
  });

  it("never reaches the shell fallback once there is a title", () => {
    assert.equal(
      sessionLabel({ name: "", title: "pnpm dev", agent: undefined, projectId: "p1" }, projects),
      "pnpm dev"
    );
  });
});
