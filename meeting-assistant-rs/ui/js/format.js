/**
 * Markdown formatting as pure edits on text.
 *
 * Every function takes an `Edit` — the text and the selection — and returns a
 * new one. Nothing here touches the DOM, so the whole module is testable in
 * Node (`ui/test-format.mjs`) and the editor is left with one job: applying an
 * `Edit` to a textarea and recording it for undo.
 *
 * The rule throughout is that formatting **toggles**. Pressing Bold on bold text
 * unbolds it, pressing Bullets on a list removes the bullets. A toolbar whose
 * buttons can only add is one where every mistake needs the keyboard to undo.
 *
 * @typedef {{ text: string, start: number, end: number }} Edit
 */

/** Start of the line containing `index`. */
function lineStart(text, index) {
  return text.lastIndexOf("\n", index - 1) + 1;
}

/** End of the line containing `index` (the position of its `\n`, or the end). */
function lineEnd(text, index) {
  const next = text.indexOf("\n", index);
  return next === -1 ? text.length : next;
}

/**
 * The whole lines a selection touches.
 *
 * A selection ending at the very start of a line — what a triple-click or a
 * shift+down produces — does not include that line. Otherwise selecting two
 * lines and pressing Bullets would bullet three.
 */
function selectedLines({ text, start, end }) {
  const effectiveEnd = end > start && text[end - 1] === "\n" ? end - 1 : end;
  const from = lineStart(text, start);
  const to = lineEnd(text, effectiveEnd);
  return { from, to, lines: text.slice(from, to).split("\n") };
}

/**
 * Wrap the selection in `marker` on both sides, or unwrap it if it already is.
 *
 * Whitespace at the edges of the selection stays outside the markers: a
 * double-click on Windows selects the word *and* its trailing space, and
 * `**word **` is not bold in CommonMark.
 *
 * @param {Edit} edit
 * @param {string} marker `**`, `*`, `~~` or `` ` ``
 * @returns {Edit}
 */
export function toggleWrap(edit, marker) {
  const { text } = edit;
  let { start, end } = edit;
  const m = marker.length;

  while (start < end && /\s/.test(text[start])) start++;
  while (end > start && /\s/.test(text[end - 1])) end--;

  // Markers just outside the selection: `**|word|**`.
  if (text.slice(start - m, start) === marker && text.slice(end, end + m) === marker) {
    return {
      text: text.slice(0, start - m) + text.slice(start, end) + text.slice(end + m),
      start: start - m,
      end: end - m,
    };
  }

  // Markers inside the selection: `|**word**|`.
  const inner = text.slice(start, end);
  if (inner.length >= 2 * m && inner.startsWith(marker) && inner.endsWith(marker)) {
    return {
      text: text.slice(0, start) + inner.slice(m, -m) + text.slice(end),
      start,
      end: end - 2 * m,
    };
  }

  // Nothing selected: an empty pair with the caret between, ready to type into.
  return {
    text: text.slice(0, start) + marker + inner + marker + text.slice(end),
    start: start + m,
    end: end + m,
  };
}

