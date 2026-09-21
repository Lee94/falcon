import { describe, it } from "node:test";
import assert from "node:assert/strict";
import {
  WHEEL_GESTURE_GAP_MS,
  WheelAxisLock,
  innerTakesWheel,
  overflowCanConsume,
  overflowScrollable,
  revealScrollLeft,
  settleTarget,
  wheelDeltaPx,
  type OverflowBox,
} from "./termCanvas.js";

describe("WheelAxisLock", () => {
  it("手势第一条事件定轴，之后斜着的事件不改判", () => {
    const lock = new WheelAxisLock();
    assert.equal(lock.classify({ deltaX: 12, deltaY: 3, timeStamp: 0 }), "x");
    // 惯性尾巴里 |dy| 略大于 |dx|，仍归横向
    assert.equal(lock.classify({ deltaX: 2, deltaY: 3, timeStamp: 16 }), "x");
    assert.equal(lock.classify({ deltaX: 0, deltaY: 1, timeStamp: 32 }), "x");
  });

  it("相等算纵向", () => {
    const lock = new WheelAxisLock();
    assert.equal(lock.classify({ deltaX: 4, deltaY: 4, timeStamp: 0 }), "y");
  });

  it("空事件不定轴也不续手势", () => {
    const lock = new WheelAxisLock();
    assert.equal(lock.classify({ deltaX: 0, deltaY: 0, timeStamp: 0 }), null);
    assert.equal(lock.classify({ deltaX: 0, deltaY: 9, timeStamp: 10 }), "y");
    assert.equal(lock.classify({ deltaX: 0, deltaY: 0, timeStamp: 20 }), null);
    // 空事件没有刷新时间戳：从上一条有位移的事件起算，超过间隔就是新手势
    assert.equal(
      lock.classify({ deltaX: 9, deltaY: 0, timeStamp: 10 + WHEEL_GESTURE_GAP_MS + 1 }),
      "x"
    );
  });

  it("静默超过间隔算新手势，重新定轴", () => {
    const lock = new WheelAxisLock();
    assert.equal(lock.classify({ deltaX: 12, deltaY: 0, timeStamp: 0 }), "x");
    // 正好在间隔内：还是同一手势，小幅纵向抖动不改判
    assert.equal(
      lock.classify({ deltaX: 0, deltaY: 5, timeStamp: WHEEL_GESTURE_GAP_MS }),
      "x"
    );
    assert.equal(
      lock.classify({ deltaX: 1, deltaY: 5, timeStamp: WHEEL_GESTURE_GAP_MS * 2 + 1 }),
      "y"
    );
  });

  it("另一根轴明显更大时改判（惯性中换方向）", () => {
    const lock = new WheelAxisLock();
    assert.equal(lock.classify({ deltaX: 30, deltaY: 0, timeStamp: 0 }), "x");
    // 小抖动不算：8px 以下、或没到主轴两倍
    assert.equal(lock.classify({ deltaX: 3, deltaY: 7, timeStamp: 16 }), "x");
    assert.equal(lock.classify({ deltaX: 6, deltaY: 10, timeStamp: 32 }), "x");
    assert.equal(lock.classify({ deltaX: 2, deltaY: 20, timeStamp: 48 }), "y");
    // 改判后保持
    assert.equal(lock.classify({ deltaX: 3, deltaY: 2, timeStamp: 64 }), "y");
  });

  it("reset 之后重新定轴", () => {
    const lock = new WheelAxisLock();
    assert.equal(lock.classify({ deltaX: 12, deltaY: 0, timeStamp: 0 }), "x");
    lock.reset();
    assert.equal(lock.classify({ deltaX: 0, deltaY: 5, timeStamp: 1 }), "y");
  });
});

describe("wheelDeltaPx", () => {
  it("像素原样，行 / 页按给定尺寸折算", () => {
    assert.equal(wheelDeltaPx(7, 0), 7);
    assert.equal(wheelDeltaPx(3, 1, 16), 48);
    assert.equal(wheelDeltaPx(-1, 2, 16, 640), -640);
  });
});

function box(over: Partial<OverflowBox> = {}): OverflowBox {
  return {
    overflowX: "hidden",
    overflowY: "hidden",
    scrollLeft: 0,
    scrollTop: 0,
    clientWidth: 100,
    clientHeight: 100,
    scrollWidth: 100,
    scrollHeight: 100,
    ...over,
  };
}

describe("overflowScrollable", () => {
  it("auto / scroll / overlay 可滚，其余不行", () => {
    assert.equal(overflowScrollable("auto"), true);
    assert.equal(overflowScrollable("scroll"), true);
    assert.equal(overflowScrollable("overlay"), true);
    assert.equal(overflowScrollable("hidden"), false);
    assert.equal(overflowScrollable("visible"), false);
    assert.equal(overflowScrollable("clip"), false);
  });
});

