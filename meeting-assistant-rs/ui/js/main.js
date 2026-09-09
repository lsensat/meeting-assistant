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
import { collapsible } from "./collapsible.js";

const el = (id) => document.getElementById(id);

const ui = {
  status: el("status"),
  statusAction: el("status-action"),
  lampWhisper: el("lamp-whisper"),
  lampSummary: el("lamp-summary"),
  queue: el("queue"),
  queueList: el("queue-list"),
  queueCount: el("queue-count"),
  queueToggle: el("queue-toggle"),
  queuePause: el("queue-pause"),
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
  devicesFallback: el("devices-fallback"),
};

/**
 * The last config read from Rust.
 *
 * The lamps need it and are driven by a Rust event, which carries the *state of
 * the machine* — which models are installed, whether Ollama answered — but not
 * the user's *choices*. Whether Whisper is ready is the intersection of the two:
 * the selected model must be one of the installed ones.
 */
let currentConfig = {};

/** Paths from the last `complete`, used by the three result buttons. */
let results = { folder: "", transcript_file: "", summary_file: "" };
let recording = false;
let tick = null;

// ---------------------------------------------------------------- helpers

/**
 * Longest status message that may reach the screen.
 *
 * A meeting whose summary failed put roughly 300 characters of Ollama allocator
 * internals here. The line wrapped to ten of them, the window grew to fit, and
 * the queue card below was squeezed to a sliver — one bad error took the whole
 * layout with it. Nothing is lost: the full text goes to the tooltip and the
 * console, which is where a message that long is useful anyway.
 */
const STATUS_MAX = 150;

