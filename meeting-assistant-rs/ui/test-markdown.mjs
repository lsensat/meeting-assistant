/**
 * Behaviour tests for `js/markdown.js`, the DOM builder.
 *
 * # Why this exists separately from `check.mjs`
 *
 * `check.mjs` proves every module *compiles*. That is not enough for this one
 * file: it is half of the feature's security design, and its failure mode is
 * exactly the threat — a document producing an element it should not be able to.
 * A syntax check cannot see that.
 *
 * # Why a hand-rolled DOM and not jsdom
 *
 * The frontend has no bundler and no `package.json`, deliberately, and adding a
 * dependency to test one 130-line file would cost more than the file. The stub
 * below implements only what `markdown.js` touches, and — importantly — it
 * records `createElement` calls, which is the thing under test.
 *
 * Run: node ui/test-markdown.mjs
 */

// ---------------------------------------------------------------- DOM stub

/** Every element name the code under test asked for. */
let created = [];

class El {
  constructor(name) {
    this.name = name;
    this.children = [];
    this.attrs = {};
    this.listeners = {};
    this.classes = new Set();
  }
  get classList() {
    const s = this.classes;
    return { add: (c) => s.add(c), remove: (c) => s.delete(c), contains: (c) => s.has(c) };
  }
  append(...nodes) {
    this.children.push(...nodes);
  }
  replaceChildren(...nodes) {
    this.children = nodes;
  }
  addEventListener(type, fn) {
    (this.listeners[type] ??= []).push(fn);
  }
  /** Text as a reader would see it — proves content arrived as text. */
  get text() {
    return this.children.map((c) => (typeof c === "string" ? c : c.text)).join("");
  }
  /** Every element name in this subtree. */
  get names() {
    return this.children.flatMap((c) => (typeof c === "string" ? [] : [c.name, ...c.names]));
  }
  find(name) {
    for (const c of this.children) {
      if (typeof c === "string") continue;
      if (c.name === name) return c;
      const deeper = c.find(name);
      if (deeper) return deeper;
    }
    return null;
  }
}

globalThis.document = {
  createElement(name) {
    created.push(name);
    return new El(name);
  },
  createTextNode(v) {
    return String(v);
  },
};

const { render } = await import("./js/markdown.js");

// ------------------------------------------------------------------ harness

let failures = 0;
function test(name, fn) {
  created = [];
  try {
    fn();
    console.log(`  ok       ${name}`);
  } catch (error) {
    failures += 1;
    console.error(`  FAILED   ${name}`);
    console.error(`           ${error.message}`);
  }
}
function assert(cond, message) {
  if (!cond) throw new Error(message);
}

const start = (tag, extra = {}) => ({ t: "start", tag, ...extra });
const end = () => ({ t: "end" });
const text = (v) => ({ t: "text", v });

/** Render into a fresh root and return it. */
function draw(tokens, onLink = () => {}) {
  const root = new El("root");
  render(root, tokens, onLink);
  return root;
}

// -------------------------------------------------------------------- tests

test("headings, lists and emphasis render as their elements", () => {
  const root = draw([
    start("heading", { level: 1 }),
    text("Title"),
    end(),
    start("list", { ordered: false }),
    start("item"),
    text("one"),
    end(),
    end(),
  ]);
  assert(root.names.includes("h1"), `expected h1, got ${root.names}`);
  assert(root.names.includes("ul"), `expected ul, got ${root.names}`);
  assert(root.names.includes("li"), `expected li, got ${root.names}`);
  assert(root.text.includes("Title"), "heading text lost");
});

test("an ordered list is an ol", () => {
  const root = draw([start("list", { ordered: true }), start("item"), text("x"), end(), end()]);
  assert(root.names.includes("ol"), `expected ol, got ${root.names}`);
});

// --- the security properties ------------------------------------------------

test("a tag outside the allowlist creates no element", () => {
  for (const tag of ["div", "span", "input", "button", "form", "script", "iframe", "img", "style"]) {
    created = [];
    draw([start(tag), text("x"), end()]);
    assert(
      created.length === 0,
      `${tag} produced ${created} — a document can build it`
    );
  }
});

test("prototype keys as a tag create nothing", () => {
  // `TAGS[token.tag]` without an own-property check would return a function for
  // each of these, and `createElement(function)` is not a thing we want to find
  // out about in production.
  for (const tag of ["__proto__", "constructor", "toString", "hasOwnProperty", "valueOf"]) {
    created = [];
    draw([start(tag), text("x"), end()]);
    assert(created.length === 0, `${tag} produced ${created}`);
  }
});

