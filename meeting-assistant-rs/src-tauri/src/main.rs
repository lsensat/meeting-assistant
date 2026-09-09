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
        // Must be registered FIRST: it decides whether this process lives at
        // all, and doing that before anything else is initialised avoids a
        // second instance briefly touching state the first one owns.
        //
        // The closure runs in the ALREADY RUNNING instance while the new one
        // exits, so doing nothing here would make a relaunch appear to fail.
        // That matters more than usual because closing the window only hides it
        // (see `on_window_event` below) — relaunching from the Dock or Start
        // menu is the natural way to get a tray-only app back, and it must
        // behave as "reopen". `show_window` is what the macOS Dock click
        // already does via `RunEvent::Reopen`.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            meeting_assistant::tray::show_window(app);
        }))
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
            commands::delete_whisper_model,
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
            commands::window_urls,
            commands::close_setup,
            commands::set_api_key,
            commands::has_api_key,
            commands::open_path,
            commands::open_sound_settings,
            commands::nudge_main_height,
            commands::open_settings,
            commands::close_settings,
            commands::startup_check,
            commands::list_jobs,
            commands::is_processing_paused,
            commands::set_processing_paused,
            commands::discard_job,
            commands::retry_job,
        ])
        .setup(|app| {
            meeting_assistant::tray::init(app.handle())?;

            // Pick up anything left unfinished, before the worker starts looking.
            //
            // Meetings survive a quit because their audio and their state file
            // are both on disk from the moment recording stops. Without this the
            // work would simply be forgotten — and worse, silently: the folder
            // would sit there looking like a finished meeting with no summary.
            let state = tauri::Manager::state::<AppState>(app);
            let config = state.config_snapshot();
            state
                .queue
                .absorb(meeting_assistant::queue::scan(&config.output_folder));

            // Restored, not reset. The switch means "not now, do it tonight",
            // and a horizon that long has to survive closing the app.
            state.queue.set_paused(config.processing_paused);

            commands::spawn_worker(app.handle().clone());
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

            // No "still running in the menu bar" notice. It was emitted as the
            // window hid, so it landed in a status line nobody could see and
            // was only ever read later, out of context, on reopening. The tray
            // icon is the affordance that says the app is still there.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }

            // Moving the window to a display with a different scale factor
            // leaves its size pin resolved against the OLD scale — see
            // `reapply_main_size_pin`. Windows then enforces a clamp that no
            // longer matches the window, and the layout is squeezed.
            if let tauri::WindowEvent::ScaleFactorChanged { .. } = event {
                meeting_assistant::commands::reapply_main_size_pin(window);
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
