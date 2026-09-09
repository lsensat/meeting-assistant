/**
 * The collapsible panel, once.
 *
 * "Microphone and Audio" and "Processing" are the same object wearing different
 * content: a header button that shows and hides a detail region, marks its
 * panel collapsed so the padding transition runs, carries the state to
 * assistive tech through `aria-expanded`, and refits the window afterwards.
 *
 * They were two near-identical functions, and the copies had already drifted in
 * the way duplicated code does. The queue's version was reported as feeling
 * laggy against the device panel's "super smooth" — same markup, same CSS, so
 * the difference was never the styling. It was that the queue re-rendered and
 * re-measured on a 700ms timer, including while collapsed, so a toggle landed
 * in the middle of work the panel above it never did.
 *
 * One implementation means one behaviour. What differs between the two panels
 * is passed in.
 */

/**
 * @param {object} spec
 * @param {HTMLElement} spec.panel    gets `is-collapsed`
 * @param {HTMLElement} spec.toggle   the header button
 * @param {HTMLElement} spec.detail   the region shown and hidden
 * @param {() => void} spec.refit     called once layout has settled
 * @param {(expanded: boolean) => void} [spec.persist] store the new state
 * @returns {{set: (expanded: boolean, persist?: boolean) => void, isOpen: () => boolean}}
 */
export function collapsible({ panel, toggle, detail, refit, persist }) {
  const isOpen = () => toggle.getAttribute("aria-expanded") === "true";

  function set(expanded, shouldPersist = true) {
    if (expanded) {
      detail.removeAttribute("hidden");
    } else {
      detail.setAttribute("hidden", "");
    }
    toggle.setAttribute("aria-expanded", String(expanded));
    panel.classList.toggle("is-collapsed", !expanded);

    // Next frame, so layout has settled with the detail shown or hidden.
    requestAnimationFrame(refit);

    if (shouldPersist && persist) persist(expanded);
  }

  toggle.addEventListener("click", () => set(!isOpen()));

  return { set, isOpen };
}
