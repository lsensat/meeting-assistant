//! The teleprompter window: a frameless, transparent, always-on-top overlay
//! that scrolls a script, hidden from screen capture where the OS allows it.
//!
//! The pure parts (settings, clamping, key terms, hover timing, resize maths)
//! live in `meeting_core::teleprompter` and are tested there. This file owns the
//! window, the file on disk and the polling thread.
//!
//! # Click-through is decided here, not in the page
//!
//! A click-through window delivers no mouse events to its webview, so the page
//! cannot tell when the cursor comes back. A thread therefore polls the cursor
//! every [`POLL`] and flips `set_ignore_cursor_events`; the page is only told
//! the outcome through [`EV_TELEPROMPTER_HOVER`] and reports back whether it
//! needs to stay clickable (`pinned`: an editor or panel is open).
//!
//! # Locking rule
//!
//! Never hold `stored`, or any other mutex in [`TeleprompterState`], while
//! calling a window method. Commands run on the main thread and window calls
//! from other threads wait for it, so a lock held across such a call is a
//! deadlock waiting for the right interleaving. The window-event handler below
//! calls no window method at all, for the same reason.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use meeting_core::teleprompter::{
    self as core, Geometry, Hover, Rect, Settings, Stored, DEFAULT_HEIGHT, DEFAULT_WIDTH,
    MEETING_SCRIPT_MAX_CHARS, MIN_HEIGHT, MIN_WIDTH,
};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::state::AppState;

/// The window's label. Also listed in `capabilities/default.json`: a window
/// whose label is missing there cannot call a single command.
pub const LABEL: &str = "teleprompter";

/// Emitted to the teleprompter window only, with a `bool`, when the debounced
/// "cursor is over the window" state changes.
pub const EV_TELEPROMPTER_HOVER: &str = "teleprompter_hover";

const POLL: Duration = Duration::from_millis(50);
/// A moved or resized window is written to disk this long after it settles.
const GEOMETRY_FLUSH_AFTER: Duration = Duration::from_millis(600);
/// Imported scripts larger than this are refused rather than read.
const IMPORT_MAX_BYTES: u64 = 2 * 1024 * 1024;

/// Logical `(x, y, width, height)` of the window, and the edge being dragged.
type ResizeOrigin = ((f64, f64, f64, f64), String);

pub struct TeleprompterState {
    file: PathBuf,
    stored: Mutex<Stored>,
    /// The page has an editor or panel open, or no script: stay clickable.
    pinned: AtomicBool,
    /// Set when the user re-selects the window to recover it: the poll loop
    /// keeps it interactive until the next genuine hover transition to true.
    force_interactive: AtomicBool,
    /// The window was open at some point while a recording ran.
    seen_during_recording: AtomicBool,
    /// Logical `(x, y, width, height)` and edge when a resize drag began, for
    /// the platforms where the OS cannot resize a frameless window for us.
    resize_origin: Mutex<Option<ResizeOrigin>>,
    geometry_dirty_at: Mutex<Option<Instant>>,
    /// Which poll thread is current. A thread whose number is stale exits, so
    /// closing and quickly reopening the window never leaves two running.
    poll_generation: AtomicU64,
}

impl TeleprompterState {
    /// Read `teleprompter.json`. A missing or damaged file yields defaults.
    pub fn load(file: PathBuf) -> Self {
        let stored = std::fs::read_to_string(&file)
            .map(|text| Stored::from_json(&text))
            .unwrap_or_else(|_| Stored::defaults());
        Self {
            file,
            stored: Mutex::new(stored),
            pinned: AtomicBool::new(false),
            force_interactive: AtomicBool::new(false),
            seen_during_recording: AtomicBool::new(false),
            resize_origin: Mutex::new(None),
            geometry_dirty_at: Mutex::new(None),
            poll_generation: AtomicU64::new(0),
        }
    }

    pub fn settings(&self) -> Settings {
        self.stored.lock().expect("teleprompter poisoned").settings.clone()
    }

