/**
 * The teleprompter window: a frameless, transparent, always-on-top scroller.
 *
 * Everything that touches the window itself (move, resize, click-through,
 * closing) is a Rust command; this file draws the text and reports intent.
 * Click-through is decided in Rust by polling the cursor, because a
 * click-through window receives no mouse events to decide it from here.
 */

// Installed before anything that can throw, so a failure is reported on screen.
import "./errors.js";
import * as api from "./api.js";
import { applyLanguage, loadCatalog, setLanguage, tr } from "./i18n.js";
import {
  Scroller,
  TEXT_OPACITY_MIN,
  clampFont,
  clampOpacity,
  clampPosition,
  clampSpeed,
  keyAction,
  pxPerSecond,
  rescalePosition,
  scriptBlocks,
  stepFont,
  stepSpeed,
} from "./teleprompter-logic.js";

const SAVE_DELAY_MS = 400;
const TYPING_SAVE_DELAY_MS = 1000;
/** Wheel pixels are used as they come; a notch is about 100. */
const WHEEL_FACTOR = 1;

/** @param {string} id @returns {HTMLElement} */
const $ = (id) => /** @type {HTMLElement} */ (document.getElementById(id));

const body = document.body;
const viewport = $("tp-viewport");
const text = $("tp-text");
const playButton = $("tp-play");
const speedValue = $("tp-speed-value");
const fontValue = $("tp-font-value");
const settingsPanel = $("tp-settings");
const editorPanel = $("tp-editor");
const textOpacity = /** @type {HTMLInputElement} */ ($("tp-text-opacity"));
const overlayOpacity = /** @type {HTMLInputElement} */ ($("tp-overlay-opacity"));
const usePipeline = /** @type {HTMLInputElement} */ ($("tp-use-in-pipeline"));
const scriptBox = /** @type {HTMLTextAreaElement} */ ($("tp-script"));
const errorLine = $("tp-error");

/** @type {api.TeleprompterSettings} */
let settings = {
  script: "",
  font_size: 44,
  speed: 5,
  overlay_opacity: 0.4,
  text_opacity: 1,
  use_in_pipeline: true,
};
const scroller = new Scroller();
let position = 0;
let contentHeight = 0;
let importInFlight = false;
let lastPinned = null;
/** @type {ReturnType<typeof setTimeout>|null} */
let saveTimer = null;
/**
 * True once getTeleprompter() has resolved. Until then `settings` holds
 * placeholders, and saving them would overwrite the user's teleprompter.json.
 */
let loaded = false;

// --- display ------------------------------------------------------------------

function applyVars() {
  const root = document.documentElement.style;
  root.setProperty("--tp-font-size", `${settings.font_size}px`);
  root.setProperty("--tp-text-opacity", String(settings.text_opacity));
  root.setProperty("--tp-overlay-opacity", String(settings.overlay_opacity));
  speedValue.textContent = String(settings.speed);
  fontValue.textContent = String(settings.font_size);
  textOpacity.value = String(Math.round(settings.text_opacity * 100));
  overlayOpacity.value = String(Math.round(settings.overlay_opacity * 100));
  usePipeline.checked = settings.use_in_pipeline;
}

function measure() {
  contentHeight = text.offsetHeight;
}

function paint() {
  text.style.transform = `translate3d(0, ${-position}px, 0)`;
}

/** Draw the script. Text nodes only: a script is user content, never markup. */
function renderScript() {
  const blocks = scriptBlocks(settings.script);
  text.replaceChildren(
    ...blocks.map((block) => {
      const node = document.createElement(block.kind === "heading" ? "h2" : "p");
      node.className = block.kind === "heading" ? "tp-heading" : "tp-para";
      node.textContent = block.text;
      return node;
    }),
  );
  body.classList.toggle("is-empty", blocks.length === 0);
  measure();
  position = clampPosition(position, contentHeight, viewport.clientHeight);
  paint();
  syncPinned();
}

