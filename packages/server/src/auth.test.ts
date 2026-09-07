import assert from "node:assert/strict";
import { test } from "node:test";
import { Auth } from "./auth.js";
import type { Db } from "./db.js";

/** 只需要 getSetting / setSetting 两个方法；password_hash 设了，鉴权就是"需要的" */
function fakeDb(): Db {
  const settings = new Map<string, string>();
  return {
    getSetting: (k: string) => settings.get(k),
    setSetting: (k: string, v: string) => void settings.set(k, v),
  } as unknown as Db;
}

test("原始字节令牌：只对签发它的项目有效，按项目复用，logout 整批作废", () => {
  const auth = new Auth(fakeDb(), true);
  const a = auth.rawToken("p1");
  assert.ok(a.length >= 32);
  assert.equal(auth.rawTokenValid(a, "p1"), true);
  assert.equal(auth.rawTokenValid(a, "p2"), false);
  assert.equal(auth.rawTokenValid(undefined, "p1"), false);
  assert.equal(auth.rawTokenValid("nope", "p1"), false);

  // 同一项目短时间内再要一枚，给的还是这枚
  assert.equal(auth.rawToken("p1"), a);
  const b = auth.rawToken("p2");
  assert.notEqual(a, b);
  assert.equal(auth.rawTokenValid(b, "p2"), true);

  auth.logout(undefined);
  assert.equal(auth.rawTokenValid(a, "p1"), false);
  assert.equal(auth.rawTokenValid(b, "p2"), false);
  // 作废后再签是新的
  assert.notEqual(auth.rawToken("p1"), a);
});

test("原始字节令牌与登录 token 无关：拿着它过不了 isAuthenticated", () => {
  const auth = new Auth(fakeDb(), true);
  auth.setPassword("secret1");
  const raw = auth.rawToken("p1");
  const req = { cookies: { falcon_token: raw } } as unknown as Parameters<Auth["isAuthenticated"]>[0];
  assert.equal(auth.isAuthenticated(req), false);
});