    /// Clamp, remember and write. Returns what was actually kept.
    pub fn save(&self, settings: Settings) -> Result<Settings, String> {
        let clamped = settings.clamped();
        let mut stored = self.stored.lock().expect("teleprompter poisoned");
        stored.settings = clamped.clone();
        self.write(&stored)?;
        drop(stored);
        *self.geometry_dirty_at.lock().expect("teleprompter poisoned") = None;
        Ok(clamped)
    }

    /// Remember where the window is. Written once it has stayed put for
    /// [`GEOMETRY_FLUSH_AFTER`], so a drag is one write rather than hundreds.
    pub fn note_geometry(&self, geometry: Geometry) {
        let mut stored = self.stored.lock().expect("teleprompter poisoned");
        if stored.geometry == Some(geometry) {
            return;
        }
        stored.geometry = Some(geometry);
        drop(stored);
        *self.geometry_dirty_at.lock().expect("teleprompter poisoned") = Some(Instant::now());
    }

    /// Write everything now. Failures are logged, not raised: this runs while a
    /// window is closing and there is nobody left to tell.
    pub fn flush(&self) {
        let stored = self.stored.lock().expect("teleprompter poisoned");
        if let Err(e) = self.write(&stored) {
            eprintln!("[teleprompter] {e}");
        }
        drop(stored);
        *self.geometry_dirty_at.lock().expect("teleprompter poisoned") = None;
    }

    /// Through a `.partial` and a rename, as `queue::save` does, so a crash
    /// mid-write cannot leave half a script. Called with `stored` held, which
    /// also serialises writers.
    fn write(&self, stored: &Stored) -> Result<(), String> {
        let partial = self.file.with_extension("json.partial");
        std::fs::write(&partial, stored.to_json())
            .and_then(|()| std::fs::rename(&partial, &self.file))
            .map_err(|e| format!("could not save the teleprompter: {e}"))
    }

    /// A recording has just started.
    pub fn note_recording_started(&self, window_open: bool) {
        // OR, not store: a window opened between the session starting and this
        // call has already marked the recording as seen.
        self.seen_during_recording.fetch_or(window_open, Ordering::Relaxed);
    }

    /// The window was opened while a recording ran.
    pub fn mark_seen(&self) {
        self.seen_during_recording.store(true, Ordering::Relaxed);
    }

    /// The script to attach to the meeting that just finished, if there is one.
    ///
    /// Only when the window was open during the recording, the user allowed it,
    /// and there is something to use. **Always** clears the flag, so a script
    /// can never leak into the next meeting.
    pub fn take_for_meeting(&self) -> Option<String> {
        if !self.seen_during_recording.swap(false, Ordering::Relaxed) {
            return None;
        }
        let settings = self.settings();
        if settings.use_in_pipeline && !settings.script.trim().is_empty() {
            Some(core::truncate_chars(&settings.script, MEETING_SCRIPT_MAX_CHARS).to_string())
        } else {
            None
        }
    }

    fn geometry_is_stale(&self) -> bool {
        self.geometry_dirty_at
            .lock()
            .expect("teleprompter poisoned")
            .is_some_and(|at| at.elapsed() >= GEOMETRY_FLUSH_AFTER)
    }
}

// --- opening ---------------------------------------------------------------

