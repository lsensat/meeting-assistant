//! The IPC surface: 14 commands and 11 events.
//!
//! Events are deliberately one-to-one with the Python's queue tags
//! (`app.py`'s `messages.put((tag, payload))`) so the port can be diffed
//! against current behaviour rather than guessed at.
//!
//! `queue.Queue` plus `root.after(100, poll_messages)` becomes `app.emit()`;
//! the flow is still strictly one-way, worker → UI.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use meeting_core::config::{Config, SummaryProvider};
use meeting_core::{i18n, policy, text};

use crate::audio::devices::{self, SourceKind};
use crate::audio::recorder::Event as RecorderEvent;
use crate::queue;
use crate::pipeline::{self, PipelineConfig, Progress, Stage, StageState};
use crate::session::RecordingSession;
use crate::state::AppState;
use crate::{ollama, platform, whisper};

// --- event names -------------------------------------------------------
//
// One constant per tag so a typo is a compile error rather than an event that
// silently never arrives. This is the failure class the plan calls out as
// worth type-checking across the IPC boundary.
pub const EV_STARTUP_STATUS: &str = "startup_status";
pub const EV_STARTUP_RESULT: &str = "startup_result";
pub const EV_STATUS: &str = "status";
pub const EV_DEVICE_MIC: &str = "device_mic";
pub const EV_MIC_FALLBACK: &str = "mic_fallback";
pub const EV_DEVICE_SYSTEM: &str = "device_system";
pub const EV_SYSTEM_FALLBACK: &str = "system_fallback";
pub const EV_LOG: &str = "log";
pub const EV_ERROR: &str = "error";
pub const EV_COMPLETE: &str = "complete";
/// Model-download progress. Structured rather than a formatted string, so each
/// window localizes it with its own catalogue — the old version emitted a
/// pre-formatted English sentence on the global status channel, which appeared
/// in English regardless of language and landed in the main window's status
/// line even when the download was started from another window.
pub const EV_WHISPER_PROGRESS: &str = "whisper_progress";
/// Recording started or stopped, whoever caused it.
///
/// The window used to track this purely inside its own click handlers, so a
/// recording started from anywhere else left it showing idle with the timer at
/// zero and Start still enabled. Any front-end that can change the state must
/// announce it here, and every front-end reacts to it rather than to its own
/// clicks.
/// The queue changed: a job was added, finished, failed or was removed, or the
/// pause switch moved. Carries no payload — the frontend asks for a snapshot.
pub const EV_QUEUE_CHANGED: &str = "queue_changed";
/// Settings were saved. Every window re-reads the config and re-applies
/// anything derived from it — the language above all.
///
/// The main window used to learn this only from its own `focus` handler, so
/// changing the language with Settings still open left it in the old one: Rust
/// emitted the new language on the status line while every label the frontend
/// draws stayed as it was. `save_config` already rebuilds the tray for exactly
/// this reason — the tray cannot re-read either. The other windows were simply
/// left out.
pub const EV_CONFIG_CHANGED: &str = "config_changed";
/// The recording that just stopped is missing audio.
///
/// Emitted the moment capture stops, before any model runs — not from the
/// pipeline. The failure this exists for produced an empty transcript, which is
/// the pipeline's *error* path, and that emits no completion payload at all.
pub const EV_CAPTURE_DAMAGE: &str = "capture_damage";

pub const EV_RECORDING_STATE: &str = "recording_state";
/// Mute toggled, whoever caused it. Same reasoning as above.
pub const EV_MUTE_STATE: &str = "mute_state";

#[derive(Serialize, Clone)]
pub struct RecordingStateDto {
    pub recording: bool,
    pub processing: bool,
}

#[derive(Serialize, Clone)]
pub struct WhisperProgressDto {
    pub model: String,
    pub percent: u8,
}

#[derive(Serialize, Clone)]
pub struct DeviceDto {
    pub id: String,
    /// The raw OS name. This is the **identity**: it is what `Config` stores and
    /// what the recorder resolves against. Never show it where `label` fits.
    pub name: String,
    /// The same device, named for a person. Display only — see
    /// `meeting_core::devices`.
    pub label: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub is_default: bool,
}

#[derive(Serialize, Clone)]
pub struct DeviceListDto {
    pub microphones: Vec<DeviceDto>,
    pub system: Vec<DeviceDto>,
}

#[derive(Serialize, Clone)]
pub struct WhisperModelDto {
    pub id: String,
    pub approx_mb: u64,
    pub installed: bool,
    /// Real bytes on disk, 0 when not installed. `approx_mb` is what a model
    /// *will* cost before you fetch it; this is what deleting it would free.
    pub size_bytes: u64,
}

#[derive(Serialize, Clone)]
pub struct OllamaStatusDto {
    /// The server answered at all.
    pub running: bool,
    pub models: Vec<String>,
    pub error: Option<String>,
}

#[derive(Serialize, Clone)]
pub struct CaptureDamageDto {
    /// One line for the status bar.
    pub message: String,
    /// The whole story, for the hover text.
    pub detail: String,
}

#[derive(Serialize, Clone)]
pub struct CompleteDto {
    /// The microphone track was very quiet. The UI says so, because "the
    /// transcript is wrong" and "your input level is low" look identical from
    /// the outside and only one of them is actionable.
    pub quiet_recording: bool,
    pub folder: String,
    pub transcript_file: String,
    pub summary_file: String,
}

#[derive(Serialize, Clone)]
pub struct StartupResultDto {
    /// Which provider is configured, so the frontend knows whether the Ollama
    /// fields below mean anything.
    pub summary_provider: String,
    /// True when the configured summary path looks usable: Ollama running with
    /// a model, or a remote endpoint with a base URL, model and stored key.
    pub summary_ready: bool,
    pub ollama: OllamaStatusDto,
    /// Whether an `ollama` executable was found on this machine at all.
    /// `ollama.running` says whether it answered; this says whether there is
    /// anything to start. The difference decides whether the main window offers
    /// "Start Ollama" or "Get Ollama" — offering to start something absent is a
    /// dead end.
    pub ollama_installed: bool,
    pub whisper_installed: Vec<String>,
    pub folder_ok: bool,
    pub has_microphone: bool,
    pub has_system_audio: bool,
}

// --- config ------------------------------------------------------------

/// Current settings as JSON.
///
/// Serialized through `Config::to_json` rather than a `Serialize` derive so
/// there is exactly one definition of the on-disk shape, already covered by
/// `meeting-core`'s tests.
#[tauri::command]
pub fn get_config(state: State<AppState>) -> Result<serde_json::Value, String> {
    let json = state.config_snapshot().to_json();
    serde_json::from_str(&json).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn save_config(app: AppHandle, payload: String, state: State<AppState>) -> Result<(), String> {
    // The base for `from_json`'s defaults must be the documents directory, the
    // same one `main.rs` passes when loading. Passing `app_data_dir()` here
    // meant a payload with a missing or empty `output_folder` silently resolved
    // to `~/Library/Application Support/.../meeting-assistant/meetings` instead
    // of `~/Documents/meeting-assistant/meetings` — a different default on save
    // than on load.
    let defaults_base = platform::documents_dir();
    let parsed = Config::from_json(&payload, &defaults_base);

    // The config file itself still lives under Application Support; only the
    // *default* it falls back to comes from documents.
    let app_folder = platform::app_data_dir();
    std::fs::create_dir_all(&app_folder).map_err(|e| e.to_string())?;
    std::fs::write(&state.config_file, parsed.to_json()).map_err(|e| e.to_string())?;

    // Replace wholesale rather than mutating field by field. The Python did
    // `config.clear()` then `update()` while worker threads read the same dict
    // (deferred fix #2); here the lock makes the swap atomic.
    *state.config.lock().expect("config poisoned") = parsed;

    // The tray menu bakes in the language and the selected devices, and unlike
    // the DOM it has no way to re-read them. Nothing else tells Rust that the
    // language changed.
    crate::tray::rebuild(&app);
    let _ = app.emit(EV_CONFIG_CHANGED, ());
    Ok(())
}

/// The whole i18n catalogue, so the frontend can localize without a round trip
/// per string. Shared source of truth with the Rust side.
#[tauri::command]
pub fn ui_log(message: String) {
    eprintln!("[ui] {message}");
}

/// The running version, for the Settings window.
///
/// The builds are unsigned and installed by hand, so "which version am I on"
/// is a question the app is the only reliable answer to — a user comparing a
/// bug against a release cannot otherwise tell.
#[tauri::command]
pub fn app_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The third-party notices, for the Acknowledgements section in Settings.
///
/// `include_str!` rather than a bundled resource so the shipped text and the
/// file in the repository cannot drift apart: there is one copy, and it is the
/// one CI reads. The obligation is to deliver the notice **with the binary**,
/// and a file sitting in a git repository does not do that.
#[tauri::command]
pub fn third_party_licenses() -> &'static str {
    include_str!("../../THIRD-PARTY-LICENSES.md")
}

