//! Menu bar / system tray, so a meeting can run without a window on screen.
//!
//! # This is a second front-end, not a shortcut bar
//!
//! It can change the same state the window can, which is why the commands emit
//! [`EV_RECORDING_STATE`] and [`EV_MUTE_STATE`]: both front-ends react to the
//! events rather than to their own clicks, so neither can drift from the truth
//! in `AppState`.
//!
//! # Two items deliberately show the window instead of acting directly
//!
//! **Finalize** needs a meeting title and **Cancel** needs a confirmation, and
//! a native menu can collect neither. Rather than pass an empty title (silently
//! dropping the feature) or duplicate the dialogs natively, both reveal the
//! window and hand off to the flow already implemented there. One code path,
//! one set of translations, one place where the confirmation wording lives.
//!
//! # Language changes require a rebuild
//!
//! Unlike the DOM there is no `data-i18n` walk here: the strings are baked into
//! the menu when it is built. [`rebuild`] is called when the language changes.

use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{AppHandle, Emitter, Manager};

use meeting_core::config::Language;
use meeting_core::i18n;

use crate::audio::devices::{self, SourceKind};
use crate::state::AppState;

/// Asks the main window to run its stop flow, including the title prompt.
pub const EV_REQUEST_STOP: &str = "request_stop";
/// Asks the main window to run its cancel flow, including the confirmation.
pub const EV_REQUEST_CANCEL: &str = "request_cancel";

const ID_STATUS: &str = "status";
const ID_START: &str = "start";
const ID_FINALIZE: &str = "finalize";
const ID_CANCEL: &str = "cancel";
const ID_MUTE: &str = "mute";
const ID_FOLDER: &str = "folder";
const ID_SETTINGS: &str = "settings";
const ID_SHOW: &str = "show";
const ID_QUIT: &str = "quit";
/// Device items are `mic:<name>` / `sys:<name>`, so the handler can recover
/// which list a click came from and what it selected.
const PREFIX_MIC: &str = "mic:";
const PREFIX_SYS: &str = "sys:";

/// Build the tray and attach it to the app. Called once at startup.
pub fn init(app: &AppHandle) -> tauri::Result<TrayIcon> {
    let menu = build_menu(app)?;

    // The two platforms want opposite images here, so this is not one icon
    // with a flag — it is two icons.
    //
    // macOS: a dedicated monochrome glyph, NOT the app icon. `icon_as_template`
    // uses only the ALPHA channel and repaints the silhouette to suit a light or
    // dark menu bar. The app icon's alpha is a filled rounded square, so passing
    // it here painted a solid black block. `tray.png` is transparent except for
    // the microphone itself.
    //
    // Windows: there is no template concept — `icon_as_template` is ignored and
    // the image is drawn as it is. `tray.png` is pure black, so on the default
    // dark taskbar it rendered as nothing at all: the icon was reported missing
    // from the Windows notification area. The colour app icon is what shows up.
    #[cfg(target_os = "macos")]
    let (icon, as_template) = (
        tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))
            .expect("tray glyph is a valid PNG"),
        true,
    );
    #[cfg(not(target_os = "macos"))]
    let (icon, as_template) = (
        tauri::image::Image::from_bytes(include_bytes!("../icons/32x32.png"))
            .expect("app icon is a valid PNG"),
        false,
    );

    TrayIconBuilder::with_id("main")
        .icon(icon)
        .icon_as_template(as_template)
        // Windows shows nothing on hover without this, which reads as a
        // stray unidentified icon among a dozen others in the notification area.
        .tooltip("Meeting Assistant")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(handle_menu_event)
        .build(app)
}

/// Rebuild the whole menu: after a language change, or when the device list or
/// recording state changes.
pub fn rebuild(app: &AppHandle) {
    if let Some(tray) = app.tray_by_id("main") {
        if let Ok(menu) = build_menu(app) {
            let _ = tray.set_menu(Some(menu));
        }
    }
}

