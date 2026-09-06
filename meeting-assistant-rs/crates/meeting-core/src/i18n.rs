//! Translated UI strings. Port of `TEXTS` and `tr()` in `app.py`.
//!
//! The catalog is a single JSON file, `i18n.json`, embedded at compile time and
//! also handed to the frontend through the `get_i18n` command — one source of
//! truth for both sides rather than a Rust copy and a JavaScript copy that
//! drift apart.
//!
//! # Simplification from the Python
//!
//! `app.py` had four parallel lookup paths: `tr()` over `TEXTS`, plus
//! `summary_type_display`, `transcription_language_display` and
//! `language_display` over three separate label dicts, each with its own
//! `*_from_display` inverse for reading the value back out of a widget.
//!
//! Here those labels live in the same flat namespace under `summary_type.*`,
//! `transcription_language.*` and `app_language.*`, so there is one lookup and
//! no inverse at all — the UI keeps ids and only ever renders labels. That also
//! removes the `parse_whisper_value` class of bug, where the selected value had
//! to be recovered by splitting a display string.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::config::Language;

/// The generated catalog. Regenerate from `app.py` rather than editing by hand
/// while the Python original still exists.
const CATALOG_JSON: &str = include_str!("../i18n.json");

type Catalog = HashMap<String, HashMap<String, String>>;

fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        serde_json::from_str(CATALOG_JSON)
            .expect("embedded i18n.json is valid and checked by tests")
    })
}

/// Look up a translated string.
///
/// Falls back the same way the Python did: requested language, then English,
/// then the key itself, so a missing translation degrades to something visible
/// rather than panicking or blanking the UI.
///
/// The result borrows from the key rather than being `'static`, because the
/// last fallback hands the key straight back; catalog hits are `'static` and
/// coerce into it.
pub fn tr(language: Language, key: &str) -> &str {
    let catalog = catalog();

    catalog
        .get(language.as_str())
        .and_then(|m| m.get(key))
        .or_else(|| catalog.get("en").and_then(|m| m.get(key)))
        .map(String::as_str)
        .unwrap_or(key)
}

/// Look up a string and substitute `{placeholder}` arguments.
///
/// ```
/// # use meeting_core::{config::Language, i18n::tr_args};
/// let s = tr_args(Language::En, "summarizing", &[("model", "gemma3:4b")]);
/// assert_eq!(s, "Summarizing with gemma3:4b...");
/// ```
///
/// Placeholders with no matching argument are left as-is. The Python caught the
/// `KeyError` from `str.format` and returned the whole unformatted string; this
/// substitutes what it can, which is strictly more useful and never worse.
pub fn tr_args(language: Language, key: &str, args: &[(&str, &str)]) -> String {
    let template = tr(language, key);

    if args.is_empty() || !template.contains('{') {
        return template.to_string();
    }

    let mut out = template.to_string();
    for (name, value) in args {
        out = out.replace(&format!("{{{name}}}"), value);
    }
    out
}

/// Every key in the catalog, sorted. Used by the frontend handshake and tests.
pub fn keys() -> Vec<&'static str> {
    let mut keys: Vec<&str> = catalog()
        .get("en")
        .map(|m| m.keys().map(String::as_str).collect())
        .unwrap_or_default();
    keys.sort_unstable();
    keys
}