#[tauri::command]
pub fn get_i18n() -> Result<serde_json::Value, String> {
    serde_json::from_str(i18n::catalog_json()).map_err(|e| e.to_string())
}

// --- devices -----------------------------------------------------------

fn snapshot_dto(kind: SourceKind) -> Vec<DeviceDto> {
    let Ok(snapshot) = devices::snapshot(kind) else {
        return Vec::new();
    };
    let default_id = snapshot.default_id().map(|s| s.to_string());

    // Computed over the whole list, because shortening two devices on one
    // adapter can collide and `display_labels` resolves that by keeping the
    // full name for the entries that clash.
    let names: Vec<String> = snapshot.devices.iter().map(|d| d.name.clone()).collect();
    let labels = meeting_core::devices::display_labels(&names);

    snapshot
        .devices
        .into_iter()
        .zip(labels)
        .map(|(d, label)| DeviceDto {
            is_default: Some(&d.id) == default_id.as_ref(),
            id: d.id,
            name: d.name,
            label,
            sample_rate: d.sample_rate,
            channels: d.channels,
        })
        .collect()
}

/// # Why every command in this file that does I/O is `command(async)`
///
/// A bare `#[tauri::command]` on a synchronous function runs **on the main
/// thread** — `tauri-macros`' `ExecutionContext` defaults to `Blocking`. Any
/// wait inside one therefore freezes every window in the app, not just the
/// caller. The `(async)` attribute moves the same synchronous body to the
/// blocking threadpool without changing its signature.
///
/// This was not a precaution. With these bare, the Windows build opened the
/// settings and setup windows as blank white rectangles marked "not
/// responding", and the main window sat on "Checking environment…" forever:
/// `list_ollama_models` was holding the main thread for its full HTTP timeout
/// on a machine with no Ollama installed. It looked like three separate bugs.
///
/// Device enumeration is here too — it goes through COM on Windows and is not
/// reliably fast.
#[tauri::command(async)]
pub fn list_devices() -> DeviceListDto {
    DeviceListDto {
        microphones: snapshot_dto(SourceKind::Microphone),
        system: snapshot_dto(SourceKind::SystemAudio),
    }
}

/// Same as [`list_devices`]; a separate command because the UI treats an
/// explicit refresh differently from the initial load.
#[tauri::command(async)]
pub fn refresh_devices() -> DeviceListDto {
    list_devices()
}

// --- models ------------------------------------------------------------

/// Probe Ollama for its installed models.
///
/// `(async)` because this makes a blocking HTTP request. Held on the main
/// thread it froze the whole UI for the length of the timeout on any machine
/// where Ollama is not running — which is every machine that has not installed
/// it yet, i.e. exactly the ones opening Settings in order to configure it.
#[tauri::command(async)]
pub fn list_ollama_models() -> OllamaStatusDto {
    match ollama::list_models() {
        Ok(models) => OllamaStatusDto {
            running: true,
            models,
            error: None,
        },
        Err(e) => OllamaStatusDto {
            running: false,
            models: Vec::new(),
            error: Some(e.to_string()),
        },
    }
}

/// Delete an installed Whisper model.
///
/// Three guards, none of which the UI is trusted to enforce on its own — the
/// settings window disables the control in each of these cases, but a command
/// that destroys gigabytes must not depend on that:
///
/// 1. **Not during a meeting or its processing.** whisper.cpp memory-maps the
///    model file; removing it mid-transcription risks a crash rather than a
///    clean error.
/// 2. **Not during a download**, which may be writing this very model.
/// 3. **Not the selected model.** Deleting what the next meeting is about to
///    load turns a space-saving action into a silent 3 GB re-download. Choosing
///    a different model first is one click and makes the intent explicit.
#[tauri::command(async)]
pub fn delete_whisper_model(id: String, state: State<AppState>) -> Result<(), String> {
    // `is_processing` covers QUEUED meetings, not just the running one: a
    // meeting waiting its turn still needs this model to exist when the worker
    // reaches it, and by then the user is nowhere near this dialog.
    if state.is_recording() || state.is_processing() {
        return Err("A meeting is in progress.".into());
    }

    if state.downloading.load(std::sync::atomic::Ordering::SeqCst) {
        return Err("A model download is in progress.".into());
    }

    if state.config_snapshot().whisper_model == id {
        return Err("The selected model cannot be deleted. Choose another model first.".into());
    }

    whisper::delete_model(&id).map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub fn list_whisper_models() -> Vec<WhisperModelDto> {
    whisper::MODELS
        .iter()
        .map(|spec| WhisperModelDto {
            id: spec.id.to_string(),
            approx_mb: spec.approx_mb,
            installed: whisper::is_installed(spec.id),
            size_bytes: whisper::installed_size(spec.id),
        })
        .collect()
}

/// Download a Whisper model, reporting progress on [`EV_WHISPER_PROGRESS`].
///
/// Guarded against re-entry: the setup wizard and the settings window can both
/// reach this, and two concurrent downloads of the same model would race on the
/// same temporary file.
#[tauri::command]
pub async fn download_whisper_model(
    app: AppHandle,
    model: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    if state
        .downloading
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("a model download is already running".into());
    }

    // Blocking IO must not run on the async runtime's thread, or the whole UI
    // stops responding for the length of a multi-gigabyte download.
    let downloading = Arc::clone(&state.downloading);
    let result = tauri::async_runtime::spawn_blocking(move || {
        let outcome = whisper::download_model(&model, |percent| {
            let _ = app.emit(
                EV_WHISPER_PROGRESS,
                WhisperProgressDto {
                    model: model.clone(),
                    percent,
                },
            );
            // A download the user started deliberately from Settings or the
            // wizard is not interrupted by a recording: they asked for it, and
            // abandoning it wastes what has already arrived.
            true
        })
        .map(|_| ())
        .map_err(|e| e.to_string());

        // Released here rather than on the caller's side so an early return or
        // a panic in the download cannot leave the flag stuck at true, which
        // would block every later download until restart.
        downloading.store(false, Ordering::SeqCst);
        outcome
    })
    .await
    .map_err(|e| e.to_string())?;

    result
}

