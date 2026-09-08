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

/// Ollama is on loopback: if it does not answer in three seconds it is not
/// running. The Python allowed 15 s (`app.py:894-911`), but that was a startup
/// probe in a thread nobody waited on — here Settings and the setup wizard both
/// block on this before they can render their model list, so the timeout is
/// how long a machine without Ollama waits to see its own settings.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

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
    /// Ollama could not load the model because the machine ran out of memory.
    ///
    /// Its own report of this is a wall of JSON — allocation sizes, a failed
    /// `GGML_ASSERT`, and often a second failure while terminating the process.
    /// None of it tells the user the one thing they can act on.
    ///
    /// # Why the advice names text-only models specifically
    ///
    /// The report that prompted this said "failed **before projector CPU
    /// offload retry**", and a projector is a vision encoder: the model was
    /// multimodal, and a large share of what it was trying to allocate was a
    /// component this app can never use. Nothing here sends an image. So the
    /// useful remedy is not merely "something smaller" — a text-only model of
    /// the same parameter count avoids the cost entirely.
    OutOfMemory(String),
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
            Self::OutOfMemory(model) => write!(
                f,
                "Ollama ran out of memory loading \"{model}\". Pick a smaller, text-only model in Settings and retry — this app only ever sends text."
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
        if is_out_of_memory(&detail) {
            return Err(OllamaError::OutOfMemory(model.to_string()));
        }
        // Truncated. Ollama's error bodies run to several hundred characters of
        // allocator internals, and the whole thing used to reach the status
        // line and wrap it over ten lines, pushing the rest of the window
        // aside. The full body is still logged.
        return Err(OllamaError::Http(format!(
            "status {status}: {}",
            truncate(&detail, 120)
        )));
    }

    let parsed: ChatResponse = response
        .json()
        .map_err(|e| OllamaError::Malformed(e.to_string()))?;

    parsed
        .message
        .map(|m| m.content)
        .ok_or_else(|| OllamaError::Malformed("response had no message".into()))
}

/// Whether an Ollama error body is really "the model does not fit in memory".
///
/// Matched on the phrases rather than the status code: Ollama reports this as a
/// generic 500, and the same 500 covers unrelated faults. The three checked here
/// are what a failed model load actually emits — its own summary, the
/// allocator's, and the assertion that fires when the buffer comes back null.
fn is_out_of_memory(detail: &str) -> bool {
    let lower = detail.to_lowercase();
    lower.contains("out of memory")
        || lower.contains("out-of-memory")
        || lower.contains("failed to allocate")
}

/// Cut to `max` characters on a char boundary, marking that it was cut.
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max).collect();
    format!("{}…", kept.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_out_of_memory_body_is_recognised() {
        // Verbatim from the Windows machine that hit this.
        let body = r#"{"error":"llama-server startup failed before projector CPU offload retry: llama-server reported out-of-memory during startup: ggml_backend_cpu_buffer_type_alloc_buffer: failed to allocate buffer of size 851226048"}"#;
        assert!(is_out_of_memory(body));
    }

    #[test]
    fn an_unrelated_failure_is_not_called_out_of_memory() {
        // Claiming the wrong cause would send the user to change a model that
        // was never the problem.
        assert!(!is_out_of_memory(r#"{"error":"model is required"}"#));
        assert!(!is_out_of_memory("status 500: something else entirely"));
    }

    #[test]
    fn a_long_error_body_is_cut_short() {
        let long = "x".repeat(400);
        let cut = truncate(&long, 120);
        assert_eq!(cut.chars().count(), 121, "120 characters plus the ellipsis");
        assert!(cut.ends_with('…'));
        // Short ones are left exactly as they are.
        assert_eq!(truncate("brief", 120), "brief");
    }

    #[test]
    fn truncation_does_not_split_a_character() {
        // Ollama's messages can carry non-ASCII; cutting by bytes would panic.
        let text = "é".repeat(200);
        assert_eq!(truncate(&text, 10).chars().count(), 11);
    }

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