function setStatus(text, detail) {
  const full = String(text ?? "");
  const clipped = full.length > STATUS_MAX ? `${full.slice(0, STATUS_MAX).trimEnd()}…` : full;

  ui.status.textContent = clipped;
  // `detail` is the long form of a message deliberately kept short on screen:
  // three wrapped lines pushed the toolbar off the bottom of the window, and a
  // sentence of advice is worth reading once, not on every successful meeting.
  const hover = detail ?? (clipped !== full ? full : null);
  if (hover) {
    ui.status.title = hover;
    if (clipped !== full) console.log("[status]", full);
  } else {
    ui.status.removeAttribute("title");
  }

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

/**
 * Reset the result buttons between meetings.
 *
 * Was `resetStages`, which also drove three chips that no longer exist.
 */
function resetResults() {
  setAvailable(ui.transcript, false);
  setAvailable(ui.summary, false);
  // The folder button stays available: the output folder exists whether or not
  // a meeting has run, and "show me where recordings go" is useful at rest.
}

/** Heroicons outline, 24x24. */
const ICON_POWER = "M5.636 5.636a9 9 0 1 0 12.728 0M12 3v9";
const ICON_EXTERNAL =
  "M13.5 6H5.25A2.25 2.25 0 0 0 3 8.25v10.5A2.25 2.25 0 0 0 5.25 21h10.5A2.25 " +
  "2.25 0 0 0 18 18.75V10.5m-10.5 6L21 3m0 0h-5.25M21 3v5.25";

/**
 * @param {HTMLElement} lamp
 * @param {"on"|"pending"|"off"} state
 * @param {string} key i18n key for the tooltip, e.g. `lamp_ollama_pending`.
 */
function setLamp(lamp, state, key) {
  lamp.dataset.state = state;
  // Read lazily on hover by `initTooltips`, so changing the value is enough.
  lamp.setAttribute("data-tooltip", key);
  // The same words as the tooltip: the state is otherwise colour-only.
  lamp.setAttribute("aria-label", tr(key));
}

/** Which lamp keys apply, given the configured summary provider. */
function summaryLampKeys() {
  return currentConfig.summary_provider === "openai_compatible"
    ? { on: "lamp_api_on", pending: "lamp_api_pending", off: "lamp_api_off" }
    : { on: "lamp_ollama_on", pending: "lamp_ollama_pending", off: "lamp_ollama_off" };
}

/**
 * Amber while a check is in flight — not off.
 *
 * "Not ready" and "not known yet" are different things, and showing red for the
 * second would report a fault that may not exist. This is the traffic light's
 * amber, and it is what both lamps show from launch until the first result.
 */
function setLampsPending() {
  setLamp(ui.lampWhisper, "pending", "lamp_whisper_pending");
  setLamp(ui.lampSummary, "pending", summaryLampKeys().pending);
}

function hideStatusAction() {
  ui.statusAction.hidden = true;
  ui.statusAction.onclick = null;
}

/**
 * Offer the one action that can fix a stopped Ollama.
 *
 * The app has already tried: `startup_check` launches Ollama when it finds it
 * and waits for it to answer. Reaching here means that failed, or that Ollama
 * was stopped after launch — and until now the only remedy was restarting the
 * app, which is what the (unused) `ollama_stopped` string used to advise.
 *
 * @param {boolean} installed Whether an `ollama` binary exists on this machine.
 */
function offerOllamaAction(installed) {
  // Offering to start something that is not installed is a dead end, so the
  // other half of the branch offers the download instead.
  const key = installed ? "setup_open_ollama" : "setup_get_ollama";
  const action = ui.statusAction;

  // A power symbol for "start the thing", an open-in-new for "go and get it" —
  // two different actions should not wear the same icon. Swapping the path
  // rather than two `<svg>` elements avoids `hidden`, which does nothing on an
  // SVGElement.
  action.querySelector("path").setAttribute("d", installed ? ICON_POWER : ICON_EXTERNAL);
  // Read lazily on hover, so changing it here is enough.
  action.setAttribute("data-tooltip", key);
  action.setAttribute("aria-label", tr(key));
  action.hidden = false;

  action.onclick = async () => {
    if (!installed) {
      api.openUrl("https://ollama.com/download");
      return;
    }

    // Re-runs the whole check rather than reimplementing the probe here: it
    // starts Ollama if it is still down, waits for it properly, and re-emits
    // the result, which lands back in the same handler that put this button up.
    action.disabled = true;
    setStatus(tr("checking_ollama"));
    setLamp(ui.lampSummary, "pending", summaryLampKeys().pending);
    try {
      await api.startupCheck();
    } catch (error) {
      setStatus(`Startup check failed: ${String(error)}`);
    }
    action.disabled = false;
  };

  // Showing the button can wrap the status row onto a second line.
  resizeToContent();
}

// ------------------------------------------------------------------ queue

/** Heroicons outline. */
const ICON_PAUSE = "M15.75 5.25v13.5m-7.5-13.5v13.5";
const ICON_PLAY = "M5.25 5.653c0-.856.917-1.398 1.667-.986l11.54 6.348a1.125 1.125 0 0 1 0 1.971l-11.54 6.347a1.125 1.125 0 0 1-1.667-.985V5.653Z";
const ICON_TRASH =
  "m14.74 9-.346 9m-4.788 0L9.26 9m9.968-3.21c.342.052.682.107 1.022.166m-1.022-.165L18.16 " +
  "19.673a2.25 2.25 0 0 1-2.244 2.077H8.084a2.25 2.25 0 0 1-2.244-2.077L4.772 5.79m14.456 " +
  "0a48.108 48.108 0 0 0-3.478-.397m-12 .562c.34-.059.68-.114 1.022-.165m0 0a48.11 48.11 0 " +
  "0 1 3.478-.397m7.5 0v-.916c0-1.18-.91-2.164-2.09-2.201a51.964 51.964 0 0 0-3.32 0c-1.18 " +
  ".037-2.09 1.022-2.09 2.201v.916m7.5 0a48.667 48.667 0 0 0-7.5 0";
const ICON_RETRY =
  "M16.023 9.348h4.992v-.001M2.985 19.644v-4.992m0 0h4.992m-4.993 0 3.181 3.183a8.25 8.25 " +
  "0 0 0 13.803-3.7M4.031 9.865a8.25 8.25 0 0 1 13.803-3.7l3.181 3.182m0-4.991v4.99";

/** True while processing is held. Mirrored from Rust, which owns the truth. */
let processingPaused = false;

function iconButton(className, path, tooltipKey, onClick) {
  const button = document.createElement("button");
  button.className = className;
  button.setAttribute("data-tooltip", tooltipKey);
  button.setAttribute("aria-label", tr(tooltipKey));

  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("aria-hidden", "true");
  const shape = document.createElementNS("http://www.w3.org/2000/svg", "path");
  shape.setAttribute("d", path);
  svg.append(shape);
  button.append(svg);

  button.addEventListener("click", onClick);
  return button;
}

/**
 * `2026-09-07_14-03-22` — the job id — as a clock time.
 *
 * The id is the meeting's own start time, so this needs no extra field: the
 * thing the user recognises a meeting by is already the thing that identifies
 * it. Anything unexpected falls back to the raw id rather than showing nothing.
 */
function jobTime(id) {
  const match = /^\d{4}-\d{2}-\d{2}_(\d{2})-(\d{2})/.exec(id);
  return match ? `${match[1]}:${match[2]}` : id;
}

/** Which of the three processing steps a stage is. */
const STAGE_STEP = { audio: 1, whisper: 2, summary: 3 };

/**
 * "Transcribing 2/3".
 *
 * The step number is what turns a stage name into progress. "Transcribing" on
 * its own says nothing about how much is left; "2/3" says there is a whole
 * summary still to come, which is the difference between a card that informs
 * and a card that just moves.
 */
function stageLabel(job) {
  if (job.stage === "failed") return tr("queue_stage_failed");
  // Paused work is not "waiting its turn"; say which it is.
  if (!job.running && processingPaused) return tr("queue_paused");

  const name = tr(`queue_stage_${job.stage}`);
  const step = STAGE_STEP[job.stage];
  if (!step) return name;

  // The number as well as the bar. Transcription advances once per 30 seconds
  // of audio, so on a slow machine a 44px bar can sit still for a long time and
  // is indistinguishable from a frozen app — which is how it was read.
  return job.running
    ? `${name} ${step}/3 · ${job.percent}%`
    : `${name} ${step}/3`;
}

function queueCard(job) {
  const card = document.createElement("div");
  card.className = "queue-card";
  card.dataset.state = job.stage;

  const body = document.createElement("div");
  body.className = "queue-card-body";

  const line = document.createElement("div");
  line.className = "queue-card-line";

  const time = document.createElement("span");
  time.className = "queue-card-time";
  time.textContent = jobTime(job.id);

  const title = document.createElement("span");
  title.className = "queue-card-title";
  title.textContent = job.title || tr("queue_untitled");

  const stage = document.createElement("span");
  stage.className = "queue-card-stage";
  stage.textContent = stageLabel(job);

  // Fixed width and on the line, not a full-width rule beneath it. A bar that
  // spans the card reads as a divider between meetings rather than as the
  // progress of one, and at this size the row has the space for it.
  const bar = document.createElement("div");
  bar.className = "queue-bar";
  const fill = document.createElement("div");
  fill.className = "queue-bar-fill";
  // Through the CSSOM, not a `style` attribute: the CSP has no
  // `unsafe-inline` in `style-src`, which would block the attribute form.
  fill.style.width = `${job.running ? job.percent : 0}%`;
  bar.append(fill);

  line.append(time, title, stage, bar);
  body.append(line);
  card.append(body);

  // The parts a poll changes, kept on the element. A tick then writes three
  // strings and a width instead of discarding and rebuilding every card, which
  // is what made opening and closing this panel feel heavy: the rebuild landed
  // on top of the toggle's own resize.
  card.dataset.id = job.id;
  card._parts = { time, title, stage, fill };

  if (job.stage === "failed") {
    card.append(
      iconButton("retry-button", ICON_RETRY, "queue_retry", () => retryJob(job.id)),
    );
  }
  card.append(
    iconButton("trash-button", ICON_TRASH, "queue_discard", () => confirmDiscard(job)),
  );

  // A failure is otherwise invisible: the error itself only reaches the status
  // line, which the next meeting overwrites.
  if (job.error) card.title = job.error;

  return card;
}

async function retryJob(id) {
  try {
    await api.retryJob(id);
  } catch (error) {
    setStatus(String(error));
  }
  await renderQueue();
}

/**
 * Ask before deleting a recording.
 *
 * @param {{id: string, title: string}} job
 */
function confirmDiscard(job) {
  const modal = el("discard-modal");

  const close = () => {
    modal.hidden = true;
    document.removeEventListener("keydown", onKey);
  };
  const onKey = (event) => {
    if (event.key === "Escape") close();
  };

  el("discard-no").onclick = close;
  el("discard-yes").onclick = async () => {
    close();
    try {
      await api.discardJob(job.id);
    } catch (error) {
      setStatus(String(error));
    }
    await renderQueue();
  };

  document.addEventListener("keydown", onKey);
  modal.hidden = false;
  el("discard-no").focus();
}

/** The job ids currently on screen, so a tick knows whether to rebuild. */
let renderedIds = "";

/** Redraw the queue from Rust's snapshot. */
async function renderQueue() {
  let jobs = [];
  try {
    jobs = await api.listJobs();
    processingPaused = await api.isProcessingPaused();
  } catch {
    // The window can outlive a command failing; an empty queue is the honest
    // thing to draw, and the status line reports anything that matters.
  }

  // Hidden rather than empty: an "Processing" heading over nothing is noise on
  // every launch, and the window should not carry height it has no use for.
  ui.queue.hidden = jobs.length === 0;
  ui.queueCount.textContent = jobs.length > 1 ? `(${jobs.length})` : "";

  ui.queuePause.querySelector("path").setAttribute("d", processingPaused ? ICON_PLAY : ICON_PAUSE);
  const pauseKey = processingPaused ? "queue_resume" : "queue_pause";
  ui.queuePause.setAttribute("data-tooltip", pauseKey);
  ui.queuePause.setAttribute("aria-label", tr(pauseKey));

  // Rebuild only when the set of meetings changes. Between those moments a
  // meeting's percentage and stage move, and both are text on an element that
  // already exists — replacing the DOM to change "5%" to "17%" throws away the
  // browser's layout for the whole list and forces a re-measure of the window.
  const ids = jobs.map((job) => job.id).join("\u0000");
  if (ids !== renderedIds) {
    renderedIds = ids;
    ui.queueList.replaceChildren(...jobs.map(queueCard));
    resizeToContent();
    return;
  }

  for (const job of jobs) {
    const card = ui.queueList.querySelector(`[data-id="${CSS.escape(job.id)}"]`);
    const parts = card?._parts;
    if (!parts) continue;

    card.dataset.state = job.stage;
    parts.title.textContent = job.title || tr("queue_untitled");
    parts.stage.textContent = stageLabel(job);
    parts.fill.style.width = `${job.running ? job.percent : 0}%`;
    if (job.error) card.title = job.error;
  }
}

/**
 * Poll while something is running.
 *
 * `queue_changed` covers every discrete transition, but not a percentage
 * climbing inside one — the worker emits that on a timer and this reads it. It
 * stops as soon as nothing is running, so an idle app does no work.
 */
function startQueuePolling() {
  setInterval(async () => {
    // Nothing to draw into: the panel is gone, or its list is collapsed. The
    // header's count and pause icon are redrawn by `queue_changed` anyway, and
    // those are the only parts visible while closed.
    if (ui.queue.hidden || !queueIsOpen()) return;
    const jobs = await api.listJobs().catch(() => []);
    if (jobs.some((job) => job.running)) await renderQueue();
  }, 700);
}

/**
 * The two panels, built from one implementation.
 *
 * Constructed lazily on first use rather than at module scope, because
 * `resizeToContent` is declared below them and the panels must not capture it
 * before it exists.
 */
let queuePanel;
let devicesPanel;

function setQueueExpanded(expanded, persist = true) {
  queuePanel ??= collapsible({
    panel: ui.queue,
    toggle: ui.queueToggle,
    detail: ui.queueList,
    refit: resizeToContent,
    persist: (open) =>
      api.getConfig().then((config) => api.saveConfig({ ...config, queue_expanded: open })),
  });
  queuePanel.set(expanded, persist);
}

/** Whether the queue detail is on screen; the poll skips work when it is not. */
function queueIsOpen() {
  return ui.queueToggle.getAttribute("aria-expanded") === "true";
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

  announceFallback(mic?.automatic === true || system?.automatic === true);
}

/**
 * Show or hide the marker that says a device the user did not choose is in use.
 *
 * **The panel is never opened by the app.** It used to open itself here, then
 * only on the transition into a fallback, and both fought the user: the events
 * that drive this fire on a 1.5-second poll and again whenever the recorder
 * opens a stream, so a panel closed during a meeting reopened moments later.
 *
 * The warning still has to be reachable — a fallback means the recording is not
 * coming from the device that was chosen, which is worth knowing before the
 * meeting rather than after. A dot in the header carries that while collapsed,
 * and the panel opens only when the user opens it.
 *
 * @param {boolean} active
 */
function announceFallback(active) {
  if (active) {
    ui.devicesFallback.removeAttribute("hidden");
  } else {
    ui.devicesFallback.setAttribute("hidden", "");
  }
}

/**
 * The height this window's content needs, in CSS pixels.
 *
 * # Measured from the parts, not from the whole
 *
 * The obvious measurement — where the last row's bottom edge falls — is a trap
 * here, because `main` fills the viewport and a spacer absorbs the slack. That
 * makes the answer depend on the window's current height, so every correction
 * changed the thing being measured. In practice the window oscillated: a run
 * logged it walking 242 → 239, back to 278, and around again, never settling.
 *
 * Summing the children instead is independent of the window entirely. A flex
 * column does not stretch its children on the main axis, so each one reports its
 * own natural height whatever size the window is, and margins do not collapse
 * inside a flex container. The spacer is skipped precisely because it is the
 * part that varies.
 */
function contentHeight() {
  const main = document.querySelector("main");
  let total = 0;

  for (const child of main.children) {
    if (child.classList.contains("spacer")) continue;
    const style = getComputedStyle(child);
    if (style.display === "none") continue;
    total +=
      child.getBoundingClientRect().height +
      parseFloat(style.marginTop) +
      parseFloat(style.marginBottom);
  }

  return Math.ceil(total);
}

/**
 * Passes spent chasing a size this window cannot have.
 *
 * The delta is satisfied in one step whenever the window manager allows it. When
 * it does not — the clamp in `nudge_main_height`, or a screen too short — the
 * shortfall would otherwise be re-requested forever. Four attempts is far more
 * than convergence needs and stops an argument nobody can win.
 */
let resizePasses = 0;

/**
 * Fit the window to its content.
 *
 * Asks for a **difference**, not a height. Working out an absolute height meant
 * knowing what the title bar costs, and every way of learning that was wrong:
 * Tauri reports the same value for a window's outer and inner size on macOS, and
 * reading the size straight after setting it can return the previous one. The
 * arithmetic built on those numbers lost a pixel per pass — a window measured
 * walking 242, 241, 240, 239 — until the toolbar was pushed off the bottom.
 *
 * How much more room the content needs than it has is something this side can
 * measure exactly, and it is all the other side needs to know.
 */
let resizeQueued = false;

/**
 * Fold every resize request made in one frame into a single measurement.
 *
 * Several things ask for a refit at once — a status line changing, the queue
 * panel appearing, a panel being toggled — and each used to start its own
 * measure/IPC/re-measure loop against the same shared `resizePasses` budget.
 * They exhausted it between them and gave up early, which left the window a
 * few lines short of its content and the toolbar clipped off the bottom.
 */
function resizeToContent() {
  if (resizeQueued) return;
  resizeQueued = true;
  requestAnimationFrame(() => {
    resizeQueued = false;
    measureAndResize();
  });
}

function measureAndResize() {
  const delta = Math.round(contentHeight() - window.innerHeight);

  if (delta === 0) {
    resizePasses = 0;
    return;
  }
  if (resizePasses >= 4) {
    resizePasses = 0;
    return;
  }
  resizePasses += 1;

  api.nudgeMainHeight(delta).then(() => requestAnimationFrame(measureAndResize));
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
  devicesPanel ??= collapsible({
    panel: ui.devicesToggle.closest(".audio-panel"),
    toggle: ui.devicesToggle,
    detail: ui.devicesDetail,
    refit: resizeToContent,
    persist: (open) =>
      api.getConfig().then((config) => api.saveConfig({ ...config, devices_expanded: open })),
  });
  devicesPanel.set(expanded, persist);
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
  // Every discrete change: a meeting enqueued, finished, failed or discarded,
  // and the pause switch moving. The percentage climbing within a job comes
  // from the poll instead.
  api.on(api.EVENTS.queueChanged, () => {
    renderQueue();
  });

  api.on(api.EVENTS.startupStatus, setStatus);

  api.on(api.EVENTS.startupResult, (result) => {
    // Cleared on every result: a previous run may have left an offer standing
    // for a problem that has since been resolved.
    hideStatusAction();

    // Whisper is ready only if the model the user actually selected is one of
    // the installed ones. "Some model is installed" is a different question:
    // transcription loads the configured model, and would stop to download it.
    const whisperReady =
      Array.isArray(result.whisper_installed) &&
      result.whisper_installed.includes(String(currentConfig.whisper_model ?? ""));
    setLamp(
      ui.lampWhisper,
      whisperReady ? "on" : "off",
      whisperReady ? "lamp_whisper_on" : "lamp_whisper_off",
    );

    // `summary_ready` is Rust's own verdict and already covers both providers —
    // Ollama running with at least one model, or a remote endpoint with a base
    // URL, a model and a stored key.
    const keys = summaryLampKeys();
    setLamp(ui.lampSummary, result.summary_ready ? "on" : "off",
            result.summary_ready ? keys.on : keys.off);

    // Ollama's state is only worth reporting when Ollama is the configured
    // engine. A user on a remote endpoint would otherwise see
    // "Ollama is not responding" on every launch, about a component they
    // deliberately are not using.
    const usesOllama = result.summary_provider === "ollama";

    if (usesOllama && !result.ollama.running) {
      setStatus(tr("ollama_not_responding"));
      offerOllamaAction(result.ollama_installed === true);
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
    announceFallback(false);
  });
  api.on(api.EVENTS.micFallback, (name) => {
    ui.deviceMic.textContent = `${tr("microphone")}: ${name} (${tr("automatic")})`;
    // Mid-recording is when this matters most — the device changed under you.
    // Once, though: the recorder re-reports its device on every stream open.
    announceFallback(true);
  });
  api.on(api.EVENTS.deviceSystem, (name) => {
    ui.deviceSystem.textContent = `${tr("computer_audio")}: ${name}`;
  });
  api.on(api.EVENTS.systemFallback, (name) => {
    ui.deviceSystem.textContent = `${tr("computer_audio")}: ${name} (${tr("automatic")})`;
    announceFallback(true);
  });

  // Recorder diagnostics are not surfaced in the UI; the console keeps them
  // reachable without adding a log panel the original never had.
  api.on(api.EVENTS.log, (message) => console.log("[recorder]", message));

  api.on(api.EVENTS.error, (message) => {
    // A failed job also shows as failed on its own card, which is where the
    // meeting it belongs to can actually be identified.
    setStatus(message || tr("error_occurred"));
    setRecording(false);
  });

  api.on(api.EVENTS.complete, (payload) => {
    results = payload;
    // Enabled here rather than when each stage reported done. The paths these
    // buttons open arrive with THIS event, so enabling them earlier left them
    // clickable while `results` still pointed at the previous meeting.
    setAvailable(ui.transcript, true);
    setAvailable(ui.summary, true);
    setStatus(
      tr(payload.quiet_recording ? "processed_ok_quiet" : "processed_ok"),
      payload.quiet_recording ? tr("processed_ok_quiet_detail") : undefined,
    );
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
    resetResults();
    setStatus(tr("ready"));
  } catch (error) {
    setStatus(String(error));
  }
}

// --------------------------------------------------------------- controls

function wireControls() {
  ui.start.addEventListener("click", async () => {
    resetResults();
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

  ui.settings.addEventListener("click", () => api.openSettings());
}

// ------------------------------------------------------------------ start

async function main() {
  await loadCatalog();

  const config = await api.getConfig();
  currentConfig = config;
  setLanguage(String(config.language ?? "en"));
  applyLanguage();

  initTooltips(el("tooltip"));
  wireEvents();
  wireControls();
  resetResults();
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

  // Amber until the first result: the check has started but has not answered.
  setLampsPending();

  setQueueExpanded(config.queue_expanded !== false, false);
  await renderQueue();
  startQueuePolling();

  ui.queuePause.addEventListener("click", async () => {
    ui.queuePause.disabled = true;
    try {
      await api.setProcessingPaused(!processingPaused);
    } catch (error) {
      setStatus(String(error));
    }
    ui.queuePause.disabled = false;
    await renderQueue();
  });

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
    currentConfig = latest;
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
