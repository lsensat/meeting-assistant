// Release builds must not open a console window on Windows. No effect on macOS.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Tauri entry point: build the app, register commands, own the state.

use meeting_assistant::commands;
use meeting_assistant::platform;
use meeting_assistant::state::AppState;
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
            commands::cancel_recording,
            commands::toggle_mute,
            commands::is_muted,
            commands::elapsed_seconds,
            commands::open_path,
            commands::open_settings,
            commands::close_settings,
            commands::startup_check,
        ])
        .run(tauri::generate_context!())
        .expect("failed to start Meeting Assistant");
}