/// Whether the first-run wizard should be shown.
///
/// First run is the absence of the config *file*; the `setup_completed` flag
/// additionally reopens a wizard that was abandoned half-way.
/// Start the local Ollama app or daemon, if one can be found.
///
/// Used by the setup wizard so the user does not have to leave the app to get
/// the local engine running.
#[tauri::command(async)]
pub fn start_ollama() -> Result<(), String> {
    platform::start_ollama().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn needs_setup(state: State<AppState>) -> bool {
    !state.config_file.exists() || !state.config_snapshot().setup_completed
}

/// Open, or focus, the first-run wizard.
///
/// Its own window: the main view is 375x275 and Settings is 590x610, and a
/// 5-step wizard fits neither. See `open_settings` for why reusing or resizing
/// an existing window was rejected.
/// Open the inspector for a window when `MA_DEBUG=1`.
///
/// The Windows build is only ever run as a release artifact from CI, so a
/// webview that fails to load has no console anyone can reach. This is the
/// hatch. It is a no-op unless the variable is set, so it costs nothing in
/// normal use.
fn debug_inspect(window: &tauri::WebviewWindow) {
    if std::env::var("MA_DEBUG").as_deref() == Ok("1") {
        window.open_devtools();
    }
}

/// # `(async)` is load-bearing, not a style choice
///
/// `WebviewWindowBuilder::new` carries this warning in Tauri's own source
/// (`tauri-2.11.5/src/webview/webview_window.rs:58`):
///
/// > On Windows, this function deadlocks when used in a synchronous command
/// > and event handlers.
///
/// A bare `#[tauri::command]` on a synchronous function *is* a synchronous
/// command — it runs on the main thread. So opening the wizard on first run
/// deadlocked the app's own main thread, and every symptom that followed was
/// downstream of it: the wizard and Settings painted white because WebView2
/// never finished initialising and so never navigated; `app.emit` dispatches to
/// the main thread, so `startup_check`'s status events were never delivered and
/// the status line kept its static placeholder; and `set_main_height` never ran,
/// so expanding the device panel clipped the toolbar instead of growing the
/// window. Three rounds of fixes to three "separate bugs" achieved nothing.
///
/// Do not remove the `(async)`.
#[tauri::command(async)]
pub fn open_setup(app: AppHandle) -> Result<(), String> {
    if let Some(existing) = app.get_webview_window("setup") {
        existing.show().map_err(|e| e.to_string())?;
        existing.set_focus().map_err(|e| e.to_string())?;
        return Ok(());
    }

    let window =
        tauri::WebviewWindowBuilder::new(&app, "setup", tauri::WebviewUrl::App("setup.html".into()))
            .title("Welcome to Meeting Assistant")
            .inner_size(620.0, 560.0)
            .resizable(false)
            .maximizable(false)
            .center()
            .build()
            .map_err(|e| e.to_string())?;

    debug_inspect(&window);
    Ok(())
}

/// What each window's webview currently has loaded.
///
/// Diagnostic. The Windows wizard opened as a plain white rectangle, and from a
/// screenshot that is indistinguishable between three very different faults: the
/// document not loading at all, the stylesheet being refused, or the module
/// graph throwing. `html, body` carries a dark background, so white means no CSS
/// applied — but only the URL says whether the webview ever navigated.
///
/// There is no console on a release Windows build, so the app has to be able to
/// answer this itself.
///
/// `(async)` because the first version of this was a synchronous command, and a
/// synchronous command runs on the main thread — the very thing it was written
/// to diagnose. It reported nothing at all, because it hung on the same
/// deadlock. **A diagnostic that depends on the thing being diagnosed cannot
/// report.**
/// Wait for a freshly launched Ollama to become answerable.
///
/// Replaces a flat `sleep(3)`. Ollama binds its port almost immediately but
/// cannot serve `/api/tags` until it has finished discovering GPUs. On the
/// machine that reported this, the log shows the port bound at `12:07:28.108`
/// and the first request served at `12:07:31` — the three-second probe landed
/// exactly on the boundary, and losing that race made the app declare Ollama
/// dead for the rest of the session with no way back except a restart.
///
/// Polling also makes the common case faster rather than slower: a warm Ollama
/// answers on the first attempt instead of always costing three seconds.
fn wait_for_ollama() -> OllamaStatusDto {
    const BUDGET: std::time::Duration = std::time::Duration::from_secs(20);
    const INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

    let deadline = std::time::Instant::now() + BUDGET;
    loop {
        let status = list_ollama_models();
        if status.running || std::time::Instant::now() >= deadline {
            return status;
        }
        std::thread::sleep(INTERVAL);
    }
}

#[tauri::command(async)]
pub fn window_urls(app: AppHandle) -> Vec<(String, String)> {
    app.webview_windows()
        .iter()
        .map(|(label, window)| {
            let url = window
                .url()
                .map(|u| u.to_string())
                .unwrap_or_else(|e| format!("<error: {e}>"));
            (label.clone(), url)
        })
        .collect()
}

#[tauri::command]
pub fn close_setup(app: AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("setup") {
        window.close().map_err(|e| e.to_string())?;
    }
    Ok(())
}
/// The main window's fixed logical width.
const MAIN_WIDTH: f64 = 375.0;

/// The logical height the main window is currently pinned to, as `f64` bits.
///
/// Zero means "never pinned". Needed because the pin has to be re-applied from
/// outside this command — see `reapply_main_size_pin`.
static PINNED_HEIGHT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Re-apply the size pin after the display's scale factor changes.
///
/// `set_min_size`/`set_max_size` take a *logical* size, which the runtime
/// resolves to physical pixels against the scale factor in force **at the time
/// of the call**. Those physical numbers are what the window manager then
/// enforces, and nothing recomputes them when the scale changes.
///
/// So a window pinned to 375x430 on a 100% display carries a 375x430 *physical*
/// clamp onto a 150% display, where the same window must be 562x645 physical to
/// look the same size. The clamp is now smaller than the window, and Windows
/// enforces it through `WM_GETMINMAXINFO` — the window is squeezed, and the
/// layout inside it has nowhere to go.
///
/// Re-stating the same logical size at the new scale produces the right
/// physical numbers. The size itself does not change; only the constraint does.
pub fn reapply_main_size_pin(window: &tauri::Window) {
    let bits = PINNED_HEIGHT.load(std::sync::atomic::Ordering::Relaxed);
    if bits == 0 {
        return;
    }
    let height = f64::from_bits(bits);

    // Released first, for the same reason as in `nudge_main_height`: the old
    // pin is enforced against the new one.
    let unpinned: Option<tauri::LogicalSize<f64>> = None;
    let _ = window.set_min_size(unpinned);
    let _ = window.set_max_size(unpinned);

    let size = tauri::LogicalSize::new(MAIN_WIDTH, height);
    let _ = window.set_size(size);
    let _ = window.set_min_size(Some(size));
    let _ = window.set_max_size(Some(size));
}


/// Grow or shrink the main window by `delta` logical pixels.
///
/// # A delta, not a height
///
/// This took a height, and the frontend worked one out by measuring the title
/// bar: ask for H, see what `innerHeight` became, call the difference the
/// chrome. Every part of that is unreliable. Tauri reports `outer_size` and
/// `inner_size` as identical on macOS, so the difference is not the title bar;
/// and the size read straight after `set_size` can still be the OLD one,
/// because AppKit applies the change on the next pass of the run loop. Built on
/// those numbers the arithmetic drifted a pixel per pass — measured, a window
/// walking 242, 241, 240, 239 — until the toolbar was pushed out of view.
///
/// A delta needs none of it. The frontend knows only how much more or less room
/// its content needs than it has, which it can measure exactly; this adds that
/// to whatever the window currently is. Neither side needs to know what a title
/// bar costs, and there is no round-trip figure to be stale.
#[tauri::command]
pub fn nudge_main_height(app: AppHandle, delta: f64) -> Result<(), String> {
    const MIN: f64 = 200.0;
    // Raised from 420 for the processing queue, which adds a panel of up to
    // three cards. The queue list scrolls past that, so this is a ceiling on
    // the window rather than on how many meetings can be waiting.
    const MAX: f64 = 640.0;

    let window = app
        .get_webview_window("main")
        .ok_or("main window is missing")?;

    // INNER, not outer.
    //
    // `set_size` below sets the **inner** size — `WindowMessage::SetSize` maps to
    // `set_inner_size` in tauri-runtime-wry — while `outer_size` includes the
    // title bar and borders. Reading one and writing the other adds their
    // difference to the window on every pass, so it can never converge: asking
    // for `(inner + chrome) + delta` and getting it as an inner size leaves the
    // window exactly `chrome` pixels too tall, forever, and `main { height:
    // 100vh }` hands the surplus to the spacer as blank space above the toolbar.
    //
    // It never showed on macOS because Tauri reports outer and inner as equal
    // there — measured, both 242 — so the difference was zero. On Windows it is
    // the title bar, and the resize gave up after four passes with the gap still
    // there.
    let scale = window.scale_factor().map_err(|e| e.to_string())?;
    let current = window
        .inner_size()
        .map_err(|e| e.to_string())?
        .to_logical::<f64>(scale)
        .height;

    let target = (current + delta).clamp(MIN, MAX);

    // Release the previous pin BEFORE resizing.
    //
    // The min and max set by the LAST call are still in force, and a window
    // manager enforces them against `set_size` — on Windows strictly, through
    // WM_GETMINMAXINFO. A window pinned to 275 could therefore never be made
    // 340: the request was silently clamped back and the extra content was
    // simply cut off at the bottom. That is the clipped toolbar, and it only
    // appeared once something made the layout taller than it was at the first
    // resize.
    let unpinned: Option<tauri::LogicalSize<f64>> = None;
    let _ = window.set_min_size(unpinned);
    let _ = window.set_max_size(unpinned);

    window
        .set_size(tauri::LogicalSize::new(MAIN_WIDTH, target))
        .map_err(|e| e.to_string())?;

    // Pinned again so the window cannot be dragged to a size the fixed layout
    // has no answer for. `resizable` must stay true in tauri.conf.json: with it
    // false, programmatic resizing is unreliable on macOS.
    PINNED_HEIGHT.store(target.to_bits(), std::sync::atomic::Ordering::Relaxed);
    let fixed = Some(tauri::LogicalSize::new(MAIN_WIDTH, target));
    let _ = window.set_min_size(fixed);
    let _ = window.set_max_size(fixed);

    if std::env::var("MA_DEBUG").as_deref() == Ok("1") {
        eprintln!("[resize] {current:.0} {delta:+.0} -> {target:.0}");
    }

    Ok(())
}

/// Open, or focus, the settings window.
///
/// # Why a second window and not a navigation
///
/// Settings is its own 590x610 window while the main view is 375x275. Two
/// earlier approaches were both wrong:
///
/// * `location.href` into the same window loaded the 590px-wide form into the
///   375px window and produced a horizontal scrollbar;
/// * resizing that one window fixed the scrollbar but meant closing the window
///   while settings was showing **quit the app**, with no way back to the main
///   view short of relaunching.
///
/// A separate window makes closing it mean "close settings", which is what the
/// close button on a settings window should do.
/// `(async)` for the same reason as `open_setup` — see the note there.
#[tauri::command(async)]
pub fn open_settings(app: AppHandle) -> Result<(), String> {
    if let Some(existing) = app.get_webview_window("settings") {
        existing.show().map_err(|e| e.to_string())?;
        existing.set_focus().map_err(|e| e.to_string())?;
        return Ok(());
    }

    let window = tauri::WebviewWindowBuilder::new(
        &app,
        "settings",
        tauri::WebviewUrl::App("settings.html".into()),
    )
    .title("Settings")
    .inner_size(590.0, 610.0)
    .resizable(false)
    .maximizable(false)
    .build()
    .map_err(|e| e.to_string())?;

    debug_inspect(&window);
    Ok(())
}

/// Open the open-source notices in their own window.
///
/// Its own window rather than a disclosure inside Settings: expanded inline it
/// pushed everything below it down the page, and the notices are reference
/// material — something you open, read and close — not a setting to be scanned
/// past on the way to something else.
///
/// `(async)` for the same reason as `open_settings`: building a webview window
/// from a synchronous command deadlocks on Windows.
#[tauri::command(async)]
pub fn open_licenses(app: AppHandle) -> Result<(), String> {
    if let Some(existing) = app.get_webview_window("licenses") {
        existing.show().map_err(|e| e.to_string())?;
        existing.set_focus().map_err(|e| e.to_string())?;
        return Ok(());
    }

    let window = tauri::WebviewWindowBuilder::new(
        &app,
        "licenses",
        tauri::WebviewUrl::App("licenses.html".into()),
    )
    .title("Acknowledgements")
    .inner_size(640.0, 620.0)
    .build()
    .map_err(|e| e.to_string())?;

    debug_inspect(&window);
    Ok(())
}

/// Close the settings window from inside it, after a save.
#[tauri::command]
pub fn close_settings(app: AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("settings") {
        window.close().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Store (or clear, when empty) the summary API key in the OS keychain.
///
/// Deliberately a separate command from `save_config`: the key must never
/// travel through the config payload, or it ends up written to `config.json`
/// alongside the meeting recordings.
#[tauri::command(async)]
pub fn set_api_key(key: String) -> Result<(), String> {
    crate::summary::store_api_key(&key).map_err(|e| e.to_string())
}

/// Whether a key is stored. Never returns the key itself — the settings UI
/// shows "saved", not the value.
#[tauri::command(async)]
pub fn has_api_key() -> bool {
    crate::summary::has_api_key()
}

// --- files -------------------------------------------------------------

/// Reveal a file or folder in the OS file manager.
///
/// # Why this goes through the opener plugin rather than a shell command
///
/// This used to run `cmd /C start "" <path>` on Windows. `cmd.exe` re-parses
/// its command line and honours `& ^ % ( ) !` as metacharacters, while Rust's
/// `Command` quotes arguments to the MSVCRT convention that `cmd.exe` does not
/// follow — the same class of hole as CVE-2024-24576.
///
/// That was reachable from the meeting title: `sanitize_name` strips
/// `<>:"/\|?*` but not `&`, so a meeting called `standup & calc` produced a
/// folder of that name, and pressing "Open folder" would have run `calc`.
///
/// `tauri-plugin-opener` uses the OS APIs directly, with no shell in the path.
#[tauri::command(async)]
pub fn open_path(app: AppHandle, path: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;

    let path = std::path::Path::new(&path);
    if !path.exists() {
        return Err(format!("{} does not exist", path.display()));
    }

    app.opener()
        .open_path(path.to_string_lossy(), None::<&str>)
        .map_err(|e| e.to_string())
}

/// Open the OS sound settings, where the input and output levels live.
///
/// # Why a command and not `openUrl`
///
/// The frontend already has `opener.openUrl`, but the ACL scopes it to
/// `https://ollama.com/*` on purpose: unscoped, any string the frontend could
/// produce would become a URL the OS opens. Widening that to admit
/// `ms-settings:` would give away considerably more than one button needs —
/// the scheme reaches every page in Windows Settings.
///
/// This takes no arguments. The URI is a compile-time constant chosen by
/// `cfg`, so there is no string for the frontend to influence at all.
#[tauri::command(async)]
pub fn open_sound_settings(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;

    let uri = crate::platform::sound_settings_uri();
    if uri.is_empty() {
        return Err("no sound settings on this platform".into());
    }

    app.opener()
        .open_url(uri, None::<&str>)
        .map_err(|e| e.to_string())
}

// --- startup -----------------------------------------------------------

/// The startup probe. Port of the checks around `app.py:894-911`.
///
/// Runs off the UI thread because reaching Ollama can block for seconds when it
/// is cold — the Python's version blocked in a thread that was never joined or
/// cancelled on quit (deferred fix #10).
#[tauri::command]
pub async fn startup_check(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let config = state.config_snapshot();
    let language = config.language;

    tauri::async_runtime::spawn_blocking(move || {
        let emit_status = |key: &str| {
            let _ = app.emit(EV_STARTUP_STATUS, i18n::tr(language, key));
        };
        emit_status("checking_environment");

        emit_status("checking_folder");
        let folder_ok = std::fs::create_dir_all(&config.output_folder).is_ok();

        // 885 KB, once, so the first meeting does not stop to fetch it. Its
        // result is ignored on purpose: without it transcription falls back to
        // the whole track, which is slower but correct, and a machine that is
        // offline at launch must still be able to record.
        crate::vad::prefetch();

        // Probe Ollama only when it is the configured provider. Launching a
        // local server for someone who chose a remote endpoint is both
        // surprising and slow, and its "not running" state would be reported
        // as a startup error they cannot act on.
        let uses_ollama = config.summary_provider == SummaryProvider::Ollama;
        // Short-circuited: a user on a remote endpoint should not pay for a
        // filesystem search for a binary they have no use for.
        let ollama_installed = uses_ollama && platform::find_ollama().is_some();

        let ollama_status = if uses_ollama {
        emit_status("checking_ollama");
            let status = list_ollama_models();

            // The Python auto-started Ollama when it was installed but not
            // running, then told the user to reopen the app if it had to
            // (`app.py:200`). Reopening is no longer the remedy: this waits for
            // it properly, and the main window offers a retry if it still fails.
            if !status.running && ollama_installed && platform::start_ollama().is_ok() {
                wait_for_ollama()
            } else {
                status
            }
        } else {
            OllamaStatusDto {
                running: false,
                models: Vec::new(),
                error: None,
            }
        };
        emit_status("searching_models");
        let whisper_installed = whisper::installed_models();

        let mics = devices::snapshot(SourceKind::Microphone)
            .map(|s| s.devices)
            .unwrap_or_default();
        let system = devices::snapshot(SourceKind::SystemAudio)
            .map(|s| s.devices)
            .unwrap_or_default();

        let _ = app.emit(
            EV_STARTUP_RESULT,
            StartupResultDto {
                summary_provider: config.summary_provider.as_str().to_string(),
                summary_ready: if uses_ollama {
                    ollama_status.running && !ollama_status.models.is_empty()
                } else {
                    !config.api_base_url.is_empty()
                        && !config.api_model.is_empty()
                        && crate::summary::has_api_key()
                },
                ollama: ollama_status,
                ollama_installed,
                whisper_installed,
                folder_ok,
                has_microphone: !mics.is_empty(),
                has_system_audio: !system.is_empty(),
            },
        );
    })
    .await
    .map_err(|e| e.to_string())
}

// --- recording ---------------------------------------------------------

#[tauri::command]
pub fn start_recording(app: AppHandle, state: State<AppState>) -> Result<(), String> {
    if state.is_recording() {
        return Err("already recording".into());
    }
    let config = state.config_snapshot();

    // One folder per meeting, named for when it started. The user's optional
    // title is appended later, after the WAVs close — see `pipeline::run`.
    let folder = config.output_folder.join(timestamp_folder_name());

    let session = RecordingSession::start(
        &folder,
        &config.microphone_name,
        &config.system_audio_name,
        Arc::clone(&state.muted),
        // Recording outranks processing. The session holds the queue for as
        // long as it lives; see `queue::RecordingHold`.
        Arc::clone(&state.queue),
    )
    .map_err(|e| e.to_string())?;

    // The queue is now held, so its cards stop moving. `startQueuePolling`
    // re-renders only while something is running, so without this the view
    // freezes mid-meeting and every card reads "Waiting" with no explanation.
    emit_queue_changed(&app);

    // Forward recorder events to the UI. The receiver is cloned out of the
    // session so this thread does not hold the state lock.
    // Everything the recorder reports, written beside the audio it reports on.
    //
    // This is the only place that sees every event, so it is where the meeting's
    // diagnostics file is written. Until now these went to `Event::Log`, which
    // the frontend forwards to `console.log` — a console a release build cannot
    // open — so a meeting that recorded nothing but silence could not be
    // explained at all, on the machine where it happened or anywhere else.
    let log = std::sync::Arc::new(crate::diagnostics::MeetingLog::create(&folder));
    log.line(&format!(
        "configured microphone: {:?}",
        config.microphone_name
    ));
    log.line(&format!(
        "configured system audio: {:?}",
        config.system_audio_name
    ));

    let events = session.events.clone();
    let forwarder = app.clone();
    let event_log = std::sync::Arc::clone(&log);
    std::thread::spawn(move || {
        while let Ok(event) = events.recv() {
            let (name, payload) = match event {
                RecorderEvent::Device(SourceKind::Microphone, name) => (EV_DEVICE_MIC, name),
                RecorderEvent::Fallback(SourceKind::Microphone, name) => (EV_MIC_FALLBACK, name),
                RecorderEvent::Device(SourceKind::SystemAudio, name) => (EV_DEVICE_SYSTEM, name),
                RecorderEvent::Fallback(SourceKind::SystemAudio, name) => {
                    (EV_SYSTEM_FALLBACK, name)
                }
                RecorderEvent::Log(message) => (EV_LOG, message),
                RecorderEvent::Error(message) => (EV_ERROR, message),
            };
            event_log.line(&format!("{name}: {payload}"));
            let _ = forwarder.emit(name, payload);
        }
        event_log.line("recorder events ended");
    });

    // Held so `stop_recording` can write the outcome into the same file.
    *state.meeting_log.lock().expect("log poisoned") = Some(log);

    *state.current_folder.lock().expect("folder poisoned") = Some(folder);
    *state.session.lock().expect("session poisoned") = Some(session);

    emit_recording_state(&app, true, false);
    Ok(())
}

/// Toggle microphone mute. Works whether or not a recording is running.
///
/// The flag lives in `AppState` and the session borrows it, so muting before
/// you hit record is already in force on the very first sample rather than
/// being silently ignored.
#[tauri::command]
pub fn toggle_mute(app: AppHandle, state: State<AppState>) -> bool {
    let muted = state.toggle_muted();

    // Into the meeting's log as well. A muted microphone writes silence through
    // the same path a dead device does, so a track that goes quiet at 40s looks
    // identical to one that lost its device there — unless the log says which.
    if let Some(log) = state.meeting_log.lock().expect("log poisoned").as_ref() {
        log.line(if muted {
            "microphone MUTED by the user — silence from here is deliberate"
        } else {
            "microphone unmuted by the user"
        });
    }

    let _ = app.emit(EV_MUTE_STATE, muted);
    muted
}

/// Announce the recording state to every window and to the tray.
///
/// The tray menu is rebuilt rather than notified: its items are native and
/// their enabled state and status label are baked in at build time.
pub fn emit_recording_state(app: &AppHandle, recording: bool, processing: bool) {
    let _ = app.emit(
        EV_RECORDING_STATE,
        RecordingStateDto {
            recording,
            processing,
        },
    );
    crate::tray::rebuild(app);
}

#[tauri::command]
pub fn is_muted(state: State<AppState>) -> bool {
    state.is_muted()
}

#[tauri::command]
pub fn elapsed_seconds(state: State<AppState>) -> f64 {
    state
        .session
        .lock()
        .expect("session poisoned")
        .as_ref()
        .map(|s| s.elapsed_seconds())
        .unwrap_or(0.0)
}

/// Discard the meeting: stop recording and delete the folder without processing.
///
/// # This deletes audio
///
/// Cancel means "this meeting should not exist", so the WAVs go with it —
/// keeping them would leave orphan folders the user never asked for and has no
/// way to identify later. The folder path is logged before removal so a
/// mis-click is at least traceable in the log.
///
/// The session is stopped first, and only then is anything removed: deleting a
/// directory out from under two open WAV writers is exactly the corruption the
/// rename ordering elsewhere exists to avoid.
#[tauri::command]
pub fn cancel_recording(app: AppHandle, state: State<AppState>) -> Result<(), String> {
    let session = state
        .session
        .lock()
        .expect("session poisoned")
        .take()
        .ok_or("not recording")?;

    let summary = session.stop();

    let _ = app.emit(
        EV_LOG,
        format!("meeting cancelled; deleting {}", summary.folder.display()),
    );

    // Bounded on purpose. `remove_dir_all` is recursive and irreversible, and
    // the folder is built from a config value the frontend can set. It is
    // app-constructed today, so this is defence in depth rather than a fix for
    // a live hole — but an unbounded recursive delete is one bug away from
    // being very bad indeed.
    let output_folder = state.config_snapshot().output_folder;
    if !summary.folder.starts_with(&output_folder) {
        let _ = app.emit(
            EV_LOG,
            format!(
                "refusing to delete {}: outside the output folder {}",
                summary.folder.display(),
                output_folder.display()
            ),
        );
        *state.current_folder.lock().expect("folder poisoned") = None;
        // The recording is over, so the queue is no longer held.
        emit_queue_changed(&app);
        emit_recording_state(&app, false, false);
        return Ok(());
    }

    if let Err(e) = std::fs::remove_dir_all(&summary.folder) {
        let _ = app.emit(
            EV_LOG,
            format!("could not delete {}: {e}", summary.folder.display()),
        );
    }

    *state.current_folder.lock().expect("folder poisoned") = None;
    // Same as the early return above: the session is gone, so is the hold.
    emit_queue_changed(&app);
    emit_recording_state(&app, false, false);
    Ok(())
}

/// Stop recording and run the pipeline.
///
/// Returns as soon as the audio is closed; the rest is reported through
/// `stage`, `status`, `complete` and `error`.
#[tauri::command]
pub fn stop_recording(app: AppHandle, state: State<AppState>) -> Result<(), String> {
    let session = state
        .session
        .lock()
        .expect("session poisoned")
        .take()
        .ok_or("not recording")?;

    let language = state.config_snapshot().language;
    let _ = app.emit(EV_STATUS, i18n::tr(language, "finalizing_recording"));

    // Blocks until both writer threads have flushed and closed their WAVs.
    // Nothing may rename the folder before this returns.
    let summary = session.stop();

    // Assess the capture before anything else touches it.
    //
    // Deliberately here and not at the end of the pipeline. The failure that
    // prompted this arrived as an empty transcript, which is `NoVoice` — the
    // error path, which emits no `CompleteDto` at all. Reporting damage the way
    // `quiet_recording` is reported would therefore have stayed silent for
    // exactly the meeting that needed it, and behind a backlog would have
    // arrived hours late for the ones it did cover.
    //
    // Everything needed is already in hand and no model has to run.
    // The outcome, into the same file the recorder threads were writing to.
    //
    // This is the half that makes the log answer a question rather than merely
    // describe events: the counters beside what was asked of the device tell
    // you whether audio arrived, and if not, what the device had said about
    // itself first.
    if let Some(log) = state.meeting_log.lock().expect("log poisoned").take() {
        log.line("--- stopped ---");
        log.line(&format!("elapsed: {:.3}s", summary.elapsed_seconds));
        for track in &summary.tracks {
            match track {
                Err(e) => log.line(&format!("track FAILED: {e}")),
                Ok(t) => log.line(&format!(
                    "{}: {:.3}s written ({} frames), overflows={}, glitches={}, gap={} frames ({:.3}s), \
                     lead-in={} frames ({:.3}s), device=\"{}\", fallback={}",
                    t.kind.label(),
                    t.duration_seconds,
                    t.frames,
                    t.overflows,
                    t.glitches,
                    t.gap_frames,
                    t.gap_frames as f64 / 48_000.0,
                    t.lead_in_frames,
                    t.lead_in_frames as f64 / 48_000.0,
                    t.final_device,
                    t.automatic_fallback,
                )),
            }
        }
        if let Some(skew) = summary.skew_seconds() {
            log.line(&format!("skew between tracks: {skew:.3}s"));
        }
        match assess_capture(&summary, language) {
            Some(d) => log.line(&format!("VERDICT: damaged — {}", d.detail.replace('\n', " / "))),
            None => log.line("VERDICT: capture looks intact"),
        }
    }

    if let Some(damage) = assess_capture(&summary, language) {
        // Its own event rather than `EV_STATUS`, which carries only a string:
        // the status line already knows how to show a short message with the
        // long form on hover, and a damage report is exactly that shape.
        let _ = app.emit(EV_LOG, damage.detail.clone());
        let _ = app.emit(EV_CAPTURE_DAMAGE, damage);
    }

    *state.pending.lock().expect("pending poisoned") = Some(summary);
    emit_recording_state(&app, false, state.is_processing());
    Ok(())
}

/// Everything wrong with what was just captured, localised for the user.
///
/// `None` means the recording looks intact.
///
/// A failed track is included even though it carries no counters:
/// `RecorderError::NoDevice` means no `TrackSummary` exists at all, and that is
/// the **most** severe case — the system-audio track never opening is a
/// plausible cause of a worthless summary, and it is invisible to any check
/// that reads only per-track integers.
fn assess_capture(
    summary: &crate::session::SessionSummary,
    language: meeting_core::config::Language,
) -> Option<CaptureDamageDto> {
    use meeting_core::policy::{assess_track, TrackDamage, TrackFacts};

    let mut details: Vec<String> = Vec::new();

    for (index, track) in summary.tracks.iter().enumerate() {
        // The recorder reports tracks in the order the session opened them.
        let name = i18n::tr(
            language,
            if index == 0 { "microphone" } else { "computer_audio" },
        );

        match track {
            Err(error) => details.push(i18n::tr_args(
                language,
                "capture_track_failed",
                &[("track", name), ("error", error)],
            )),
            Ok(t) => {
                let damage = assess_track(
                    summary.elapsed_seconds,
                    TrackFacts {
                        duration_seconds: t.duration_seconds,
                        overflows: t.overflows,
                        gap_frames: t.gap_frames,
                        glitches: t.glitches,
                        sample_rate: meeting_core::convert::TARGET_SAMPLE_RATE,
                    },
                );

                match damage {
                    Some(TrackDamage::Dropped { callbacks }) => details.push(i18n::tr_args(
                        language,
                        "capture_dropped",
                        &[("track", name), ("count", &callbacks.to_string())],
                    )),
                    Some(TrackDamage::Silence { seconds }) => details.push(i18n::tr_args(
                        language,
                        "capture_silence",
                        &[("track", name), ("seconds", &seconds.to_string())],
                    )),
                    Some(TrackDamage::Short { missing_seconds }) => details.push(i18n::tr_args(
                        language,
                        "capture_short",
                        &[("track", name), ("seconds", &missing_seconds.to_string())],
                    )),
                    Some(TrackDamage::Glitched {
                        missing_seconds,
                        glitches,
                    }) => details.push(i18n::tr_args(
                        language,
                        "capture_glitched",
                        &[
                            ("track", name),
                            ("seconds", &missing_seconds.to_string()),
                            ("glitches", &glitches.to_string()),
                        ],
                    )),
                    None => {}
                }
            }
        }
    }

    if details.is_empty() {
        return None;
    }

    Some(CaptureDamageDto {
        message: i18n::tr(language, "capture_damaged").to_string(),
        detail: i18n::tr_args(
            language,
            "capture_damaged_detail",
            &[("details", &details.join("\n"))],
        ),
    })
}

/// Process the meeting that [`stop_recording`] just finished capturing.
///
/// Separate from stopping on purpose. The two used to be one command taking the
/// title, which meant capture continued for as long as the title dialog was
/// open — recording the user typing a name onto the end of the meeting, with
/// the timer still counting up. The Python has the same split: `stop_event.set()`
/// fires before `simpledialog.askstring` (`app.py:3958` vs `3968`).
#[tauri::command]
pub fn finalize_meeting(
    app: AppHandle,
    meeting_title: String,
    state: State<AppState>,
) -> Result<(), String> {
    let summary = state
        .pending
        .lock()
        .expect("pending poisoned")
        .take()
        .ok_or("no meeting is waiting to be processed")?;

    // Snapshotted HERE, at enqueue, and stored with the meeting — not read
    // when the worker reaches it. Changing the summary type or the provider
    // between meetings must not reach back and rewrite what a meeting already
    // waiting in the queue will produce.
    let config = state.config_snapshot();

    let id = summary
        .folder
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "meeting".to_string());

    let mut meeting = queue::MeetingState::new(
        id,
        text::sanitize_name(&meeting_title),
        serde_json::from_str(&config.to_json()).unwrap_or(serde_json::Value::Null),
    );
    // The WAVs are final by now, so this is the cheapest moment to learn how
    // long the meeting was — and it is written with the rest of the state, so
    // the length survives a restart without re-reading the audio.
    meeting.duration_seconds =
        pipeline::wav_duration(&summary.folder.join(crate::session::MIC_FILENAME));

    // Written before the job is visible to the worker, so a crash in between
    // leaves a meeting the startup scan will find rather than one it will not.
    queue::save(&summary.folder, &meeting).map_err(|e| e.to_string())?;

    state.queue.enqueue(queue::Job {
        folder: summary.folder,
        state: meeting,
    });

    // The status line still says "Finishing recording..." from `stop_recording`,
    // and by now that is finished: the WAVs were flushed and closed before this
    // command was reached. Left alone it sat there while the meeting waited in
    // the queue, describing work that was already done.
    //
    // Deliberately not "waiting to be processed": the meeting's own card says
    // "Waiting", and the status line should not narrate what is already on
    // screen a few pixels below it.
    let _ = app.emit(
        EV_STATUS,
        i18n::tr(state.config_snapshot().language, "meeting_saved"),
    );

    emit_queue_changed(&app);
    emit_recording_state(&app, false, state.is_processing());
    Ok(())
}

/// Percentages the stages occupy on the queue card's progress bar.
///
/// Transcription is the long pole by a wide margin, so it gets most of the bar.
/// The numbers are honest about that rather than dividing the bar into three
/// equal thirds, which would sit at 33% for minutes and then leap to 100%.
const PERCENT_AUDIO: u8 = 3;
const PERCENT_WHISPER_START: u8 = 5;
const PERCENT_WHISPER_END: u8 = 80;

/// The single background worker.
///
/// One, not several: each Whisper job loads its own copy of the model — 3.1 GB
/// for large-v3 — and Ollama serialises requests internally anyway, so running
/// two would double the memory to no purpose.
pub fn spawn_worker(app: AppHandle) {
    std::thread::spawn(move || {
        let Some(state) = app.try_state::<AppState>() else {
            return;
        };
        let queue = Arc::clone(&state.queue);

        while let Some(job) = queue.next() {
            let id = job.state.id.clone();
            let (updated, folder) = run_job(&app, &queue, job);

            // Persisted before the queue is told, so a crash in between leaves
            // the truth on disk rather than only in memory.
            //
            // NOT ignored. This write failing is how a pause silently lost its
            // resume point and a failure silently stayed marked `Queued`: it was
            // going to a directory the rename had moved, returning ENOENT into a
            // `let _ =`. If it fails now, it is visible.
            if let Err(e) = queue::save(&folder, &updated) {
                let _ = app.emit(
                    EV_LOG,
                    format!("could not record {} state at {}: {e}", id, folder.display()),
                );
            }
            queue.finish(&id, updated, folder);
            emit_queue_changed(&app);
            emit_recording_state(&app, state.is_recording(), state.is_processing());
        }
    });
}

/// Run one meeting to completion, to a pause, or to a failure.
///
/// Returns the state to persist and the folder it belongs in — the folder is
/// returned because `pipeline::run` renames it partway through, so the path the
/// job started with is not the one the state file must be written to.
fn run_job(
    app: &AppHandle,
    queue: &Arc<queue::Queue>,
    job: queue::Job,
) -> (queue::MeetingState, std::path::PathBuf) {
    let mut meeting = job.state;
    let config = Config::from_json(
        &serde_json::to_string(&meeting.config).unwrap_or_default(),
        &platform::documents_dir(),
    );
    let language = config.language;

    let pipeline_config = PipelineConfig {
        folder: job.folder.clone(),
        meeting_title: meeting.title.clone(),
        output_folder: config.output_folder.clone(),
        whisper_model: config.whisper_model.clone(),
        transcription_language: config.transcription_language.whisper_code().map(str::to_string),
        provider: crate::summary::ProviderConfig::from_config(&config),
        language,
        summary_type: config.summary_type,
        custom_summary_prompt: config.custom_summary_prompt.clone(),
        keep_audio: config.keep_audio,
        speaker_me: i18n::tr(language, "speaker_me").to_string(),
        speaker_meeting: i18n::tr(language, "speaker_meeting").to_string(),
        resume: pipeline::ResumePoint {
            mic_offset_seconds: meeting.mic_offset_seconds,
            system_offset_seconds: meeting.system_offset_seconds,
            summary_chunk: meeting.summary_chunk,
        },
    };

    let control = queue.control();

    // True only while the transcription stage is running; see the ticker below.
    let transcribing = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // `full()` blocks its thread for the whole file, so the only way to observe
    // a transcription in progress is from another thread reading the control.
    // The ticker ends when the job does, via the same abort flag.
    let ticker = {
        let total = pipeline::wav_duration(&job.folder.join(crate::session::MIC_FILENAME))
            .unwrap_or(0.0)
            + pipeline::wav_duration(&job.folder.join(crate::session::SYSTEM_FILENAME))
                .unwrap_or(0.0);
        let queue = Arc::clone(queue);
        let app = app.clone();
        let control = control.clone();
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&done);
        let transcribing = Arc::clone(&transcribing);

        std::thread::spawn(move || {
            while !flag.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(500));
                // Only while transcribing. The control keeps its last position
                // after the final track, so an ungated ticker went on writing a
                // stale transcription percentage over the summary stage's.
                if total > 0.0 && transcribing.load(std::sync::atomic::Ordering::Relaxed) {
                    let fraction = (control.seconds_done() / total).clamp(0.0, 1.0);
                    let span = (PERCENT_WHISPER_END - PERCENT_WHISPER_START) as f64;
                    queue.set_percent(PERCENT_WHISPER_START + (fraction * span) as u8);
                    emit_queue_changed(&app);
                }
            }
        });
        done
    };

    // Where the meeting ends up, learned from the pipeline rather than assumed.
    // Shared because the closure below is moved into `run` and this is read
    // after it returns — on every outcome, including a failure.
    let renamed: Arc<std::sync::Mutex<Option<std::path::PathBuf>>> =
        Arc::new(std::sync::Mutex::new(None));
    let renamed_sink = Arc::clone(&renamed);

    let emitter = app.clone();
    let queue_for_stage = Arc::clone(queue);
    let stage_id = meeting.id.clone();
    let stage_app = app.clone();
    let stage_transcribing = Arc::clone(&transcribing);

    // Filled by the progress callback, read once the run is over. An `Arc`
    // because the callback outlives this scope on the pipeline's thread.
    let timings: Arc<std::sync::Mutex<queue::Timings>> =
        Arc::new(std::sync::Mutex::new(meeting.timings.clone()));
    let timings_sink = Arc::clone(&timings);
    // When the current stage was entered, so its duration can be added on exit.
    let stage_started = Arc::new(std::sync::Mutex::new(None::<std::time::Instant>));

    let result = pipeline::run(pipeline_config, &control, move |progress| match progress {
        Progress::Stage(stage, stage_state) => {
            // Both edges are timed, even though only entry drives the UI: a
            // stage's cost is the thing being measured, and `Done` is the only
            // event that knows when it stopped.
            if let Ok(mut clocks) = timings_sink.lock() {
                let clock = match stage {
                    Stage::Audio => &mut clocks.audio,
                    Stage::Whisper => &mut clocks.whisper,
                    Stage::Summary => &mut clocks.summary,
                };
                match stage_state {
                    StageState::Working => {
                        clock.begin(timestamp_iso());
                        if let Ok(mut started) = stage_started.lock() {
                            *started = Some(std::time::Instant::now());
                        }
                    }
                    _ => {
                        // `Instant`, not the wall clock: the elapsed time must
                        // not move if the system clock is adjusted mid-run.
                        let elapsed = stage_started
                            .lock()
                            .ok()
                            .and_then(|started| *started)
                            .map(|started| started.elapsed().as_secs_f64())
                            .unwrap_or(0.0);
                        clock.end(timestamp_iso(), elapsed);
                    }
                }
            }

            // Only on entry from here down. `Done` for one stage arrives
            // immediately before `Working` for the next, and acting on both
            // would briefly show the finished stage as if it were the current
            // one.
            if stage_state != StageState::Working {
                return;
            }

            queue_for_stage.set_stage(
                &stage_id,
                match stage {
                    Stage::Audio => queue::Stage::Audio,
                    Stage::Whisper => queue::Stage::Whisper,
                    Stage::Summary => queue::Stage::Summary,
                },
            );
            queue_for_stage.set_percent(match stage {
                Stage::Audio => PERCENT_AUDIO,
                Stage::Whisper => PERCENT_WHISPER_START,
                Stage::Summary => PERCENT_WHISPER_END,
            });
            stage_transcribing.store(
                stage == Stage::Whisper,
                std::sync::atomic::Ordering::Relaxed,
            );

            // The card is how a stage reaches the user now, so it has to be
            // redrawn here — there is no longer a stage event doing it.
            emit_queue_changed(&stage_app);
        }
        Progress::Folder(folder) => {
            if let Ok(mut slot) = renamed_sink.lock() {
                *slot = Some(folder);
            }
        }
        Progress::Status(status) => {
            let _ = emitter.emit(EV_STATUS, status);
        }
    });

    ticker.store(true, std::sync::atomic::Ordering::Relaxed);

    // The folder as it stands now.
    //
    // Taken from what the pipeline reported, not from the outcome: `Paused` and
    // the error path both used to fall back to `job.folder`, which the rename
    // had already invalidated. Everything downstream then wrote to a directory
    // that no longer existed — the state file included, silently.
    let folder = renamed
        .lock()
        .ok()
        .and_then(|slot| slot.clone())
        .unwrap_or_else(|| job.folder.clone());

    // Whatever happened — finished, paused or failed — the stages that did run
    // are worth recording. A failure's timings are the most interesting of all.
    if let Ok(clocks) = timings.lock() {
        meeting.timings = clocks.clone();
        // Beside the timing it explains. See `MeetingState::whisper_tuning`.
        meeting.whisper_tuning = Some(whisper::tuning_summary());
        // Recorded here, before the match below, so it reaches the file on the
        // paused and failed paths too — those being the runs most worth it.
        meeting.whisper_vad = control.vad_summary();
    }

    match result {
        Ok(pipeline::RunOutcome::Finished(output)) => {
            let _ = app.emit(
                EV_COMPLETE,
                CompleteDto {
                    quiet_recording: output.quiet_recording,
                    folder: output.folder.to_string_lossy().into_owned(),
                    transcript_file: output.transcript_file.to_string_lossy().into_owned(),
                    summary_file: output.summary_file.to_string_lossy().into_owned(),
                },
            );
            meeting.stage = queue::Stage::Done;
            meeting.error = None;
        }
        Ok(pipeline::RunOutcome::Paused(resume)) => {
            // Not an error and not reported as one. The stage records how far it
            // got so the card can say so and the next run can pick it up.
            meeting.mic_offset_seconds = resume.mic_offset_seconds;
            meeting.system_offset_seconds = resume.system_offset_seconds;
            meeting.summary_chunk = resume.summary_chunk;
            meeting.stage = if resume.summary_chunk > 0 {
                queue::Stage::Summary
            } else {
                queue::Stage::Whisper
            };
        }
        Err(e) => {
            let message = e.to_string();
            let _ = app.emit(EV_ERROR, message.clone());
            meeting.stage = queue::Stage::Failed;
            meeting.error = Some(message);
        }
    }

    (meeting, folder)
}

