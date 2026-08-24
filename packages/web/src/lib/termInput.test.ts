import { test } from "node:test";
import assert from "node:assert/strict";
import {
  ctrlByte,
  deleteSeq,
  extraKeySeq,
  isCtrlArmed,
  mouseReportCoord,
  repairMouseReport,
  setCtrlArmed,
  takeStickyCtrl,
} from "./termInput.js";

test("extraKeySeq: 普通模式发 CSI，应用模式发 SS3", () => {
  assert.equal(extraKeySeq("up", false), "\x1b[A");
  assert.equal(extraKeySeq("down", false), "\x1b[B");
  assert.equal(extraKeySeq("right", false), "\x1b[C");
  assert.equal(extraKeySeq("left", false), "\x1b[D");
  assert.equal(extraKeySeq("up", true), "\x1bOA");
  assert.equal(extraKeySeq("left", true), "\x1bOD");
});

test("extraKeySeq: Esc / Tab 不受光标键模式影响", () => {
  assert.equal(extraKeySeq("esc", false), "\x1b");
  assert.equal(extraKeySeq("esc", true), "\x1b");
  assert.equal(extraKeySeq("tab", false), "\t");
  assert.equal(extraKeySeq("tab", true), "\t");
});

test("extraKeySeq: Ctrl+方向键走 CSI 1;5 修饰形态，DECCKM 不参与", () => {
  assert.equal(extraKeySeq("right", false, true), "\x1b[1;5C");
  assert.equal(extraKeySeq("right", true, true), "\x1b[1;5C");
  assert.equal(extraKeySeq("left", true, true), "\x1b[1;5D");
  // Ctrl 对 Esc / Tab 没有意义，回落到本体
  assert.equal(extraKeySeq("esc", false, true), "\x1b");
  assert.equal(extraKeySeq("tab", false, true), "\t");
});

test("ctrlByte: 字母大小写同义，特例 Space 与 ?", () => {
  assert.equal(ctrlByte("c"), "\x03");
  assert.equal(ctrlByte("C"), "\x03");
  assert.equal(ctrlByte("a"), "\x01");
  assert.equal(ctrlByte("z"), "\x1a");
  assert.equal(ctrlByte("["), "\x1b");
  assert.equal(ctrlByte("_"), "\x1f");
  assert.equal(ctrlByte(" "), "\x00");
  assert.equal(ctrlByte("?"), "\x7f");
});

test("ctrlByte: 转不了的输入返回 null", () => {
  assert.equal(ctrlByte("1"), null);
  assert.equal(ctrlByte("中"), null);
  assert.equal(ctrlByte(""), null);
  assert.equal(ctrlByte("ab"), null);
});

test("takeStickyCtrl: 未点亮时原样透传", () => {
  setCtrlArmed(false);
  assert.equal(takeStickyCtrl("c"), "c");
  assert.equal(isCtrlArmed(), false);
});

test("takeStickyCtrl: 点亮后下一个单字符被转换并熄灭", () => {
  setCtrlArmed(true);
  assert.equal(takeStickyCtrl("c"), "\x03");
  assert.equal(isCtrlArmed(), false);
  // 熄灭后再输入不再转换
  assert.equal(takeStickyCtrl("c"), "c");
});

test("takeStickyCtrl: 转不成控制字节的单字符也消耗粘滞态", () => {
  setCtrlArmed(true);
  assert.equal(takeStickyCtrl("1"), "1");
  assert.equal(isCtrlArmed(), false);
});

test("takeStickyCtrl: 多字符输入（粘贴 / IME / 序列）不消耗粘滞态", () => {
  setCtrlArmed(true);
  assert.equal(takeStickyCtrl("\x1b[A"), "\x1b[A");
  assert.equal(takeStickyCtrl("你好"), "你好");
  assert.equal(isCtrlArmed(), true);
  setCtrlArmed(false);
});

test("deleteSeq: 向后删除折算退格，向前删除折算 Delete 键", () => {
  assert.equal(deleteSeq("deleteContentBackward"), "\x7f");
  assert.equal(deleteSeq("deleteWordBackward"), "\x7f");
  assert.equal(deleteSeq("deleteSoftLineBackward"), "\x7f");
  assert.equal(deleteSeq("deleteContentForward"), "\x1b[3~");
  assert.equal(deleteSeq("deleteWordForward"), "\x1b[3~");
});

test("deleteSeq: 组合输入的内部删除与剪切拖拽不拦", () => {
  assert.equal(deleteSeq("deleteCompositionText"), null);
  assert.equal(deleteSeq("deleteByComposition"), null);
  assert.equal(deleteSeq("deleteByCut"), null);
  assert.equal(deleteSeq("deleteByDrag"), null);
});

test("deleteSeq: 非删除类输入不拦", () => {
  assert.equal(deleteSeq("insertText"), null);
  assert.equal(deleteSeq("insertCompositionText"), null);
  assert.equal(deleteSeq(""), null);
});

test("mouseReportCoord: 从合法 SGR 报文提取坐标", () => {
  assert.equal(mouseReportCoord("\x1b[<65;25;37M"), "25;37");
  assert.equal(mouseReportCoord("\x1b[<0;1;1m"), "1;1");
});

test("mouseReportCoord: 非报文或坐标损坏返回 null", () => {
  assert.equal(mouseReportCoord("\x1b[<65;NaN;NaNM"), null);
  assert.equal(mouseReportCoord("hello"), null);
  assert.equal(mouseReportCoord("\x1b[A"), null);
});

test("repairMouseReport: 正常输入原样放行", () => {
  assert.equal(repairMouseReport("ls -la", "25;37"), "ls -la");
  assert.equal(repairMouseReport("\x1b[<65;25;37M", "1;1"), "\x1b[<65;25;37M");
  assert.equal(repairMouseReport("\x1b[A", null), "\x1b[A");
});

test("repairMouseReport: NaN 坐标用最近有效坐标修复，保留按键号与按放类型", () => {
  assert.equal(repairMouseReport("\x1b[<65;NaN;NaNM", "25;37"), "\x1b[<65;25;37M");
  assert.equal(repairMouseReport("\x1b[<64;NaN;NaNM", "3;9"), "\x1b[<64;3;9M");
  assert.equal(repairMouseReport("\x1b[<0;NaN;NaNm", "5;5"), "\x1b[<0;5;5m");
});

test("repairMouseReport: 无坐标可用或按键号损坏时丢弃", () => {
  assert.equal(repairMouseReport("\x1b[<65;NaN;NaNM", null), null);
  assert.equal(repairMouseReport("\x1b[<NaN;NaN;NaNM", "25;37"), null);
});
