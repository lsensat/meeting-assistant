//! Persisted settings. Port of `load_config` / `write_config` in `app.py`.
//!
//! The Python reads the *widgets* as the source of truth when saving, so
//! `write_config` can only run on the Tk main thread. Here the `Config` struct
//! is the source of truth and the UI is just a view of it, which is what lets
//! a recording thread take an owned snapshot instead of reaching back into the
//! UI (deferred fixes #1 and #2).
//!
//! # Parity notes
//!
//! * A malformed or unreadable file falls back to **pure defaults**, silently,
//!   exactly as the Python's bare `except Exception: pass` does.
//! * Unknown keys in the file are ignored. The Python's `data.update(saved)`
//!   would carry them into the in-memory dict, but nothing ever read them, and
//!   they were dropped on the next save anyway.
//! * Out-of-range values clamp to defaults rather than failing the load.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// UI language. Anything else in the file clamps to English.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    En,
    Es,
}

impl Language {
    pub fn as_str(self) -> &'static str {
        match self {
            Language::En => "en",
            Language::Es => "es",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "en" => Some(Language::En),
            "es" => Some(Language::Es),
            _ => None,
        }
    }
}

/// Language forced on Whisper. `Auto` means let it detect, and maps to the
/// Python's `selected_transcription_language = None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TranscriptionLanguage {
    Auto,
    En,
    Es,
}

impl TranscriptionLanguage {
    pub fn as_str(self) -> &'static str {
        match self {
            TranscriptionLanguage::Auto => "auto",
            TranscriptionLanguage::En => "en",
            TranscriptionLanguage::Es => "es",
        }
    }

    /// The value to hand Whisper: `None` for auto-detect.
    pub fn whisper_code(self) -> Option<&'static str> {
        match self {
            TranscriptionLanguage::Auto => None,
            other => Some(other.as_str()),
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(TranscriptionLanguage::Auto),
            "en" => Some(TranscriptionLanguage::En),
            "es" => Some(TranscriptionLanguage::Es),
            _ => None,
        }
    }
}

/// Which prompt template drives the final summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SummaryType {
    MeetingMinutes,
    Executive,
    Actions,
    Brief,
    Custom,
}

impl SummaryType {
    pub const ALL: [SummaryType; 5] = [
        SummaryType::MeetingMinutes,
        SummaryType::Executive,
        SummaryType::Actions,
        SummaryType::Brief,
        SummaryType::Custom,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            SummaryType::MeetingMinutes => "meeting_minutes",
            SummaryType::Executive => "executive",
            SummaryType::Actions => "actions",
            SummaryType::Brief => "brief",
            SummaryType::Custom => "custom",
        }
    }

    /// Resolve a stored value to a variant.
    ///
    /// Accepts current ids *and* the display labels that very old configs
    /// stored, in both languages — port of the `old_summary_map` at
    /// `app.py:462`. Anything unrecognised returns `None` and the caller
    /// clamps to the default, matching the Python's final validation step.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            // Current ids.
            "meeting_minutes" => Some(SummaryType::MeetingMinutes),
            "executive" => Some(SummaryType::Executive),
            "actions" => Some(SummaryType::Actions),
            "brief" => Some(SummaryType::Brief),
            "custom" => Some(SummaryType::Custom),

            // Legacy Spanish display labels.
            "Acta de reunión" => Some(SummaryType::MeetingMinutes),
            "Resumen ejecutivo" => Some(SummaryType::Executive),
            "Acciones y decisiones" => Some(SummaryType::Actions),
            "Resumen breve" => Some(SummaryType::Brief),
            "Personalizado" => Some(SummaryType::Custom),

            // Legacy English display labels.
            "Meeting minutes" => Some(SummaryType::MeetingMinutes),
            "Executive summary" => Some(SummaryType::Executive),
            "Actions and decisions" => Some(SummaryType::Actions),
            "Brief summary" => Some(SummaryType::Brief),
            "Custom" => Some(SummaryType::Custom),

            _ => None,
        }
    }
}

