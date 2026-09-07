/**
 * Main window controller.
 *
 * Replaces the Tk main loop plus `poll_messages` (`root.after(100, ...)`) with
 * event listeners; the data flow is the same one-way worker → UI it always was.
 */

// Installed before anything that can throw, so a failure in the modules
// below is reported on screen instead of leaving a blank or half-built window.
import "./errors.js";
import * as api from "./api.js";
import { applyLanguage, loadCatalog, setLanguage, tr } from "./i18n.js";
import { initTooltips } from "./tooltip.js";

const el = (id) => document.getElementById(id);

const ui = {
  status: el("status"),
  timer: el("timer"),
  start: el("start-button"),
  stop: el("stop-button"),
  mute: el("mute-button"),
  transcript: el("transcript-button"),
  summary: el("summary-button"),
  folder: el("folder-button"),
  settings: el("settings-button"),
  cancel: el("cancel-button"),
  micOn: el("mic-on"),
  micOff: el("mic-off"),
  deviceMic: el("device-mic"),
  deviceSystem: el("device-system"),
  devicesToggle: el("devices-toggle"),
  devicesDetail: el("devices-detail"),
  stages: {
    audio: el("stage-audio"),
    whisper: el("stage-whisper"),
    summary: el("stage-summary"),
  },
};

/** Paths from the last `complete`, used by the three result buttons. */
let results = { folder: "", transcript_file: "", summary_file: "" };
let recording = false;
let tick = null;

// ---------------------------------------------------------------- helpers

function setStatus(text) {
  ui.status.textContent = text;
  // The element carries a data-i18n default; once a live status replaces it,
  // a language switch must not clobber the message.
  ui.status.removeAttribute("data-i18n");

  // A long message wraps to a second line and makes the content taller. Nothing
  // re-measured after that, so the toolbar was clipped off the bottom — the
  // "still running in the menu bar" notice made it obvious, but any long error
  // does it too. `resizeToContent` is a no-op when the height has not changed,
  // so calling it on every status update is cheap.
  resizeToContent();
}

function formatElapsed(seconds) {
  const whole = Math.max(0, Math.floor(seconds));
  const h = String(Math.floor(whole / 3600)).padStart(2, "0");
  const m = String(Math.floor((whole % 3600) / 60)).padStart(2, "0");
  const s = String(whole % 60).padStart(2, "0");
  return `${h}:${m}:${s}`;
}

/**
 * Result buttons are only clickable once their stage reports `done`. The
 * unavailable look is a modifier class and the click is gated separately, so a
 * disabled-looking button can never fire.
 */
function setAvailable(button, available) {
  button.classList.toggle("btn--unavailable", !available);
}

function isAvailable(button) {
  return !button.classList.contains("btn--unavailable");
}

function setStage(name, state) {
  const chip = ui.stages[name];
  if (!chip) return;
  chip.dataset.state = state;

  const marker = chip.querySelector(".marker");
  if (!marker) return;

  // pending/working use a glyph prefix; done and error swap to a real icon.
  if (state === "done") {
    marker.textContent = "✓";
  } else if (state === "error") {
    marker.textContent = "✕";
  } else {
    marker.textContent = state === "working" ? "●" : "○";
  }
}

function resetStages() {
  for (const name of Object.keys(ui.stages)) setStage(name, "pending");
  setAvailable(ui.transcript, false);
  setAvailable(ui.summary, false);
  // The folder button stays available: the output folder exists whether or not
  // a meeting has run, and "show me where recordings go" is useful at rest.
}

function setRecording(active) {
  recording = active;
  ui.start.disabled = active;
  ui.stop.disabled = !active;
  ui.cancel.disabled = !active;

  if (active) {
    // The timer is driven from Rust rather than a local counter, so it cannot
    // drift away from the actual recording length.
    tick = setInterval(async () => {
      ui.timer.textContent = formatElapsed(await api.elapsedSeconds());
    }, 250);
  } else {
    clearInterval(tick);
    tick = null;
  }
}

/**
 * Ask for the meeting title.
 *
 * Resolves to "" when skipped or dismissed, which the Rust side treats as "no
 * title" and leaves the timestamp folder name alone.
 *
 * @returns {Promise<string>}
 */
