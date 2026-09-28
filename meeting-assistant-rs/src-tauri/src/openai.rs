//! OpenAI-compatible chat client.
//!
//! One implementation covers OpenAI, Groq, OpenRouter, Together, LM Studio and
//! a self-hosted vLLM, because they all speak `POST {base}/chat/completions`
//! with the same message shape. That is the whole reason this is a generic
//! client rather than a set of named providers.
//!
//! # This is the only code in the app that sends data off the machine
//!
//! Everything else — recording, transcription, local summaries — stays on the
//! device. Reaching this module means the user explicitly chose a remote
//! provider in Settings. It must never be used as a fallback when Ollama fails.
//!
//! Only the **transcript text** is ever sent. Audio never leaves the machine
//! under any configuration.

use std::sync::OnceLock;
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

/// One client, built once and never dropped — for the reason spelled out on
/// `ollama::CLIENT`: dropping a `reqwest::blocking::Client` joins a thread and
/// drops a tokio runtime, and every Tauri command runs inside one.
///
/// Unlike Ollama's, this one talks to the open internet, so it is the client
/// whose connection reuse is actually worth something.
static CLIENT: OnceLock<Result<reqwest::blocking::Client, String>> = OnceLock::new();

/// Shared with `anthropic.rs`, which talks to the internet the same way.
pub(crate) fn client() -> Result<&'static reqwest::blocking::Client, OpenAiError> {
    CLIENT
        .get_or_init(|| {
            reqwest::blocking::Client::builder()
                .build()
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(|e| OpenAiError::Http(e.clone()))
}

/// Matches the Ollama path's temperature so switching provider does not also
/// silently change the character of the summaries.
const TEMPERATURE: f64 = 0.2;

/// Same generous ceiling as the local path: a long transcript against a slow
/// endpoint is legitimate. This guards against a wedged connection, not against
/// slowness.
const CHAT_TIMEOUT: Duration = Duration::from_secs(600);

/// Errors from either remote client — this one and `anthropic.rs`. The name
/// predates the second; the variants were never specific to OpenAI.
#[derive(Debug)]
pub enum OpenAiError {
    /// No base URL configured — the user selected the remote provider but
    /// never finished setting it up.
    NotConfigured,
    /// The endpoint is plain HTTP and not on the loopback interface, so the
    /// transcript would cross the network in cleartext.
    InsecureEndpoint(String),
    /// No API key in the keychain for this endpoint.
    MissingKey,
    /// 401/403. Worth separating from a generic HTTP error because the fix is
    /// specific and obvious.
    Unauthorized,
    /// The endpoint could not be reached at all.
    Unreachable(String),
    Http(String),
    Malformed(String),
    /// The model declined the request (Anthropic's `stop_reason: "refusal"`).
    /// A successful HTTP response with nothing usable in it, so it has to be
    /// said in words rather than surfacing as an empty summary.
    Refused,
}

impl std::fmt::Display for OpenAiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InsecureEndpoint(url) => write!(
                f,
                "Refusing to send the transcript to {url} over plain HTTP. Use an https:// endpoint."
            ),
            Self::NotConfigured => write!(
                f,
                "No API provider configured. Open Settings and choose a provider and a model."
            ),
            Self::MissingKey => write!(
                f,
                "No API key found. Open Settings and enter the key for this endpoint."
            ),
            Self::Unauthorized => write!(
                f,
                "The API key was rejected. Open Settings and check the key for this endpoint."
            ),
            Self::Unreachable(e) => write!(f, "Could not reach the API endpoint: {e}"),
            Self::Http(e) => write!(f, "The API request failed: {e}"),
            Self::Malformed(e) => write!(f, "Unexpected response from the API: {e}"),
            Self::Refused => write!(
                f,
                "The model declined to summarise this transcript. Try again, or choose another model in Settings."
            ),
        }
    }
}

impl std::error::Error for OpenAiError {}