/// Tell the frontend the queue moved; it then asks for a fresh snapshot.
///
/// One signal rather than a job id threaded through every stage, status,
/// completion and error event. The frontend renders the queue from
/// [`list_jobs`], so it cannot drift out of step with the worker the way an
/// incrementally-applied event stream can.
pub fn emit_queue_changed(app: &AppHandle) {
    let _ = app.emit(EV_QUEUE_CHANGED, ());
}

/// Everything in the queue, for rendering.
#[tauri::command(async)]
pub fn list_jobs(state: State<AppState>) -> Vec<queue::JobView> {
    state.queue.view()
}

#[tauri::command(async)]
pub fn is_processing_paused(state: State<AppState>) -> bool {
    state.queue.is_paused()
}

/// Why the queue is not moving, so the UI can say which.
///
/// Separate from [`is_processing_paused`], which must keep meaning *the user
/// pressed pause* because it drives that button's next action — and that action
/// writes `config.processing_paused` to disk.
#[tauri::command(async)]
pub fn processing_blocked_reason(state: State<AppState>) -> Option<queue::BlockedReason> {
    state.queue.blocked_reason()
}

/// Hold, or release, all processing.
///
/// Persisted to the config because the choice has a horizon of hours — "do this
/// at lunch", "do this tonight" — and resetting it on the next launch would
/// silently start the very work the user postponed.
#[tauri::command(async)]
pub fn set_processing_paused(
    app: AppHandle,
    paused: bool,
    state: State<AppState>,
) -> Result<(), String> {
    state.queue.set_paused(paused);

    // Written through the same path as any other setting, so there is one place
    // that knows how the config reaches disk.
    let mut config = state.config_snapshot();
    config.processing_paused = paused;
    let app_folder = platform::app_data_dir();
    std::fs::create_dir_all(&app_folder).map_err(|e| e.to_string())?;
    std::fs::write(&state.config_file, config.to_json()).map_err(|e| e.to_string())?;
    *state.config.lock().expect("config poisoned") = config;

    emit_queue_changed(&app);
    Ok(())
}

