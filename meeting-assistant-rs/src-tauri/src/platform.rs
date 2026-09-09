//! The few places where the two platforms genuinely differ outside audio.
//!
//! Everything here is a small shim over a path or a process launch. If a `cfg`
//! is needed anywhere other than this file or `audio/devices.rs`, it is in the
//! wrong place.

use std::path::PathBuf;

/// Per-user application data directory.
///
/// The Python kept everything beside `app.py` (`APP_FOLDER`, `app.py:52`),
/// which works for a folder you unzip but not for a signed `.app` bundle:
/// the bundle is read-only in the general case and macOS expects app state
/// under Application Support.
pub fn app_data_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        home.join("Library/Application Support/MeetingAssistant")
    }

    #[cfg(target_os = "windows")]
    {
        let base = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        base.join("MeetingAssistant")
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        std::env::temp_dir().join("MeetingAssistant")
    }
}

/// The user's documents directory, where recordings default to.
///
/// Falls back to the home directory if `Documents` does not exist — some
/// systems localize or relocate it — and to a temp directory as a last resort,
/// because failing to resolve this must never stop the app from starting.
pub fn documents_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from);

    #[cfg(not(target_os = "windows"))]
    let home = std::env::var_os("HOME").map(PathBuf::from);

    let Some(home) = home else {
        return std::env::temp_dir();
    };

    let documents = home.join("Documents");
    if documents.is_dir() {
        documents
    } else {
        home
    }
}

/// Where Whisper GGUF weights are cached.
///
/// The Python relied on the Hugging Face cache via `scan_cache_dir`
/// (`app.py:695`) because faster-whisper downloaded through `huggingface_hub`.
/// whisper.cpp takes a plain file path, so the port owns the directory.
pub fn models_dir() -> PathBuf {
    app_data_dir().join("models")
}

/// The `ollama` executable, or `None` if it cannot be found.
///
/// Port of `find_ollama_executable` (`app.py:770`), which searched `PATH` and
/// then the usual Windows install locations. The macOS equivalents are
/// Homebrew (both architectures) and the app bundle's bundled binary.
/// # `PATH` is trusted, deliberately
///
/// A hostile `ollama` earlier in `PATH` would be launched by [`start_ollama`].
/// `PATH` is searched **first** on purpose: a user who has built or installed
/// Ollama somewhere of their own choosing means it, and preferring the Homebrew
/// or bundle paths would silently ignore that.
///
/// The trade-off is accepted because writing to a directory on `PATH` already
/// implies code execution as this user, so it grants an attacker nothing they
/// did not already have. Do not "harden" this by reordering without weighing
/// that against breaking legitimate custom installs.
pub fn find_ollama() -> Option<PathBuf> {
    if let Some(found) = which("ollama") {
        return Some(found);
    }

    let candidates: Vec<PathBuf> = {
        #[cfg(target_os = "macos")]
        {
            vec![
                PathBuf::from("/opt/homebrew/bin/ollama"),
                PathBuf::from("/usr/local/bin/ollama"),
                PathBuf::from("/Applications/Ollama.app/Contents/Resources/ollama"),
            ]
        }

        #[cfg(target_os = "windows")]
        {
            let mut candidates = Vec::new();
            if let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
                candidates.push(local.join("Programs/Ollama/ollama.exe"));
                candidates.push(local.join("Ollama/ollama.exe"));
            }
            if let Some(program_files) = std::env::var_os("ProgramFiles").map(PathBuf::from) {
                candidates.push(program_files.join("Ollama/ollama.exe"));
            }
            candidates
        }

        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            Vec::new()
        }
    };

    candidates.into_iter().find(|c| c.exists())
}

/// Start Ollama in the background.
///
/// On macOS the daemon is owned by the `.app`, so `open -a` is the supported
/// way in; running the CLI binary directly would leave a child process tied to
/// our lifetime. On Windows the executable is launched directly, as the Python
/// did.
pub fn start_ollama() -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        // Imported here rather than at the top of the file: `Path` is used only
        // by this macOS-only block, and a top-level import is dead weight on
        // Windows — which the compiler says so, every build.
        use std::path::Path;

        if Path::new("/Applications/Ollama.app").exists() {
            std::process::Command::new("open")
                .args(["-a", "Ollama"])
                .spawn()?;
            return Ok(());
        }
    }

    let Some(exe) = find_ollama() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "ollama executable not found",
        ));
    };

    let mut command = std::process::Command::new(exe);
    command.arg("serve");

    // Without this, `ollama serve` inherits a console and Windows opens a
    // terminal window in the user's face, full of GIN request logs, for the
    // lifetime of the server. It is a background service the app started on the
    // user's behalf; they did not ask to watch it run.
    //
    // The flag suppresses the console only. The server still starts, still logs
    // to its own file, and `list_models` still reaches it on 127.0.0.1:11434.
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    command.spawn()?;
    Ok(())
}

/// Minimal `shutil.which`. Avoids a dependency for one lookup.
fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

/// The URI that opens the OS sound settings.
///
/// Both are URI schemes rather than executables, so opening one goes through
/// the same OS handler a link would and needs no shell — see `open_path` in
/// `commands.rs` for why that matters here.
///
/// **Windows**: `ms-settings:sound` is the Sound page, which carries both
/// Output and Input. `ms-settings:sound-devices` exists but lands on "Manage
/// sound devices", which is one level away from the volume sliders a user
/// looking for a quiet microphone actually wants.
///
/// **macOS**: verified against `Sound.appex`, whose `Info.plist` declares
/// `allowsXAppleSystemPreferencesURLScheme` and the bundle identifier below.
/// Pre-Ventura the pane was `com.apple.preference.sound`, still recorded there
/// as `legacyBundleIdentifier`; this app requires a newer macOS than that.
pub fn sound_settings_uri() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "ms-settings:sound"
    }

    #[cfg(target_os = "macos")]
    {
        "x-apple.systempreferences:com.apple.Sound-Settings.extension"
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        ""
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Recordings must not default beside the binary. The Python's
    /// `APP_FOLDER / "meetings"` put them wherever the app happened to live —
    /// which on the maintainer's machine was inside OneDrive.
    #[test]
    fn documents_dir_is_not_the_app_bundle() {
        let documents = documents_dir();
        assert!(documents.is_absolute(), "got {}", documents.display());
        assert!(!documents.starts_with(app_data_dir()));
    }

    #[test]
    fn model_dir_sits_under_app_data() {
        assert!(models_dir().starts_with(app_data_dir()));
        assert!(models_dir().ends_with("models"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_app_data_is_under_application_support() {
        let dir = app_data_dir();
        assert!(
            dir.to_string_lossy().contains("Library/Application Support"),
            "unexpected app data dir: {}",
            dir.display()
        );
    }

    /// `which` underpins Ollama discovery; a wrong result there shows up as
    /// "Ollama is not installed" on a machine where it plainly is.
    #[test]
    fn which_finds_a_binary_that_exists_and_misses_one_that_does_not() {
        assert!(which("sh").is_some() || which("cmd.exe").is_some());
        assert!(which("definitely-not-a-real-binary-xyzzy").is_none());
    }
}