/// Open the teleprompter, or bring it forward if it is already open.
///
/// # `(async)` is load-bearing
///
/// Building a webview window from a synchronous command deadlocks on Windows;
/// see the long note on `commands::open_setup`. The tray calls this from its own
/// thread for the same reason.
#[tauri::command(async)]
pub fn open_teleprompter(app: AppHandle) -> Result<(), String> {
    if let Some(existing) = app.get_webview_window(LABEL) {
        // Escape hatch: re-selecting the item always recovers a stuck window.
        let state = app.state::<AppState>();
        state.teleprompter.force_interactive.store(true, Ordering::SeqCst);
        let _ = existing.unminimize();
        existing.show().map_err(|e| e.to_string())?;
        existing.set_focus().map_err(|e| e.to_string())?;
        existing.set_ignore_cursor_events(false).map_err(|e| e.to_string())?;
        return Ok(());
    }

    let state = app.state::<AppState>();
    // A fresh window starts with no pinning claim until the page reports one.
    state.teleprompter.pinned.store(false, Ordering::Relaxed);
    state.teleprompter.force_interactive.store(false, Ordering::SeqCst);
    let geometry = state.teleprompter.stored.lock().expect("teleprompter poisoned").geometry;
    let (width, height) = geometry
        .map(|g| (g.width, g.height))
        .unwrap_or((DEFAULT_WIDTH as f64, DEFAULT_HEIGHT as f64));

    let window = tauri::WebviewWindowBuilder::new(
        &app,
        LABEL,
        tauri::WebviewUrl::App("teleprompter.html".into()),
    )
    .title("Teleprompter")
    // Frameless and see-through: the page draws its own translucent backdrop.
    // Transparency needs `macOSPrivateApi` on macOS (tauri.conf.json + the
    // `macos-private-api` feature, which must be changed together).
    .transparent(true)
    .decorations(false)
    .shadow(false)
    .always_on_top(true)
    .visible_on_all_workspaces(true)
    .skip_taskbar(true)
    // Excluded from screenshots and screen sharing where the OS supports it.
    .content_protected(true)
    // The first click on an unfocused window should act, not just focus it.
    .accept_first_mouse(true)
    .resizable(true)
    .min_inner_size(MIN_WIDTH as f64, MIN_HEIGHT as f64)
    .inner_size(width, height)
    // Shown only once positioned, so it never flashes in the wrong place.
    .visible(false)
    .focused(true)
    // No `match_title_bar`: there is no title bar.
    .build()
    .map_err(|e| e.to_string())?;

    let monitors: Vec<Rect> = app
        .available_monitors()
        .unwrap_or_default()
        .iter()
        .map(|m| Rect {
            x: m.position().x,
            y: m.position().y,
            width: m.size().width,
            height: m.size().height,
        })
        .collect();
    match geometry {
        Some(g) if core::on_any_monitor(g.x, g.y, &monitors) => {
            let _ = window.set_position(tauri::PhysicalPosition::new(g.x, g.y));
        }
        // Saved on a monitor that is no longer there, or never saved.
        _ => {
            let _ = window.center();
        }
    }
    window.show().map_err(|e| e.to_string())?;
    let _ = window.set_focus();
    crate::commands::debug_inspect(&window);

    // Flush on close. No window method is called from here: this runs on the
    // main thread in the middle of event dispatch.
    let handle = app.clone();
    window.on_window_event(move |event| {
        if matches!(
            event,
            tauri::WindowEvent::CloseRequested { .. } | tauri::WindowEvent::Destroyed
        ) {
            handle.state::<AppState>().teleprompter.flush();
        }
    });

    if state.is_recording() {
        state.teleprompter.mark_seen();
    }

    let generation = state.teleprompter.poll_generation.fetch_add(1, Ordering::SeqCst) + 1;
    let poll_app = app.clone();
    std::thread::spawn(move || poll(poll_app, generation));
    Ok(())
}