/// The live elapsed time goes in the tooltip, not the menu.
///
/// Tauri v2 has no "menu will open" hook, so a live status *item* would mean
/// rewriting a menu item once a second — wasteful, and it flickers on some
/// platforms. The tooltip updates cheaply and is visible on hover.
pub fn set_tooltip(app: &AppHandle, text: &str) {
    if let Some(tray) = app.tray_by_id("main") {
        let _ = tray.set_tooltip(Some(text));
    }
}

fn build_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let state = app.state::<AppState>();
    let config = state.config_snapshot();
    let language = config.language;
    let recording = state.is_recording();
    let processing = state.is_processing();

    let tr = |key: &str| i18n::tr(language, key).to_string();

    // Status line: disabled, so it reads as a label rather than an action.
    let status_text = if recording {
        tr("tray_recording")
    } else if processing {
        tr("tray_processing")
    } else {
        tr("tray_idle")
    };
    let status = MenuItem::with_id(app, ID_STATUS, status_text, false, None::<&str>)?;

    // Enabled whenever nothing is being recorded — a queued or running meeting
    // no longer blocks a new one.
    //
    // This used to be `!recording && !processing`, which refused to start a
    // meeting while an earlier one was still transcribing. The main window
    // allowed it, so the two disagreed; and the queue exists precisely so that
    // more than one meeting can be in flight. Now that a recording holds the
    // queue rather than competing with it, refusing the recording is backwards:
    // the recording is the thing that cannot be repeated.
    let start = MenuItem::with_id(app, ID_START, tr("tray_start"), !recording, None::<&str>)?;
    let finalize = MenuItem::with_id(app, ID_FINALIZE, tr("tray_finalize"), recording, None::<&str>)?;
    let cancel = MenuItem::with_id(app, ID_CANCEL, tr("tray_cancel"), recording, None::<&str>)?;

    // Mute is available at rest as well as mid-meeting, matching the window.
    let mute = CheckMenuItem::with_id(
        app,
        ID_MUTE,
        tr("tray_mute"),
        true,
        state.is_muted(),
        None::<&str>,
    )?;

    let mic_menu = device_submenu(
        app,
        language,
        SourceKind::Microphone,
        &tr("tray_microphone"),
        PREFIX_MIC,
        &config.microphone_name,
    )?;
    let sys_menu = device_submenu(
        app,
        language,
        SourceKind::SystemAudio,
        &tr("tray_computer_audio"),
        PREFIX_SYS,
        &config.system_audio_name,
    )?;

    let folder = MenuItem::with_id(app, ID_FOLDER, tr("tray_open_folder"), true, None::<&str>)?;
    let settings = MenuItem::with_id(app, ID_SETTINGS, tr("tray_settings"), true, None::<&str>)?;
    let show = MenuItem::with_id(app, ID_SHOW, tr("tray_show"), true, None::<&str>)?;
    let quit = MenuItem::with_id(app, ID_QUIT, tr("tray_quit"), true, None::<&str>)?;
    let sep = || PredefinedMenuItem::separator(app);

    Menu::with_items(
        app,
        &[
            &status,
            &sep()?,
            &start,
            &finalize,
            &cancel,
            &sep()?,
            &mute,
            &mic_menu,
            &sys_menu,
            &sep()?,
            &folder,
            &settings,
            &sep()?,
            &show,
            &quit,
        ],
    )
}

