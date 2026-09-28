/**
 * Paint the right palette before the first frame.
 *
 * A classic script in `<head>`, not a module: modules run after the page has
 * parsed, and the window would flash dark before a light theme arrived over
 * IPC. This reads what `theme.js` saved last time; `theme.js` then confirms it
 * against the real config.
 */
(() => {
  let appearance = "dark";
  try {
    appearance = localStorage.getItem("appearance") || "dark";
  } catch {
    /* storage unavailable: dark, the default */
  }
  const light =
    appearance === "light" ||
    (appearance === "system" && matchMedia("(prefers-color-scheme: light)").matches);
  document.documentElement.dataset.theme = light ? "light" : "dark";
})();