function askMeetingTitle() {
  const modal = el("title-modal");
  const input = el("meeting-title");

  return new Promise((resolve) => {
    const finish = (value) => {
      modal.hidden = true;
      input.value = "";
      document.removeEventListener("keydown", onKey);
      resolve(value);
    };

    const onKey = (event) => {
      if (event.key === "Enter") finish(input.value.trim());
      if (event.key === "Escape") finish("");
    };

    el("title-confirm").onclick = () => finish(input.value.trim());
    document.addEventListener("keydown", onKey);

    modal.hidden = false;
    input.focus();
  });
}

/**
 * Show which devices a recording would use, before one has started.
 *
 * The panel is otherwise only written by `device_mic`/`device_system`, which
 * the recorder emits when it opens a stream — so at rest it sat on its "—"
 * placeholders and told the user nothing. This resolves the same way the
 * recorder will: the configured device if it is still present, otherwise the
 * OS default.
 *
 * @param {Record<string, unknown>} config
 */
async function showResolvedDevices(config) {
  const devices = await api.listDevices();

  const resolve = (list, configured) => {
    if (!list.length) return null;
    const chosen =
      list.find((device) => device.name === configured) ??
      list.find((device) => device.is_default) ??
      list[0];
    // `configured !== ""` matters: with nothing configured — the default —
    // comparing against "" made this always true, so every launch claimed both
    // devices had fallen back. That is why "(automatic)" appeared permanently.
    // It also matters here specifically, because the panel auto-expands on a
    // fallback and would otherwise never stay collapsed.
    return {
      // `label` is for the panel; `name` stays the identity that `configured`
      // is compared against. Comparing labels would report a fallback whenever
      // two devices collided and kept their full names.
      label: chosen.label ?? chosen.name,
      automatic: configured !== "" && chosen.name !== configured,
    };
  };

  const mic = resolve(devices.microphones, String(config.microphone_name ?? ""));
  const system = resolve(devices.system, String(config.system_audio_name ?? ""));

  ui.deviceMic.textContent = mic
    ? `${tr("microphone")}: ${mic.label}${mic.automatic ? ` (${tr("automatic")})` : ""}`
    : `${tr("microphone")}: ${tr("microphone_missing")}`;

  ui.deviceSystem.textContent = system
    ? `${tr("computer_audio")}: ${system.label}${system.automatic ? ` (${tr("automatic")})` : ""}`
    : `${tr("computer_audio")}: ${tr("computer_audio_missing")}`;

  // Open on a genuine fallback: hiding the panel must not hide the one thing
  // it exists to tell you, which is that the app is not using the device you
  // chose. Never auto-collapses — that would fight the user.
  if (mic?.automatic || system?.automatic) setDevicesExpanded(true);
}

/** Must match `tauri.conf.json`'s window height. */
const CONFIG_WINDOW_HEIGHT = 275;

/**
 * Height of the window furniture — the title bar — in CSS pixels.
 *
 * `set_size` and the configured height both cover the whole window, while the
 * layout lives in the smaller webview inside it. Ignoring the difference is why
 * the toolbar kept getting clipped: every height asked for was a title bar too
 * short.
 *
 * This is **re-measured on every resize**, not derived once at startup. The
 * one-shot version assumed the window stood at exactly `CONFIG_WINDOW_HEIGHT`
 * at the moment of the first measurement. On Windows it does not: DPI scaling
 * and a title bar of a different height meant the first launch opened visibly
 * too tall, with empty space under the toolbar, while the second launch
 * happened to land correctly. Comparing the height last *asked for* against the
 * `innerHeight` that actually resulted needs no such assumption and converges
 * after one round-trip on any platform, title bar, or scale factor.
 */
let chromeHeight = Math.max(0, CONFIG_WINDOW_HEIGHT - window.innerHeight);

/**
 * The height the content needs, in CSS pixels.
 *
 * Nothing in this window is sized against the viewport, so the bottom of the
 * last row depends only on the content above it — this returns the same answer
 * whatever size the window currently is, and cannot feed back into itself.
 */
function contentHeight() {
  const row = document.querySelector(".result-row");
  // `main` now stretches to fill the window, so the toolbar's bottom is pinned
  // to the viewport and measuring it alone would always report the current
  // height back — the window would never resize again. The spacer holds exactly
  // the surplus, so subtracting it recovers the natural height. When the content
  // needs more room than the window has, the spacer is 0 and this exceeds
  // `innerHeight`, which is what makes the window grow.
  const surplus = document.querySelector(".spacer").getBoundingClientRect().height;
  // rect.bottom excludes the row's own 10px bottom margin.
  return Math.ceil(row.getBoundingClientRect().bottom - surplus + 12);
}