/// Where the summary is generated.
///
/// # The offline guarantee
///
/// The app is fully offline by default and that is a feature, not an accident:
/// meeting transcripts carry names, decisions and business detail. `Remote`
/// sends the transcript text to a third party, so it must always be an
/// explicit choice — never a default, and never a fallback when `Ollama`
/// fails. Audio never leaves the machine either way; only the already-local
/// transcript can be sent, and only in this mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SummaryProvider {
    /// Local Ollama over loopback. The default.
    Ollama,
    /// Any OpenAI-compatible `/chat/completions` endpoint: OpenAI, Groq,
    /// OpenRouter, LM Studio, a self-hosted vLLM. One client covers them all.
    OpenAiCompatible,
}

impl SummaryProvider {
    pub fn as_str(self) -> &'static str {
        match self {
            SummaryProvider::Ollama => "ollama",
            SummaryProvider::OpenAiCompatible => "openai_compatible",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "ollama" => Some(SummaryProvider::Ollama),
            "openai_compatible" => Some(SummaryProvider::OpenAiCompatible),
            _ => None,
        }
    }
}

/// The English default custom prompt, from `DEFAULT_CONFIG` at `app.py:401`.
pub const DEFAULT_CUSTOM_PROMPT: &str = "Summarize the meeting clearly. Include only information present in the transcript and do not invent owners, dates, decisions or actions.";

pub const DEFAULT_WHISPER_MODEL: &str = "small";

/// Settings as the app uses them, already validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub language: Language,
    pub transcription_language: TranscriptionLanguage,
    pub whisper_model: String,
    pub ollama_model: String,
    pub summary_type: SummaryType,
    pub output_folder: PathBuf,
    pub keep_audio: bool,
    pub microphone_name: String,
    pub system_audio_name: String,
    pub custom_summary_prompt: String,
    pub summary_provider: SummaryProvider,
    /// Base URL for [`SummaryProvider::OpenAiCompatible`], without a trailing
    /// slash, e.g. `https://api.openai.com/v1`.
    pub api_base_url: String,
    /// Model id for the remote provider, e.g. `gpt-4o-mini`.
    pub api_model: String,
    /// Whether the device panel on the main window is expanded.
    ///
    /// Collapsed by default: the two device lines rarely change and cost about
    /// a fifth of a 275px window. The app reopens it by itself when a device
    /// actually falls back, which is the one case the panel exists for.
    pub devices_expanded: bool,
    /// Whether the processing queue section in the main window is open.
    /// Mirrors `devices_expanded`: a view preference, not a setting.
    pub queue_expanded: bool,
    /// Hold all meeting processing until the user says otherwise.
    ///
    /// Persisted deliberately. The point of the switch is deferring work to
    /// lunch or the evening, and a choice with that horizon has to survive
    /// quitting the app — resetting it on launch would silently start the very
    /// work the user postponed.
    pub processing_paused: bool,
    /// Whether the first-run wizard has been completed.
    ///
    /// First run is really "no config file exists"; this flag additionally
    /// reopens a wizard that was abandoned half-way. Note the migration case in
    /// [`Config::from_json`]: a file written before this key existed must count
    /// as completed, or every existing user gets the wizard on upgrade.
    pub setup_completed: bool,
}

/// The API key is deliberately NOT a field on [`Config`].
///
/// It lives in the OS keychain (macOS Keychain, Windows Credential Manager).
/// A key in a plaintext JSON file sitting next to meeting recordings is the
/// kind of thing that ends up in a backup, a screen share or a support bundle.
pub const KEYCHAIN_SERVICE: &str = "com.meetingassistant.app";
pub const KEYCHAIN_ACCOUNT: &str = "summary-api-key";

impl Config {
    /// Defaults, matching `DEFAULT_CONFIG` at `app.py:391` except for the
    /// output folder.
    ///
    /// `base` is the user's documents directory, and recordings default to
    /// `<base>/meeting-assistant/meetings`.
    ///
    /// # Deliberate divergence from the Python
    ///
    /// `app.py:397` uses `str(APP_FOLDER / "meetings")` — the directory the
    /// script itself lives in. That is fine for a folder you unzip, but it
    /// means the default output location follows wherever the app was put: on
    /// the maintainer's machine it landed inside a OneDrive-synced folder, so
    /// every meeting recording was silently uploaded to OneDrive.
    ///
    /// It is also unworkable for a packaged app: the macOS `.app` bundle is
    /// read-only in the general case, so `<bundle>/meetings` cannot be created
    /// at all. Documents is the conventional location on both platforms.
    pub fn defaults(base: &Path) -> Self {
        Self {
            language: Language::En,
            transcription_language: TranscriptionLanguage::Auto,
            whisper_model: DEFAULT_WHISPER_MODEL.to_string(),
            ollama_model: String::new(),
            summary_type: SummaryType::MeetingMinutes,
            output_folder: base.join("meeting-assistant").join("meetings"),
            keep_audio: true,
            microphone_name: String::new(),
            system_audio_name: String::new(),
            custom_summary_prompt: DEFAULT_CUSTOM_PROMPT.to_string(),
            devices_expanded: false,
            // Closed. The panel appears on its own when a meeting finishes,
            // and appearing *and* unfolding at once is the app deciding to
            // take space the user did not ask for.
            queue_expanded: false,
            processing_paused: false,
            summary_provider: SummaryProvider::Ollama,
            api_base_url: String::new(),
            api_model: String::new(),
            setup_completed: false,
        }
    }

