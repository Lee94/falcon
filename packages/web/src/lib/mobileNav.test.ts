import { test } from "node:test";
import assert from "node:assert/strict";
import { siblingSession, swipeDir } from "./mobileNav.js";

const at = (x: number, y: number, t: number) => ({ x, y, t });

test("swipeDir: 快速长距横滑判定方向（左滑=1 下一个，右滑=-1 上一个）", () => {
  assert.equal(swipeDir(at(300, 100, 0), at(100, 110, 200)), 1);
  assert.equal(swipeDir(at(100, 100, 0), at(300, 90, 200)), -1);
});

test("swipeDir: 位移不足 64px 不算", () => {
  assert.equal(swipeDir(at(100, 100, 0), at(160, 100, 100)), 0);
});

test("swipeDir: 超过 500ms 的慢速拖动按选区处理", () => {
  assert.equal(swipeDir(at(300, 100, 0), at(100, 100, 900)), 0);
});

test("swipeDir: 斜向手势按滚动处理", () => {
  assert.equal(swipeDir(at(300, 100, 0), at(200, 180, 200)), 0);
});

const sessions = [
  { id: "a1", projectId: "p1" },
  { id: "b1", projectId: "p2" },
  { id: "a2", projectId: "p1" },
  { id: "a3", projectId: "p1" },
];

test("siblingSession: 只在同项目内取相邻，跳过别的项目", () => {
  assert.equal(siblingSession(sessions, "a1", 1)?.id, "a2");
  assert.equal(siblingSession(sessions, "a2", 1)?.id, "a3");
  assert.equal(siblingSession(sessions, "a2", -1)?.id, "a1");
});

test("siblingSession: 到头不回绕", () => {
  assert.equal(siblingSession(sessions, "a3", 1), null);
  assert.equal(siblingSession(sessions, "a1", -1), null);
  assert.equal(siblingSession(sessions, "b1", 1), null);
});

test("siblingSession: 当前会话不在列表里返回 null", () => {
  assert.equal(siblingSession(sessions, "gone", 1), null);
});
