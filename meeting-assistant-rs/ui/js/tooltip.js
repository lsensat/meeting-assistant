/**
 * Tooltips: 450ms delay, centred below the trigger with a 6px offset,
 * dismissed on both mouse-leave and mouse-down.
 *
 * The mouse-down dismissal matters — without it the tooltip lingers over the
 * dialog a click just opened.
 */

import { tr } from "./i18n.js";

const DELAY_MS = 450;
const OFFSET_PX = 6;
/** Keep this much clear of every window edge. */
const EDGE_PX = 4;

let timer = null;

/** @param {HTMLElement} tooltip */
export function initTooltips(tooltip) {
  const hide = () => {
    clearTimeout(timer);
    tooltip.classList.remove("visible");
  };

  /**
   * Delegated, not bound per element.
   *
   * The queue's cards are created and replaced as meetings come and go, so
   * anything bound at startup would miss every one of them. Listening on the
   * document means an element becomes a tooltip trigger simply by carrying the
   * attribute, whenever it appears.
   */
  const show = (trigger) => {
    clearTimeout(timer);
    timer = setTimeout(() => {
        // `data-tooltip` is a catalog key; `data-tooltip-text` is text already
        // built by the caller, for the things a static catalog cannot express —
        // a meeting's own title, length and stage.
        const literal = trigger.getAttribute("data-tooltip-text");
        tooltip.textContent = literal ?? tr(trigger.getAttribute("data-tooltip"));

        const box = trigger.getBoundingClientRect();
        tooltip.classList.add("visible");

        // Measure after making it visible, or the size is zero and both the
        // centring and the flip below are computed against nothing.
        const { width, height } = tooltip.getBoundingClientRect();

        // The windows are small and fixed-size, so a tooltip below a control in
        // the bottom row runs off the edge and is clipped — the toolbar sits
        // roughly 20px from the bottom. Flip above the trigger when there is no
        // room beneath it.
        const below = box.bottom + OFFSET_PX;
        const fitsBelow = below + height <= window.innerHeight - EDGE_PX;
        tooltip.style.top = `${fitsBelow ? below : box.top - OFFSET_PX - height}px`;

        // Clamp horizontally too: the mute and settings buttons sit at the far
        // left and right, so a centred tooltip on either overhangs its side.
        const centred = box.left + box.width / 2 - width / 2;
        const maxLeft = window.innerWidth - width - EDGE_PX;
        tooltip.style.left = `${Math.min(Math.max(EDGE_PX, centred), Math.max(EDGE_PX, maxLeft))}px`;
    }, DELAY_MS);
  };

  document.addEventListener("mouseover", (event) => {
    const trigger = event.target.closest?.("[data-tooltip], [data-tooltip-text]");
    if (trigger) show(trigger);
  });
  document.addEventListener("mouseout", (event) => {
    if (event.target.closest?.("[data-tooltip], [data-tooltip-text]")) hide();
  });
  document.addEventListener("mousedown", hide);
}