function setPlayLabels() {
  const name = scroller.playing ? tr("teleprompter_pause") : tr("teleprompter_play");
  const key = scroller.playing ? "teleprompter_pause" : "teleprompter_play";
  // Keep the data attributes in step so applyLanguage() re-translates correctly.
  playButton.setAttribute("data-i18n-title", key);
  playButton.setAttribute("data-i18n-label", key);
  playButton.title = name;
  playButton.setAttribute("aria-label", name);
  playButton.textContent = scroller.playing ? "⏸" : "▶";
  body.classList.toggle("is-playing", scroller.playing);
}

function localize() {
  applyLanguage();
  scriptBox.placeholder = tr("teleprompter_placeholder");
  setPlayLabels();
}

// --- state the window needs from Rust -------------------------------------------

const scriptEmpty = () => settings.script.trim() === "";
const editorOpen = () => !editorPanel.hidden;
const settingsOpen = () => !settingsPanel.hidden;

/** Keep the window interactive while something needs the pointer. */
function syncPinned() {
  const pinned = editorOpen() || settingsOpen() || importInFlight || scriptEmpty();
  if (pinned === lastPinned) return;
  lastPinned = pinned;
  api.teleprompterSetPinned(pinned).catch(() => {});
}

// --- saving -----------------------------------------------------------------

async function saveNow() {
  if (!loaded) return;
  if (saveTimer !== null) {
    clearTimeout(saveTimer);
    saveTimer = null;
  }
  try {
    const saved = await api.saveTeleprompter(settings);
    // Adopt Rust's clamped numbers, but keep the local script: the user may
    // have typed more while this save was in flight.
    settings = { ...saved, script: settings.script };
    applyVars();
  } catch (error) {
    showError(String(error));
  }
}

/** @param {number} [delay] */
function scheduleSave(delay = SAVE_DELAY_MS) {
  if (!loaded) return;
  if (saveTimer !== null) clearTimeout(saveTimer);
  saveTimer = setTimeout(saveNow, delay);
}

/** @param {string} message */
function showError(message) {
  errorLine.textContent = message;
  errorLine.hidden = false;
  // The error line lives in the editor panel, so it has to be open to be seen.
  if (!editorOpen()) setEditor(true);
}

/** Write out a debounced change now (window losing focus or being hidden). */
function flushPending() {
  if (saveTimer !== null) saveNow().catch(() => {});
}

// --- actions ----------------------------------------------------------------

function togglePlay() {
  if (scriptEmpty()) return;
  scroller.toggle();
  setPlayLabels();
}

function pause() {
  scroller.pause();
  setPlayLabels();
}

function restart() {
  position = 0;
  paint();
}

function setSpeed(level) {
  settings.speed = clampSpeed(level);
  applyVars();
  scheduleSave();
}

function setFont(px) {
  const next = clampFont(px);
  if (next === settings.font_size) return;
  const before = contentHeight;
  settings.font_size = next;
  applyVars();
  // Reflow is synchronous on the next read, so the new height is available now.
  measure();
  position = clampPosition(rescalePosition(position, before, contentHeight), contentHeight, viewport.clientHeight);
  paint();
  scheduleSave();
}

/**
 * Open or close the editor. EVERY close commits the textarea (render + save),
 * whichever route got here: Done, Esc, the Edit toggle, or opening Settings.
 * @param {boolean} open
 * @returns {Promise<void>} resolves when the commit's save has finished
 */
function setEditor(open) {
  const wasOpen = editorOpen();
  editorPanel.hidden = !open;
  body.classList.toggle("is-editing", open);
  if (open) {
    settingsPanel.hidden = true;
    body.classList.remove("is-settings-open");
    scriptBox.value = settings.script;
    // Before the script has loaded the box must not be editable: the error
    // is shown, but nothing typed could ever be saved.
    scriptBox.disabled = !loaded;
    pause();
    scriptBox.focus();
  } else {
    errorLine.hidden = true;
  }
  syncPinned();
  if (!open && wasOpen && loaded) {
    settings.script = scriptBox.value;
    renderScript();
    return saveNow();
  }
  return Promise.resolve();
}