/// The whole catalog as JSON, for the `get_i18n` command.
pub fn catalog_json() -> &'static str {
    CATALOG_JSON
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_parses() {
        assert_eq!(catalog().len(), 2, "expected exactly en and es");
    }

    /// The safety net for the whole module: a key present in one language and
    /// missing in the other would silently fall back to English in the UI.
    ///
    /// This replaced a `keys().len() == 110` assertion, which could only ever
    /// fail on a legitimate addition and caught nothing. Its companion "keys
    /// are sorted" check was tautological too, since `keys()` sorts before
    /// returning.
    #[test]
    fn both_languages_have_identical_key_sets() {
        let en: std::collections::BTreeSet<_> = catalog()["en"].keys().collect();
        let es: std::collections::BTreeSet<_> = catalog()["es"].keys().collect();

        let only_en: Vec<_> = en.difference(&es).collect();
        let only_es: Vec<_> = es.difference(&en).collect();

        assert!(only_en.is_empty(), "keys missing from Spanish: {only_en:?}");
        assert!(only_es.is_empty(), "keys missing from English: {only_es:?}");
    }

    #[test]
    fn no_string_is_empty() {
        for (lang, entries) in catalog() {
            for (key, value) in entries {
                assert!(!value.trim().is_empty(), "{lang}.{key} is empty");
            }
        }
    }

    /// A placeholder present in one language but not the other means one of them
    /// renders a literal `{model}` to the user.
    #[test]
    fn placeholders_agree_across_languages() {
        fn placeholders(s: &str) -> std::collections::BTreeSet<String> {
            let mut found = std::collections::BTreeSet::new();
            let mut rest = s;
            while let Some(start) = rest.find('{') {
                let after = &rest[start + 1..];
                match after.find('}') {
                    Some(end) => {
                        found.insert(after[..end].to_string());
                        rest = &after[end + 1..];
                    }
                    None => break,
                }
            }
            found
        }

        for (key, en_value) in &catalog()["en"] {
            let es_value = &catalog()["es"][key];
            assert_eq!(
                placeholders(en_value),
                placeholders(es_value),
                "placeholder mismatch on key {key:?}"
            );
        }
    }

    // --- lookup behaviour --------------------------------------------------

    #[test]
    fn looks_up_both_languages() {
        assert_eq!(tr(Language::En, "settings"), "Settings");
        assert_eq!(tr(Language::Es, "settings"), "Configuración");
    }

    #[test]
    fn unknown_key_returns_the_key() {
        assert_eq!(tr(Language::En, "no_such_key"), "no_such_key");
    }

    #[test]
    fn substitutes_arguments() {
        assert_eq!(
            tr_args(Language::En, "summarizing", &[("model", "gemma3:4b")]),
            "Summarizing with gemma3:4b..."
        );
        assert_eq!(
            tr_args(Language::Es, "summarizing", &[("model", "gemma3:4b")]),
            "Resumiendo con gemma3:4b..."
        );
    }

    #[test]
    fn substitutes_several_arguments() {
        let s = tr_args(
            Language::En,
            "summary_block",
            &[("index", "2"), ("total", "5"), ("model", "gemma3:4b")],
        );
        assert_eq!(s, "Summary: block 2/5 with gemma3:4b...");
    }

    #[test]
    fn unmatched_placeholder_is_left_alone() {
        let s = tr_args(Language::En, "summarizing", &[]);
        assert!(s.contains("{model}"), "got {s:?}");
    }

    // --- the folded-in label namespaces ------------------------------------

    #[test]
    fn summary_type_labels_are_translated() {
        assert_eq!(
            tr(Language::En, "summary_type.meeting_minutes"),
            "Meeting minutes"
        );
        assert_eq!(
            tr(Language::Es, "summary_type.meeting_minutes"),
            "Acta de reunión"
        );
    }

    #[test]
    fn every_summary_type_has_a_label() {
        for summary_type in crate::config::SummaryType::ALL {
            let key = format!("summary_type.{}", summary_type.as_str());
            assert_ne!(tr(Language::En, &key), key, "missing label for {key}");
            assert_ne!(tr(Language::Es, &key), key, "missing label for {key}");
        }
    }

    #[test]
    fn transcription_language_labels_are_translated() {
        assert_eq!(
            tr(Language::En, "transcription_language.auto"),
            "Auto-detect"
        );
        assert_eq!(
            tr(Language::Es, "transcription_language.auto"),
            "Detección automática"
        );
    }

    #[test]
    fn app_language_labels_are_endonyms() {
        // Shown the same whichever language the UI is in, as in the Python.
        for language in [Language::En, Language::Es] {
            assert_eq!(tr(language, "app_language.en"), "English");
            assert_eq!(tr(language, "app_language.es"), "Español");
        }
    }

}