    /// Parse settings from JSON text, clamping anything invalid to its default.
    /// Never fails: unparseable input yields pure defaults.
    pub fn from_json(text: &str, app_folder: &Path) -> Self {
        let defaults = Self::defaults(app_folder);

        let raw: RawConfig = match serde_json::from_str(text) {
            Ok(raw) => raw,
            Err(_) => return defaults,
        };

        Self {
            language: raw
                .language
                .as_deref()
                .and_then(Language::parse)
                .unwrap_or(defaults.language),

            transcription_language: raw
                .transcription_language
                .as_deref()
                .and_then(TranscriptionLanguage::parse)
                .unwrap_or(defaults.transcription_language),

            whisper_model: raw.whisper_model.unwrap_or(defaults.whisper_model),
            ollama_model: raw.ollama_model.unwrap_or(defaults.ollama_model),

            summary_type: raw
                .summary_type
                .as_deref()
                .and_then(SummaryType::parse)
                .unwrap_or(defaults.summary_type),

            output_folder: raw
                .output_folder
                .filter(|s| !s.is_empty())
                .filter(|s| !is_foreign_path(s))
                .map(PathBuf::from)
                .unwrap_or(defaults.output_folder),

            keep_audio: raw.keep_audio.unwrap_or(defaults.keep_audio),

            // Legacy names carry backend-specific decoration; strip it on the
            // way in so a config written by the Python app still matches a
            // WASAPI endpoint name. See R3.
            microphone_name: raw
                .microphone_name
                .map(|n| strip_loopback_suffix(&n).to_string())
                .unwrap_or(defaults.microphone_name),

            system_audio_name: raw
                .system_audio_name
                .map(|n| strip_loopback_suffix(&n).to_string())
                .unwrap_or(defaults.system_audio_name),

            custom_summary_prompt: raw
                .custom_summary_prompt
                .unwrap_or(defaults.custom_summary_prompt),

            devices_expanded: raw.devices_expanded.unwrap_or(defaults.devices_expanded),
            queue_expanded: raw.queue_expanded.unwrap_or(defaults.queue_expanded),
            processing_paused: raw.processing_paused.unwrap_or(defaults.processing_paused),

            summary_provider: raw
                .summary_provider
                .as_deref()
                .and_then(SummaryProvider::parse)
                .unwrap_or(defaults.summary_provider),

            api_base_url: raw
                .api_base_url
                .map(|s| s.trim_end_matches('/').to_string())
                .unwrap_or(defaults.api_base_url),

            api_model: raw.api_model.unwrap_or(defaults.api_model),

            // A config file that predates this key belongs to a user who has
            // been running the app for a while; defaulting to `false` would
            // show them the first-run wizard on upgrade. The absence of the
            // FILE is what means "first run", not the absence of this key.
            setup_completed: raw.setup_completed.unwrap_or(true),
        }
    }