function setSettings(open) {
  settingsPanel.hidden = !open;
  body.classList.toggle("is-settings-open", open);
  if (open && editorOpen()) setEditor(false).catch((e) => showError(String(e)));
  syncPinned();
}

async function closeWindow() {
  await saveNow();
  await api.closeTeleprompter();
}

/** @param {ReturnType<typeof keyAction>} action */
function perform(action) {
  switch (action) {
    case "toggle":
      togglePlay();
      break;
    case "faster":
      setSpeed(stepSpeed(settings.speed, 1));
      break;
    case "slower":
      setSpeed(stepSpeed(settings.speed, -1));
      break;
    case "bigger":
      setFont(stepFont(settings.font_size, 1));
      break;
    case "smaller":
      setFont(stepFont(settings.font_size, -1));
      break;
    case "close":
      closeWindow().catch((e) => showError(String(e)));
      break;
    case "closeEditor":
      finishEditing().catch((e) => showError(String(e)));
      break;
    default:
      break;
  }
}

async function finishEditing() {
  await setEditor(false);
}

async function importScript() {
  importInFlight = true;
  errorLine.hidden = true;
  syncPinned();
  try {
    const imported = await api.importTeleprompterScript();
    if (typeof imported === "string") {
      scriptBox.value = imported;
      settings.script = imported;
      renderScript();
      await saveNow();
    }
  } catch (error) {
    errorLine.textContent = tr("teleprompter_import_failed", { error: String(error) });
    errorLine.hidden = false;
  } finally {
    importInFlight = false;
    syncPinned();
  }
}

// --- wiring -----------------------------------------------------------------

playButton.addEventListener("click", togglePlay);
$("tp-restart").addEventListener("click", restart);
$("tp-slower").addEventListener("click", () => perform("slower"));
$("tp-faster").addEventListener("click", () => perform("faster"));
$("tp-smaller").addEventListener("click", () => perform("smaller"));
$("tp-bigger").addEventListener("click", () => perform("bigger"));
$("tp-edit").addEventListener("click", () => {
  setEditor(!editorOpen()).catch((e) => showError(String(e)));
});
$("tp-settings-toggle").addEventListener("click", () => setSettings(!settingsOpen()));
$("tp-close").addEventListener("click", () => perform("close"));
$("tp-import").addEventListener("click", importScript);
$("tp-done").addEventListener("click", () => {
  finishEditing().catch((e) => showError(String(e)));
});

textOpacity.addEventListener("input", () => {
  settings.text_opacity = clampOpacity(Number(textOpacity.value) / 100, TEXT_OPACITY_MIN);
  applyVars();
  scheduleSave();
});
overlayOpacity.addEventListener("input", () => {
  settings.overlay_opacity = clampOpacity(Number(overlayOpacity.value) / 100);
  applyVars();
  scheduleSave();
});
usePipeline.addEventListener("change", () => {
  settings.use_in_pipeline = usePipeline.checked;
  scheduleSave();
});
scriptBox.addEventListener("input", () => {
  settings.script = scriptBox.value;
  body.classList.toggle("is-empty", scriptEmpty());
  scheduleSave(TYPING_SAVE_DELAY_MS);
});

document.addEventListener("keydown", (event) => {
  if (event.metaKey || event.ctrlKey || event.altKey) return;
  // A focused slider or checkbox keeps its own arrow/space behaviour.
  const target = /** @type {HTMLElement} */ (event.target);
  const control =
    target instanceof HTMLInputElement && (target.type === "range" || target.type === "checkbox");
  const action = keyAction(event.key, { editing: editorOpen() });
  if (!action) return;
  if (control && action !== "close") return;
  // Also stops Space from clicking whichever button has focus.
  event.preventDefault();
  perform(action);
});

// Drag from anywhere on the stage that is not a control, panel or edge.
$("tp-stage").addEventListener("mousedown", (event) => {
  if (event.button !== 0) return;
  const target = /** @type {HTMLElement} */ (event.target);
  if (target.closest(".tp-controls, .tp-panel, .tp-edge")) return;
  event.preventDefault();
  api.teleprompterStartDrag().catch(() => {});
});

