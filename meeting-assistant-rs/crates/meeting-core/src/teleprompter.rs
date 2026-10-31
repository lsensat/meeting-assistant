//! Teleprompter logic that needs no window: settings and their clamping, the
//! on-disk shape, how a script is turned into a Whisper prompt and a summary
//! context, and the small geometry and timing rules the window code applies.
//!
//! Everything here is pure so it runs in the portable test suite; the window,
//! the polling thread and the files live in `src-tauri/src/teleprompter.rs`.
//!
//! The first group of constants is mirrored verbatim in
//! `ui/js/teleprompter-logic.js`. Change one side and you must change the other.

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---- Mirrored in the frontend -------------------------------------------------

/// Font size bounds and default, in CSS pixels.
pub const FONT_MIN: u32 = 16;
pub const FONT_MAX: u32 = 160;
pub const FONT_DEFAULT: u32 = 44;
pub const FONT_STEP: u32 = 4;

/// Scroll speed levels. The frontend turns a level into pixels per second.
pub const SPEED_MIN: u32 = 1;
pub const SPEED_MAX: u32 = 30;
pub const SPEED_DEFAULT: u32 = 5;

/// Text opacity never goes fully transparent: invisible text is a bug, not a setting.
pub const TEXT_OPACITY_MIN: f64 = 0.2;
pub const TEXT_OPACITY_DEFAULT: f64 = 1.0;
pub const OVERLAY_OPACITY_DEFAULT: f64 = 0.4;

// ---- Backend only -------------------------------------------------------------

/// The most script text kept on disk.
pub const SCRIPT_MAX_CHARS: usize = 200_000;
/// The most script text copied into a `meeting.json`.
pub const MEETING_SCRIPT_MAX_CHARS: usize = 20_000;
/// Whisper keeps only the last 224 prompt tokens; 400 characters stays inside that.
pub const WHISPER_PROMPT_MAX_CHARS: usize = 400;
/// Sent only with the final summary pass, so a small local model's context survives.
pub const SUMMARY_SCRIPT_MAX_CHARS: usize = 3_000;

/// Window sizes, logical pixels.
pub const MIN_WIDTH: u32 = 240;
pub const MIN_HEIGHT: u32 = 120;
pub const DEFAULT_WIDTH: u32 = 640;
pub const DEFAULT_HEIGHT: u32 = 300;

/// The cursor must rest inside this long before the controls appear...
pub const HOVER_ENTER_MS: u64 = 300;
/// ...and outside this long before the window turns click-through again.
pub const HOVER_LEAVE_MS: u64 = 700;
/// Logical pixels around the window that still count as inside, so grabbing a
/// resize edge does not flip the window to click-through under the cursor.
pub const EDGE_MARGIN: f64 = 8.0;

/// The largest window size accepted from disk.
const GEOMETRY_MAX: f64 = 10_000.0;

// ---- Settings -----------------------------------------------------------------

/// What the frontend sees and saves.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Settings {
    pub script: String,
    pub font_size: u32,
    pub speed: u32,
    pub overlay_opacity: f64,
    pub text_opacity: f64,
    pub use_in_pipeline: bool,
}

impl Settings {
    pub fn defaults() -> Self {
        Self {
            script: String::new(),
            font_size: FONT_DEFAULT,
            speed: SPEED_DEFAULT,
            overlay_opacity: OVERLAY_OPACITY_DEFAULT,
            text_opacity: TEXT_OPACITY_DEFAULT,
            use_in_pipeline: true,
        }
    }

    /// Bring every field into range. NaN and infinities become the default.
    pub fn clamped(mut self) -> Self {
        self.script = clean_script(&self.script);
        self.font_size = self.font_size.clamp(FONT_MIN, FONT_MAX);
        self.speed = self.speed.clamp(SPEED_MIN, SPEED_MAX);
        self.overlay_opacity = clamp_opacity(self.overlay_opacity, 0.0, OVERLAY_OPACITY_DEFAULT);
        self.text_opacity =
            clamp_opacity(self.text_opacity, TEXT_OPACITY_MIN, TEXT_OPACITY_DEFAULT);
        self
    }
}

fn clamp_opacity(value: f64, min: f64, default: f64) -> f64 {
    let value = if value.is_finite() { value } else { default };
    let value = value.clamp(min, 1.0);
    (value * 100.0).round_ties_even() / 100.0
}

