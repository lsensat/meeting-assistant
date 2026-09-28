//! Anthropic's Messages API, over raw HTTP.
//!
//! Anthropic is not OpenAI-compatible in the way the providers behind
//! `openai.rs` are: the endpoint is `POST {base}/messages`, the key goes in
//! `x-api-key` rather than a bearer token, the system prompt is a top-level
//! field, and the reply is a list of content blocks rather than `choices`.
//! There is no official Rust SDK, so this is the documented HTTP shape.
//!
//! The same rules as `openai.rs` apply: this sends the transcript off the
//! machine, only ever because the user chose it, never as a fallback.

use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use crate::openai::{client, is_transport_safe, OpenAiError as ApiError};

const API_VERSION: &str = "2023-06-01";

/// Room for a long summary without truncating it mid-sentence. Non-streaming,
/// so kept where a single response stays well inside the request timeout.
const MAX_TOKENS: u32 = 16_000;

/// Same ceiling as the OpenAI-compatible path: a long transcript is legitimate;
/// this guards against a wedged connection.
const CHAT_TIMEOUT: Duration = Duration::from_secs(600);
const LIST_TIMEOUT: Duration = Duration::from_secs(20);

/// Models whose safety classifiers can decline a request. For these the
/// request opts into server-side fallbacks, so a false positive on an ordinary
/// meeting is answered by the model Anthropic recommends for that case instead
/// of failing the summary. Other models reject the parameter, so it is not
/// sent to them.
fn wants_fallbacks(model: &str) -> bool {
    model.starts_with("claude-opus-5") || model.starts_with("claude-fable-5")
}

const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";

#[derive(Debug, Deserialize)]
struct Block {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
}

#[derive(Debug, Deserialize)]
struct MessageResponse {
    #[serde(default)]
    content: Vec<Block>,
    #[serde(default)]
    stop_reason: Option<String>,
}

/// The text of a response: every `text` block, in order.
///
/// Not `content[0]`. Current models may return `thinking` blocks before the
/// answer, and a declined request returns no content at all — so the stop
/// reason is checked first and only text blocks are kept.
fn text_of(response: MessageResponse) -> Result<String, ApiError> {
    if response.stop_reason.as_deref() == Some("refusal") {
        return Err(ApiError::Refused);
    }
    let text: String = response
        .content
        .into_iter()
        .filter(|b| b.kind == "text")
        .map(|b| b.text)
        .collect();
    if text.trim().is_empty() {
        return Err(ApiError::Malformed("the response contained no text".into()));
    }
    Ok(text)
}

/// Anthropic's error bodies are `{"error": {"message": ...}}`; show that
/// message rather than the raw JSON. Truncated either way, as in `openai.rs`.
fn error_detail(body: &str) -> String {
    let message = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| body.to_string());
    message.chars().take(300).collect()
}

fn check(base_url: &str, api_key: &str) -> Result<(), ApiError> {
    if base_url.trim().is_empty() {
        return Err(ApiError::NotConfigured);
    }
    if api_key.trim().is_empty() {
        return Err(ApiError::MissingKey);
    }
    if !is_transport_safe(base_url) {
        return Err(ApiError::InsecureEndpoint(base_url.to_string()));
    }
    Ok(())
}

fn failure(response: reqwest::blocking::Response) -> ApiError {
    let status = response.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return ApiError::Unauthorized;
    }
    let detail = error_detail(&response.text().unwrap_or_default());
    ApiError::Http(format!("status {status}: {detail}"))
}

