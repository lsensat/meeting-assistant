/**
 * Make a percentage that arrives in jumps look like one that moves.
 *
 * Whisper decodes in 30-second windows and reports only when a window closes,
 * so the real number sits still and then leaps: 5%, nothing for a minute, 17%.
 * A bar doing that is indistinguishable from a frozen app, which is how it was
 * read on Windows.
 *
 * # Why the rate is measured and not a constant
 *
 * "How long does Whisper take per 30 seconds of audio" has no fixed answer. It
 * depends on the model (`tiny` against `large` is an order of magnitude), the
 * machine, whether the build found AVX2 or Metal, and how loaded the box is.
 * A constant tuned on one laptop would run ahead on a slow machine — the worst
 * failure, a bar at 100% with minutes left — and crawl on a fast one.
 *
 * So this learns the rate from the run it is describing: two real reports give
 * a slope, and the slope predicts the next one. The first window has nothing to
 * learn from and does not move, which is honest — at that point the app genuinely
 * does not know how fast this machine is.
 *
 * # What keeps it truthful
 *
 * - It never goes backwards.
 * - It never runs past the next expected report. Extrapolating one window ahead
 *   is a prediction; two would be an invention.
 * - It never reaches 100% on its own. Completion is a fact reported by Rust,
 *   not something a timer is allowed to conclude.
 */

/** Per-job estimate. Keyed by job id by the caller. */
export class Smoother {
  constructor() {
    /** The last percentage Rust actually reported. */
    this.real = 0;
    /** When that report landed, in ms. */
    this.realAt = performance.now();
    /** Percent per millisecond, smoothed across reports. */
    this.rate = 0;
    /** The size of the last real jump: the distance to the next checkpoint. */
    this.step = 0;
    /** What is on screen. Monotonic. */
    this.shown = 0;
  }

  /**
   * Feed the number Rust reported.
   *
   * @param {number} percent
   */
  observe(percent) {
    if (percent <= this.real) {
      // A restart — a retry, or a job resumed from zero. Drop what was learned
      // rather than carry a slope from a different run.
      if (percent < this.real) {
        this.real = percent;
        this.shown = percent;
        this.rate = 0;
        this.step = 0;
        this.realAt = performance.now();
      }
      return;
    }

    const now = performance.now();
    const elapsed = now - this.realAt;
    const jump = percent - this.real;

    if (elapsed > 0) {
      const observed = jump / elapsed;
      // Averaged, not replaced: windows differ in how much speech they hold,
      // and one dense window should not reset the whole estimate.
      this.rate = this.rate === 0 ? observed : this.rate * 0.6 + observed * 0.4;
    }

    this.step = jump;
    this.real = percent;
    this.realAt = now;
    if (this.shown < percent) this.shown = percent;
  }

  /**
   * What to draw now.
   *
   * @returns {number} 0-99, never decreasing
   */
  value() {
    if (this.rate > 0) {
      const predicted = this.real + this.rate * (performance.now() - this.realAt);
      // One window ahead at most, and never all the way to 100.
      const ceiling = Math.min(this.real + this.step, 99);
      this.shown = Math.max(this.shown, Math.min(predicted, ceiling));
    }
    return Math.min(Math.floor(this.shown), 99);
  }
}
