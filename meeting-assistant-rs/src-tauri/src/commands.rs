//! The IPC surface: 14 commands and 11 events.
//!
//! Events are deliberately one-to-one with the Python's queue tags
//! (`app.py`'s `messages.put((tag, payload))`) so the port can be diffed
//! against current behaviour rather than guessed at.
//!
//! `queue.Queue` plus `root.after(100, poll_messages)` becomes `app.emit()`;
//! the flow is still strictly one-way, worker → UI.

use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use meeting_core::config::{Config, Language, SummaryProvider};
use meeting_core::{i18n, policy, text};

use crate::audio::devices::{self, SourceKind};
use crate::audio::recorder::Event as RecorderEvent;
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
pub const EV_STAGE: &str = "stage";
pub const EV_LOG: &str = "log";
pub const EV_ERROR: &str = "error";
pub const EV_COMPLETE: &str = "complete";

#[derive(Serialize, Clone)]
pub struct DeviceDto {
    pub id: String,
    pub name: String,
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
}

#[derive(Serialize, Clone)]
pub struct OllamaStatusDto {
    /// The server answered at all.
    pub running: bool,
    pub models: Vec<String>,
    pub error: Option<String>,
}

#[derive(Serialize, Clone)]
pub struct StageDto {
    pub stage: &'static str,
    pub state: &'static str,
}

#[derive(Serialize, Clone)]
pub struct CompleteDto {
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
pub fn save_config(payload: String, state: State<AppState>) -> Result<(), String> {
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
    Ok(())
}

/// The whole i18n catalogue, so the frontend can localize without a round trip
/// per string. Shared source of truth with the Rust side.
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

    snapshot
        .devices
        .into_iter()
        .map(|d| DeviceDto {
            is_default: Some(&d.id) == default_id.as_ref(),
            id: d.id,
            name: d.name,
            sample_rate: d.sample_rate,
            channels: d.channels,
        })
        .collect()
}

#[tauri::command]
pub fn list_devices() -> DeviceListDto {
    DeviceListDto {
        microphones: snapshot_dto(SourceKind::Microphone),
        system: snapshot_dto(SourceKind::SystemAudio),
    }
}

/// Same as [`list_devices`]; a separate command because the UI treats an
/// explicit refresh differently from the initial load.
#[tauri::command]
pub fn refresh_devices() -> DeviceListDto {
    list_devices()
}

// --- models ------------------------------------------------------------

#[tauri::command]
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

#[tauri::command]
pub fn list_whisper_models() -> Vec<WhisperModelDto> {
    whisper::MODELS
        .iter()
        .map(|spec| WhisperModelDto {
            id: spec.id.to_string(),
            approx_mb: spec.approx_mb,
            installed: whisper::is_installed(spec.id),
        })
        .collect()
}

