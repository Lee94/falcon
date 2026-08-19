import assert from "node:assert/strict";
import { describe, it } from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { Db } from "./db.js";

describe("upsertZellijHost", () => {
  it("keeps omitted fields but clears fields explicitly set to null", () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "mojito-db-"));
    const db = new Db(dir);

    db.upsertZellijHost("h", 22, "u", {
      authorized: 1,
      installed_version: "0.44.3",
      verified_durable: 0,
    });

    // 省略的字段保持原值
    db.upsertZellijHost("h", 22, "u", { base_url: "https://mirror.example" });
    let row = db.getZellijHost("h", 22, "u")!;
    assert.equal(row.verified_durable, 0);
    assert.equal(row.installed_version, "0.44.3");
    assert.equal(row.base_url, "https://mirror.example");

    // 显式 null = 清空：重试前作废持久性判定、换下载源作废安装记录。
    // 用 ?? 合并的话这里会被旧值顶回去、清空静默失效——这正是曾经的 bug。
    db.upsertZellijHost("h", 22, "u", {
      verified_durable: null,
      installed_version: null,
      base_url: null,
    });
    row = db.getZellijHost("h", 22, "u")!;
    assert.equal(row.verified_durable, null);
    assert.equal(row.installed_version, null);
    assert.equal(row.base_url, null);
    assert.equal(row.authorized, 1);
  });
});
