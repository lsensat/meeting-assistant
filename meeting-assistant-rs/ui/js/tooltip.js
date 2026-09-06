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

let timer = null;

/** @param {HTMLElement} tooltip */
export function initTooltips(tooltip) {
  const hide = () => {
    clearTimeout(timer);
    tooltip.classList.remove("visible");
  };

  for (const trigger of document.querySelectorAll("[data-tooltip]")) {
    trigger.addEventListener("mouseenter", () => {
      clearTimeout(timer);
      timer = setTimeout(() => {
        tooltip.textContent = tr(trigger.getAttribute("data-tooltip"));

        const box = trigger.getBoundingClientRect();
        tooltip.classList.add("visible");

        // Measure after making it visible, or the width is zero and the
        // centring is wrong on first show.
        const width = tooltip.getBoundingClientRect().width;
        tooltip.style.left = `${Math.max(2, box.left + box.width / 2 - width / 2)}px`;
        tooltip.style.top = `${box.bottom + OFFSET_PX}px`;
      }, DELAY_MS);
    });

    trigger.addEventListener("mouseleave", hide);
    trigger.addEventListener("mousedown", hide);
  }
}
