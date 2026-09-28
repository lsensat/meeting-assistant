/**
 * Behaviour tests for the editor's DOM-free parts: `format.js` (what each
 * toolbar button does to the text), `History` (undo/redo) and `Autosave`.
 *
 * Plain Node, no dependencies, for the same reason as `test-markdown.mjs`.
 *
 * Selections are written into the text with `[` and `]` (or a single `|` for a
 * caret), so each case reads as before → after.
 *
 * Run: node ui/test-editor.mjs
 */

import assert from "node:assert/strict";
import * as format from "./js/format.js";
import { History } from "./js/editor.js";
import { Autosave } from "./js/autosave.js";

let passed = 0;
async function test(name, fn) {
  try {
    await fn();
    passed += 1;
  } catch (error) {
    console.error(`FAIL  ${name}`);
    throw error;
  }
}

/** `"a [b] c"` → `{text: "a b c", start: 2, end: 3}`. `|` is a caret. */
function parse(marked) {
  const caret = marked.indexOf("|");
  if (caret !== -1) {
    return { text: marked.replace("|", ""), start: caret, end: caret };
  }
  const start = marked.indexOf("[");
  const end = marked.indexOf("]") - 1;
  return { text: marked.replace("[", "").replace("]", ""), start, end };
}

/** The inverse of `parse`. */
function show({ text, start, end }) {
  if (start === end) return text.slice(0, start) + "|" + text.slice(start);
  return text.slice(0, start) + "[" + text.slice(start, end) + "]" + text.slice(end);
}

function check(fn, before, after) {
  assert.equal(show(fn(parse(before))), after, `from ${JSON.stringify(before)}`);
}

// ----------------------------------------------------------------- inline

await test("bold wraps and unwraps", () => {
  const bold = (e) => format.toggleWrap(e, "**");
  check(bold, "a [word] b", "a **[word]** b");
  check(bold, "a **[word]** b", "a [word] b");
  check(bold, "a [**word**] b", "a [word] b");
  check(bold, "a | b", "a **|** b");
});

await test("trailing whitespace stays outside the markers", () => {
  // A double-click on Windows selects the word and its space.
  check((e) => format.toggleWrap(e, "**"), "a [word ]b", "a **[word]** b");
});

await test("inline code on one line, a fence across lines", () => {
  check(format.toggleCode, "run [npm test] now", "run `[npm test]` now");
  check(format.toggleCode, "x\n[a\nb]", "x\n[```\na\nb\n```]");
  check(format.toggleCode, "[```\na\nb\n```]", "[a\nb]");
});

// Links are checked by hand: the result contains brackets, which `show` uses to
// mark the selection.
await test("a link around a selection selects the URL", () => {
  const out = format.insertLink(parse("see [docs] here"));
  assert.equal(out.text, "see [docs](https://) here");
  assert.equal(out.text.slice(out.start, out.end), "https://");
});

await test("a link with nothing selected selects the placeholder text", () => {
  const out = format.insertLink(parse("see | here"), "text");
  assert.equal(out.text, "see [text](https://) here");
  assert.equal(out.text.slice(out.start, out.end), "text");
});

// ----------------------------------------------------------------- blocks

await test("bullets toggle on every touched line", () => {
  check(format.toggleBullets, "[one\ntwo]", "[- one\n- two]");
  check(format.toggleBullets, "[- one\n- two]", "[one\ntwo]");
  check(format.toggleBullets, "on|e", "- one|");
});

await test("a selection ending at a line start does not take that line", () => {
  check(format.toggleBullets, "[one\n]two", "[- one]\ntwo");
});

await test("numbers replace bullets and count from one", () => {
  check(format.toggleNumbers, "[- a\n- b\n\n- c]", "[1. a\n2. b\n\n3. c]");
  check(format.toggleNumbers, "[1. a\n2. b]", "[a\nb]");
});

await test("quote toggles", () => {
  check(format.toggleQuote, "[a\nb]", "[> a\n> b]");
  check(format.toggleQuote, "[> a\n> b]", "[a\nb]");
});

await test("heading cycles # → ## → ### → text", () => {
  check(format.cycleHeading, "Tit|le", "# Title|");
  check(format.cycleHeading, "# Tit|le", "## Title|");
  check(format.cycleHeading, "## Tit|le", "### Title|");
  check(format.cycleHeading, "### Tit|le", "Title|");
});

// -------------------------------------------------------------- keyboard

await test("Enter continues a list, and on an empty item leaves it", () => {
  check(format.continueBlock, "- one|", "- one\n- |");
  check(format.continueBlock, "  * one|", "  * one\n  * |");
  check(format.continueBlock, "3. one|", "3. one\n4. |");
  check(format.continueBlock, "> said|", "> said\n> |");
  check(format.continueBlock, "- one\n- |", "- one\n|");
  assert.equal(format.continueBlock(parse("plain|")), null, "not a list: plain newline");
});