test("document text never becomes markup", () => {
  const root = draw([start("paragraph"), text("<script>steal()</script>"), end()]);
  // It arrives as a string child — i.e. a text node — not as elements.
  assert(created.join() === "p", `expected only a paragraph, got ${created}`);
  assert(
    root.text.includes("<script>steal()</script>"),
    "the markup should be present as literal text"
  );
});

test("a heading level cannot name an arbitrary element", () => {
  for (const [level, expected] of [
    [1, "h1"],
    [6, "h6"],
    [99, "h6"],
    [0, "h2"],
    [-4, "h1"],
    ["3", "h3"],
    ["1;alert(1)", "h2"],
    [null, "h2"],
    [undefined, "h2"],
    [{}, "h2"],
  ]) {
    created = [];
    draw([start("heading", { level }), text("x"), end()]);
    assert(
      created.join() === expected,
      `level ${JSON.stringify(level)} produced ${created}, expected ${expected}`
    );
  }
});

test("surplus end tokens cannot escape the root", () => {
  const root = draw([
    start("paragraph"),
    text("inside"),
    end(),
    end(),
    end(),
    end(),
    // If the stack had escaped, this would be appended somewhere else — or
    // throw. It must land in the root.
    start("paragraph"),
    text("after"),
    end(),
  ]);
  assert(root.text.includes("after"), "content after surplus ends was lost");
  assert(root.children.length === 2, `expected 2 children of root, got ${root.children.length}`);
});

test("a dropped tag keeps the tree balanced", () => {
  // `image` is not in the map; its end must not close the paragraph.
  const root = draw([
    start("paragraph"),
    text("before "),
    start("image"),
    text("alt"),
    end(),
    text(" after"),
    end(),
  ]);
  const p = root.find("p");
  assert(p !== null, "no paragraph");
  assert(
    p.text === "before alt after",
    `text landed outside the paragraph: ${JSON.stringify(p.text)}`
  );
  assert(root.children.length === 1, "the paragraph was closed early");
});

// --- links ------------------------------------------------------------------

test("a link with an href never navigates, and calls back instead", () => {
  const opened = [];
  const root = draw(
    [start("link", { href: "https://example.com/x" }), text("click"), end()],
    (url) => opened.push(url)
  );

  const a = root.find("a");
  assert(a !== null, "no anchor");
  assert(a.href === "https://example.com/x", "href not set");

  let defaultPrevented = false;
  a.listeners.click[0]({ preventDefault: () => (defaultPrevented = true) });

  assert(defaultPrevented, "the click was allowed to navigate the window");
  assert(opened.join() === "https://example.com/x", `handler got ${opened}`);
});

test("a link Rust refused is marked inert and has no href", () => {
  // Rust strips the destination for javascript:, file:, data: and relative
  // links, so the token arrives without one.
  const root = draw([start("link"), text("click me"), end()]);
  const a = root.find("a");
  assert(a !== null, "no anchor");
  assert(a.href === undefined, `href was set to ${a.href}`);
  assert(a.classes.has("inert"), "not marked inert");
  assert(!a.listeners.click, "an inert link should have no click handler");
  assert(root.text.includes("click me"), "the words are part of the document");
});

// --- robustness -------------------------------------------------------------

test("an unknown token type is ignored", () => {
  const root = draw([start("paragraph"), { t: "future-thing", v: "x" }, text("kept"), end()]);
  assert(root.text.includes("kept"), "a later token was lost");
});

test("a text token with no value does not throw", () => {
  const root = draw([start("paragraph"), { t: "text" }, end()]);
  assert(root.find("p") !== null, "paragraph missing");
});

test("rendering twice replaces rather than appends", () => {
  const root = new El("root");
  render(root, [start("paragraph"), text("first"), end()], () => {});
  render(root, [start("paragraph"), text("second"), end()], () => {});
  assert(!root.text.includes("first"), "the previous document was left behind");
  assert(root.text.includes("second"), "the new document is missing");
});

// ------------------------------------------------------------------- report

if (failures > 0) {
  console.error(`\n${failures} test(s) failed.`);
  process.exit(1);
}
console.log("\nmarkdown.js behaves.");
