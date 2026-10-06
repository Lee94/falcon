import { describe, it } from "node:test";
import assert from "node:assert/strict";
import {
  CANVAS_SWIPE_PX,
  CanvasSwipe,
  WHEEL_GESTURE_GAP_MS,
  WheelAxisLock,
  innerTakesWheel,
  overflowCanConsume,
  overflowScrollable,
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

describe("CanvasSwipe", () => {
  it("累计横移过了门槛才翻，一段手势至多翻一块", () => {
    const swipe = new CanvasSwipe();
    const step = CANVAS_SWIPE_PX / 4;
    assert.equal(swipe.push(step, 0), 0);
    assert.equal(swipe.push(step, 16), 0);
    assert.equal(swipe.push(step, 32), 0);
    assert.equal(swipe.push(step, 48), 1);
    // 惯性尾巴还在同一段手势里：不再翻
    for (let t = 64; t < 600; t += 16) assert.equal(swipe.push(step * 4, t), 0);
  });

  it("停顿之后是新手势，可以再翻；方向跟着累计的符号走", () => {
    const swipe = new CanvasSwipe();
    assert.equal(swipe.push(CANVAS_SWIPE_PX, 0), 1);
    assert.equal(swipe.push(-CANVAS_SWIPE_PX, WHEEL_GESTURE_GAP_MS + 1), -1);
  });

  it("来回抖动抵消掉，不翻", () => {
    const swipe = new CanvasSwipe();
    for (let i = 0; i < 20; i++) {
      assert.equal(swipe.push(i % 2 ? -30 : 30, i * 16), 0);
    }
  });

  it("这段手势被窗口内部接走过，整段都不翻", () => {
    const swipe = new CanvasSwipe();
    swipe.hold(0);
    assert.equal(swipe.push(CANVAS_SWIPE_PX * 3, 16), 0);
    assert.equal(swipe.push(CANVAS_SWIPE_PX * 3, 16 + WHEEL_GESTURE_GAP_MS + 1), 1);
  });
});
