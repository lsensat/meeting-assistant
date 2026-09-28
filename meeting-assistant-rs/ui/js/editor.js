/**
 * The Markdown source editor: a textarea, its own undo history, and the
 * keyboard shortcuts that drive `format.js`.
 *
 * # Why the undo history is ours and not the browser's
 *
 * A textarea's native undo only knows about typing. The moment a toolbar
 * button sets `.value`, WebKit and WebView2 both **discard** the native stack —
 * so one press of Bold would make everything typed before it impossible to
 * undo. `execCommand("insertText")` keeps the native stack alive, but it is
 * deprecated, behaves differently in the two engines this app ships on, and
 * gives no redo control at all.
 *
 * So every change — typed or formatted — goes through {@link History}, and
 * Ctrl/Cmd+Z, Ctrl/Cmd+Shift+Z, Ctrl+Y and the macOS Edit menu's Undo (which
 * arrives as a `historyUndo` input event) are all routed to it.
 */

import * as format from "./format.js";

/**
 * How long a pause in typing ends one undo step. Without grouping, Undo would
 * remove one character at a time; with no limit, it would remove a paragraph.
 */
const TYPING_GROUP_MS = 1000;
/** Steps kept. Snapshots are whole documents, and summaries are small. */
const HISTORY_LIMIT = 500;

/** @typedef {{ text: string, start: number, end: number }} Snapshot */

/**
 * Undo and redo over whole-document snapshots.
 *
 * Kept free of the DOM so it can be tested in Node.
 */
export class History {
  constructor(now = () => Date.now()) {
    /** @type {Snapshot[]} */
    this.past = [];
    /** @type {Snapshot[]} */
    this.future = [];
    this.now = now;
    /** The kind and time of the last typed change, for grouping. */
    this.lastKind = null;
    this.lastAt = 0;
  }

  /**
   * Remember `before` as an undo step, unless it belongs to the same run of
   * typing as the previous one.
   *
   * @param {Snapshot} before the document *before* the change
   * @param {string|null} kind `"insert"` or `"delete"` for typing, which may be
   *   grouped; `null` for anything else — a button, a paste, Enter — which is
   *   always its own step
   */
  record(before, kind = null) {
    const at = this.now();
    const grouped = kind !== null && kind === this.lastKind && at - this.lastAt < TYPING_GROUP_MS;
    this.lastKind = kind;
    this.lastAt = at;
    this.future = [];
    if (grouped) return;

    this.past.push(before);
    if (this.past.length > HISTORY_LIMIT) this.past.shift();
  }

  /**
   * @param {Snapshot} current what is on screen now, which Redo will restore
   * @returns {Snapshot|null} what to show instead, or null if nothing to undo
   */
  undo(current) {
    const previous = this.past.pop();
    if (!previous) return null;
    this.future.push(current);
    this.lastKind = null;
    return previous;
  }

  /**
   * @param {Snapshot} current
   * @returns {Snapshot|null}
   */
  redo(current) {
    const next = this.future.pop();
    if (!next) return null;
    this.past.push(current);
    this.lastKind = null;
    return next;
  }

  get canUndo() {
    return this.past.length > 0;
  }

  get canRedo() {
    return this.future.length > 0;
  }

  /** Forget everything — a different document was loaded. */
  clear() {
    this.past = [];
    this.future = [];
    this.lastKind = null;
  }
}

/** Ctrl on Windows, Cmd on macOS. */
const isMac = /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
const mod = (event) => (isMac ? event.metaKey : event.ctrlKey);

/**
 * Wire a textarea up as the editor.
 *
 * @param {HTMLTextAreaElement} textarea
 * @param {{
 *   onChange: (text: string) => void,
 *   onHistory?: (canUndo: boolean, canRedo: boolean) => void,
 *   linkPlaceholder?: () => string,
 * }} hooks
 */
