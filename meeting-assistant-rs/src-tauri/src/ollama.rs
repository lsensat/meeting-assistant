//! Ollama client. Port of the `ollama` Python package usage in `app.py`.
//!
//! Only two endpoints are used — `/api/tags` to list installed models and
//! `/api/chat` to summarize — so this talks HTTP directly rather than taking a
//! wrapper crate that may lag behind the server.
//!
//! Blocking on purpose: the pipeline already runs on its own worker thread, and
//! a blocking client keeps tokio out of the dependency tree entirely.

use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

/// Same host and port the Python `ollama` package defaults to, on both
/// platforms. Loopback only — nothing here should ever reach the network.
pub const BASE_URL: &str = "http://127.0.0.1:11434";

/// Matches the Python's `options={"temperature": 0.2}` (`app.py:2160`, `2196`).
/// Low but not zero: the summary should be stable without being degenerate.
const TEMPERATURE: f64 = 0.2;

/// The startup probe is allowed to be slow — Ollama may be cold-starting — but
/// not unbounded. The Python used 15 s (`app.py:894-911`).
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// Summarization is the long pole: a 7B model on a chunk of transcript can
/// legitimately take minutes on CPU. This is a guard against a wedged server,
/// not a performance target.
const CHAT_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug)]
pub enum OllamaError {
    /// No model was selected in settings. Sending an empty model name gets a
    /// `400 {"error":"model is required"}` back, which is accurate but reads
    /// like a crash rather than "you have not picked a model yet".
    NoModelSelected,
    /// The named model is not installed on this Ollama.
    ModelNotFound(String),
    /// The server is not reachable at all — almost always "Ollama is not
    /// running", which the UI reports differently from a request failure.
    Unreachable(String),
    Http(String),
    /// A 2xx response whose body was not the shape we expect.
    Malformed(String),
}

impl std::fmt::Display for OllamaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoModelSelected => write!(
                f,
                "No Ollama model selected. Open Settings and choose one."
            ),
            Self::ModelNotFound(model) => write!(
                f,
                "The Ollama model \"{model}\" is not installed. Open Settings and choose an installed model."
            ),
            Self::Unreachable(e) => write!(
                f,
                "Ollama is not running. Start Ollama and try again. ({e})"
            ),
            Self::Http(e) => write!(f, "Ollama request failed: {e}"),
            Self::Malformed(e) => write!(f, "Unexpected response from Ollama: {e}"),
        }
    }
}

impl std::error::Error for OllamaError {}

/// One installed model, as reported by `/api/tags`.
///
/// **Three shapes are tolerated on purpose.** The Python accepted `item.model`,
/// `item.name`, or a dict with either key (`app.py:819-824`), because the field
/// moved between Ollama versions. Keeping both here means a server upgrade
/// cannot silently produce an empty model list.
#[derive(Debug, Deserialize)]
struct TagEntry {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

impl TagEntry {
    fn resolve(self) -> Option<String> {
        self.model.or(self.name).filter(|s| !s.is_empty())
    }
}

#[derive(Debug, Deserialize)]
struct TagsResponse {
    #[serde(default)]
    models: Vec<TagEntry>,
}

#[derive(Debug, Deserialize)]
struct ChatMessage {
    #[serde(default)]
    content: String,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    message: Option<ChatMessage>,
}

fn client(timeout: Duration) -> Result<reqwest::blocking::Client, OllamaError> {
    reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| OllamaError::Http(e.to_string()))
}

/// Installed model names, sorted and deduplicated.
///
/// Port of `ollama_runtime_status` (`app.py:806`). An empty list with `Ok` means
/// Ollama is running but has no models — a different UI state from unreachable,
/// so the distinction must survive.
pub fn list_models() -> Result<Vec<String>, OllamaError> {
    let response = client(PROBE_TIMEOUT)?
        .get(format!("{BASE_URL}/api/tags"))
        .send()
        .map_err(|e| OllamaError::Unreachable(e.to_string()))?;

    if !response.status().is_success() {
        return Err(OllamaError::Http(format!("status {}", response.status())));
    }

    let parsed: TagsResponse = response
        .json()
        .map_err(|e| OllamaError::Malformed(e.to_string()))?;

    let mut names: Vec<String> = parsed.models.into_iter().filter_map(TagEntry::resolve).collect();
    names.sort();
    names.dedup();
    Ok(names)
}

/// True when the server answers at all.
pub fn is_running() -> bool {
    list_models().is_ok()
}

/// One `/api/chat` round trip with `stream: false`.
///
/// The message shape mirrors the Python exactly: a system message carrying the
/// meeting-analysis framing, then a single user message holding the instruction
/// and the transcript text.
pub fn chat(model: &str, system: &str, user: &str) -> Result<String, OllamaError> {
    // Caught here rather than at the server: the default config ships with an
    // empty `ollama_model`, so a first run with nothing chosen in Settings
    // would otherwise surface a raw 400 body to the user.
    if model.trim().is_empty() {
        return Err(OllamaError::NoModelSelected);
    }

    let body = json!({
        "model": model,
        "stream": false,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
        "options": {"temperature": TEMPERATURE},
    });

    let response = client(CHAT_TIMEOUT)?
        .post(format!("{BASE_URL}/api/chat"))
        .json(&body)
        .send()
        .map_err(|e| OllamaError::Unreachable(e.to_string()))?;

    if !response.status().is_success() {
        let status = response.status();
        let detail = response.text().unwrap_or_default();

        // 404 from /api/chat means the model name is not pulled. Say so, rather
        // than repeating Ollama's JSON at the user.
        if status.as_u16() == 404 {
            return Err(OllamaError::ModelNotFound(model.to_string()));
        }
        return Err(OllamaError::Http(format!("status {status}: {detail}")));
    }

    let parsed: ChatResponse = response
        .json()
        .map_err(|e| OllamaError::Malformed(e.to_string()))?;

    parsed
        .message
        .map(|m| m.content)
        .ok_or_else(|| OllamaError::Malformed("response had no message".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three response shapes the Python tolerated. A server upgrade that
    /// renames this field must not silently yield "no models installed".
    #[test]
    fn tag_entries_accept_model_or_name() {
        let parsed: TagsResponse = serde_json::from_str(
            r#"{"models":[
                {"model":"a:latest"},
                {"name":"b:latest"},
                {"model":"c:latest","name":"ignored"}
            ]}"#,
        )
        .expect("parse");

        let names: Vec<String> = parsed.models.into_iter().filter_map(TagEntry::resolve).collect();
        assert_eq!(names, vec!["a:latest", "b:latest", "c:latest"]);
    }

    #[test]
    fn tag_entries_with_neither_field_are_skipped() {
        let parsed: TagsResponse =
            serde_json::from_str(r#"{"models":[{},{"model":""},{"name":"real"}]}"#).expect("parse");
        let names: Vec<String> = parsed.models.into_iter().filter_map(TagEntry::resolve).collect();
        assert_eq!(names, vec!["real"]);
    }

    /// A running server with zero models is `Ok(vec![])`, not an error — the UI
    /// shows a different message for each.
    #[test]
    fn missing_models_field_is_an_empty_list() {
        let parsed: TagsResponse = serde_json::from_str("{}").expect("parse");
        assert!(parsed.models.is_empty());
    }

    #[test]
    fn chat_response_extracts_content() {
        let parsed: ChatResponse =
            serde_json::from_str(r#"{"message":{"role":"assistant","content":"hello"}}"#)
                .expect("parse");
        assert_eq!(parsed.message.map(|m| m.content).as_deref(), Some("hello"));
    }
}
