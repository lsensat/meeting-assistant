/**
 * The teleprompter's logic: clamps, key map, scroll timing, script blocks, and
 * that every translation key the window uses exists in both languages.
 *
 * Run with: node ui/test-teleprompter.mjs
 */

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const L = await import("./js/teleprompter-logic.js");

let passed = 0;
function test(name, body) {
  body();
  passed += 1;
}

test("constants mirror the Rust contract", () => {
  assert.deepEqual([L.FONT_MIN, L.FONT_MAX, L.FONT_DEFAULT, L.FONT_STEP], [16, 160, 44, 4]);
  assert.deepEqual([L.SPEED_MIN, L.SPEED_MAX, L.SPEED_DEFAULT], [1, 30, 5]);
  assert.equal(L.TEXT_OPACITY_MIN, 0.2);
  assert.equal(L.TEXT_OPACITY_DEFAULT, 1.0);
  assert.equal(L.OVERLAY_OPACITY_DEFAULT, 0.4);
});

test("clampFont", () => {
  assert.equal(L.clampFont(5), 16);
  assert.equal(L.clampFont(500), 160);
  assert.equal(L.clampFont(44), 44);
  assert.equal(L.clampFont(44.6), 45);
  assert.equal(L.clampFont(NaN), 44);
  assert.equal(L.clampFont(undefined), 44);
});

test("clampSpeed", () => {
  assert.equal(L.clampSpeed(0), 1);
  assert.equal(L.clampSpeed(99), 30);
  assert.equal(L.clampSpeed(7), 7);
  assert.equal(L.clampSpeed(NaN), 5);
});

test("clampOpacity", () => {
  assert.equal(L.clampOpacity(-1), 0);
  assert.equal(L.clampOpacity(2), 1);
  assert.equal(L.clampOpacity(0.123), 0.12);
  assert.equal(L.clampOpacity(0.05, L.TEXT_OPACITY_MIN), 0.2);
  assert.equal(L.clampOpacity(NaN, L.TEXT_OPACITY_MIN), 1);
  assert.equal(L.clampOpacity(NaN), 0.4);
});

test("stepFont and stepSpeed move one step and stop at the ends", () => {
  assert.equal(L.stepFont(44, 1), 48);
  assert.equal(L.stepFont(44, -1), 40);
  assert.equal(L.stepFont(160, 1), 160);
  assert.equal(L.stepFont(16, -1), 16);
  assert.equal(L.stepSpeed(5, 1), 6);
  assert.equal(L.stepSpeed(5, -1), 4);
  assert.equal(L.stepSpeed(30, 1), 30);
  assert.equal(L.stepSpeed(1, -1), 1);
});

test("pxPerSecond scales with level and font", () => {
  assert.equal(L.pxPerSecond(5, 44), 44);
  assert.equal(L.pxPerSecond(10, 44), 88);
  assert.equal(L.pxPerSecond(5, 88), 88);
});

test("keyAction outside the editor", () => {
  const k = (key) => L.keyAction(key, { editing: false });
  assert.equal(k(" "), "toggle");
  assert.equal(k("ArrowUp"), "faster");
  assert.equal(k("ArrowRight"), "faster");
  assert.equal(k("ArrowDown"), "slower");
  assert.equal(k("ArrowLeft"), "slower");
  assert.equal(k("+"), "bigger");
  assert.equal(k("="), "bigger");
  assert.equal(k("-"), "smaller");
  assert.equal(k("_"), "smaller");
  assert.equal(k("Escape"), "close");
  assert.equal(k("a"), null);
});

test("keyAction while editing: only Escape, and it closes the editor", () => {
  const k = (key) => L.keyAction(key, { editing: true });
  assert.equal(k("Escape"), "closeEditor");
  for (const key of [" ", "ArrowUp", "ArrowDown", "+", "-", "a"]) assert.equal(k(key), null);
});

test("Scroller: starts paused and does not move", () => {
  const s = new L.Scroller();
  assert.equal(s.playing, false);
  assert.equal(s.advance(1000, 100), 0);
});

test("Scroller: first frame after play only starts the clock", () => {
  const s = new L.Scroller();
  s.play();
  assert.equal(s.advance(5000, 100), 0);
  assert.equal(s.advance(5050, 100), 5);
});