/// Watch the cursor and the window until the window goes away.
///
/// One thread does everything that touches the window repeatedly: hover
/// debouncing, click-through, hover events and remembering the geometry. Doing
/// it here, rather than from `WindowEvent::Moved`/`Resized` handlers, keeps
/// window getters out of event dispatch.
fn poll(app: AppHandle, generation: u64) {
    let started = Instant::now();
    let mut hover = Hover::new();
    // A new window accepts mouse events.
    let mut ignoring = false;
    let mut tracking = true;

    loop {
        std::thread::sleep(POLL);

        let state = app.state::<AppState>();
        if state.teleprompter.poll_generation.load(Ordering::SeqCst) != generation {
            return;
        }
        let Some(window) = app.get_webview_window(LABEL) else {
            return;
        };

        let scale = window.scale_factor().unwrap_or(1.0);
        let (Ok(position), Ok(outer), Ok(inner)) =
            (window.outer_position(), window.outer_size(), window.inner_size())
        else {
            continue;
        };

        // Remember where the window is. A minimised Windows window reports a
        // position near -32000; that is not a place to come back to.
        if position.x > -30_000 && position.y > -30_000 {
            let logical = inner.to_logical::<f64>(scale);
            state.teleprompter.note_geometry(Geometry {
                x: position.x,
                y: position.y,
                width: logical.width,
                height: logical.height,
            });
        }
        if state.teleprompter.geometry_is_stale() {
            state.teleprompter.flush();
        }

        // Without a cursor position (some Wayland sessions) the window is never
        // made click-through: a window that cannot be clicked and cannot tell
        // when it should be again is worse than one that stays in the way.
        let was_tracking = tracking;
        let inside = match app.cursor_position() {
            Ok(cursor) => {
                tracking = true;
                let primary_scale = app
                    .primary_monitor()
                    .ok()
                    .flatten()
                    .map(|m| m.scale_factor())
                    .unwrap_or(scale);
                core::cursor_over_window(
                    std::env::consts::OS == "macos",
                    (cursor.x, cursor.y),
                    (position.x, position.y),
                    (outer.width, outer.height),
                    scale,
                    primary_scale,
                )
            }
            Err(_) => {
                tracking = false;
                true
            }
        };

        let now_ms = started.elapsed().as_millis() as u64;
        let changed = if tracking {
            hover.tick(inside, now_ms)
        } else if !hover.hovered || was_tracking {
            // Lost tracking: show the controls and say so once.
            let was_hovered = hover.hovered;
            hover = Hover::new();
            (!was_hovered).then_some(true)
        } else {
            None
        };
        if let Some(hovered) = changed {
            let _ = app.emit_to(LABEL, EV_TELEPROMPTER_HOVER, hovered);
            // A genuine hover ends the recovery override.
            if hovered {
                state.teleprompter.force_interactive.store(false, Ordering::SeqCst);
            }
        }

        let pinned = state.teleprompter.pinned.load(Ordering::Relaxed);
        let forced = state.teleprompter.force_interactive.load(Ordering::SeqCst);
        let want = !forced && core::ignore_cursor(hover.hovered, pinned, tracking);
        if want != ignoring && window.set_ignore_cursor_events(want).is_ok() {
            ignoring = want;
        }
    }
}

// --- commands --------------------------------------------------------------

