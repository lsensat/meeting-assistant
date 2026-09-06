/**
 * Main window controller.
 *
 * Replaces the Tk main loop plus `poll_messages` (`root.after(100, ...)`) with
 * event listeners; the data flow is the same one-way worker → UI it always was.
 */

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
    el("title-skip").onclick = () => finish("");
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
    return { name: chosen.name, automatic: chosen.name !== configured };
  };

  const mic = resolve(devices.microphones, String(config.microphone_name ?? ""));
  const system = resolve(devices.system, String(config.system_audio_name ?? ""));

  ui.deviceMic.textContent = mic
    ? `${tr("microphone")}: ${mic.name}${mic.automatic ? ` (${tr("automatic")})` : ""}`
    : `${tr("microphone")}: ${tr("microphone_missing")}`;

  ui.deviceSystem.textContent = system
    ? `${tr("computer_audio")}: ${system.name}${system.automatic ? ` (${tr("automatic")})` : ""}`
    : `${tr("computer_audio")}: ${tr("computer_audio_missing")}`;
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
  ui.micOn.hidden = muted;
  ui.micOff.hidden = !muted;
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

  api.on(api.EVENTS.deviceMic, (name) => {
    ui.deviceMic.textContent = `${tr("microphone")}: ${name}`;
  });
  api.on(api.EVENTS.micFallback, (name) => {
    ui.deviceMic.textContent = `${tr("microphone")}: ${name} (${tr("automatic")})`;
  });
  api.on(api.EVENTS.deviceSystem, (name) => {
    ui.deviceSystem.textContent = `${tr("computer_audio")}: ${name}`;
  });
  api.on(api.EVENTS.systemFallback, (name) => {
    ui.deviceSystem.textContent = `${tr("computer_audio")}: ${name} (${tr("automatic")})`;
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

// --------------------------------------------------------------- controls

function wireControls() {
  ui.start.addEventListener("click", async () => {
    resetStages();
    ui.timer.textContent = "00:00:00";
    try {
      await api.startRecording();
      setRecording(true);
      setStatus(tr("recording"));
    } catch (error) {
      setStatus(String(error));
    }
  });

  ui.stop.addEventListener("click", async () => {
    setRecording(false);
    setStatus(tr("finalizing_recording"));
    try {
      // The meeting title is asked for at stop, matching the original flow.
      const title = await askMeetingTitle();
      await api.stopRecording(title);
    } catch (error) {
      setStatus(String(error));
    }
  });

  ui.cancel.addEventListener("click", async () => {
    // Irreversible: the recording is deleted, so confirm before doing it.
    if (!(await confirmAction(tr("cancel_confirm")))) return;
    setRecording(false);
    try {
      await api.cancelRecording();
      ui.timer.textContent = "00:00:00";
      resetStages();
      setStatus(tr("ready"));
    } catch (error) {
      setStatus(String(error));
    }
  });

  ui.mute.addEventListener("click", async () => {
    applyMuted(await api.toggleMute());
  });

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

  showResolvedDevices(config);
  applyMuted(await api.isMuted());

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

  api.startupCheck();
}

main();