#[derive(Debug, Deserialize)]
struct Message {
    #[serde(default)]
    content: String,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: Option<Message>,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    #[serde(default)]
    choices: Vec<Choice>,
}

/// One `/chat/completions` round trip.
///
/// The system/user split matches the Ollama path exactly, so the prompts in
/// `meeting-core::prompts` are used unchanged regardless of provider. A summary
/// must not depend on which engine produced it.
pub fn chat(
    base_url: &str,
    api_key: &str,
    model: &str,
    system: &str,
    user: &str,
) -> Result<String, OpenAiError> {
    if base_url.trim().is_empty() || model.trim().is_empty() {
        return Err(OpenAiError::NotConfigured);
    }
    if api_key.trim().is_empty() {
        return Err(OpenAiError::MissingKey);
    }

    // The transcript is the most sensitive thing this app holds, and http://
    // would put it on the wire in the clear. Loopback is exempt: LM Studio and
    // a local vLLM serve plain HTTP on 127.0.0.1, and that traffic never leaves
    // the machine.
    if !is_transport_safe(base_url) {
        return Err(OpenAiError::InsecureEndpoint(base_url.to_string()));
    }

    let body = json!({
        "model": model,
        "temperature": TEMPERATURE,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
    });

    let response = client()?
        .post(format!("{}/chat/completions", base_url.trim_end_matches('/')))
        .bearer_auth(api_key)
        .json(&body)
        .timeout(CHAT_TIMEOUT)
        .send()
        .map_err(|e| OpenAiError::Unreachable(e.to_string()))?;

    let status = response.status();
    if !status.is_success() {
        if status.as_u16() == 401 || status.as_u16() == 403 {
            return Err(OpenAiError::Unauthorized);
        }
        let detail = response.text().unwrap_or_default();
        // Deliberately truncated: some gateways echo the whole request back in
        // the error body, and the request contains the transcript.
        let detail: String = detail.chars().take(300).collect();
        return Err(OpenAiError::Http(format!("status {status}: {detail}")));
    }

    let parsed: ChatResponse = response
        .json()
        .map_err(|e| OpenAiError::Malformed(e.to_string()))?;

    parsed
        .choices
        .into_iter()
        .next()
        .and_then(|c| c.message)
        .map(|m| m.content)
        .ok_or_else(|| OpenAiError::Malformed("response contained no choices".into()))
}

const LIST_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Deserialize)]
struct ModelList {
    #[serde(default)]
    data: Vec<ModelEntry>,
}

#[derive(Debug, Deserialize)]
struct ModelEntry {
    id: String,
}

/// The models an OpenAI-compatible endpoint offers, from `GET {base}/models`.
///
/// Every provider in the list serves this, and it costs no tokens — which also
/// makes it the way to find out whether a key works before a meeting depends
/// on it. Sorted, because providers return them in no useful order.
pub fn list_models(base_url: &str, api_key: &str) -> Result<Vec<String>, OpenAiError> {
    if base_url.trim().is_empty() {
        return Err(OpenAiError::NotConfigured);
    }
    if !is_transport_safe(base_url) {
        return Err(OpenAiError::InsecureEndpoint(base_url.to_string()));
    }

    // A key is optional here: a local server (LM Studio, vLLM) often has none,
    // and OpenRouter lists models without one.
    let mut request = client()?
        .get(format!("{}/models", base_url.trim_end_matches('/')))
        .timeout(LIST_TIMEOUT);
    if !api_key.trim().is_empty() {
        request = request.bearer_auth(api_key);
    }

    let response = request
        .send()
        .map_err(|e| OpenAiError::Unreachable(e.to_string()))?;

    let status = response.status();
    if !status.is_success() {
        if status.as_u16() == 401 || status.as_u16() == 403 {
            return Err(OpenAiError::Unauthorized);
        }
        let detail: String = response.text().unwrap_or_default().chars().take(300).collect();
        return Err(OpenAiError::Http(format!("status {status}: {detail}")));
    }

    let parsed: ModelList = response
        .json()
        .map_err(|e| OpenAiError::Malformed(e.to_string()))?;
    Ok(tidy_model_ids(parsed.data.into_iter().map(|m| m.id)))
}

