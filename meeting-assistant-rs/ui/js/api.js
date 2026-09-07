/**
 * The IPC boundary, in one file.
 *
 * Command and event names are the failure class that dies silently on a typo:
 * a misspelled `invoke` name rejects at runtime, and a misspelled `listen` name
 * simply never fires. Naming each one exactly once here means a typo is a
 * missing export — caught by `tsc --checkJs` — rather than a silent no-op.
 */

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const { open } = window.__TAURI__.dialog;
const opener = window.__TAURI__.opener;

/** Diagnostic: what each window's webview actually has loaded.
 * @returns {Promise<[string, string][]>} */
export const windowUrls = () => invoke("window_urls");

/** @returns {Promise<Record<string, unknown>>} */
export const getConfig = () => invoke("get_config");

/** @param {Record<string, unknown>} config */
export const saveConfig = (config) =>
  invoke("save_config", { payload: JSON.stringify(config) });

/** @returns {Promise<Record<string, Record<string, string>>>} */
export const getI18n = () => invoke("get_i18n");

/** @returns {Promise<{microphones: Device[], system: Device[]}>} */
export const listDevices = () => invoke("list_devices");
export const refreshDevices = () => invoke("refresh_devices");

/** @returns {Promise<{running: boolean, models: string[], error: string|null}>} */
export const listOllamaModels = () => invoke("list_ollama_models");

/** @returns {Promise<{id: string, approx_mb: number, installed: boolean}[]>} */
export const listWhisperModels = () => invoke("list_whisper_models");

/** @param {string} model */
export const downloadWhisperModel = (model) =>
  invoke("download_whisper_model", { model });

export const startRecording = () => invoke("start_recording");

/**
 * Stop capturing immediately. Does NOT process the meeting — call
 * `finalizeMeeting` for that. Split so the title prompt cannot keep the
 * recording running while it is open.
 */
export const stopRecording = () => invoke("stop_recording");

/** Process the meeting that stopRecording just finished. @param {string} meetingTitle */
export const finalizeMeeting = (meetingTitle) =>
  invoke("finalize_meeting", { meetingTitle });

/** Discard the meeting: stops recording and DELETES the folder. */
export const cancelRecording = () => invoke("cancel_recording");

/** @returns {Promise<boolean>} the new muted state */
export const toggleMute = () => invoke("toggle_mute");

/** @returns {Promise<boolean>} */
export const isMuted = () => invoke("is_muted");

/**
 * Store the summary API key in the OS keychain. Empty string clears it.
 * Never goes through saveConfig — the key must not reach config.json.
 * @param {string} key
 */
export const setApiKey = (key) => invoke("set_api_key", { key });

/** @returns {Promise<boolean>} whether a key is stored (never the key itself) */
export const hasApiKey = () => invoke("has_api_key");

/** @returns {Promise<number>} */
export const elapsedSeconds = () => invoke("elapsed_seconds");

/** @param {string} path */
export const openPath = (path) => invoke("open_path", { path });

export const startupCheck = () => invoke("startup_check");

/** Settings is its own 590x610 window, so closing it closes settings, not the app. */
export const openSettings = () => invoke("open_settings");
export const closeSettings = () => invoke("close_settings");

/**
 * Resize the main window to `height`. Done in Rust: the capabilities file does
 * not grant the JS window-resize permission, so a `setSize` from here would be
 * rejected by the ACL.
 * @param {number} height
 */
export const setMainHeight = (height) => invoke("set_main_height", { height });

/** @returns {Promise<boolean>} whether the first-run wizard should be shown */
export const needsSetup = () => invoke("needs_setup");
export const openSetup = () => invoke("open_setup");
export const closeSetup = () => invoke("close_setup");

/** Start the local Ollama app/daemon if it can be found. */
export const startOllama = () => invoke("start_ollama");

/** Open an external URL in the default browser. */
export const openUrl = (url) => opener.openUrl(url);

/** Native folder picker, replacing the Python's `filedialog`. */
export const browseFolder = () => open({ directory: true, multiple: false });

/**
 * The eleven events, one-to-one with the Python's queue tags so behaviour can
 * be diffed against the original.
 */
export const EVENTS = {
  startupStatus: "startup_status",
  startupResult: "startup_result",
  status: "status",
  deviceMic: "device_mic",
  micFallback: "mic_fallback",
  deviceSystem: "device_system",
  systemFallback: "system_fallback",
  stage: "stage",
  log: "log",
  error: "error",
  complete: "complete",
  /** Structured {model, percent}; each window localizes it itself. */
  whisperProgress: "whisper_progress",
  /** {recording, processing} — emitted by the commands, not by any UI. */
  recordingState: "recording_state",
  /** boolean */
  muteState: "mute_state",
  /** The tray asking this window to run a flow that needs user input. */
  requestStop: "request_stop",
  requestCancel: "request_cancel",
};

/**
 * @param {string} name one of EVENTS
 * @param {(payload: any) => void} handler
 */
export const on = (name, handler) => listen(name, (event) => handler(event.payload));