#[tauri::command]
pub fn close_teleprompter(app: AppHandle) -> Result<(), String> {
    app.state::<AppState>().teleprompter.flush();
    if let Some(window) = app.get_webview_window(LABEL) {
        window.close().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub fn get_teleprompter(state: State<AppState>) -> Settings {
    state.teleprompter.settings()
}

#[tauri::command]
pub fn save_teleprompter(settings: Settings, state: State<AppState>) -> Result<Settings, String> {
    state.teleprompter.save(settings).map_err(|e| {
        eprintln!("[teleprompter] {e}");
        meeting_core::i18n::tr(state.config_snapshot().language, "teleprompter_save_failed")
            .to_string()
    })
}

/// Ask for a script file and return its text, without saving it.
///
/// The dialog and the read happen here: the page has the dialog plugin but no
/// filesystem permission, and a path coming from the page is not to be trusted.
/// `(async)` because `blocking_pick_file` must not run on the main thread.
#[tauri::command(async)]
pub fn import_teleprompter_script(app: AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;

    let mut dialog = app
        .dialog()
        .file()
        .add_filter("Script", &["md", "markdown", "txt"]);
    if let Some(window) = app.get_webview_window(LABEL) {
        dialog = dialog.set_parent(&window);
    }
    let Some(picked) = dialog.blocking_pick_file() else {
        return Ok(None);
    };
    let path = picked.into_path().map_err(|e| e.to_string())?;

    let size = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
    if size > IMPORT_MAX_BYTES {
        let language = app.state::<AppState>().config_snapshot().language;
        return Err(meeting_core::i18n::tr_args(
            language,
            "teleprompter_import_too_large",
            &[
                ("size", &format!("{:.1}", size as f64 / (1024.0 * 1024.0))),
                ("limit", &(IMPORT_MAX_BYTES / (1024 * 1024)).to_string()),
            ],
        ));
    }
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&bytes);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    Ok(Some(text.replace("\r\n", "\n")))
}

/// The page reports whether it needs to stay clickable. The poll thread picks
/// it up on its next tick, so it takes effect within [`POLL`].
#[tauri::command]
pub fn teleprompter_set_pinned(pinned: bool, state: State<AppState>) {
    state.teleprompter.pinned.store(pinned, Ordering::Relaxed);
}

#[tauri::command]
pub fn teleprompter_start_drag(app: AppHandle) -> Result<(), String> {
    let window = app.get_webview_window(LABEL).ok_or("the teleprompter is not open")?;
    window.start_dragging().map_err(|e| e.to_string())
}

fn direction(edge: &str) -> Option<tauri_runtime::ResizeDirection> {
    use tauri_runtime::ResizeDirection as D;
    Some(match edge {
        "n" => D::North,
        "s" => D::South,
        "e" => D::East,
        "w" => D::West,
        "ne" => D::NorthEast,
        "nw" => D::NorthWest,
        "se" => D::SouthEast,
        "sw" => D::SouthWest,
        _ => return None,
    })
}

/// Begin resizing from an edge or corner (`n`, `s`, `e`, `w`, `ne`, `nw`, `se`,
/// `sw`). `Ok(true)`: the OS is doing it. `Ok(false)`: it cannot, and the page
/// must drive the resize with [`teleprompter_resize_by`].
///
/// macOS cannot resize a frameless window by dragging, and tao reports that as
/// an error. But `start_resize_dragging` only *sends* the request and ignores
/// the result, so the error never reaches us and the call returns `Ok` while
/// doing nothing. macOS is therefore recognised by name instead of by failure.
#[tauri::command]
pub fn teleprompter_start_resize(
    app: AppHandle,
    direction: String,
    state: State<AppState>,
) -> Result<bool, String> {
    let dir = self::direction(&direction).ok_or_else(|| format!("unknown edge {direction:?}"))?;
    let window = app.get_webview_window(LABEL).ok_or("the teleprompter is not open")?;

    let scale = window.scale_factor().map_err(|e| e.to_string())?;
    let position = window
        .outer_position()
        .map_err(|e| e.to_string())?
        .to_logical::<f64>(scale);
    let size = window
        .inner_size()
        .map_err(|e| e.to_string())?
        .to_logical::<f64>(scale);
    *state.teleprompter.resize_origin.lock().expect("teleprompter poisoned") =
        Some(((position.x, position.y, size.width, size.height), direction));

    if std::env::consts::OS == "macos" {
        return Ok(false);
    }
    // `WebviewWindow` does not forward this; the `Window` underneath has it.
    // (`Manager::get_window` is behind tauri's `unstable` feature.)
    let host = AsRef::<tauri::Webview>::as_ref(&window).window();
    Ok(host.start_resize_dragging(dir).is_ok())
}

/// Resize by the cumulative logical distance the pointer has moved since
/// [`teleprompter_start_resize`]. Only used where the OS cannot do it.
#[tauri::command]
pub fn teleprompter_resize_by(
    app: AppHandle,
    dx: f64,
    dy: f64,
    state: State<AppState>,
) -> Result<(), String> {
    let origin = state
        .teleprompter
        .resize_origin
        .lock()
        .expect("teleprompter poisoned")
        .clone();
    let Some((start, edge)) = origin else {
        return Ok(());
    };
    if !dx.is_finite() || !dy.is_finite() {
        return Ok(());
    }
    let window = app.get_webview_window(LABEL).ok_or("the teleprompter is not open")?;

    let (x, y, width, height) = core::resize_rect(start, &edge, dx, dy);
    window
        .set_position(tauri::LogicalPosition::new(x, y))
        .map_err(|e| e.to_string())?;
    window
        .set_size(tauri::LogicalSize::new(width, height))
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ma-teleprompter-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir.join("teleprompter.json")
    }

    fn with_script(state: &TeleprompterState, script: &str, use_in_pipeline: bool) {
        state
            .save(Settings {
                script: script.into(),
                use_in_pipeline,
                ..Settings::defaults()
            })
            .expect("save");
    }

    #[test]
    fn a_missing_file_loads_defaults_and_saving_persists() {
        let file = temp_file("persist");
        let state = TeleprompterState::load(file.clone());
        assert_eq!(state.settings(), Settings::defaults());

        with_script(&state, "hello", true);
        state.note_geometry(Geometry { x: 1, y: 2, width: 300.0, height: 200.0 });
        state.flush();

        let again = TeleprompterState::load(file);
        assert_eq!(again.settings().script, "hello");
        assert_eq!(
            again.stored.lock().unwrap().geometry,
            Some(Geometry { x: 1, y: 2, width: 300.0, height: 200.0 })
        );
    }

    #[test]
    fn save_returns_the_clamped_settings() {
        let state = TeleprompterState::load(temp_file("clamp"));
        let kept = state
            .save(Settings { font_size: 9999, ..Settings::defaults() })
            .expect("save");
        assert_eq!(kept.font_size, core::FONT_MAX);
        assert_eq!(state.settings(), kept);
    }

    #[test]
    fn the_script_reaches_a_meeting_only_when_it_was_seen_and_allowed() {
        let state = TeleprompterState::load(temp_file("take"));
        with_script(&state, "Acme Corp", true);

        // Never open during a recording.
        state.note_recording_started(false);
        assert_eq!(state.take_for_meeting(), None);

        // Open at the start.
        state.note_recording_started(true);
        assert_eq!(state.take_for_meeting().as_deref(), Some("Acme Corp"));
        // ...and the flag is spent: the next meeting gets nothing.
        assert_eq!(state.take_for_meeting(), None);

        // Opened part-way through.
        state.note_recording_started(false);
        state.mark_seen();
        assert_eq!(state.take_for_meeting().as_deref(), Some("Acme Corp"));
    }

    #[test]
    fn the_flag_is_spent_even_when_nothing_is_attached() {
        let state = TeleprompterState::load(temp_file("spent"));

        with_script(&state, "Acme", false);
        state.note_recording_started(true);
        assert_eq!(state.take_for_meeting(), None, "toggle off");

        with_script(&state, "Acme", true);
        assert_eq!(state.take_for_meeting(), None, "flag was spent by the last call");

        with_script(&state, "  \n ", true);
        state.note_recording_started(true);
        assert_eq!(state.take_for_meeting(), None, "blank script");
    }

    #[test]
    fn the_meeting_copy_is_capped() {
        let state = TeleprompterState::load(temp_file("cap"));
        with_script(&state, &"é".repeat(MEETING_SCRIPT_MAX_CHARS + 500), true);
        state.note_recording_started(true);
        let script = state.take_for_meeting().expect("script");
        assert_eq!(script.chars().count(), MEETING_SCRIPT_MAX_CHARS);
    }

    #[test]
    fn unchanged_geometry_does_not_mark_the_file_dirty() {
        let state = TeleprompterState::load(temp_file("dirty"));
        let g = Geometry { x: 0, y: 0, width: 400.0, height: 200.0 };
        state.note_geometry(g);
        assert!(state.geometry_dirty_at.lock().unwrap().is_some());
        state.flush();
        assert!(state.geometry_dirty_at.lock().unwrap().is_none());
        state.note_geometry(g);
        assert!(state.geometry_dirty_at.lock().unwrap().is_none());
    }

    #[test]
    fn every_edge_maps_to_a_direction() {
        for edge in ["n", "s", "e", "w", "ne", "nw", "se", "sw"] {
            assert!(direction(edge).is_some(), "{edge}");
        }
        assert!(direction("up").is_none());
        assert!(direction("").is_none());
    }
}