/** Last height asked for, so an unchanged measurement costs nothing. */
let appliedHeight = null;

/** Last height Rust reported it applied — the request after clamping. */
let grantedHeight = null;

function resizeToContent() {
  // The height Rust actually applied, versus the `innerHeight` that resulted,
  // is the real chrome. Comparing against `appliedHeight` instead would read a
  // clamped request as an enormous title bar and grow without bound. Still put
  // through a sanity bound: a measurement taken mid-resize can be transiently
  // absurd, and a bad value here is what puts the toolbar off-screen.
  const observed = (grantedHeight ?? CONFIG_WINDOW_HEIGHT) - window.innerHeight;
  if (observed >= 0 && observed <= 200) chromeHeight = observed;

  const wanted = contentHeight() + chromeHeight;
  if (wanted === appliedHeight) return;

  appliedHeight = wanted;
  api.setMainHeight(wanted).then((granted) => {
    grantedHeight = granted;
    // Converge without waiting for the next status change. Once the window has
    // settled, this re-reads the chrome and either corrects the height or — the
    // normal case — measures the same number and returns at the guard above, so
    // it cannot loop.
    requestAnimationFrame(resizeToContent);
  });
}

/**
 * Show or hide the device detail, and resize the window to match.
 *
 * The height is measured rather than hardcoded: a device name wrapping to a
 * third line would break a fixed delta, and "MacBook Air Microphone
 * (automatic)" is already close to the width.
 *
 * @param {boolean} expanded
 * @param {boolean} [persist] write it to config; false during startup
 */
function setDevicesExpanded(expanded, persist = true) {
  if (expanded) {
    ui.devicesDetail.removeAttribute("hidden");
  } else {
    ui.devicesDetail.setAttribute("hidden", "");
  }
  ui.devicesToggle.setAttribute("aria-expanded", String(expanded));
  ui.devicesToggle.closest(".audio-panel").classList.toggle("is-collapsed", !expanded);

  // Next frame, so layout has settled with the detail shown or hidden.
  requestAnimationFrame(resizeToContent);

  if (persist) {
    api.getConfig().then((config) =>
      api.saveConfig({ ...config, devices_expanded: expanded }),
    );
  }
}

/**
 * Confirm a destructive action in-page.
 *
 * @param {string} message
 * @returns {Promise<boolean>}
 */
function confirmAction(message) {
  const modal = el("confirm-modal");
  el("confirm-message").textContent = message;

  return new Promise((resolve) => {
    const finish = (answer) => {
      modal.hidden = true;
      document.removeEventListener("keydown", onKey);
      resolve(answer);
    };
    const onKey = (event) => {
      if (event.key === "Escape") finish(false);
      if (event.key === "Enter") finish(true);
    };

    el("confirm-yes").onclick = () => finish(true);
    el("confirm-no").onclick = () => finish(false);
    document.addEventListener("keydown", onKey);

    modal.hidden = false;
  });
}

/**
 * Reflect the mute state: swap the glyph, keep the colours.
 *
 * Greying the button would read as "disabled" rather than "muted", so the
 * background and icon colour stay put and only the diagonal slash appears.
 *
 * @param {boolean} muted
 */
function applyMuted(muted) {
  ui.mute.classList.toggle("is-muted", muted);

  // setAttribute, not `.hidden`. `hidden` is an IDL attribute of HTMLElement,
  // and <svg> is an SVGElement — which does not inherit from HTMLElement. So
  // `svg.hidden = true` silently sets a plain JS property that reflects to
  // nothing, and the [hidden] CSS rule never matches. The markup's initial
  // `hidden` worked, which is why the slashed mic started hidden and then
  // never appeared.
  const show = (svg, visible) =>
    visible ? svg.removeAttribute("hidden") : svg.setAttribute("hidden", "");
  show(ui.micOn, !muted);
  show(ui.micOff, muted);
  if (recording) setStatus(muted ? tr("recording_muted") : tr("recording"));
}

// ----------------------------------------------------------------- events