await test("Tab indents and Shift+Tab outdents", () => {
  check((e) => format.indent(e), "- on|e", "  - on|e");
  check((e) => format.indent(e, true), "  - on|e", "- on|e");
  check((e) => format.indent(e), "[a\nb]", "[  a\n  b]");
});

// ---------------------------------------------------------------- history

await test("undo and redo walk the snapshots", () => {
  let t = 0;
  const h = new History(() => t);
  const s = (text) => ({ text, start: text.length, end: text.length });

  h.record(s(""));           // → "a"   (a button: always its own step)
  t = 5000;
  h.record(s("a"));          // → "ab"
  assert.equal(h.undo(s("ab")).text, "a");
  assert.equal(h.undo(s("a")).text, "");
  assert.equal(h.undo(s("")), null, "nothing left");
  assert.equal(h.redo(s("")).text, "a");
  assert.equal(h.redo(s("a")).text, "ab");
  assert.equal(h.redo(s("ab")), null);
});

await test("a run of typing is one undo step, a pause ends it", () => {
  let t = 0;
  const h = new History(() => t);
  const s = (text) => ({ text, start: text.length, end: text.length });

  h.record(s(""), "insert");   t = 100;
  h.record(s("h"), "insert");  t = 200;
  h.record(s("he"), "insert"); t = 5000;   // pause
  h.record(s("hel"), "insert");

  assert.equal(h.undo(s("hell")).text, "hel", "back to the pause");
  assert.equal(h.undo(s("hel")).text, "", "the whole first run at once");
});

await test("a new change clears redo", () => {
  const h = new History(() => 0);
  const s = (text) => ({ text, start: 0, end: 0 });
  h.record(s("a"));
  h.undo(s("b"));
  assert.ok(h.canRedo);
  h.record(s("a"));
  assert.ok(!h.canRedo);
});

// --------------------------------------------------------------- autosave

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** A fake backend with a version counter and an optional delay. */
function backend({ latency = 0 } = {}) {
  const disk = { text: "start", version: "v0", n: 0 };
  const calls = [];
  const save = async (text, base) => {
    calls.push({ text, base });
    await sleep(latency);
    if (base !== null && base !== disk.version) return { t: "conflict" };
    disk.n += 1;
    disk.text = text;
    disk.version = `v${disk.n}`;
    return { t: "saved", version: disk.version };
  };
  return { disk, calls, save };
}

await test("typing is saved once, after the pause", async () => {
  const b = backend();
  const states = [];
  const a = new Autosave({ save: b.save, onState: (s) => states.push(s), delay: 20 });
  a.reset("v0");
  a.changed("s");
  a.changed("st");
  a.changed("sta");
  await sleep(60);
  assert.equal(b.calls.length, 1);
  assert.equal(b.disk.text, "sta");
  assert.equal(states.at(-1), "saved");
  assert.ok(!a.unsaved);
});

await test("changes made during a save are saved after it, on its version", async () => {
  const b = backend({ latency: 30 });
  const a = new Autosave({ save: b.save, onState: () => {}, delay: 5 });
  a.reset("v0");
  a.changed("one");
  await sleep(15);           // first save is in flight
  a.changed("two");
  await a.flush();
  assert.equal(b.disk.text, "two");
  assert.deepEqual(
    b.calls.map((c) => c.base),
    ["v0", "v1"],
    "the second save quotes the first one's result, so it is not a conflict",
  );
});

await test("a conflict stops autosave until the user chooses", async () => {
  const b = backend();
  const states = [];
  const a = new Autosave({ save: b.save, onState: (s) => states.push(s), delay: 5 });
  a.reset("v0");
  b.disk.version = "elsewhere";   // someone else wrote the file
  a.changed("mine");
  await a.flush();
  assert.equal(states.at(-1), "conflict");
  assert.equal(b.disk.text, "start", "theirs is untouched");

  a.changed("mine, more");
  await a.flush();
  assert.equal(b.calls.length, 1, "no further saves while blocked");

  await a.overwrite();
  assert.equal(b.disk.text, "mine, more");
  assert.equal(states.at(-1), "saved");
});

await test("a failed save keeps the text dirty for the next attempt", async () => {
  let fail = true;
  const b = backend();
  const save = async (text, base) => {
    if (fail) throw new Error("disk full");
    return b.save(text, base);
  };
  const states = [];
  const a = new Autosave({ save, onState: (s) => states.push(s), delay: 5 });
  a.reset("v0");
  a.changed("x");
  await a.flush();
  assert.equal(states.at(-1), "error");
  assert.ok(a.unsaved);

  fail = false;
  await a.flush();
  assert.equal(b.disk.text, "x");
  assert.ok(!a.unsaved);
});

console.log(`editor: ${passed} tests pass.`);