describe("overflowCanConsume", () => {
  it("overflow hidden / 内容没溢出都不接", () => {
    assert.equal(overflowCanConsume(box({ overflowY: "hidden", scrollHeight: 400 }), "y", 10), false);
    assert.equal(overflowCanConsume(box({ overflowY: "auto" }), "y", 10), false);
    assert.equal(overflowCanConsume(box({ overflowX: "auto", scrollWidth: 400 }), "y", 10), false);
  });

  it("delta 为 0 不接", () => {
    assert.equal(overflowCanConsume(box({ overflowY: "auto", scrollHeight: 400 }), "y", 0), false);
  });

  it("纵向：还能往下 / 往上才接，贴边不接", () => {
    const y = box({ overflowY: "auto", scrollHeight: 400 });
    assert.equal(overflowCanConsume(y, "y", 10), true);
    assert.equal(overflowCanConsume(y, "y", -10), false);
    assert.equal(overflowCanConsume({ ...y, scrollTop: 50 }, "y", -10), true);
    assert.equal(overflowCanConsume({ ...y, scrollTop: 300 }, "y", 10), false);
    assert.equal(overflowCanConsume({ ...y, scrollTop: 300 }, "y", -10), true);
  });

  it("横向同理", () => {
    const x = box({ overflowX: "scroll", scrollWidth: 400 });
    assert.equal(overflowCanConsume(x, "x", 10), true);
    assert.equal(overflowCanConsume(x, "x", -10), false);
    assert.equal(overflowCanConsume({ ...x, scrollLeft: 20 }, "x", -10), true);
    assert.equal(overflowCanConsume({ ...x, scrollLeft: 300 }, "x", 10), false);
  });

  it("亚像素贴边当不能滚", () => {
    assert.equal(
      overflowCanConsume(box({ overflowY: "auto", scrollHeight: 100.4, scrollTop: 0 }), "y", 10),
      false
    );
    assert.equal(
      overflowCanConsume(
        box({ overflowX: "auto", scrollWidth: 400, clientWidth: 100, scrollLeft: 0.4 }),
        "x",
        -10
      ),
      false
    );
  });
});

describe("innerTakesWheel", () => {
  it("从内到外任一还能滚就归内部", () => {
    const inner = box({ overflowY: "hidden" });
    const scroller = box({ overflowY: "auto", scrollHeight: 400 });
    assert.equal(innerTakesWheel([inner, scroller], "y", 10), true);
    assert.equal(innerTakesWheel([inner], "y", 10), false);
  });

  it("只认被问的那根轴", () => {
    const y = box({ overflowY: "auto", scrollHeight: 400 });
    assert.equal(innerTakesWheel([y], "x", 10), false);
    assert.equal(innerTakesWheel([y], "y", 10), true);
  });
});

describe("settleTarget", () => {
  const lefts = [0, 400, 800];

  it("离最近列边不超过 proximity 才吸附", () => {
    assert.equal(settleTarget(30, lefts, 80, 800), 0);
    assert.equal(settleTarget(370, lefts, 80, 800), 400);
    assert.equal(settleTarget(200, lefts, 80, 800), null);
  });

  it("已经对齐就不动", () => {
    assert.equal(settleTarget(400, lefts, 80, 800), null);
    assert.equal(settleTarget(400.4, lefts, 80, 800), null);
  });

  it("最后一列吸不到边时按能滚到的最远处算", () => {
    assert.equal(settleTarget(560, lefts, 80, 600), 600);
    assert.equal(settleTarget(770, lefts, 80, 600), null);
  });

  it("没有列时不动", () => {
    assert.equal(settleTarget(100, [], 80, 800), null);
  });
});

describe("revealScrollLeft", () => {
  it("整列可见不动", () => {
    assert.equal(revealScrollLeft({ scrollLeft: 0, viewport: 1000, left: 0, width: 500 }), null);
    assert.equal(revealScrollLeft({ scrollLeft: 0, viewport: 1000, left: 500, width: 500 }), null);
  });

  it("左边露不全对齐左边，右边露不全对齐右边", () => {
    assert.equal(revealScrollLeft({ scrollLeft: 300, viewport: 1000, left: 0, width: 500 }), 0);
    assert.equal(
      revealScrollLeft({ scrollLeft: 0, viewport: 1000, left: 1000, width: 500 }),
      500
    );
    assert.equal(
      revealScrollLeft({ scrollLeft: 0, viewport: 1000, left: 800, width: 500 }),
      300
    );
  });

  it("列比视口宽时对齐左边", () => {
    assert.equal(revealScrollLeft({ scrollLeft: 0, viewport: 500, left: 600, width: 640 }), 600);
    assert.equal(revealScrollLeft({ scrollLeft: 600, viewport: 500, left: 600, width: 640 }), null);
  });
});