export function createEditor(textarea, { onChange, onHistory = () => {}, linkPlaceholder }) {
  const history = new History();

  /** @returns {Snapshot} */
  const snapshot = () => ({
    text: textarea.value,
    start: textarea.selectionStart,
    end: textarea.selectionEnd,
  });

  const notifyHistory = () => onHistory(history.canUndo, history.canRedo);

  /** Put a snapshot on screen. */
  function show({ text, start, end }) {
    textarea.value = text;
    textarea.setSelectionRange(start, end);
    textarea.focus();
  }

  /**
   * Apply a formatting function as one undo step.
   *
   * @param {(edit: Snapshot) => Snapshot|null} fn
   */
  function apply(fn) {
    const before = snapshot();
    const after = fn(before);
    if (!after || after.text === before.text) {
      if (after) show(after);
      return false;
    }
    history.record(before);
    show(after);
    onChange(after.text);
    notifyHistory();
    return true;
  }

  function undo() {
    const target = history.undo(snapshot());
    if (!target) return;
    show(target);
    onChange(target.text);
    notifyHistory();
  }

  function redo() {
    const target = history.redo(snapshot());
    if (!target) return;
    show(target);
    onChange(target.text);
    notifyHistory();
  }

  /** Toolbar and shortcut actions, by name. */
  const actions = {
    bold: () => apply((e) => format.toggleWrap(e, "**")),
    italic: () => apply((e) => format.toggleWrap(e, "*")),
    strike: () => apply((e) => format.toggleWrap(e, "~~")),
    heading: () => apply(format.cycleHeading),
    bullets: () => apply(format.toggleBullets),
    numbers: () => apply(format.toggleNumbers),
    quote: () => apply(format.toggleQuote),
    code: () => apply(format.toggleCode),
    link: () => apply((e) => format.insertLink(e, linkPlaceholder?.())),
    undo,
    redo,
  };

  // Typed changes. The snapshot is taken *before* the browser applies the
  // input, which is the state Undo has to return to.
  let pending = null;
  textarea.addEventListener("beforeinput", (event) => {
    if (event.inputType === "historyUndo") {
      event.preventDefault();
      undo();
      return;
    }
    if (event.inputType === "historyRedo") {
      event.preventDefault();
      redo();
      return;
    }

    // Grouped only for plain typing and deleting. Paste, cut, drop and
    // anything else is one step on its own.
    const kind = event.inputType === "insertText"
      ? "insert"
      : event.inputType.startsWith("delete")
        ? "delete"
        : null;
    // A space or punctuation ends a word, and a word is a sensible undo step.
    const boundary = kind === "insert" && /[\s.,;:!?]/.test(event.data ?? "");
    pending = { before: snapshot(), kind: boundary ? null : kind };
  });

  textarea.addEventListener("input", () => {
    if (pending) history.record(pending.before, pending.kind);
    pending = null;
    onChange(textarea.value);
    notifyHistory();
  });

  textarea.addEventListener("keydown", (event) => {
    const key = event.key.toLowerCase();

    if (mod(event) && !event.altKey) {
      const shortcut =
        key === "z" && !event.shiftKey ? "undo"
        : (key === "z" && event.shiftKey) || (key === "y" && !isMac) ? "redo"
        : key === "b" ? "bold"
        : key === "i" ? "italic"
        : key === "k" ? "link"
        : key === "e" ? "code"
        : null;
      if (shortcut) {
        event.preventDefault();
        actions[shortcut]();
      }
      return;
    }

    if (event.key === "Enter" && !event.shiftKey && !event.isComposing) {
      if (apply(format.continueBlock)) event.preventDefault();
      return;
    }

    if (event.key === "Tab" && !event.ctrlKey && !event.metaKey && !event.altKey) {
      // Tab would otherwise move focus out of the editor, which in a text
      // editor is never what the key means. Escape then Tab still leaves it.
      if (tabTraps) {
        event.preventDefault();
        apply((e) => format.indent(e, event.shiftKey));
      }
      return;
    }

    if (event.key === "Escape") tabTraps = false;
  });

  // Tab indents while typing; after Escape it moves focus as usual, so the
  // editor is never a keyboard trap.
  let tabTraps = true;
  textarea.addEventListener("focus", () => {
    tabTraps = true;
  });

  return {
    actions,
    /** Replace the whole document, as a fresh load: no undo back past it. */
    load(text) {
      textarea.value = text;
      textarea.setSelectionRange(0, 0);
      textarea.scrollTop = 0;
      history.clear();
      notifyHistory();
    },
    /**
     * Replace the whole document as one undoable step, **without** reporting
     * it as a change. For reloading from disk: the new text is already saved,
     * so autosave must not write it back, but Undo should still reach what
     * was there before.
     */
    replace(text) {
      const before = snapshot();
      if (before.text === text) return;
      history.record(before);
      const caret = Math.min(before.start, text.length);
      show({ text, start: caret, end: caret });
      notifyHistory();
    },
    get text() {
      return textarea.value;
    },
  };
}
