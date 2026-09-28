/**
 * Save shortly after the user stops typing, and never lose a change to a race.
 *
 * # The rules
 *
 * - A save starts {@link Autosave#delay} ms after the last change, so typing a
 *   sentence is one write and not forty.
 * - Continuous typing still saves at least every {@link Autosave#maxWait} ms: a
 *   debounce alone would never fire for someone who never pauses, and a crash
 *   then loses everything since they started.
 * - **One save in flight at a time.** Changes made while a save runs are saved
 *   after it, against the version it returned. Two overlapping saves would each
 *   quote the same base version, and the second would be refused as a conflict
 *   with the first.
 * - A conflict stops autosaving until the user decides — see
 *   `library::save_source` for why a timer must not overwrite someone else's
 *   change.
 *
 * DOM-free, so `ui/test-autosave.mjs` can drive it with a fake backend.
 *
 * @typedef {"saved"|"pending"|"saving"|"conflict"|"error"} SaveState
 */

export class Autosave {
  /**
   * @param {{
   *   save: (text: string, baseVersion: string|null) =>
   *     Promise<{t: "saved", version: string} | {t: "conflict"}>,
   *   onState: (state: SaveState, detail?: string) => void,
   *   delay?: number,
   *   maxWait?: number,
   * }} options
   */
  constructor({ save, onState, delay = 800, maxWait = 5000 }) {
    this.save = save;
    this.onState = onState;
    this.delay = delay;
    this.maxWait = maxWait;

    /** The version on disk that the next save must quote. */
    this.version = null;
    /** Text not yet written, or null when everything is saved. */
    this.dirty = null;
    this.timer = null;
    this.firstChangeAt = 0;
    /** The save in flight, so `flush` can wait for it. */
    this.running = null;
    this.blocked = false;
  }

  /** A document was loaded: start clean from its version. */
  reset(version) {
    clearTimeout(this.timer);
    this.timer = null;
    this.version = version;
    this.dirty = null;
    this.blocked = false;
    this.onState("saved");
  }

  /** The text changed. */
  changed(text) {
    this.dirty = text;
    if (this.blocked) return;

    this.onState("pending");
    const now = Date.now();
    if (this.timer === null) this.firstChangeAt = now;
    clearTimeout(this.timer);

    const overdue = now - this.firstChangeAt >= this.maxWait;
    this.timer = setTimeout(() => this.#run(), overdue ? 0 : this.delay);
  }

  /**
   * Write anything unsaved now, and wait for it. Called before switching
   * document, switching to Read, or closing the window.
   */
  async flush() {
    clearTimeout(this.timer);
    this.timer = null;
    if (this.running) await this.running;
    if (this.dirty !== null && !this.blocked) await this.#run();
  }

  /** Whether there is anything the user would lose. */
  get unsaved() {
    return this.dirty !== null;
  }

  /**
   * The user chose to overwrite the file after a conflict: save what is in
   * the editor without quoting a version.
   */
  async overwrite() {
    this.blocked = false;
    this.version = null;
    if (this.dirty === null) return;
    await this.#run();
  }

  async #run() {
    this.timer = null;
    if (this.running) {
      // Picked up by the loop below once the current save returns.
      return this.running;
    }

    this.running = (async () => {
      while (this.dirty !== null && !this.blocked) {
        const text = this.dirty;
        this.onState("saving");
        let outcome;
        try {
          outcome = await this.save(text, this.version);
        } catch (error) {
          // Kept dirty, so the next change or flush retries it.
          this.onState("error", String(error));
          return;
        }

        if (outcome.t === "conflict") {
          this.blocked = true;
          this.onState("conflict");
          return;
        }

        this.version = outcome.version;
        // Only clear what was actually written: typing during the save left
        // newer text in `dirty`, and the loop saves that next.
        if (this.dirty === text) this.dirty = null;
      }
      if (this.dirty === null) this.onState("saved");
    })();

    try {
      await this.running;
    } finally {
      this.running = null;
    }
  }
}
