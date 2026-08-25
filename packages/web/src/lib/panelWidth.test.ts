import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  PANEL_WIDTH_DEFAULT,
  PANEL_WIDTH_MAX,
  PANEL_WIDTH_MIN,
  clampPanelWidth,
  parsePanelWidth,
  resizePanelWidth,
} from "./panelWidth.js";

describe("clampPanelWidth", () => {
  it("passes through a value in range, rounded", () => {
    assert.equal(clampPanelWidth(300), 300);
    assert.equal(clampPanelWidth(300.6), 301);
  });

  it("clamps to min / max", () => {
    assert.equal(clampPanelWidth(PANEL_WIDTH_MIN - 40), PANEL_WIDTH_MIN);
    assert.equal(clampPanelWidth(PANEL_WIDTH_MAX + 80), PANEL_WIDTH_MAX);
  });

  it("falls back to default on NaN / Infinity", () => {
    assert.equal(clampPanelWidth(Number.NaN), PANEL_WIDTH_DEFAULT);
    assert.equal(clampPanelWidth(Number.POSITIVE_INFINITY), PANEL_WIDTH_DEFAULT);
  });
});

describe("parsePanelWidth", () => {
  it("accepts a finite number", () => {
    assert.equal(parsePanelWidth(320), 320);
  });

  it("rejects missing / wrong types", () => {
    assert.equal(parsePanelWidth(undefined), PANEL_WIDTH_DEFAULT);
    assert.equal(parsePanelWidth("260"), PANEL_WIDTH_DEFAULT);
    assert.equal(parsePanelWidth(null), PANEL_WIDTH_DEFAULT);
  });
});

describe("resizePanelWidth", () => {
  it("right-edge handle grows when dragged right", () => {
    assert.equal(
      resizePanelWidth({ startWidth: 260, startX: 260, clientX: 300, edge: "right" }),
      300
    );
  });

  it("left-edge handle grows when dragged left", () => {
    assert.equal(
      resizePanelWidth({ startWidth: 260, startX: 800, clientX: 740, edge: "left" }),
      320
    );
  });

  it("clamps at the ends", () => {
    assert.equal(
      resizePanelWidth({ startWidth: 260, startX: 0, clientX: -400, edge: "right" }),
      PANEL_WIDTH_MIN
    );
    assert.equal(
      resizePanelWidth({ startWidth: 260, startX: 800, clientX: 0, edge: "left" }),
      PANEL_WIDTH_MAX
    );
  });
});