/// Remove a meeting from the queue and delete it.
///
/// # This deletes audio
///
/// Same meaning as cancelling a recording: the meeting should not exist, so its
/// folder goes with it. It exists because a meeting started by accident should
/// not have to wait its turn in a queue to be got rid of, and pausing everything
/// is not a way to remove one thing.
///
/// The job is taken out of the queue and aborted **before** anything is removed.
/// `Queue::take` deliberately returns the folder rather than deleting it, so the
/// pipeline is never standing inside a directory that is being unlinked.
#[tauri::command(async)]
pub fn discard_job(app: AppHandle, id: String, state: State<AppState>) -> Result<(), String> {
    let Some(folder) = state.queue.take(&id) else {
        return Err("that meeting is no longer in the queue".into());
    };

    // Bounded: the path comes from the queue entry, never from the caller.
    let _ = app.emit(EV_LOG, format!("discarding meeting {}", folder.display()));
    std::fs::remove_dir_all(&folder).map_err(|e| e.to_string())?;

    emit_queue_changed(&app);
    Ok(())
}

/// Put a failed meeting back in line, resuming rather than restarting.
#[tauri::command(async)]
pub fn retry_job(app: AppHandle, id: String, state: State<AppState>) -> Result<(), String> {
    if !state.queue.retry(&id) {
        return Err("that meeting is not waiting to be retried".into());
    }
    emit_queue_changed(&app);
    Ok(())
}