$("tp-stage").addEventListener(
  "wheel",
  (event) => {
    if (event.target instanceof Element && event.target.closest(".tp-panel")) return;
    event.preventDefault();
    position = clampPosition(position + event.deltaY * WHEEL_FACTOR, contentHeight, viewport.clientHeight);
    paint();
  },
  { passive: false },
);

// Edge resize. The OS does it when it can; otherwise we send cumulative deltas.
for (const edge of document.querySelectorAll(".tp-edge")) {
  const element = /** @type {HTMLElement} */ (edge);
  const direction = /** @type {"n"|"s"|"e"|"w"|"ne"|"nw"|"se"|"sw"} */ (element.dataset.edge);
  /** @type {{x: number, y: number}|null} */
  let origin = null;
  let manual = false;
  let queued = false;
  let latest = { dx: 0, dy: 0 };

  element.addEventListener("pointerdown", async (event) => {
    if (event.button !== 0) return;
    event.preventDefault();
    const pointerId = event.pointerId;
    // Capture synchronously: after the await the press may already be over.
    element.setPointerCapture(pointerId);
    origin = { x: event.screenX, y: event.screenY };
    manual = false;
    let native = false;
    try {
      native = await api.teleprompterStartResize(direction);
    } catch {
      native = false;
    }
    if (native) {
      origin = null;
      if (element.hasPointerCapture(pointerId)) element.releasePointerCapture(pointerId);
    } else {
      manual = true;
    }
  });

  element.addEventListener("pointermove", (event) => {
    if (!manual || !origin) return;
    latest = { dx: event.screenX - origin.x, dy: event.screenY - origin.y };
    if (queued) return;
    queued = true;
    requestAnimationFrame(() => {
      queued = false;
      api.teleprompterResizeBy(latest.dx, latest.dy).catch(() => {});
    });
  });

  const release = (/** @type {PointerEvent} */ event) => {
    if (element.hasPointerCapture(event.pointerId)) element.releasePointerCapture(event.pointerId);
    origin = null;
    manual = false;
  };
  element.addEventListener("pointerup", release);
  element.addEventListener("pointercancel", release);
}

// Alt+F4 or a tray Quit gives no chance to run closeWindow(); save what is pending.
window.addEventListener("blur", flushPending);
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "hidden") flushPending();
});

// The window size changed: the viewport height did, so the end of the text moved.
window.addEventListener("resize", () => {
  measure();
  position = clampPosition(position, contentHeight, viewport.clientHeight);
  paint();
});

function frame(now) {
  const delta = scroller.advance(now, pxPerSecond(settings.speed, settings.font_size));
  if (delta > 0) {
    const max = Math.max(0, contentHeight - viewport.clientHeight);
    position = clampPosition(position + delta, contentHeight, viewport.clientHeight);
    paint();
    if (position >= max) pause();
  }
  requestAnimationFrame(frame);
}

// --- boot ---------------------------------------------------------------------

async function boot() {
  // Before any await: Rust may report the pointer leaving while we load.
  // The pointer starts over the window; Rust tells us when that stops being true.
  body.classList.add("is-hovered");
  api.on(api.EVENTS.teleprompterHover, (hovered) => {
    body.classList.toggle("is-hovered", Boolean(hovered));
  });

  // Own step: if this throws, nothing may be saved over the stored settings.
  settings = await api.getTeleprompter();
  loaded = true;

  try {
    await loadCatalog();
    setLanguage((await api.getConfig()).language);
  } catch (error) {
    showError(String(error));
  }
  localize();
  applyVars();
  renderScript();
  if (scriptEmpty() && !editorOpen()) setEditor(true);

  api.on(api.EVENTS.configChanged, async () => {
    try {
      setLanguage((await api.getConfig()).language);
      localize();
    } catch {
      /* the next change retries */
    }
  });

  requestAnimationFrame(frame);
}

boot().catch((error) => showError(String(error)));

// Re-measure once fonts settle, otherwise the first end-of-text clamp is wrong.
document.fonts?.ready.then(() => {
  measure();
  position = clampPosition(position, contentHeight, viewport.clientHeight);
}).catch(() => {});