function wireEvents() {
  api.on(api.EVENTS.startupStatus, setStatus);

  api.on(api.EVENTS.startupResult, (result) => {
    // Ollama's state is only worth reporting when Ollama is the configured
    // engine. A user on a remote endpoint would otherwise see
    // "Ollama is not responding" on every launch, about a component they
    // deliberately are not using.
    const usesOllama = result.summary_provider === "ollama";

    if (usesOllama && !result.ollama.running) {
      setStatus(tr("ollama_not_responding"));
    } else if (usesOllama && result.ollama.models.length === 0) {
      setStatus(tr("ollama_no_models"));
    } else if (!usesOllama && !result.summary_ready) {
      setStatus(tr("api_incomplete"));
    } else if (!result.has_microphone) {
      setStatus(tr("startup_no_mic"));
    } else if (!result.has_system_audio) {
      setStatus(tr("startup_no_loopback"));
    } else if (!result.folder_ok) {
      setStatus(tr("startup_bad_folder"));
    } else {
      setStatus(tr("ready"));
    }
  });

  api.on(api.EVENTS.status, setStatus);

  // The single source of truth for whether a meeting is running. It arrives
  // whoever started it — this window, or the tray. Driving the UI from the
  // event instead of from the click is what lets a second front-end exist at
  // all.
  api.on(api.EVENTS.recordingState, ({ recording: active }) => {
    setRecording(active);
    // The only place the recorder state reaches the DOM as an attribute. CSS
    // keys off it for the faceplate's accent border, so no JavaScript anywhere
    // has to know a colour.
    document.body.dataset.recorderState = active ? "recording" : "idle";
    if (active) setStatus(tr("recording"));
  });

  api.on(api.EVENTS.muteState, applyMuted);

  // Finalize and Cancel from the tray. A native menu cannot prompt for a
  // meeting title or confirm a deletion, so it reveals this window and asks it
  // to run the flow that already does both.
  api.on(api.EVENTS.requestStop, stopFlow);
  api.on(api.EVENTS.requestCancel, cancelFlow);

  api.on(api.EVENTS.deviceMic, (name) => {
    ui.deviceMic.textContent = `${tr("microphone")}: ${name}`;
  });
  api.on(api.EVENTS.micFallback, (name) => {
    ui.deviceMic.textContent = `${tr("microphone")}: ${name} (${tr("automatic")})`;
    // Mid-recording is when this matters most — the device changed under you.
    setDevicesExpanded(true);
  });
  api.on(api.EVENTS.deviceSystem, (name) => {
    ui.deviceSystem.textContent = `${tr("computer_audio")}: ${name}`;
  });
  api.on(api.EVENTS.systemFallback, (name) => {
    ui.deviceSystem.textContent = `${tr("computer_audio")}: ${name} (${tr("automatic")})`;
    setDevicesExpanded(true);
  });

  api.on(api.EVENTS.stage, ({ stage, state }) => {
    setStage(stage, state);

    // A result becomes clickable exactly when its stage completes.
    if (state === "done") {
      if (stage === "whisper") setAvailable(ui.transcript, true);
      if (stage === "summary") setAvailable(ui.summary, true);
    }
  });

  // Recorder diagnostics are not surfaced in the UI; the console keeps them
  // reachable without adding a log panel the original never had.
  api.on(api.EVENTS.log, (message) => console.log("[recorder]", message));

  api.on(api.EVENTS.error, (message) => {
    setStatus(message || tr("error_occurred"));
    for (const name of Object.keys(ui.stages)) {
      if (ui.stages[name].dataset.state === "working") setStage(name, "error");
    }
    setRecording(false);
  });

  api.on(api.EVENTS.complete, (payload) => {
    results = payload;
    setStatus(tr("processed_ok"));
  });
}

/**
 * Stop and process. Extracted so the tray can drive it: "Finalize Meeting"
 * needs a meeting title, and a native menu cannot collect a string — so it
 * shows this window and triggers this flow rather than passing an empty title
 * and silently dropping the feature.
 */
async function stopFlow() {
  try {
    // Capture stops first, and the timer stops with it via recording_state.
    // Asking for the title first would keep recording while the dialog was
    // open, appending the user typing a name to the end of the meeting.
    await api.stopRecording();
    const title = await askMeetingTitle();
    await api.finalizeMeeting(title);
  } catch (error) {
    setStatus(String(error));
  }
}