/// A submenu of devices with the current one ticked.
///
/// Hand-managed `CheckMenuItem`s rather than radio items: muda has no radio
/// variant, so mutual exclusion is ours to enforce — which happens naturally
/// because selecting one writes the config and rebuilds the menu.
fn device_submenu(
    app: &AppHandle,
    language: Language,
    kind: SourceKind,
    title: &str,
    prefix: &str,
    configured: &str,
) -> tauri::Result<Submenu<tauri::Wry>> {
    let snapshot = devices::snapshot(kind).ok();
    let devices_list = snapshot.map(|s| s.devices).unwrap_or_default();

    if devices_list.is_empty() {
        let empty_key = match kind {
            SourceKind::Microphone => "microphone_missing",
            SourceKind::SystemAudio => "computer_audio_missing",
        };
        let none = MenuItem::with_id(
            app,
            format!("{prefix}<none>"),
            i18n::tr(language, empty_key),
            false,
            None::<&str>,
        )?;
        return Submenu::with_items(app, title, true, &[&none]);
    }

    // The id keeps the RAW name — `handle_menu_event` strips the prefix and
    // writes what is left straight into the config, so a shortened name here
    // would select a device that does not exist. Only the visible text changes.
    let names: Vec<String> = devices_list.iter().map(|d| d.name.clone()).collect();
    let labels = meeting_core::devices::display_labels(&names);

    let mut items: Vec<CheckMenuItem<tauri::Wry>> = Vec::new();
    for (device, label) in devices_list.iter().zip(&labels) {
        items.push(CheckMenuItem::with_id(
            app,
            format!("{prefix}{}", device.name),
            label,
            true,
            device.name == configured,
            None::<&str>,
        )?);
    }

    let refs: Vec<&dyn tauri::menu::IsMenuItem<tauri::Wry>> =
        items.iter().map(|i| i as &dyn tauri::menu::IsMenuItem<tauri::Wry>).collect();
    Submenu::with_items(app, title, true, &refs)
}

fn handle_menu_event(app: &AppHandle, event: tauri::menu::MenuEvent) {
    let id = event.id.as_ref().to_string();

    if let Some(name) = id.strip_prefix(PREFIX_MIC) {
        select_device(app, Some(name.to_string()), None);
        return;
    }
    if let Some(name) = id.strip_prefix(PREFIX_SYS) {
        select_device(app, None, Some(name.to_string()));
        return;
    }

    match id.as_str() {
        ID_START => {
            let state = app.state::<AppState>();
            // Start needs no input, so unlike Finalize it can act directly.
            // Errors would otherwise be invisible with the window closed.
            if let Err(e) = crate::commands::start_recording(app.clone(), state) {
                let _ = app.emit(crate::commands::EV_ERROR, e);
                show_window(app);
            }
        }
        // Finalize and Cancel need a title and a confirmation respectively, and
        // a native menu can collect neither. Show the window and let it run the
        // flow it already implements.
        ID_FINALIZE => {
            show_window(app);
            let _ = app.emit(EV_REQUEST_STOP, ());
        }
        ID_CANCEL => {
            show_window(app);
            let _ = app.emit(EV_REQUEST_CANCEL, ());
        }
        ID_MUTE => {
            let state = app.state::<AppState>();
            let muted = state.toggle_muted();
            let _ = app.emit(crate::commands::EV_MUTE_STATE, muted);
            rebuild(app);
        }
        ID_FOLDER => {
            let folder = app
                .state::<AppState>()
                .current_folder
                .lock()
                .expect("folder poisoned")
                .clone()
                .unwrap_or_else(|| app.state::<AppState>().config_snapshot().output_folder);
            let _ = crate::commands::open_path(app.clone(), folder.to_string_lossy().into_owned());
        }
        ID_SETTINGS => {
            let _ = crate::commands::open_settings(app.clone());
        }
        ID_SHOW => show_window(app),
        ID_QUIT => app.exit(0),
        _ => {}
    }
}

fn select_device(app: &AppHandle, microphone: Option<String>, system: Option<String>) {
    let state = app.state::<AppState>();
    let mut config = state.config_snapshot();

    if let Some(name) = microphone {
        config.microphone_name = name;
    }
    if let Some(name) = system {
        config.system_audio_name = name;
    }

    if let Err(e) = std::fs::write(&state.config_file, config.to_json()) {
        let _ = app.emit(crate::commands::EV_LOG, format!("could not save config: {e}"));
        return;
    }
    *state.config.lock().expect("config poisoned") = config;
    rebuild(app);
}

/// Reveal the main window, whether it was hidden or merely behind something.
pub fn show_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}