/// Strip `\0` (it would panic a C string) and `\r`, then cap the length.
fn clean_script(script: &str) -> String {
    let stripped: String = script.chars().filter(|c| *c != '\0' && *c != '\r').collect();
    truncate_chars(&stripped, SCRIPT_MAX_CHARS).to_string()
}

/// Window placement: physical outer position, logical inner size. Never sent to
/// the frontend.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    pub x: i32,
    pub y: i32,
    pub width: f64,
    pub height: f64,
}

/// Settings plus the remembered placement: the contents of `teleprompter.json`.
#[derive(Clone, Debug, PartialEq)]
pub struct Stored {
    pub settings: Settings,
    pub geometry: Option<Geometry>,
}

#[derive(Serialize)]
struct StoredOut<'a> {
    #[serde(flatten)]
    settings: &'a Settings,
    #[serde(skip_serializing_if = "Option::is_none")]
    geometry: Option<Geometry>,
}

/// Numbers and booleans are read as raw JSON values so one field of the wrong
/// type costs only that field, not the whole file.
#[derive(Default, Deserialize)]
struct RawStored {
    script: Option<Value>,
    font_size: Option<Value>,
    speed: Option<Value>,
    overlay_opacity: Option<Value>,
    text_opacity: Option<Value>,
    use_in_pipeline: Option<Value>,
    geometry: Option<Value>,
}

fn as_level(value: &Option<Value>, default: u32) -> u32 {
    match value.as_ref().and_then(Value::as_f64) {
        Some(n) if n.is_finite() => n.round_ties_even().clamp(0.0, u32::MAX as f64) as u32,
        _ => default,
    }
}

fn as_float(value: &Option<Value>, default: f64) -> f64 {
    match value.as_ref().and_then(Value::as_f64) {
        Some(n) if n.is_finite() => n,
        _ => default,
    }
}

impl Stored {
    pub fn defaults() -> Self {
        Self {
            settings: Settings::defaults(),
            geometry: None,
        }
    }

    /// Never fails: garbage yields defaults, partial files keep the defaults
    /// for whatever they omit, and out-of-range values are clamped.
    pub fn from_json(text: &str) -> Self {
        let raw: RawStored = match serde_json::from_str(text) {
            Ok(raw) => raw,
            Err(_) => return Self::defaults(),
        };
        let d = Settings::defaults();

        let settings = Settings {
            script: raw
                .script
                .as_ref()
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or(d.script),
            font_size: as_level(&raw.font_size, d.font_size),
            speed: as_level(&raw.speed, d.speed),
            overlay_opacity: as_float(&raw.overlay_opacity, d.overlay_opacity),
            text_opacity: as_float(&raw.text_opacity, d.text_opacity),
            use_in_pipeline: raw
                .use_in_pipeline
                .as_ref()
                .and_then(Value::as_bool)
                .unwrap_or(d.use_in_pipeline),
        }
        .clamped();

        let geometry = raw
            .geometry
            .and_then(|v| serde_json::from_value::<Geometry>(v).ok())
            .filter(|g| g.width.is_finite() && g.height.is_finite())
            .map(|g| Geometry {
                width: g.width.clamp(MIN_WIDTH as f64, GEOMETRY_MAX),
                height: g.height.clamp(MIN_HEIGHT as f64, GEOMETRY_MAX),
                ..g
            });

        Self { settings, geometry }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(&StoredOut {
            settings: &self.settings,
            geometry: self.geometry,
        })
        .unwrap_or_else(|_| "{}".to_string())
    }
}

// ---- Text ---------------------------------------------------------------------

/// The first `max` characters, cut on a character boundary.
pub fn truncate_chars(text: &str, max: usize) -> &str {
    match text.char_indices().nth(max) {
        Some((index, _)) => &text[..index],
        None => text,
    }
}

fn is_fence(trimmed: &str) -> bool {
    trimmed.starts_with("```") || trimmed.starts_with("~~~")
}