/// Download a model, streaming percentage through `status`.
#[tauri::command]
pub async fn download_whisper_model(app: AppHandle, model: String) -> Result<(), String> {
    // Blocking IO must not run on the async runtime's thread, or the whole UI
    // stops responding for the length of a multi-gigabyte download.
    tauri::async_runtime::spawn_blocking(move || {
        let language = Language::En;
        whisper::download_model(&model, |percent| {
            let _ = app.emit(
                EV_STATUS,
                i18n::tr_args(
                    language,
                    "downloading_whisper",
                    &[("model", &format!("{model} ({percent}%)"))],
                ),
            );
        })
        .map(|_| ())
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
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
#[tauri::command]
pub fn open_settings(app: AppHandle) -> Result<(), String> {
    if let Some(existing) = app.get_webview_window("settings") {
        existing.show().map_err(|e| e.to_string())?;
        existing.set_focus().map_err(|e| e.to_string())?;
        return Ok(());
    }

    tauri::WebviewWindowBuilder::new(
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
#[tauri::command]
pub fn set_api_key(key: String) -> Result<(), String> {
    crate::summary::store_api_key(&key).map_err(|e| e.to_string())
}

/// Whether a key is stored. Never returns the key itself — the settings UI
/// shows "saved", not the value.
#[tauri::command]
pub fn has_api_key() -> bool {
    crate::summary::has_api_key()
}

// --- files -------------------------------------------------------------

#[tauri::command]
pub fn open_path(path: String) -> Result<(), String> {
    platform::open_path(std::path::Path::new(&path)).map_err(|e| e.to_string())
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

        // Probe Ollama only when it is the configured provider. Launching a
        // local server for someone who chose a remote endpoint is both
        // surprising and slow, and its "not running" state would be reported
        // as a startup error they cannot act on.
        let uses_ollama = config.summary_provider == SummaryProvider::Ollama;
        let mut ollama_status = if uses_ollama {
            emit_status("checking_ollama");
            let status = list_ollama_models();

            // The Python auto-started Ollama when it was installed but not
            // running, then told the user to reopen the app if it had to
            // (`app.py:200`).
            if !status.running
                && platform::find_ollama().is_some()
                && platform::start_ollama().is_ok()
            {
                // Give it a moment to bind the port before deciding.
                std::thread::sleep(std::time::Duration::from_secs(3));
                list_ollama_models()
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
        let _ = &mut ollama_status;

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
    if state.is_processing() {
        return Err("still processing the previous meeting".into());
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
    )
    .map_err(|e| e.to_string())?;

    // Forward recorder events to the UI. The receiver is cloned out of the
    // session so this thread does not hold the state lock.
    let events = session.events.clone();
    let forwarder = app.clone();
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
            let _ = forwarder.emit(name, payload);
        }
    });

    *state.current_folder.lock().expect("folder poisoned") = Some(folder);
    *state.session.lock().expect("session poisoned") = Some(session);

    Ok(())
}

/// Toggle microphone mute. Works whether or not a recording is running.
///
/// The flag lives in `AppState` and the session borrows it, so muting before
/// you hit record is already in force on the very first sample rather than
/// being silently ignored.
#[tauri::command]
pub fn toggle_mute(state: State<AppState>) -> bool {
    state.toggle_muted()
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

    if let Err(e) = std::fs::remove_dir_all(&summary.folder) {
        let _ = app.emit(
            EV_LOG,
            format!("could not delete {}: {e}", summary.folder.display()),
        );
    }

    *state.current_folder.lock().expect("folder poisoned") = None;
    Ok(())
}

/// Stop recording and run the pipeline.
///
/// Returns as soon as the audio is closed; the rest is reported through
/// `stage`, `status`, `complete` and `error`.
#[tauri::command]
pub fn stop_recording(
    app: AppHandle,
    meeting_title: String,
    state: State<AppState>,
) -> Result<(), String> {
    let session = state
        .session
        .lock()
        .expect("session poisoned")
        .take()
        .ok_or("not recording")?;

    let config = state.config_snapshot();
    let language = config.language;

    let _ = app.emit(EV_STATUS, i18n::tr(language, "finalizing_recording"));

    // Blocks until both writer threads have flushed and closed their WAVs.
    // Nothing may rename the folder before this returns.
    let summary = session.stop();
    *state.processing.lock().expect("processing poisoned") = true;

    for track in &summary.tracks {
        if let Err(e) = track {
            let _ = app.emit(EV_LOG, format!("track failed: {e}"));
        }
    }

    let pipeline_config = PipelineConfig {
        folder: summary.folder,
        meeting_title: text::sanitize_name(&meeting_title),
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
    };

    let handle = app.clone();
    let state_handle: Arc<AppHandle> = Arc::new(app);

    std::thread::spawn(move || {
        let emitter = handle.clone();
        let result = pipeline::run(pipeline_config, move |progress| match progress {
            Progress::Stage(stage, stage_state) => {
                let _ = emitter.emit(
                    EV_STAGE,
                    StageDto {
                        stage: stage_name(stage),
                        state: state_name(stage_state),
                    },
                );
            }
            Progress::Status(status) => {
                let _ = emitter.emit(EV_STATUS, status);
            }
        });

        match result {
            Ok(output) => {
                let _ = handle.emit(
                    EV_COMPLETE,
                    CompleteDto {
                        folder: output.folder.to_string_lossy().into_owned(),
                        transcript_file: output.transcript_file.to_string_lossy().into_owned(),
                        summary_file: output.summary_file.to_string_lossy().into_owned(),
                    },
                );
            }
            Err(e) => {
                let _ = handle.emit(EV_ERROR, e.to_string());
            }
        }

        if let Some(state) = state_handle.try_state::<AppState>() {
            *state.processing.lock().expect("processing poisoned") = false;
        }
    });

    Ok(())
}

fn stage_name(stage: Stage) -> &'static str {
    match stage {
        Stage::Audio => "audio",
        Stage::Whisper => "whisper",
        Stage::Summary => "summary",
    }
}

fn state_name(state: StageState) -> &'static str {
    match state {
        StageState::Pending => "pending",
        StageState::Working => "working",
        StageState::Done => "done",
        StageState::Error => "error",
    }
}

/// `YYYY-MM-DD_HH-MM-SS`, matching the Python's folder naming.
///
/// Hand-rolled from the Unix timestamp rather than pulling in `chrono` for one
/// format string. Civil-time conversion is the standard days-from-epoch
/// algorithm; it is correct for all dates this app will ever see.
fn timestamp_folder_name() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let (year, month, day, hour, minute, second) = civil_from_unix(now as i64);
    format!("{year:04}-{month:02}-{day:02}_{hour:02}-{minute:02}-{second:02}")
}

/// Days-from-civil, inverted. From Howard Hinnant's `civil_from_days`.
fn civil_from_unix(seconds: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = seconds.div_euclid(86_400);
    let secs_of_day = seconds.rem_euclid(86_400);

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };

    (
        year,
        m as u32,
        d as u32,
        (secs_of_day / 3600) as u32,
        ((secs_of_day % 3600) / 60) as u32,
        (secs_of_day % 60) as u32,
    )
}

/// Re-exported so `main.rs` can register it without importing `policy`.
pub use policy::Device as PolicyDevice;

#[cfg(test)]
mod tests {
    use super::*;

    /// Folder names are user-visible and sort chronologically only if this is
    /// right. A wrong epoch conversion is easy to miss and permanent in the
    /// filesystem.
    #[test]
    fn unix_epoch_converts_to_the_right_civil_date() {
        assert_eq!(civil_from_unix(0), (1970, 1, 1, 0, 0, 0));
        // 2026-09-03T07:00:00Z
        assert_eq!(civil_from_unix(1_788_418_800), (2026, 9, 3, 7, 0, 0));
        // A leap day, which the naive "365 days a year" version gets wrong.
        assert_eq!(civil_from_unix(1_709_164_800), (2024, 2, 29, 0, 0, 0));
    }

    #[test]
    fn folder_names_have_the_expected_shape() {
        let name = timestamp_folder_name();
        assert_eq!(name.len(), "YYYY-MM-DD_HH-MM-SS".len(), "got {name}");
        assert_eq!(name.as_bytes()[4], b'-');
        assert_eq!(name.as_bytes()[10], b'_');
        assert_eq!(name.as_bytes()[13], b'-');
    }
}
