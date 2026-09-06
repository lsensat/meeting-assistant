/**
 * Localization.
 *
 * The whole catalogue is fetched once from Rust, so both halves of the app read
 * the same 110 keys and neither can drift.
 *
 * `applyLanguage()` replaces `refresh_ui_language()` (`app.py:4110`) — 154 lines
 * that manually re-set roughly 40 widget texts on every language switch. Here
 * the DOM already records which element owns which key, so switching languages
 * is a walk over `[data-i18n]`.
 */

import { getI18n } from "./api.js";

/** @type {Record<string, Record<string, string>>} */
let catalog = {};
let current = "en";

export async function loadCatalog() {
  catalog = await getI18n();
}

/** @param {string} language */
export function setLanguage(language) {
  current = catalog[language] ? language : "en";
}

export function language() {
  return current;
}

/**
 * Look up a key, falling back to English and then to the key itself so a
 * missing string is visible in the UI rather than rendering as blank.
 *
 * @param {string} key
 * @param {Record<string, string|number>} [args]
 */
export function tr(key, args) {
  const table = catalog[current] ?? {};
  let value = table[key] ?? catalog.en?.[key] ?? key;

  if (args) {
    for (const [name, replacement] of Object.entries(args)) {
      value = value.replaceAll(`{${name}}`, String(replacement));
    }
  }
  return value;
}

/**
 * Re-render every translated string in the document.
 *
 * @param {ParentNode} [root]
 */
export function applyLanguage(root = document) {
  for (const element of root.querySelectorAll("[data-i18n]")) {
    element.textContent = tr(element.getAttribute("data-i18n"));
  }
  for (const element of root.querySelectorAll("[data-i18n-title]")) {
    element.title = tr(element.getAttribute("data-i18n-title"));
  }
}