test("Scroller: a long gap is clamped to 100 ms", () => {
  const s = new L.Scroller();
  s.play();
  s.advance(0, 100);
  assert.equal(s.advance(10_000, 100), 10);
});

test("Scroller: pause stops motion and resume does not jump", () => {
  const s = new L.Scroller();
  s.play();
  s.advance(0, 100);
  assert.equal(s.advance(50, 100), 5);
  s.pause();
  assert.equal(s.playing, false);
  assert.equal(s.advance(60, 100), 0);
  s.play();
  assert.equal(s.advance(60_000, 100), 0);
  assert.equal(s.advance(60_016, 100), 1.6);
});

test("Scroller: toggle flips and a backwards clock never goes negative", () => {
  const s = new L.Scroller();
  s.toggle();
  assert.equal(s.playing, true);
  s.advance(100, 100);
  assert.equal(s.advance(50, 100), 0);
  s.toggle();
  assert.equal(s.playing, false);
});

test("clampPosition", () => {
  assert.equal(L.clampPosition(-5, 1000, 300), 0);
  assert.equal(L.clampPosition(500, 1000, 300), 500);
  assert.equal(L.clampPosition(900, 1000, 300), 700);
  assert.equal(L.clampPosition(50, 100, 300), 0);
  assert.equal(L.clampPosition(NaN, 1000, 300), 0);
});

test("rescalePosition keeps the same passage in view", () => {
  assert.equal(L.rescalePosition(500, 1000, 2000), 1000);
  assert.equal(L.rescalePosition(500, 1000, 500), 250);
  assert.equal(L.rescalePosition(0, 1000, 2000), 0);
  assert.equal(L.rescalePosition(500, 0, 2000), 500);
  assert.equal(L.rescalePosition(NaN, 1000, 2000), 0);
});

test("scriptBlocks: paragraphs, headings, markup stripped", () => {
  const blocks = L.scriptBlocks(
    "# Title\n\nFirst **bold** line\nsecond `code` line\n\n\n## Part [link](http://x.y)\r\nBody",
  );
  assert.deepEqual(blocks, [
    { kind: "heading", text: "Title" },
    { kind: "para", text: "First bold line\nsecond code line" },
    { kind: "heading", text: "Part link" },
    { kind: "para", text: "Body" },
  ]);
});

test("scriptBlocks: blank input and empty headings give nothing", () => {
  assert.deepEqual(L.scriptBlocks(""), []);
  assert.deepEqual(L.scriptBlocks("  \n\n \n"), []);
  assert.deepEqual(L.scriptBlocks("#\n"), []);
  assert.deepEqual(L.scriptBlocks(undefined), []);
});

test("scriptBlocks: markup characters stay text, never HTML", () => {
  const [block] = L.scriptBlocks("<img src=x onerror=alert(1)>");
  assert.equal(block.text, "<img src=x onerror=alert(1)>");
});

// Every key the window uses must exist in both languages: `tr` falls back to
// the raw key, so a missing one shows up on screen as `teleprompter_foo`.
test("every translation key used exists in en and es", () => {
  const here = dirname(fileURLToPath(import.meta.url));
  const catalog = JSON.parse(readFileSync(join(here, "../crates/meeting-core/i18n.json"), "utf8"));
  const html = readFileSync(join(here, "teleprompter.html"), "utf8");
  const js = readFileSync(join(here, "js/teleprompter.js"), "utf8");

  const keys = new Set();
  for (const m of html.matchAll(/data-i18n(?:-title|-label)?="([^"]+)"/g)) keys.add(m[1]);
  for (const m of js.matchAll(/\btr\(\s*"([^"]+)"/g)) keys.add(m[1]);
  assert.ok(keys.size >= 15, `found only ${keys.size} keys; has the markup moved?`);

  const missing = [];
  for (const key of keys) {
    for (const lang of ["en", "es"]) {
      const table = catalog[lang] ?? catalog[Object.keys(catalog).find((n) => n.startsWith(lang))];
      if (!table || typeof table[key] !== "string" || table[key] === "") missing.push(`${lang}:${key}`);
    }
  }
  assert.deepEqual(missing, [], `missing translations: ${missing.join(", ")}`);
});

console.log(`teleprompter: ${passed} tests pass.`);