/// `YYYY-MM-DD_HH-MM-SS` in **local** time, matching the Python's folder naming.
///
/// # Why this is local and the timings below are not
///
/// This is the name a person reads. It is the meeting's identity in Finder and
/// Explorer, and the time the queue card shows. It has to match the clock on the
/// wall when the meeting happened.
///
/// It did not. This was hand-rolled from raw Unix seconds with no timezone
/// applied, which is UTC — so a meeting recorded at 10:07 in Spain was filed as
/// `08-07`, two hours adrift, and every folder ever created carries the error.
/// A previous version of this comment justified avoiding `chrono` "for one
/// format string"; the format string was never the problem, and correct local
/// time is not something to hand-roll — it needs the OS's current UTC offset and
/// its DST rules.
///
/// **Folders already on disk keep their old names.** Renaming them would be a
/// destructive migration over the user's own files, and the timestamp is also
/// the meeting id recorded inside `meeting.json`.
fn timestamp_folder_name() -> String {
    chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string()
}

/// `YYYY-MM-DDTHH:MM:SS`, **UTC**, for the stage timings in `meeting.json`.
///
/// Deliberately not local, and the opposite choice from the folder name above.
/// These are instants to subtract, not times to read: a meeting spanning a
/// daylight-saving change would otherwise record a stage as an hour shorter or
/// longer than it was.
fn timestamp_iso() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S").to_string()
}

