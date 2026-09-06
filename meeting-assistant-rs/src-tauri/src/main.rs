// Release builds must not open a console window on Windows. No effect on macOS.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Tauri entry point: build the app, register commands, own the state.

use meeting_assistant::commands;
use meeting_assistant::platform;
use meeting_assistant::state::AppState;
use tauri::{Emitter, Manager};
use meeting_core::config::Config;

fn main() {
    let app_folder = platform::app_data_dir();
    let config_file = app_folder.join("config.json");

    // A missing or malformed file yields pure defaults rather than failing to
    // start — the app must always open, even with a corrupted config.
    // Recordings default under the user's Documents, not beside the binary.
    // See `Config::defaults` for why this diverges from the Python.
    let documents = platform::documents_dir();
    let config = std::fs::read_to_string(&config_file)
        .map(|text| Config::from_json(&text, &documents))
        .unwrap_or_else(|_| Config::defaults(&documents));

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(AppState::new(config_file, config))
        .invoke_handler(tauri::generate_handler![
            commands::get_config,
            commands::save_config,
            commands::get_i18n,
            commands::list_devices,
            commands::refresh_devices,
            commands::list_ollama_models,
            commands::list_whisper_models,
            commands::download_whisper_model,
            commands::start_recording,
            commands::stop_recording,
            commands::finalize_meeting,
            commands::cancel_recording,
            commands::toggle_mute,
            commands::is_muted,
            commands::elapsed_seconds,
            commands::start_ollama,
            commands::needs_setup,
            commands::open_setup,
            commands::close_setup,
            commands::set_api_key,
            commands::has_api_key,
            commands::open_path,
            commands::open_settings,
            commands::close_settings,
            commands::startup_check,
        ])
        .setup(|app| {
            meeting_assistant::tray::init(app.handle())?;
            Ok(())
        })
        .on_window_event(|window, event| {
            // Close hides rather than quits, so a meeting survives closing the
            // window and the app keeps running in the menu bar.
            //
            // Only the MAIN window: `close_settings` and `close_setup` call
            // `window.close()`, and intercepting those would make Settings and
            // the wizard impossible to dismiss.
            if window.label() != "main" {
                return;
            }

            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();

                // Said once per launch, so the app does not appear to have
                // vanished the first time the window is closed.
                let state = window.state::<meeting_assistant::state::AppState>();
                if !state
                    .hide_notice_shown
                    .swap(true, std::sync::atomic::Ordering::SeqCst)
                {
                    let language = state.config_snapshot().language;
                    let _ = window.app_handle().emit(
                        meeting_assistant::commands::EV_STATUS,
                        meeting_core::i18n::tr(language, "tray_hidden_notice"),
                    );
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("failed to start Meeting Assistant")
        .run(|_app, _event| {
            // `RunEvent::Reopen` is the Dock-icon click and exists only on
            // macOS — the variant is not present in the enum on Windows, so
            // this must be cfg-gated rather than merely never fired.
            //
            // With every window hidden there is nothing in the Dock's window
            // list to click, so the reopen has to restore the window itself.
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = _event {
                meeting_assistant::tray::show_window(_app);
            }
        });
}