    /// Serialize for disk. Matches the Python's `indent=2, ensure_ascii=False`.
    pub fn to_json(&self) -> String {
        let raw = RawConfig {
            language: Some(self.language.as_str().to_string()),
            transcription_language: Some(self.transcription_language.as_str().to_string()),
            whisper_model: Some(self.whisper_model.clone()),
            ollama_model: Some(self.ollama_model.clone()),
            summary_type: Some(self.summary_type.as_str().to_string()),
            output_folder: Some(self.output_folder.to_string_lossy().into_owned()),
            keep_audio: Some(self.keep_audio),
            microphone_name: Some(self.microphone_name.clone()),
            system_audio_name: Some(self.system_audio_name.clone()),
            custom_summary_prompt: Some(self.custom_summary_prompt.clone()),
            devices_expanded: Some(self.devices_expanded),
            queue_expanded: Some(self.queue_expanded),
            processing_paused: Some(self.processing_paused),
            summary_provider: Some(self.summary_provider.as_str().to_string()),
            api_base_url: Some(self.api_base_url.clone()),
            api_model: Some(self.api_model.clone()),
            setup_completed: Some(self.setup_completed),
        };

        // serde_json writes non-ASCII as UTF-8 literals, matching ensure_ascii=False.
        serde_json::to_string_pretty(&raw).expect("Config always serializes")
    }
}

/// On-disk shape. Every field optional so a partial file keeps its defaults,
/// mirroring the Python's `DEFAULT_CONFIG.copy()` then `update(saved)`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct RawConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    transcription_language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    whisper_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ollama_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_folder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    keep_audio: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    microphone_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_audio_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    custom_summary_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    devices_expanded: Option<bool>,
    queue_expanded: Option<bool>,
    processing_paused: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary_provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    api_base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    api_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    setup_completed: Option<bool>,
}

// ---------------------------------------------------------------------------
// Device-name migration (risk R3)
// ---------------------------------------------------------------------------

/// PyAudioWPatch decorates loopback endpoints with this suffix; WASAPI does not.
const LOOPBACK_SUFFIX: &str = " [Loopback]";

/// Shortest overlap accepted as evidence of a truncated-name match.
///
/// PortAudio truncated capture names to 31 characters, so a real truncation
/// leaves plenty of shared prefix. Requiring a decent overlap stops a short
/// saved name like `"Mic"` from matching half the device list.
const MIN_PREFIX_MATCH_LEN: usize = 8;

/// Remove PyAudioWPatch's `" [Loopback]"` decoration.
///
/// Names saved by the Python app look like
/// `"Audífono ... (Acme Headset 3225 Series) [Loopback]"`, while the
/// same endpoint through WASAPI has no suffix. Without stripping it, every
/// upgraded config silently fails to match and falls back to a different
/// device on first launch.
/// True when `value` is a path belonging to the *other* platform.
///
/// # Why this matters more than it looks
///
/// A path is just a string, so nothing rejects a Windows path on macOS. It is
/// simply treated as **relative**, and `create_dir_all` then cheerfully creates
/// a single directory whose name contains backslashes — this really happened,
/// producing a folder literally named
/// `C:\Users\...\Documents\meeting-assistant\meetings` in the working
/// directory. Recordings would then be written somewhere the user can never
/// find, with no error at any point.
///
/// This is the config-migration case from the plan's risk R3: carrying a
/// `config.json` from the Windows app to macOS, or the reverse.
fn is_foreign_path(value: &str) -> bool {
    #[cfg(windows)]
    {
        // A POSIX absolute path is meaningless on Windows.
        value.starts_with('/')
    }

    #[cfg(not(windows))]
    {
        // A drive letter (`C:\`, `D:/`) or any backslash means it came from
        // Windows. Backslash is a legal filename character on Unix, but no
        // path this app writes ever contains one, so treating it as foreign is
        // safe and catches UNC paths (`\\server\share`) too.
        let bytes = value.as_bytes();
        let has_drive_letter = bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && (bytes[2] == b'\\' || bytes[2] == b'/');

        has_drive_letter || value.contains('\\')
    }
}

pub fn strip_loopback_suffix(name: &str) -> &str {
    name.strip_suffix(LOOPBACK_SUFFIX)
        .unwrap_or(name)
        .trim_end()
}

