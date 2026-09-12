import assert from "node:assert/strict";
import test from "node:test";

import { ISLAND, closeSequence, expandedWidthFor, openSequence } from "../src/lib/island-motion.ts";

test("island dimensions support a branded compact pill", () => {
  assert.equal(ISLAND.compactWidth, 224);
  assert.equal(ISLAND.compactHeight, 52);
  assert.equal(ISLAND.expandedWidth, 354);
  assert.equal(ISLAND.top, 18);
  assert.equal(ISLAND.scrollThreshold, 12);
  assert.equal(ISLAND.revealDuration, 0.22);
});

test("expanded width never overflows the viewport gutters", () => {
  assert.equal(expandedWidthFor(1200), 354);
  assert.equal(expandedWidthFor(390), 350);
  assert.equal(expandedWidthFor(320), 280);
});

test("opening animates width first, then height", () => {
  const steps = openSequence(354, 346);

  assert.equal(steps.length, 2);
  assert.deepEqual(steps[0], { keyframes: { width: "354px" }, duration: 0.18 });
  assert.deepEqual(steps[1], { keyframes: { height: "346px" }, duration: 0.24 });
});

test("closing reverses the order: height first, then width", () => {
  const steps = closeSequence();

  assert.equal(steps.length, 2);
  assert.deepEqual(steps[0], { keyframes: { height: "52px" }, duration: 0.22 });
  assert.deepEqual(steps[1], { keyframes: { width: "224px" }, duration: 0.18 });
});