/// Re-exported so `main.rs` can register it without importing `policy`.
pub use policy::Device as PolicyDevice;

#[cfg(test)]
mod tests {
    use super::*;

    /// A track as the recorder really reports one.
    ///
    /// `lead_in_frames` is non-zero on purpose. **Every** recording has an
    /// opening silence — a couple of hundred milliseconds while the device
    /// opens, more on Windows — and an earlier version of this helper set it to
    /// zero, a value the recorder never produces. The damage check counted that
    /// silence as an outage, so it would have reported every healthy meeting as
    /// broken, and these tests could not see it because they invented a
    /// recording that cannot exist.
    fn track(duration: f64, overflows: u64, gap_frames: u64) -> crate::audio::recorder::TrackSummary {
        crate::audio::recorder::TrackSummary {
            kind: crate::audio::devices::SourceKind::Microphone,
            path: std::path::PathBuf::from("microphone.wav"),
            frames: (duration * 48_000.0) as u64,
            duration_seconds: duration,
            automatic_fallback: false,
            final_device: "Test".into(),
            glitches: 0,
            overflows,
            gap_frames,
            // ~0.23s, measured on macOS. Windows is larger.
            lead_in_frames: 11_000,
        }
    }

    fn session(elapsed: f64, tracks: Vec<Result<crate::audio::recorder::TrackSummary, String>>)
        -> crate::session::SessionSummary
    {
        crate::session::SessionSummary {
            folder: std::path::PathBuf::from("/tmp/x"),
            elapsed_seconds: elapsed,
            tracks,
        }
    }

