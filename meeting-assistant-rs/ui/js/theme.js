/**
 * Light or dark, from the `appearance` setting. Imported by every window.
 *
 * The palette switch is CSS (`:root[data-theme="light"]` in app.css); this
 * only decides which one applies. "system" follows the operating system, and
 * follows it live: Rust hands the window's theme to the OS for that setting,
 * so the page's `prefers-color-scheme` is the OS's.
 */

import * as api from "./api.js";

const KEY = "appearance";
const systemLight = matchMedia("(prefers-color-scheme: light)");

let appearance = "dark";

function paint() {
  const light = appearance === "light" || (appearance === "system" && systemLight.matches);
  document.documentElement.dataset.theme = light ? "light" : "dark";
}

/**
 * Show `value` in this window. Settings calls it to preview a choice before
 * it is saved; everything else gets it from the config.
 *
 * @param {unknown} value "dark", "light" or "system"; anything else is dark
 */
export function applyAppearance(value) {
  appearance = value === "light" || value === "system" ? value : "dark";
  paint();
}

/** Adopt the saved setting, and remember it for `theme-boot.js`. */
async function adopt() {
  const config = await api.getConfig();
  applyAppearance(config.appearance);
  try {
    localStorage.setItem(KEY, appearance);
  } catch {
    /* the next window opens dark for a moment; nothing worse */
  }
}

systemLight.addEventListener("change", () => {
  if (appearance === "system") paint();
});

adopt().catch(() => {});
api.on(api.EVENTS.configChanged, () => adopt().catch(() => {}));