/// Sorted, de-duplicated, and without Gemini's `models/` prefix — its list
/// names models `models/gemini-...`, while chat requests take the bare id.
fn tidy_model_ids(ids: impl Iterator<Item = String>) -> Vec<String> {
    let mut ids: Vec<String> = ids
        .map(|id| id.strip_prefix("models/").map(str::to_string).unwrap_or(id))
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// True when the endpoint is https, or plain http on the loopback interface.
/// Shared with `anthropic.rs`.
pub(crate) fn is_transport_safe(base_url: &str) -> bool {
    let url = base_url.trim().to_ascii_lowercase();

    if url.starts_with("https://") {
        return true;
    }
    if let Some(rest) = url.strip_prefix("http://") {
        let host = rest
            .split(['/', ':'])
            .next()
            .unwrap_or_default();
        return matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1");
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_the_first_choice() {
        let parsed: ChatResponse = serde_json::from_str(
            r#"{"choices":[{"message":{"role":"assistant","content":"hello"}}]}"#,
        )
        .expect("parse");
        assert_eq!(
            parsed.choices.into_iter().next().and_then(|c| c.message).map(|m| m.content),
            Some("hello".to_string())
        );
    }

    /// An empty `choices` array is a valid JSON body but useless; it must not
    /// come back as an empty summary that looks like the model had nothing
    /// to say.
    #[test]
    fn an_empty_choices_array_is_an_error() {
        let parsed: ChatResponse = serde_json::from_str(r#"{"choices":[]}"#).expect("parse");
        assert!(parsed.choices.is_empty());
    }

    /// The transcript must never cross a network in cleartext. Loopback is the
    /// one exception, because LM Studio and a local vLLM serve plain http there
    /// and that traffic does not leave the machine.
    #[test]
    fn model_ids_are_sorted_deduplicated_and_unprefixed() {
        let ids = ["models/gemini-b", "gpt-4", "models/gemini-a", "gpt-4"]
            .into_iter()
            .map(String::from);
        assert_eq!(tidy_model_ids(ids), vec!["gemini-a", "gemini-b", "gpt-4"]);
    }

    #[test]
    fn plain_http_is_refused_except_on_loopback() {
        assert!(is_transport_safe("https://api.openai.com/v1"));
        assert!(is_transport_safe("HTTPS://API.OPENAI.COM/v1"));
        assert!(is_transport_safe("http://localhost:1234/v1"));
        assert!(is_transport_safe("http://127.0.0.1:8000/v1"));

        assert!(!is_transport_safe("http://api.openai.com/v1"));
        assert!(!is_transport_safe("http://192.168.1.10:8000/v1"));
        assert!(!is_transport_safe("http://evil.example.com/v1"));
        // Not a scheme we understand: refuse rather than guess.
        assert!(!is_transport_safe("api.openai.com/v1"));
        assert!(!is_transport_safe("ftp://example.com"));
        // A host that merely starts with "localhost" is not loopback.
        assert!(!is_transport_safe("http://localhost.evil.com/v1"));
    }

    /// Misconfiguration must be caught before a request is built, so the
    /// transcript is never sent to a half-configured endpoint.
    #[test]
    fn incomplete_configuration_never_sends_a_request() {
        assert!(matches!(
            chat("", "key", "model", "s", "u"),
            Err(OpenAiError::NotConfigured)
        ));
        assert!(matches!(
            chat("https://x/v1", "key", "  ", "s", "u"),
            Err(OpenAiError::NotConfigured)
        ));
        assert!(matches!(
            chat("https://x/v1", "", "model", "s", "u"),
            Err(OpenAiError::MissingKey)
        ));
    }
}