    /// The regression that matters most: a warning on every meeting is worse
    /// than no warning at all, because it teaches the user to ignore it.
    #[test]
    fn a_healthy_recording_produces_no_report() {
        let s = session(600.0, vec![Ok(track(600.0, 0, 0)), Ok(track(600.0, 0, 0))]);
        assert!(
            assess_capture(&s, meeting_core::config::Language::En).is_none(),
            "a normal recording, opening silence and all, must not be called damaged"
        );
    }

    /// The opening silence is unavoidable and belongs to alignment, not damage.
    /// Counting it as an outage is what made every meeting look broken.
    #[test]
    fn the_device_open_lead_in_is_never_damage() {
        let mut slow_to_open = track(600.0, 0, 0);
        // Two full seconds — a slow Windows loopback open, well past any
        // threshold an outage check would use.
        slow_to_open.lead_in_frames = 2 * 48_000;

        let s = session(600.0, vec![Ok(slow_to_open), Ok(track(600.0, 0, 0))]);
        assert!(
            assess_capture(&s, meeting_core::config::Language::En).is_none(),
            "a device that took its time opening is not a damaged recording"
        );
    }

    /// The reported failure: the machine could not keep up and audio was lost.
    #[test]
    fn dropped_callbacks_reach_the_user_in_their_own_language() {
        let s = session(600.0, vec![Ok(track(480.0, 37, 0)), Ok(track(600.0, 0, 0))]);

        let en = assess_capture(&s, meeting_core::config::Language::En).expect("damage should be reported");
        assert!(en.detail.contains("37"), "the count is the evidence: {}", en.detail);
        assert!(!en.message.is_empty());

        let es = assess_capture(&s, meeting_core::config::Language::Es).expect("damage should be reported");
        assert_ne!(es.message, en.message, "the message must be localised");
        assert!(es.detail.contains("37"));
    }

    /// A track that never opened has no counters at all, and is the worst case.
    #[test]
    fn a_track_that_never_started_is_reported() {
        let s = session(600.0, vec![Ok(track(600.0, 0, 0)), Err("no device".into())]);
        let d = assess_capture(&s, meeting_core::config::Language::En).expect("a dead track is damage");
        assert!(d.detail.contains("no device"), "{}", d.detail);
    }

    /// Silence padding keeps the file full length, so only `gap_frames` sees it.
    #[test]
    fn a_full_length_file_padded_with_silence_is_still_reported() {
        let s = session(600.0, vec![Ok(track(600.0, 0, 20 * 48_000)), Ok(track(600.0, 0, 0))]);
        assert!(
            assess_capture(&s, meeting_core::config::Language::En).is_some(),
            "a device outage pads the file; the length check alone would miss it"
        );
    }

    #[test]
    fn folder_names_have_the_expected_shape() {
        let name = timestamp_folder_name();
        assert_eq!(name.len(), "YYYY-MM-DD_HH-MM-SS".len(), "got {name}");
        assert_eq!(name.as_bytes()[4], b'-');
        assert_eq!(name.as_bytes()[10], b'_');
        assert_eq!(name.as_bytes()[13], b'-');
    }

    /// The bug this replaced: folder names were raw Unix seconds with no
    /// timezone applied, so a meeting recorded at 10:07 in Spain was filed as
    /// `08-07`. Shape tests could not see it — both are well-formed names.
    ///
    /// Asserted as a *difference from UTC*, because the test has to pass in CI
    /// too, where the runner's clock is UTC and there is no offset to find.
    #[test]
    fn the_folder_name_is_local_time_and_the_timings_are_utc() {
        let offset = chrono::Local::now().offset().local_minus_utc();

        let folder = timestamp_folder_name();
        let iso = timestamp_iso();

        // Same instant, formatted two ways: `YYYY-MM-DD_HH-MM-SS` and
        // `YYYY-MM-DDTHH:MM:SS`. Compare the hour field of each.
        let folder_hour: i32 = folder[11..13].parse().expect("hour");
        let utc_hour: i32 = iso[11..13].parse().expect("hour");

        let expected = (utc_hour + offset / 3600).rem_euclid(24);
        assert_eq!(
            folder_hour, expected,
            "folder name {folder} should be local time; timings {iso} should be UTC \
             (offset {offset}s)"
        );
    }
}