/** Line prefixes the block toggles recognise, so one kind can replace another. */
const BULLET = /^(\s*)[-*+] /;
const NUMBER = /^(\s*)\d+[.)] /;
const QUOTE = /^> ?/;
const HEADING = /^(#{1,6}) /;

/**
 * Apply `transform` to each touched line and select the result.
 *
 * @param {Edit} edit
 * @param {(lines: string[]) => string[]} transform
 * @returns {Edit}
 */
function mapLines(edit, transform) {
  const { from, to, lines } = selectedLines(edit);
  const replaced = transform(lines).join("\n");
  const text = edit.text.slice(0, from) + replaced + edit.text.slice(to);

  // A caret stays a caret, moved to the end of its line; a selection covers
  // every line it touched, so pressing the button again toggles them all back.
  if (edit.start === edit.end && lines.length === 1) {
    const caret = from + replaced.length;
    return { text, start: caret, end: caret };
  }
  return { text, start: from, end: from + replaced.length };
}

/** Drop any list or quote marker so a line can take a different one. */
function stripBlockPrefix(line) {
  return line.replace(BULLET, "$1").replace(NUMBER, "$1").replace(QUOTE, "");
}

/**
 * Bullet the touched lines, or unbullet them if every one already is.
 *
 * @param {Edit} edit
 * @returns {Edit}
 */
export function toggleBullets(edit) {
  return mapLines(edit, (lines) => {
    const content = lines.filter((l) => l.trim() !== "");
    const all = content.length > 0 && content.every((l) => BULLET.test(l));
    return lines.map((line) => {
      if (all) return line.replace(BULLET, "$1");
      if (line.trim() === "") return line;
      const indent = /^\s*/.exec(line)[0];
      return `${indent}- ${stripBlockPrefix(line).trimStart()}`;
    });
  });
}

/**
 * Number the touched lines 1, 2, 3…, or un-number them if every one already is.
 *
 * @param {Edit} edit
 * @returns {Edit}
 */
export function toggleNumbers(edit) {
  return mapLines(edit, (lines) => {
    const content = lines.filter((l) => l.trim() !== "");
    const all = content.length > 0 && content.every((l) => NUMBER.test(l));
    let n = 0;
    return lines.map((line) => {
      if (all) return line.replace(NUMBER, "$1");
      if (line.trim() === "") return line;
      n += 1;
      const indent = /^\s*/.exec(line)[0];
      return `${indent}${n}. ${stripBlockPrefix(line).trimStart()}`;
    });
  });
}

/**
 * Quote the touched lines, or unquote them if every one already is.
 *
 * @param {Edit} edit
 * @returns {Edit}
 */
export function toggleQuote(edit) {
  return mapLines(edit, (lines) => {
    const all = lines.every((l) => QUOTE.test(l) || l.trim() === "");
    return lines.map((line) => (all ? line.replace(QUOTE, "") : `> ${line.replace(QUOTE, "")}`));
  });
}

/**
 * Cycle the touched lines through heading levels: text → `#` → `##` → `###` →
 * text.
 *
 * Three levels, not six: a summary uses two or three, and a button that needs
 * six presses to get back where it started is not one anybody uses twice.
 * Deeper headings typed by hand are left alone until the cycle reaches them.
 *
 * @param {Edit} edit
 * @returns {Edit}
 */
export function cycleHeading(edit) {
  return mapLines(edit, (lines) =>
    lines.map((line) => {
      if (line.trim() === "") return line;
      const match = HEADING.exec(line);
      const level = match ? match[1].length : 0;
      const body = match ? line.slice(match[0].length) : stripBlockPrefix(line);
      return level >= 3 ? body : `${"#".repeat(level + 1)} ${body}`;
    }),
  );
}

/**
 * Code: inline backticks within a line, a fenced block across lines.
 *
 * @param {Edit} edit
 * @returns {Edit}
 */
export function toggleCode(edit) {
  const { text, start, end } = edit;
  const selected = text.slice(start, end);
  if (!selected.includes("\n")) return toggleWrap(edit, "`");

  // Already fenced exactly: unfence.
  const fenced = /^```[^\n]*\n([\s\S]*?)\n```$/.exec(selected);
  if (fenced) {
    return {
      text: text.slice(0, start) + fenced[1] + text.slice(end),
      start,
      end: start + fenced[1].length,
    };
  }

  const block = "```\n" + selected.replace(/\n$/, "") + "\n```";
  // A fence must start at the beginning of a line.
  const before = start > 0 && text[start - 1] !== "\n" ? "\n" : "";
  const replaced = before + block;
  return {
    text: text.slice(0, start) + replaced + text.slice(end),
    start: start + before.length,
    end: start + replaced.length,
  };
}

/**
 * Make the selection a link and select the URL placeholder, so typing or
 * pasting replaces it straight away.
 *
 * With nothing selected, the link text is the placeholder instead.
 *
 * @param {Edit} edit
 * @returns {Edit}
 */
export function insertLink(edit, placeholderText = "text") {
  const { text, start, end } = edit;
  const label = text.slice(start, end) || placeholderText;
  const url = "https://";
  const inserted = `[${label}](${url})`;
  const result = text.slice(0, start) + inserted + text.slice(end);

  if (start === end) {
    return { text: result, start: start + 1, end: start + 1 + label.length };
  }
  const urlStart = start + label.length + 3;
  return { text: result, start: urlStart, end: urlStart + url.length };
}

/**
 * Enter inside a list or quote: continue it on the next line.
 *
 * On an item with nothing after its marker, the marker is removed instead —
 * the same "press Enter twice to leave the list" every editor has taught.
 *
 * Returns `null` when the line is not a list or a quote, so the caller lets the
 * browser insert a plain newline.
 *
 * @param {Edit} edit
 * @returns {Edit|null}
 */
export function continueBlock(edit) {
  const { text, start, end } = edit;
  if (start !== end) return null;

  const from = lineStart(text, start);
  const line = text.slice(from, start);
  const match =
    /^(\s*)([-*+]) /.exec(line) ?? /^(\s*)(\d+)([.)]) /.exec(line) ?? /^()(>) ?/.exec(line);
  if (!match) return null;

  // Only when the caret is at or past the marker; Enter before it is a newline.
  if (line.trim() === match[0].trim()) {
    return { text: text.slice(0, from) + text.slice(start), start: from, end: from };
  }

  let marker;
  if (match[2] === ">") marker = "> ";
  else if (match[3]) marker = `${match[1]}${Number(match[2]) + 1}${match[3]} `;
  else marker = `${match[1]}${match[2]} `;

  const inserted = "\n" + marker;
  const caret = start + inserted.length;
  return { text: text.slice(0, start) + inserted + text.slice(end), start: caret, end: caret };
}

/**
 * Tab and Shift+Tab: indent or outdent the touched lines by two spaces.
 *
 * Two because that is what nests a list item under a `- ` bullet, which is
 * the only thing indentation does in a summary.
 *
 * @param {Edit} edit
 * @param {boolean} outdent
 * @returns {Edit}
 */
export function indent(edit, outdent = false) {
  const { from, lines } = selectedLines(edit);
  const changed = lines.map((line) =>
    outdent ? line.replace(/^ {1,2}|^\t/, "") : line.trim() === "" && lines.length > 1 ? line : `  ${line}`,
  );

  // Keep the caret where it was relative to its text.
  if (edit.start === edit.end && lines.length === 1) {
    const delta = changed[0].length - lines[0].length;
    const caret = Math.max(from, edit.start + delta);
    return {
      text: edit.text.slice(0, from) + changed[0] + edit.text.slice(from + lines[0].length),
      start: caret,
      end: caret,
    };
  }
  return mapLines(edit, () => changed);
}