/// Remove one Markdown line prefix, if there is one.
fn strip_prefix_once(line: &str) -> &str {
    // Heading: `#+` then whitespace.
    let hashes = line.len() - line.trim_start_matches('#').len();
    if hashes > 0 {
        let rest = &line[hashes..];
        if rest.starts_with(char::is_whitespace) {
            return rest.trim_start();
        }
    }
    // Quote: `>` and an optional space.
    if let Some(rest) = line.strip_prefix('>') {
        return rest.strip_prefix(' ').unwrap_or(rest);
    }
    // Bullet.
    let mut chars = line.chars();
    if let (Some('-' | '*' | '+'), Some(next)) = (chars.next(), chars.next()) {
        if next.is_whitespace() {
            return line[1..].trim_start();
        }
    }
    // Numbered item: digits, `.` or `)`, whitespace.
    let digits = line.len() - line.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits > 0 {
        let rest = &line[digits..];
        let mut rc = rest.chars();
        if let (Some('.' | ')'), Some(next)) = (rc.next(), rc.next()) {
            if next.is_whitespace() {
                return rest[1..].trim_start();
            }
        }
    }
    line
}

/// `[text](url)` becomes `text`.
fn strip_links(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '[' {
            if let Some(close) = chars[i + 1..].iter().position(|c| *c == ']') {
                let close = i + 1 + close;
                if chars.get(close + 1) == Some(&'(') {
                    if let Some(end) = chars[close + 2..].iter().position(|c| *c == ')') {
                        out.extend(&chars[i + 1..close]);
                        i = close + 2 + end + 1;
                        continue;
                    }
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// The script as plain prose: no fences, line prefixes, link targets, emphasis
/// marks or bare URLs. Line structure is kept.
pub fn strip_markdown(text: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for raw in text.lines() {
        let trimmed = raw.trim();
        if is_fence(trimmed) {
            continue;
        }
        let mut line = trimmed;
        loop {
            let next = strip_prefix_once(line);
            if next.len() == line.len() {
                break;
            }
            line = next;
        }
        let linked = strip_links(line);
        let cleaned: String = linked.chars().filter(|c| *c != '*' && *c != '`').collect();
        let kept: Vec<&str> = cleaned
            .split_whitespace()
            .filter(|w| !w.starts_with("http://") && !w.starts_with("https://"))
            .collect();
        lines.push(kept.join(" "));
    }
    lines.join("\n")
}

const STOPWORDS: [&str; 6] = ["i", "ok", "i'm", "i've", "i'll", "i'd"];
const LEADING: [char; 8] = ['¿', '¡', '"', '\'', '(', '«', '“', '['];
const TRAILING: [char; 15] = [
    '.', ',', ';', ':', '!', '?', ')', '"', '\'', '»', '”', ']', '…', '’', '”',
];

struct Token {
    clean: String,
    candidate: bool,
    /// The raw word carried trailing punctuation, which ends a phrase.
    punct_after: bool,
}

fn ends_sentence(raw: &str) -> bool {
    raw.ends_with(['.', '!', '?', ':', ';'])
}

fn is_candidate(word: &str, sentence_start: bool) -> bool {
    if word.chars().count() < 2 || word.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let lower = word.to_lowercase();
    if STOPWORDS.contains(&lower.as_str()) {
        return false;
    }
    let has_letter = word.chars().any(char::is_alphabetic);
    let has_digit = word.chars().any(|c| c.is_ascii_digit());
    let uppers = word.chars().filter(|c| c.is_uppercase()).count();
    let first_upper = word.chars().next().is_some_and(char::is_uppercase);
    (has_letter && has_digit) || uppers >= 2 || (first_upper && !sentence_start)
}

fn tokenize_line(line: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut prev_raw: Option<&str> = None;
    for raw in line.split_whitespace() {
        let sentence_start = prev_raw.map_or(true, ends_sentence);
        let clean = raw
            .trim_start_matches(LEADING)
            .trim_end_matches(TRAILING)
            .to_string();
        let punct_after = raw.trim_start_matches(LEADING).len() != clean.len();
        let candidate = is_candidate(&clean, sentence_start);
        tokens.push(Token {
            clean,
            candidate,
            punct_after,
        });
        prev_raw = Some(raw);
    }
    tokens
}

/// Names and terms worth telling Whisper about: acronyms, product names,
/// capitalised mid-sentence words (merged into phrases of up to four words) and
/// anything mixing letters and digits. Most frequent first.
pub fn key_terms(script: &str) -> Vec<String> {
    let text = strip_markdown(script);
    let lines: Vec<Vec<Token>> = text.lines().map(tokenize_line).collect();

    // (term, words, first index)
    let mut found: Vec<(String, Vec<String>, usize)> = Vec::new();
    let mut index = 0usize;
    for tokens in &lines {
        let mut run: Vec<&Token> = Vec::new();
        let mut flush = |run: &mut Vec<&Token>, index: usize| {
            for chunk in run.chunks(4) {
                let words: Vec<String> = chunk.iter().map(|t| t.clean.clone()).collect();
                found.push((words.join(" "), words, index));
            }
            run.clear();
        };
        for token in tokens {
            index += 1;
            if token.candidate {
                run.push(token);
                if token.punct_after {
                    flush(&mut run, index);
                }
            } else {
                flush(&mut run, index);
            }
        }
        flush(&mut run, index);
    }

    // Exact-form occurrence counts over every token, sentence starts included.
    let count = |words: &[String]| -> usize {
        lines
            .iter()
            .map(|tokens| {
                tokens
                    .windows(words.len())
                    .filter(|w| w.iter().zip(words).all(|(t, s)| &t.clean == s))
                    .count()
            })
            .sum()
    };

    let mut seen: Vec<String> = Vec::new();
    let mut ranked: Vec<(usize, usize, String)> = Vec::new();
    for (term, words, first) in &found {
        let key = term.to_lowercase();
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        ranked.push((count(words), *first, term.clone()));
    }
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    ranked.into_iter().map(|(_, _, term)| term).collect()
}

fn without_nul(text: &str) -> String {
    text.chars().filter(|c| *c != '\0').collect()
}

/// Cut to `max` characters at the last whitespace, trimmed.
fn cut_at_whitespace(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.trim_end().to_string();
    }
    let prefix = truncate_chars(text, max);
    let boundary = text[prefix.len()..]
        .chars()
        .next()
        .is_some_and(char::is_whitespace);
    if boundary {
        return prefix.trim_end().to_string();
    }
    match prefix.rfind(char::is_whitespace) {
        Some(at) => prefix[..at].trim_end().to_string(),
        None => prefix.to_string(),
    }
}

/// The text handed to Whisper as `initial_prompt`: a comma-separated list of
/// the script's key terms ending in a full stop, or, when it has none, the
/// script's own opening words. Empty for a blank script.
pub fn whisper_prompt(script: &str, max_chars: usize) -> String {
    let script = without_nul(script);
    if script.trim().is_empty() || max_chars == 0 {
        return String::new();
    }

    let budget = max_chars - 1;
    let mut joined = String::new();
    for term in key_terms(&script) {
        let extra = term.chars().count() + if joined.is_empty() { 0 } else { 2 };
        if joined.chars().count() + extra > budget {
            break;
        }
        if !joined.is_empty() {
            joined.push_str(", ");
        }
        joined.push_str(&term);
    }
    if !joined.is_empty() {
        joined.push('.');
        return joined;
    }

    let collapsed = strip_markdown(&script)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    cut_at_whitespace(&collapsed, max_chars)
}

/// The script as context for the final summary pass, capped at `max_chars`.
pub fn summary_context(script: &str, max_chars: usize) -> String {
    let stripped: String = script.chars().filter(|c| *c != '\0' && *c != '\r').collect();

    let mut text = String::with_capacity(stripped.len());
    let mut newlines = 0;
    for c in stripped.trim().chars() {
        if c == '\n' {
            newlines += 1;
            if newlines > 2 {
                continue;
            }
        } else {
            newlines = 0;
        }
        text.push(c);
    }

    if text.chars().count() <= max_chars {
        return text;
    }
    format!("{}\n[…]", cut_at_whitespace(&text, max_chars))
}

// ---- Geometry -----------------------------------------------------------------

/// A monitor or window rectangle in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Whether a point lies inside the rectangle grown by `margin` on every side.
pub fn inside(rect: Rect, px: f64, py: f64, margin: f64) -> bool {
    let left = rect.x as f64 - margin;
    let top = rect.y as f64 - margin;
    let right = rect.x as f64 + rect.width as f64 + margin;
    let bottom = rect.y as f64 + rect.height as f64 + margin;
    px >= left && px < right && py >= top && py < bottom
}

/// Whether a window placed at `(x, y)` would have a grabbable corner on some
/// monitor. Tested at 20px in from the top-left, where the window can be dragged.
pub fn on_any_monitor(x: i32, y: i32, monitors: &[Rect]) -> bool {
    let (px, py) = (x as f64 + 20.0, y as f64 + 20.0);
    monitors.iter().any(|m| inside(*m, px, py, 0.0))
}

/// Resize a `(x, y, width, height)` rectangle by dragging edge or corner `dir`
/// (`n`, `s`, `e`, `w` or a pair such as `ne`) by `(dx, dy)`. The north and west
/// edges move the origin; the minimum size is held with the opposite edge fixed.
pub fn resize_rect(
    start: (f64, f64, f64, f64),
    dir: &str,
    dx: f64,
    dy: f64,
) -> (f64, f64, f64, f64) {
    let (mut x, mut y, mut w, mut h) = start;
    let (min_w, min_h) = (MIN_WIDTH as f64, MIN_HEIGHT as f64);

    if dir.contains('e') {
        w = (start.2 + dx).max(min_w);
    }
    if dir.contains('w') {
        w = (start.2 - dx).max(min_w);
        x = start.0 + start.2 - w;
    }
    if dir.contains('s') {
        h = (start.3 + dy).max(min_h);
    }
    if dir.contains('n') {
        h = (start.3 - dy).max(min_h);
        y = start.1 + start.3 - h;
    }
    (x, y, w, h)
}

// ---- Hover --------------------------------------------------------------------

/// Debounces "the cursor is over the window" so a pass across it neither shows
/// the controls nor flips the window to click-through.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hover {
    pub hovered: bool,
    pub inside_since: Option<u64>,
    pub outside_since: Option<u64>,
}

impl Hover {
    /// Starts hovered: a window that has just opened shows its controls.
    pub fn new() -> Self {
        Self {
            hovered: true,
            inside_since: None,
            outside_since: None,
        }
    }

    /// Feed one cursor sample. `Some(new_state)` only when the state changes.
    pub fn tick(&mut self, inside: bool, now_ms: u64) -> Option<bool> {
        if inside {
            self.outside_since = None;
            if self.hovered {
                self.inside_since = None;
                return None;
            }
            let since = *self.inside_since.get_or_insert(now_ms);
            if now_ms.saturating_sub(since) >= HOVER_ENTER_MS {
                self.hovered = true;
                self.inside_since = None;
                return Some(true);
            }
        } else {
            self.inside_since = None;
            if !self.hovered {
                self.outside_since = None;
                return None;
            }
            let since = *self.outside_since.get_or_insert(now_ms);
            if now_ms.saturating_sub(since) >= HOVER_LEAVE_MS {
                self.hovered = false;
                self.outside_since = None;
                return Some(false);
            }
        }
        None
    }
}

impl Default for Hover {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether mouse events should pass through the window to what is beneath it.
pub fn ignore_cursor(hovered: bool, pinned: bool, tracking_available: bool) -> bool {
    tracking_available && !hovered && !pinned
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_in_range() {
        let d = Settings::defaults();
        assert_eq!(d.clone().clamped(), d);
        assert!(d.use_in_pipeline);
        assert!(d.script.is_empty());
    }

    #[test]
    fn clamped_pulls_everything_into_range() {
        let s = Settings {
            script: "a\0b\r\nc".into(),
            font_size: 5,
            speed: 999,
            overlay_opacity: 3.0,
            text_opacity: -1.0,
            use_in_pipeline: false,
        }
        .clamped();
        assert_eq!(s.script, "ab\nc");
        assert_eq!(s.font_size, FONT_MIN);
        assert_eq!(s.speed, SPEED_MAX);
        assert_eq!(s.overlay_opacity, 1.0);
        assert_eq!(s.text_opacity, TEXT_OPACITY_MIN);
        assert!(!s.use_in_pipeline);
    }

    #[test]
    fn clamped_replaces_nan_and_rounds_opacities() {
        let s = Settings {
            overlay_opacity: f64::NAN,
            text_opacity: f64::INFINITY,
            ..Settings::defaults()
        }
        .clamped();
        assert_eq!(s.overlay_opacity, OVERLAY_OPACITY_DEFAULT);
        assert_eq!(s.text_opacity, TEXT_OPACITY_DEFAULT);

        let s = Settings {
            overlay_opacity: 0.123456,
            ..Settings::defaults()
        }
        .clamped();
        assert_eq!(s.overlay_opacity, 0.12);
    }

    #[test]
    fn clamped_truncates_the_script_on_a_char_boundary() {
        let s = Settings {
            script: "é".repeat(SCRIPT_MAX_CHARS + 10),
            ..Settings::defaults()
        }
        .clamped();
        assert_eq!(s.script.chars().count(), SCRIPT_MAX_CHARS);
    }

    #[test]
    fn from_json_survives_garbage() {
        for text in ["", "not json", "[1,2]", "null", "{\"font_size\":"] {
            assert_eq!(Stored::from_json(text), Stored::defaults(), "{text:?}");
        }
    }

    #[test]
    fn from_json_keeps_defaults_for_missing_fields() {
        let s = Stored::from_json(r#"{"script":"hi","speed":9}"#);
        assert_eq!(s.settings.script, "hi");
        assert_eq!(s.settings.speed, 9);
        assert_eq!(s.settings.font_size, FONT_DEFAULT);
        assert!(s.settings.use_in_pipeline);
        assert!(s.geometry.is_none());
    }

    #[test]
    fn from_json_clamps_and_tolerates_wrong_types() {
        let s = Stored::from_json(
            r#"{"font_size":-40,"speed":1e12,"overlay_opacity":9,"text_opacity":"x",
                "use_in_pipeline":"yes","script":5}"#,
        );
        assert_eq!(s.settings.font_size, FONT_MIN);
        assert_eq!(s.settings.speed, SPEED_MAX);
        assert_eq!(s.settings.overlay_opacity, 1.0);
        assert_eq!(s.settings.text_opacity, TEXT_OPACITY_DEFAULT);
        assert!(s.settings.use_in_pipeline);
        assert_eq!(s.settings.script, "");
    }

    #[test]
    fn from_json_validates_geometry() {
        let s = Stored::from_json(r#"{"geometry":{"x":-5,"y":7,"width":10,"height":1e9}}"#);
        let g = s.geometry.expect("geometry kept");
        assert_eq!((g.x, g.y), (-5, 7));
        assert_eq!(g.width, MIN_WIDTH as f64);
        assert_eq!(g.height, GEOMETRY_MAX);
        assert!(Stored::from_json(r#"{"geometry":{"x":"a"}}"#).geometry.is_none());
    }

    #[test]
    fn round_trips_through_json() {
        let stored = Stored {
            settings: Settings {
                script: "Hola\n\"mundo\" ñ".into(),
                font_size: 60,
                speed: 12,
                overlay_opacity: 0.55,
                text_opacity: 0.8,
                use_in_pipeline: false,
            },
            geometry: Some(Geometry {
                x: -100,
                y: 40,
                width: 500.5,
                height: 250.0,
            }),
        };
        assert_eq!(Stored::from_json(&stored.to_json()), stored);

        let no_geometry = Stored::defaults();
        assert!(!no_geometry.to_json().contains("geometry"));
        assert_eq!(Stored::from_json(&no_geometry.to_json()), no_geometry);
    }

    #[test]
    fn truncate_chars_is_utf8_safe() {
        assert_eq!(truncate_chars("héllo", 2), "hé");
        assert_eq!(truncate_chars("héllo", 99), "héllo");
        assert_eq!(truncate_chars("héllo", 0), "");
    }

    #[test]
    fn strip_markdown_removes_syntax() {
        let md = "# Title\n> quote\n- item one\n1. first\n2) second\n```\ncode\n```\n\
                  See [the docs](https://x.io/a) and https://y.io now **bold** `tick`";
        let out = strip_markdown(md);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "Title");
        assert_eq!(lines[1], "quote");
        assert_eq!(lines[2], "item one");
        assert_eq!(lines[3], "first");
        assert_eq!(lines[4], "second");
        // the fence lines are dropped, the code line between them is not
        assert_eq!(lines[5], "code");
        assert_eq!(lines[6], "See the docs and now bold tick");
    }

    #[test]
    fn strip_markdown_leaves_unprefixed_text_alone() {
        assert_eq!(strip_markdown("#hashtag 3 apples -5"), "#hashtag 3 apples -5");
    }

    #[test]
    fn key_terms_english() {
        let terms = key_terms("We met Acme Corp about the Q3 plan with OpenAI.");
        assert_eq!(terms, ["Acme Corp", "Q3", "OpenAI"]);
        assert!(!terms.iter().any(|t| t == "We"));
    }

    #[test]
    fn key_terms_spanish() {
        let terms = key_terms("¿Habló María López con el equipo?");
        assert_eq!(terms, ["María López"]);
    }

    #[test]
    fn key_terms_rank_by_count_then_first_appearance() {
        let terms = key_terms("Ask Zed. Then ask Ann. Later ask Ann again, then GPT-4 and Ann.");
        assert_eq!(terms, ["Ann", "Zed", "GPT-4"]);
    }

    #[test]
    fn key_terms_dedupe_case_insensitively_and_skip_stopwords() {
        let terms = key_terms("Ask API folks. ask api folks. Then I'm OK with OK.");
        assert_eq!(terms, ["API"]);
    }

    #[test]
    fn key_terms_phrases_stop_at_punctuation_and_four_words() {
        let terms = key_terms("Meet Ann Lee, Bob Ray. Ask Ann Bob Cy Di Ed Fay.");
        assert_eq!(terms, ["Ann Lee", "Bob Ray", "Ann Bob Cy Di", "Ed Fay"]);
    }

    #[test]
    fn whisper_prompt_joins_terms_and_ends_with_a_stop() {
        let p = whisper_prompt("We met Acme Corp about the Q3 plan with OpenAI.", 400);
        assert_eq!(p, "Acme Corp, Q3, OpenAI.");
    }

    #[test]
    fn whisper_prompt_never_splits_a_term_or_exceeds_the_cap() {
        let script = "Talk to Jose Ñandú, Ünal Çelik and Zoë Wißmann.";
        for max in 1..60 {
            let p = whisper_prompt(script, max);
            assert!(p.chars().count() <= max, "max {max}: {p:?}");
        }
        let p = whisper_prompt(script, 20);
        // "Jose Ñandú" (10) + ", " + "Ünal Çelik" (10) = 22 > 19, so only one term fits.
        assert_eq!(p, "Jose Ñandú.");
    }

    #[test]
    fn whisper_prompt_falls_back_to_the_opening_text() {
        let p = whisper_prompt("all lowercase words here\n\nand more of them", 15);
        assert_eq!(p, "all lowercase");
        assert_eq!(whisper_prompt("tiny text", 400), "tiny text");
        // multibyte right at the cut
        let p = whisper_prompt("ñandú ñandú ñandú", 8);
        assert_eq!(p, "ñandú");
    }

    #[test]
    fn whisper_prompt_is_empty_for_blank_and_strips_nul() {
        assert_eq!(whisper_prompt("  \n\t ", 400), "");
        assert_eq!(whisper_prompt("\0\0", 400), "");
        assert!(!whisper_prompt("see Acme\0 Corp", 400).contains('\0'));
    }

    #[test]
    fn summary_context_cleans_and_truncates() {
        assert_eq!(summary_context("  a\r\n\n\n\n\nb\0  ", 100), "a\n\nb");
        let long = "word ".repeat(100);
        let out = summary_context(&long, 22);
        assert!(out.ends_with("\n[…]"));
        assert_eq!(out, "word word word word\n[…]");
        assert_eq!(summary_context("short", 100), "short");
    }

    #[test]
    fn rects_and_monitors() {
        let r = Rect {
            x: 0,
            y: 0,
            width: 100,
            height: 50,
        };
        assert!(inside(r, 10.0, 10.0, 0.0));
        assert!(!inside(r, 104.0, 10.0, 0.0));
        assert!(inside(r, 104.0, 10.0, 8.0));
        assert!(!inside(r, 10.0, -9.0, 8.0));

        let monitors = [Rect {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        }];
        assert!(on_any_monitor(100, 100, &monitors));
        assert!(on_any_monitor(-10, -10, &monitors));
        assert!(!on_any_monitor(-30, 100, &monitors));
        assert!(!on_any_monitor(3000, 0, &monitors));
        assert!(!on_any_monitor(0, 0, &[]));
    }

    #[test]
    fn resize_moves_the_right_edges() {
        let start = (100.0, 100.0, 400.0, 300.0);
        assert_eq!(resize_rect(start, "e", 50.0, 99.0), (100.0, 100.0, 450.0, 300.0));
        assert_eq!(resize_rect(start, "s", 99.0, 50.0), (100.0, 100.0, 400.0, 350.0));
        assert_eq!(resize_rect(start, "w", 50.0, 0.0), (150.0, 100.0, 350.0, 300.0));
        assert_eq!(resize_rect(start, "n", 0.0, 50.0), (100.0, 150.0, 400.0, 250.0));
        assert_eq!(resize_rect(start, "nw", 10.0, 20.0), (110.0, 120.0, 390.0, 280.0));
        assert_eq!(resize_rect(start, "se", 10.0, 20.0), (100.0, 100.0, 410.0, 320.0));
    }

    #[test]
    fn resize_clamps_to_the_minimum_keeping_the_far_edge() {
        let start = (100.0, 100.0, 400.0, 300.0);
        let (min_w, min_h) = (MIN_WIDTH as f64, MIN_HEIGHT as f64);
        assert_eq!(resize_rect(start, "e", -1000.0, 0.0), (100.0, 100.0, min_w, 300.0));
        assert_eq!(resize_rect(start, "s", 0.0, -1000.0), (100.0, 100.0, 400.0, min_h));
        // west: the right edge (x + w = 500) stays put
        assert_eq!(resize_rect(start, "w", 1000.0, 0.0), (500.0 - min_w, 100.0, min_w, 300.0));
        // north: the bottom edge (y + h = 400) stays put
        assert_eq!(resize_rect(start, "n", 0.0, 1000.0), (100.0, 400.0 - min_h, 400.0, min_h));
        assert_eq!(
            resize_rect(start, "nw", 1000.0, 1000.0),
            (500.0 - min_w, 400.0 - min_h, min_w, min_h)
        );
        assert_eq!(
            resize_rect(start, "se", -1000.0, -1000.0),
            (100.0, 100.0, min_w, min_h)
        );
        assert_eq!(
            resize_rect(start, "ne", -1000.0, 1000.0),
            (100.0, 400.0 - min_h, min_w, min_h)
        );
        assert_eq!(
            resize_rect(start, "sw", 1000.0, -1000.0),
            (500.0 - min_w, 100.0, min_w, min_h)
        );
    }

    #[test]
    fn hover_starts_hovered_and_leaves_after_the_delay() {
        let mut h = Hover::new();
        assert!(h.hovered);
        assert_eq!(h.tick(false, 0), None);
        assert_eq!(h.tick(false, HOVER_LEAVE_MS - 1), None);
        assert_eq!(h.tick(false, HOVER_LEAVE_MS), Some(false));
        assert_eq!(h.tick(false, HOVER_LEAVE_MS + 50), None);
    }

    #[test]
    fn hover_enters_after_the_delay() {
        let mut h = Hover::new();
        h.tick(false, 0);
        h.tick(false, 1000);
        assert!(!h.hovered);
        assert_eq!(h.tick(true, 2000), None);
        assert_eq!(h.tick(true, 2000 + HOVER_ENTER_MS - 1), None);
        assert_eq!(h.tick(true, 2000 + HOVER_ENTER_MS), Some(true));
        assert_eq!(h.tick(true, 3000), None);
    }

    #[test]
    fn hover_ignores_flicker_shorter_than_the_delay() {
        let mut h = Hover::new();
        h.tick(false, 0);
        h.tick(false, 1000);
        // a quick pass across the window
        assert_eq!(h.tick(true, 2000), None);
        assert_eq!(h.tick(true, 2200), None);
        assert_eq!(h.tick(false, 2250), None);
        assert_eq!(h.tick(true, 2300), None);
        // the pass restarted the clock rather than accumulating
        assert_eq!(h.tick(true, 2300 + HOVER_ENTER_MS - 1), None);
        assert!(!h.hovered);

        // and the same on the way out
        let mut h = Hover::new();
        assert_eq!(h.tick(false, 0), None);
        assert_eq!(h.tick(false, 500), None);
        assert_eq!(h.tick(true, 600), None);
        assert_eq!(h.tick(false, 650), None);
        assert_eq!(h.tick(false, 650 + HOVER_LEAVE_MS - 1), None);
        assert!(h.hovered);
    }

    #[test]
    fn click_through_only_when_idle_and_trackable() {
        assert!(ignore_cursor(false, false, true));
        assert!(!ignore_cursor(true, false, true));
        assert!(!ignore_cursor(false, true, true));
        assert!(!ignore_cursor(false, false, false));
    }
}