/// One Messages API round trip. Same system/user split as the other engines,
/// so the prompts are identical whichever provider answers.
pub fn chat(
    base_url: &str,
    api_key: &str,
    model: &str,
    system: &str,
    user: &str,
) -> Result<String, ApiError> {
    if model.trim().is_empty() {
        return Err(ApiError::NotConfigured);
    }
    check(base_url, api_key)?;

    // No `temperature`: current Claude models reject sampling parameters.
    let mut body = json!({
        "model": model,
        "max_tokens": MAX_TOKENS,
        "system": system,
        "messages": [{"role": "user", "content": user}],
    });

    let mut request = client()?
        .post(format!("{}/messages", base_url.trim_end_matches('/')))
        .header("x-api-key", api_key)
        .header("anthropic-version", API_VERSION);

    if wants_fallbacks(model) {
        body["fallbacks"] = json!("default");
        request = request.header("anthropic-beta", FALLBACK_BETA);
    }

    let response = request
        .json(&body)
        .timeout(CHAT_TIMEOUT)
        .send()
        .map_err(|e| ApiError::Unreachable(e.to_string()))?;

    if !response.status().is_success() {
        return Err(failure(response));
    }

    let parsed: MessageResponse = response
        .json()
        .map_err(|e| ApiError::Malformed(e.to_string()))?;
    text_of(parsed)
}

#[derive(Debug, Deserialize)]
struct ModelList {
    #[serde(default)]
    data: Vec<ModelEntry>,
}

#[derive(Debug, Deserialize)]
struct ModelEntry {
    id: String,
}

/// The models this key can use, from `GET {base}/models`.
///
/// Also the cheapest possible check that a key works: it costs no tokens, and
/// a rejected key comes back as [`ApiError::Unauthorized`].
pub fn list_models(base_url: &str, api_key: &str) -> Result<Vec<String>, ApiError> {
    check(base_url, api_key)?;

    let response = client()?
        .get(format!("{}/models?limit=1000", base_url.trim_end_matches('/')))
        .header("x-api-key", api_key)
        .header("anthropic-version", API_VERSION)
        .timeout(LIST_TIMEOUT)
        .send()
        .map_err(|e| ApiError::Unreachable(e.to_string()))?;

    if !response.status().is_success() {
        return Err(failure(response));
    }

    let parsed: ModelList = response
        .json()
        .map_err(|e| ApiError::Malformed(e.to_string()))?;
    // Newest first, as the API returns them — the order a person choosing a
    // model wants.
    Ok(parsed.data.into_iter().map(|m| m.id).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> MessageResponse {
        serde_json::from_str(json).expect("valid test JSON")
    }

    #[test]
    fn only_text_blocks_are_kept() {
        let r = parse(
            r#"{"content": [
                {"type": "thinking", "thinking": ""},
                {"type": "text", "text": "Summary."},
                {"type": "text", "text": " More."}
            ], "stop_reason": "end_turn"}"#,
        );
        assert_eq!(text_of(r).expect("text"), "Summary. More.");
    }

    #[test]
    fn a_refusal_is_an_error_not_an_empty_summary() {
        let r = parse(r#"{"content": [], "stop_reason": "refusal"}"#);
        assert!(matches!(text_of(r), Err(ApiError::Refused)));
    }

    #[test]
    fn a_reply_without_text_is_malformed() {
        let r = parse(r#"{"content": [{"type": "thinking", "thinking": ""}], "stop_reason": "end_turn"}"#);
        assert!(matches!(text_of(r), Err(ApiError::Malformed(_))));
    }

    #[test]
    fn error_bodies_show_their_message() {
        let body = r#"{"type":"error","error":{"type":"not_found_error","message":"model: claude-nope"}}"#;
        assert_eq!(error_detail(body), "model: claude-nope");
        assert_eq!(error_detail("plain text"), "plain text");
    }

    #[test]
    fn fallbacks_only_for_models_that_take_them() {
        assert!(wants_fallbacks("claude-opus-5"));
        assert!(wants_fallbacks("claude-opus-5-5"));
        assert!(wants_fallbacks("claude-fable-5-1"));
        assert!(!wants_fallbacks("claude-sonnet-5"));
        assert!(!wants_fallbacks("claude-haiku-4-5"));
    }

    #[test]
    fn incomplete_configuration_never_sends_a_request() {
        assert!(matches!(chat("https://x/v1", "key", " ", "s", "u"), Err(ApiError::NotConfigured)));
        assert!(matches!(chat("https://x/v1", "", "m", "s", "u"), Err(ApiError::MissingKey)));
        assert!(matches!(
            chat("http://api.anthropic.com/v1", "key", "m", "s", "u"),
            Err(ApiError::InsecureEndpoint(_))
        ));
    }
}