/// Find the saved device among those currently available.
///
/// Three passes, most to least confident:
///
/// 1. exact match
/// 2. either name is a prefix of the other — PortAudio truncated capture device
///    names to 31 characters, which is the entire reason
///    `expand_microphone_display_name` (`app.py:980`) exists. A config written
///    by the Python app can hold `"Micrófono de los auriculares co"` where
///    WASAPI reports the full name.
/// 3. give up, and let the caller fall back to the normal selection policy
///
/// Both sides are normalised first, so a saved loopback name matches an
/// undecorated WASAPI one. Comparison is case-insensitive because Windows
/// endpoint capitalisation is not stable across driver updates.
pub fn match_saved_device_name(saved: &str, available: &[String]) -> Option<usize> {
    let needle = strip_loopback_suffix(saved).trim().to_lowercase();

    if needle.is_empty() {
        return None;
    }

    let normalised: Vec<String> = available
        .iter()
        .map(|n| strip_loopback_suffix(n).trim().to_lowercase())
        .collect();

    if let Some(i) = normalised.iter().position(|n| *n == needle) {
        return Some(i);
    }

    // Truncation can hit either side, so test both directions.
    normalised.iter().position(|n| {
        let overlap = n.len().min(needle.len());
        overlap >= MIN_PREFIX_MATCH_LEN
            && (n.starts_with(&needle) || needle.starts_with(n.as_str()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_folder() -> PathBuf {
        PathBuf::from("/app")
    }

    // --- defaults ---------------------------------------------------------

    #[test]
    fn defaults_match_the_python() {
        let c = Config::defaults(&app_folder());
        assert_eq!(c.language, Language::En);
        assert_eq!(c.transcription_language, TranscriptionLanguage::Auto);
        assert_eq!(c.whisper_model, "small");
        assert_eq!(c.ollama_model, "");
        assert_eq!(c.summary_type, SummaryType::MeetingMinutes);
        assert_eq!(
            c.output_folder,
            PathBuf::from("/app/meeting-assistant/meetings")
        );
        assert!(c.keep_audio);
        assert_eq!(c.custom_summary_prompt, DEFAULT_CUSTOM_PROMPT);
    }

    #[cfg(not(windows))]
    #[test]
    fn windows_output_paths_fall_back_to_the_default() {
        let defaults = Config::defaults(&app_folder());

        for path in [
            r"C:\Users\example\Documents\meeting-assistant\meetings",
            r"D:/recordings",
            r"\\server\share\meetings",
            r"relative\with\backslashes",
        ] {
            let json = format!(r#"{{"output_folder": "{}"}}"#, path.replace('\\', "\\\\"));
            let c = Config::from_json(&json, &app_folder());
            assert_eq!(
                c.output_folder, defaults.output_folder,
                "expected {path:?} to be rejected as a foreign path"
            );
        }
    }

    /// The guard must not reject legitimate local paths.
    #[cfg(not(windows))]
    #[test]
    fn unix_output_paths_are_kept() {
        for path in ["/Users/example/Documents/meetings", "/tmp/x", "/a b/c-d_e"] {
            let json = format!(r#"{{"output_folder": "{path}"}}"#);
            let c = Config::from_json(&json, &app_folder());
            assert_eq!(c.output_folder, PathBuf::from(path));
        }
    }

    #[test]
    fn unparseable_json_yields_pure_defaults() {
        let c = Config::from_json("{ not json", &app_folder());
        assert_eq!(c, Config::defaults(&app_folder()));
    }

    /// Every field matches the defaults except `setup_completed` — see the
    /// test below for why that one is deliberately asymmetric.
    #[test]
    fn empty_object_yields_pure_defaults() {
        let c = Config::from_json("{}", &app_folder());
        assert_eq!(
            c,
            Config {
                setup_completed: true,
                ..Config::defaults(&app_folder())
            }
        );
    }

    /// The first-run signal is the absence of the config *file*, not the
    /// absence of this key. A file written by any earlier build has no
    /// `setup_completed`, and defaulting it to `false` would show the first-run
    /// wizard to every existing user on upgrade.
    #[test]
    fn a_config_file_without_setup_completed_counts_as_completed() {
        let c = Config::from_json(r#"{"language": "es"}"#, &app_folder());
        assert!(c.setup_completed, "an existing file means setup was done");

        // No file at all is the genuine first run.
        assert!(!Config::defaults(&app_folder()).setup_completed);

        // And an explicit false is honoured, so a wizard abandoned half-way
        // reopens.
        let abandoned = Config::from_json(r#"{"setup_completed": false}"#, &app_folder());
        assert!(!abandoned.setup_completed);
    }

    /// The remote provider is opt-in. A config that does not mention it must
    /// never resolve to sending transcripts off the machine.
    #[test]
    fn summary_provider_defaults_to_local_ollama() {
        assert_eq!(
            Config::from_json("{}", &app_folder()).summary_provider,
            SummaryProvider::Ollama
        );
        assert_eq!(
            Config::from_json(r#"{"summary_provider": "nonsense"}"#, &app_folder()).summary_provider,
            SummaryProvider::Ollama
        );
        assert_eq!(
            Config::from_json(r#"{"summary_provider": "openai_compatible"}"#, &app_folder())
                .summary_provider,
            SummaryProvider::OpenAiCompatible
        );
    }

    /// A trailing slash would produce `.../v1//chat/completions`, which some
    /// gateways reject.
    #[test]
    fn api_base_url_loses_its_trailing_slash() {
        let c = Config::from_json(r#"{"api_base_url": "https://api.openai.com/v1/"}"#, &app_folder());
        assert_eq!(c.api_base_url, "https://api.openai.com/v1");
    }

    /// The key must never reach the config file, in either direction.
    #[test]
    fn the_api_key_is_never_serialized() {
        let c = Config::from_json(
            r#"{"api_model": "gpt-4o-mini", "api_key": "sk-secret-value"}"#,
            &app_folder(),
        );
        assert_eq!(c.api_model, "gpt-4o-mini");
        assert!(
            !c.to_json().contains("sk-secret-value"),
            "an api_key in the input must not survive a round trip"
        );
        assert!(!c.to_json().contains("api_key"));
    }

    #[test]
    fn partial_file_keeps_other_defaults() {
        let c = Config::from_json(r#"{"keep_audio": false}"#, &app_folder());
        assert!(!c.keep_audio);
        assert_eq!(c.whisper_model, "small");
        assert_eq!(c.language, Language::En);
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let c = Config::from_json(r#"{"language": "es", "nonsense": 42}"#, &app_folder());
        assert_eq!(c.language, Language::Es);
    }

    // --- validation clamps ------------------------------------------------

    #[test]
    fn invalid_language_clamps_to_english() {
        let c = Config::from_json(r#"{"language": "fr"}"#, &app_folder());
        assert_eq!(c.language, Language::En);
    }

    #[test]
    fn invalid_transcription_language_clamps_to_auto() {
        let c = Config::from_json(r#"{"transcription_language": "de"}"#, &app_folder());
        assert_eq!(c.transcription_language, TranscriptionLanguage::Auto);
    }

    #[test]
    fn invalid_summary_type_clamps_to_meeting_minutes() {
        let c = Config::from_json(r#"{"summary_type": "whatever"}"#, &app_folder());
        assert_eq!(c.summary_type, SummaryType::MeetingMinutes);
    }

    // --- legacy summary_type migration ------------------------------------

    #[test]
    fn legacy_spanish_summary_labels_migrate() {
        for (label, expected) in [
            ("Acta de reunión", SummaryType::MeetingMinutes),
            ("Resumen ejecutivo", SummaryType::Executive),
            ("Acciones y decisiones", SummaryType::Actions),
            ("Resumen breve", SummaryType::Brief),
            ("Personalizado", SummaryType::Custom),
        ] {
            let json = format!(r#"{{"summary_type": "{label}"}}"#);
            assert_eq!(
                Config::from_json(&json, &app_folder()).summary_type,
                expected,
                "label {label:?}"
            );
        }
    }

    #[test]
    fn legacy_english_summary_labels_migrate() {
        for (label, expected) in [
            ("Meeting minutes", SummaryType::MeetingMinutes),
            ("Executive summary", SummaryType::Executive),
            ("Actions and decisions", SummaryType::Actions),
            ("Brief summary", SummaryType::Brief),
            ("Custom", SummaryType::Custom),
        ] {
            let json = format!(r#"{{"summary_type": "{label}"}}"#);
            assert_eq!(
                Config::from_json(&json, &app_folder()).summary_type,
                expected,
                "label {label:?}"
            );
        }
    }

    // --- round trip -------------------------------------------------------

    #[test]
    fn round_trips_through_json() {
        let mut original = Config::defaults(&app_folder());
        original.language = Language::Es;
        original.transcription_language = TranscriptionLanguage::Es;
        original.summary_type = SummaryType::Actions;
        original.ollama_model = "gemma3:4b".to_string();
        original.keep_audio = false;

        let restored = Config::from_json(&original.to_json(), &app_folder());
        assert_eq!(restored, original);
    }

    #[test]
    fn non_ascii_survives_a_round_trip() {
        let mut original = Config::defaults(&app_folder());
        original.microphone_name = "Micrófono de los auriculares".to_string();
        original.custom_summary_prompt = "Resume la reunión sin inventar.".to_string();

        let json = original.to_json();
        // ensure_ascii=False equivalent: the accented text is written literally.
        assert!(json.contains("Micrófono"));

        assert_eq!(Config::from_json(&json, &app_folder()), original);
    }

    #[test]
    fn reads_a_real_config_from_the_python_app() {
        // Verbatim from the repo's config.json, including Windows paths and
        // the decorated loopback name.
        let json = r#"{
  "language": "en",
  "transcription_language": "auto",
  "whisper_model": "small",
  "ollama_model": "gemma3:4b",
  "summary_type": "meeting_minutes",
  "output_folder": "C:\\Users\\example\\Documents\\meeting-assistant\\meetings",
  "keep_audio": true,
  "microphone_name": "Micrófono de los auriculares co",
  "system_audio_name": "Audífono de los auriculares con micrófono (Acme Headset 3225 Series) [Loopback]",
  "custom_summary_prompt": "Summarize the meeting clearly."
}"#;

        let c = Config::from_json(json, &app_folder());
        assert_eq!(c.ollama_model, "gemma3:4b");
        assert!(c.keep_audio);

        // The Windows output path must NOT survive onto a Unix host. Treated
        // as relative it would create a directory literally named
        // `C:\Users\...` in the working directory, and every recording would
        // land somewhere the user cannot find.
        #[cfg(not(windows))]
        assert_eq!(c.output_folder, Config::defaults(&app_folder()).output_folder);
        // The loopback decoration is gone, so this can match a WASAPI endpoint.
        assert_eq!(
            c.system_audio_name,
            "Audífono de los auriculares con micrófono (Acme Headset 3225 Series)"
        );
    }

    // --- R3: device-name migration ----------------------------------------

    #[test]
    fn strips_the_loopback_suffix() {
        assert_eq!(
            strip_loopback_suffix("Speakers (Onboard) [Loopback]"),
            "Speakers (Onboard)"
        );
        assert_eq!(
            strip_loopback_suffix("Speakers (Onboard)"),
            "Speakers (Onboard)"
        );
    }

    #[test]
    fn exact_name_matches() {
        let available = vec!["Microphone Array".to_string(), "USB Mic".to_string()];
        assert_eq!(match_saved_device_name("USB Mic", &available), Some(1));
    }

    #[test]
    fn decorated_saved_name_matches_undecorated_endpoint() {
        let available = vec!["Speakers (Onboard)".to_string()];
        assert_eq!(
            match_saved_device_name("Speakers (Onboard) [Loopback]", &available),
            Some(0)
        );
    }

    #[test]
    fn truncated_saved_name_matches_full_endpoint() {
        // Exactly the case in the repo's config.json: PortAudio truncated the
        // capture device name, WASAPI reports it in full.
        let available =
            vec!["Micrófono de los auriculares con micrófono (Acme)".to_string()];
        assert_eq!(
            match_saved_device_name("Micrófono de los auriculares co", &available),
            Some(0)
        );
    }

    #[test]
    fn matching_is_case_insensitive() {
        let available = vec!["SPEAKERS (Onboard)".to_string()];
        assert_eq!(
            match_saved_device_name("Speakers (onboard)", &available),
            Some(0)
        );
    }

    #[test]
    fn unrelated_name_does_not_match() {
        let available = vec!["Microphone Array (Intel)".to_string()];
        assert_eq!(
            match_saved_device_name("Acme Headset", &available),
            None
        );
    }

    #[test]
    fn empty_saved_name_does_not_match() {
        let available = vec!["Microphone Array".to_string()];
        assert_eq!(match_saved_device_name("", &available), None);
        assert_eq!(match_saved_device_name("   ", &available), None);
    }

    #[test]
    fn short_prefixes_do_not_match_promiscuously() {
        // "Mic" is a prefix of both; too short to be evidence of anything.
        let available = vec!["Microphone Array".to_string(), "Mic Pro".to_string()];
        assert_eq!(match_saved_device_name("Mic", &available), None);
    }

    #[test]
    fn exact_match_wins_over_a_prefix_match() {
        let available = vec![
            "Acme Headset 3225 Series Extra".to_string(),
            "Acme Headset".to_string(),
        ];
        assert_eq!(
            match_saved_device_name("Acme Headset", &available),
            Some(1)
        );
    }
}
