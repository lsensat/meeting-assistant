/**
 * The teleprompter's arithmetic, kept apart from the DOM.
 *
 * Pure on purpose: no `window`, no `document`, no IPC at the top level, so
 * `node ui/test-teleprompter.mjs` can import it directly. The first group of
 * constants mirrors `meeting_core::teleprompter` and must change with it.
 */

// --- mirrored from Rust (meeting_core::teleprompter) -------------------------

export const FONT_MIN = 16;
export const FONT_MAX = 160;
export const FONT_DEFAULT = 44;
export const FONT_STEP = 4;

export const SPEED_MIN = 1;
export const SPEED_MAX = 30;
export const SPEED_DEFAULT = 5;

export const TEXT_OPACITY_MIN = 0.2;
export const TEXT_OPACITY_DEFAULT = 1.0;
export const OVERLAY_OPACITY_DEFAULT = 0.4;
/** Slider step for both opacities. */
export const OPACITY_STEP = 0.05;

// --- JS only ----------------------------------------------------------------

/** A frame longer than this (tab stalled, window hidden) must not make the text jump. */
export const MAX_FRAME_MS = 100;

/** @param {number} value @param {number} min @param {number} max @param {number} fallback */
function clamp(value, min, max, fallback) {
  const n = Number(value);
  if (!Number.isFinite(n)) return fallback;
  return Math.min(max, Math.max(min, n));
}

/** @param {number} px */
export const clampFont = (px) => Math.round(clamp(px, FONT_MIN, FONT_MAX, FONT_DEFAULT));

/** @param {number} level */
export const clampSpeed = (level) => Math.round(clamp(level, SPEED_MIN, SPEED_MAX, SPEED_DEFAULT));

/**
 * Clamp an opacity to `[min, 1]`, rounded to two decimals.
 *
 * @param {number} value
 * @param {number} [min] 0 for the background, `TEXT_OPACITY_MIN` for the text
 */
export function clampOpacity(value, min = 0) {
  const n = clamp(value, min, 1, min === 0 ? OVERLAY_OPACITY_DEFAULT : TEXT_OPACITY_DEFAULT);
  return Math.round(n * 100) / 100;
}

/** @param {number} px @param {number} dir +1 or -1 */
export const stepFont = (px, dir) => clampFont(px + Math.sign(dir) * FONT_STEP);

/** @param {number} level @param {number} dir +1 or -1 */
export const stepSpeed = (level, dir) => clampSpeed(level + Math.sign(dir));

/**
 * Scroll speed. Proportional to the font so a given level reads at the same
 * number of lines per second whatever the size.
 *
 * @param {number} level
 * @param {number} fontSize
 */
export const pxPerSecond = (level, fontSize) => level * fontSize * 0.2;

/**
 * What a key press means.
 *
 * While the script editor is open the keys belong to the textarea: only Escape
 * is ours, and it closes the editor rather than the window.
 *
 * @param {string} key `KeyboardEvent.key`
 * @param {{editing: boolean}} state
 * @returns {"toggle"|"faster"|"slower"|"bigger"|"smaller"|"close"|"closeEditor"|null}
 */
export function keyAction(key, { editing }) {
  if (editing) return key === "Escape" ? "closeEditor" : null;
  switch (key) {
    case " ":
      return "toggle";
    case "ArrowUp":
    case "ArrowRight":
      return "faster";
    case "ArrowDown":
    case "ArrowLeft":
      return "slower";
    case "+":
    case "=":
      return "bigger";
    case "-":
    case "_":
      return "smaller";
    case "Escape":
      return "close";
    default:
      return null;
  }
}

/** Time-based auto-scroll: distance depends on elapsed time, not on frame rate. */
export class Scroller {
  constructor() {
    this.playing = false;
    /** @type {number|null} */
    this.last = null;
  }

  /** Start; the clock restarts so time spent paused is never scrolled through. */
  play() {
    this.playing = true;
    this.last = null;
  }

  pause() {
    this.playing = false;
    this.last = null;
  }

  toggle() {
    if (this.playing) this.pause();
    else this.play();
  }

  /**
   * Distance to move for the frame at `nowMs`.
   *
   * @param {number} nowMs monotonic milliseconds
   * @param {number} pxPerSec
   * @returns {number} pixels, 0 while paused
   */
  advance(nowMs, pxPerSec) {
    if (!this.playing) return 0;
    if (this.last === null) {
      this.last = nowMs;
      return 0;
    }
    const dt = Math.min(Math.max(nowMs - this.last, 0), MAX_FRAME_MS);
    this.last = nowMs;
    return (dt / 1000) * pxPerSec;
  }
}

/**
 * Keep the scroll position inside the text: from the start, to the point where
 * the last line has reached the top of the viewport.
 *
 * @param {number} pos
 * @param {number} contentH height of the text
 * @param {number} viewportH
 */
export function clampPosition(pos, contentH, viewportH) {
  const max = Math.max(0, contentH - viewportH);
  if (!Number.isFinite(pos)) return 0;
  return Math.min(max, Math.max(0, pos));
}

/**
 * Where the same passage sits after the text reflows to a new height (a font
 * change), so the line being read stays put.
 *
 * @param {number} pos
 * @param {number} oldH
 * @param {number} newH
 */
export function rescalePosition(pos, oldH, newH) {
  if (!(oldH > 0) || !(newH > 0) || !Number.isFinite(pos)) return pos > 0 && Number.isFinite(pos) ? pos : 0;
  return (pos / oldH) * newH;
}

/** Inline Markdown to plain text: emphasis marks, code ticks and link targets go. */
function plain(line) {
  return line
    .replace(/\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/[*`]/g, "")
    .trim();
}

/**
 * Split a script into display blocks. Blank lines separate paragraphs; a line
 * starting with `#` is a heading. Everything is text: the caller must render it
 * with `textContent`.
 *
 * @param {string} text
 * @returns {{kind: "heading"|"para", text: string}[]}
 */
export function scriptBlocks(text) {
  /** @type {{kind: "heading"|"para", text: string}[]} */
  const blocks = [];
  /** @type {string[]} */
  let para = [];

  const flush = () => {
    const joined = plain(para.join("\n"));
    if (joined) blocks.push({ kind: "para", text: joined });
    para = [];
  };

  for (const raw of String(text ?? "").replace(/\r\n?/g, "\n").split("\n")) {
    const line = raw.trim();
    if (line === "") {
      flush();
    } else if (line.startsWith("#")) {
      flush();
      const heading = plain(line.replace(/^#+\s*/, ""));
      if (heading) blocks.push({ kind: "heading", text: heading });
    } else {
      para.push(line);
    }
  }
  flush();
  return blocks;
}