/** Discard and delete. Confirmed here for the same reason as stopFlow. */
async function cancelFlow() {
  if (!(await confirmAction(tr("cancel_confirm")))) return;
  try {
    await api.cancelRecording();
    ui.timer.textContent = "00:00:00";
    resetStages();
    setStatus(tr("ready"));
  } catch (error) {
    setStatus(String(error));
  }
}

// --------------------------------------------------------------- controls

function wireControls() {
  ui.start.addEventListener("click", async () => {
    resetStages();
    ui.timer.textContent = "00:00:00";
    try {
      // No setRecording here: the recording_state event does it, so this
      // window behaves identically whether the meeting was started from here
      // or from the tray.
      await api.startRecording();
    } catch (error) {
      setStatus(String(error));
    }
  });

  ui.stop.addEventListener("click", stopFlow);
  ui.cancel.addEventListener("click", cancelFlow);

  ui.mute.addEventListener("click", () => api.toggleMute());






  ui.transcript.addEventListener("click", () => {
    if (isAvailable(ui.transcript)) api.openPath(results.transcript_file);
  });
  ui.summary.addEventListener("click", () => {
    if (isAvailable(ui.summary)) api.openPath(results.summary_file);
  });
  // Falls back to the configured output folder, so this works before any
  // meeting has been recorded as well as after one.
  ui.folder.addEventListener("click", async () => {
    const target = results.folder || String((await api.getConfig()).output_folder ?? "");
    if (target) {
      try {
        await api.openPath(target);
      } catch (error) {
        setStatus(String(error));
      }
    }
  });

  ui.devicesToggle.addEventListener("click", () => {
    const open = ui.devicesToggle.getAttribute("aria-expanded") === "true";
    setDevicesExpanded(!open);
  });

  ui.settings.addEventListener("click", () => api.openSettings());
}

// ------------------------------------------------------------------ start

async function main() {
  await loadCatalog();

  const config = await api.getConfig();
  setLanguage(String(config.language ?? "en"));
  applyLanguage();

  initTooltips(el("tooltip"));
  wireEvents();
  wireControls();
  resetStages();
  setRecording(false);

  // Applied before the first device query so the window does not visibly jump
  // from expanded to collapsed on launch. `persist: false` — restoring the
  // saved state is not a user action and must not rewrite the config.
  setDevicesExpanded(config.devices_expanded === true, false);

  showResolvedDevices(config);
  applyMuted(await api.isMuted());

  // First run opens the wizard over the main window. `needs_setup` keys off the
  // absence of the config FILE, so an existing user upgrading never sees it.
  if (await api.needsSetup()) {
    await api.openSetup();

    // On Windows the wizard opened as a blank white window. Confirm the webview
    // actually navigated, and if it did not, say so where it can be seen —
    // there is no console on a release build, and a white rectangle looks the
    // same whichever of three very different things went wrong.
    setTimeout(async () => {
      try {
        const entry = (await api.windowUrls()).find(([label]) => label === "setup");
        const url = entry?.[1] ?? "";
        if (!url.includes("setup.html")) {
          setStatus(`Wizard did not load: ${url || "window missing"}`);
        }
      } catch (error) {
        setStatus(`Wizard check failed: ${String(error)}`);
      }
    }, 2500);
  }

  // Watch for device changes while idle.
  //
  // During a recording the recorder polls at 1 s and re-opens the stream
  // itself. At rest nothing was watching at all, so plugging in headphones left
  // the panel showing the old device until the window happened to regain focus.
  setInterval(async () => {
    if (recording) return;
    showResolvedDevices(await api.getConfig());
  }, 1500);

  // Settings is a separate window, so this one has to notice when it changes.
  // Regaining focus is the moment settings was closed or saved; re-reading the
  // config here keeps the language and the device panel from going stale.
  window.addEventListener("focus", async () => {
    if (recording) return;
    const latest = await api.getConfig();
    setLanguage(String(latest.language ?? "en"));
    applyLanguage();
    showResolvedDevices(latest);
  });

  // Awaited and reported. Unawaited, a rejection here was invisible: the status
  // line kept its static "Checking environment..." placeholder, which reads
  // exactly like a check still in progress.
  try {
    await api.startupCheck();
  } catch (error) {
    setStatus(`Startup check failed: ${String(error)}`);
  }
}

main().catch((error) => {
  // Without this the window renders its static HTML and looks merely unfinished.
  const status = el("status");
  if (status) status.textContent = `Startup failed: ${String(error)}`;
  throw error;
});
