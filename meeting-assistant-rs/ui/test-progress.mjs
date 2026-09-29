/**
 * The progress smoother: it may predict one window ahead, never more, and a
 * jump that is not a window must not become the prediction.
 *
 * Run with: node ui/test-progress.mjs
 */

import assert from "node:assert/strict";

let now = 0;
Object.defineProperty(globalThis.performance, "now", { value: () => now, configurable: true });

const { Smoother } = await import("./js/progress.js");

let passed = 0;
function test(name, body) {
  now = 0;
  body();
  passed += 1;
}

test("a silent track's jump is not extrapolated", () => {
  // The microphone track had no speech and finished in seconds: 5% -> 42%.
  // It used to be read as a window's worth of progress, so the card drew
  // 42 + 37 = 79% and held it while the real figure climbed from 42.
  const s = new Smoother();
  s.observe(5);
  now = 3_000;
  s.observe(42);
  // The card redraws continuously, so the estimate is read between reports.
  for (now = 3_000; now <= 60_000; now += 1_000) s.value();
  s.observe(43);
  now = 120_000;
  assert.ok(s.value() <= 44, `showed ${s.value()}%`);
});

test("windows are still predicted one ahead", () => {
  const s = new Smoother();
  s.observe(10);
  now = 30_000;
  s.observe(12);
  for (now = 30_000; now <= 45_000; now += 1_000) s.value();
  const v = s.value();
  assert.ok(v >= 12 && v <= 14, `showed ${v}%`);
});

test("it never goes backwards and never reaches 100", () => {
  const s = new Smoother();
  s.observe(97);
  now = 1_000;
  s.observe(98);
  now = 10_000_000;
  assert.ok(s.value() <= 99);
  const before = s.value();
  s.observe(98);
  assert.ok(s.value() >= before);
});

test("a checkpoint jump is shown at once", () => {
  const s = new Smoother();
  s.observe(5);
  now = 1_000;
  s.observe(80);
  assert.equal(s.value(), 80);
});

console.log(`progress: ${passed} tests pass.`);
