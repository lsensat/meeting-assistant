/**
 * Make failures visible in the window they happen in.
 *
 * Every window here loads a module graph and then talks to Rust over IPC. Both
 * of those fail silently by default: a module that throws while loading leaves
 * the page showing its static HTML, and a rejected `invoke` whose promise
 * nobody awaited is swallowed entirely. The result is a window that looks
 * merely unfinished rather than broken.
 *
 * That cost a full build-test-report round trip on Windows. The main window sat
 * on "Checking environment...", which is the *static* placeholder in
 * `index.html` — indistinguishable, from a screenshot, from a check that had
 * started and hung. Had the error been on screen we would have known which.
 *
 * Import this **first**, before any module that can throw, and keep it free of
 * imports of its own so it cannot be taken down by the failure it exists to
 * report.
 */

/** @param {string} message */
function show(message) {
  // Built without innerHTML: this renders text of unknown provenance — an error
  // string may contain a path, a URL, or a fragment of a response body.
  let banner = document.getElementById("fatal-error");
  if (!banner) {
    banner = document.createElement("div");
    banner.id = "fatal-error";
    // A class, not a `style` attribute. The CSP here is `style-src 'self'`
    // with no `unsafe-inline`, which blocks the attribute form outright — so
    // this banner rendered as unstyled black text at the top of a dark
    // document and was, in practice, invisible. That is how a syntax error in
    // `main.js` reached `main` and two release builds: the one mechanism built
    // to make such a failure visible had been silently disabled by the app's
    // own security policy.
    banner.className = "fatal-error";
    // `document.body` is null if this fires while the head is still parsing.
    (document.body ?? document.documentElement).append(banner);
  }
  banner.append(banner.childNodes.length ? `\n\n${message}` : message);

  // Also to the terminal. The banner above is styled with a `style` attribute,
  // which this app's own CSP (`style-src 'self'`, no `unsafe-inline`) blocks —
  // so it renders as unstyled text at the top of the document and is easy to
  // miss entirely. A frontend error that nobody can see is how a syntax error
  // in main.js survived into main and two release builds.
  try {
    window.__TAURI__?.core?.invoke("ui_log", { message });
  } catch {
    // Nothing else to try.
  }
}

window.addEventListener("error", (event) => {
  // A failed <script>/<link> load fires `error` on the element, with no message.
  const target = /** @type {HTMLElement|null} */ (event.target);
  if (target && target !== /** @type {unknown} */ (window) && "src" in target) {
    show(`Failed to load: ${String(target.src ?? "")}`);
    return;
  }
  show(event.error?.stack ?? event.message ?? "Unknown error");
  // Capture phase, because resource errors do not bubble.
}, true);

window.addEventListener("unhandledrejection", (event) => {
  const reason = event.reason;
  show(`Unhandled rejection: ${reason?.stack ?? String(reason)}`);
});

/**
 * The single most likely first failure, reported precisely rather than as
 * "Cannot destructure property of undefined".
 *
 * `withGlobalTauri` injects `window.__TAURI__` before page scripts run. If it
 * is missing, every `api.js` export is unreachable and nothing else in the
 * window will work, so say exactly that instead of letting a TypeError from a
 * module's top level stand in for it.
 */
export function assertTauriPresent() {
  if (!window.__TAURI__) {
    show(
      "window.__TAURI__ is missing.\n\n" +
        "The page loaded but the Tauri IPC global was not injected, so no " +
        "command can be called. Check `withGlobalTauri` in tauri.conf.json and " +
        "that this window's label is listed in capabilities/default.json.",
    );
    return false;
  }
  return true;
}
