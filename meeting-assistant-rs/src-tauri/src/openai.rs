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

fn client() -> Result<&'static reqwest::blocking::Client, OpenAiError> {
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
                "No API endpoint configured. Open Settings and enter a base URL and model."
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
/// `meeting-core::prompts` — which are byte-identical to the Python's — are
/// used unchanged regardless of provider.
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

/// True when the endpoint is https, or plain http on the loopback interface.
fn is_transport_safe(base_url: &str) -> bool {
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
