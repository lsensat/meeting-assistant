//! The remote summary providers the app knows by name.
//!
//! # Why a list and not just a URL field
//!
//! Every provider publishes one base URL, so asking the user to type it is
//! asking them to look up something the app already knows — and to get it
//! subtly wrong. A user who pasted Anthropic's `.../v1/messages` endpoint into
//! the old URL field had a configuration that could never work: the client
//! appended `/chat/completions` to it. Picking a provider removes that class
//! of mistake; **Other** keeps the free-form URL for anything not listed
//! (a self-hosted vLLM, LM Studio, a company gateway).
//!
//! # Two protocols
//!
//! Anthropic has its own Messages API. Everything else here speaks the
//! OpenAI-compatible `/chat/completions` shape, which is why one client covers
//! OpenAI, Gemini, Mistral, OpenRouter, Groq and "Other" alike.

/// How the app talks to a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiProtocol {
    /// Anthropic's Messages API: `POST {base}/messages`, `x-api-key`.
    Anthropic,
    /// `POST {base}/chat/completions` with a bearer token.
    OpenAiCompatible,
}

/// A provider the app knows by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApiPreset {
    /// Stored in `config.json` as `api_provider`.
    pub id: &'static str,
    /// Shown in the provider list. A brand name, so not translated.
    pub label: &'static str,
    pub base_url: &'static str,
    pub protocol: ApiProtocol,
}

/// The `api_provider` value for a provider that is not in [`PRESETS`]: the
/// user supplies the base URL, and it is spoken to as OpenAI-compatible.
pub const CUSTOM: &str = "custom";

/// The providers the app knows by name, in the order the list shows them.
pub const PRESETS: [ApiPreset; 6] = [
    ApiPreset {
        id: "anthropic",
        label: "Anthropic (Claude)",
        base_url: "https://api.anthropic.com/v1",
        protocol: ApiProtocol::Anthropic,
    },
    ApiPreset {
        id: "openai",
        label: "OpenAI",
        base_url: "https://api.openai.com/v1",
        protocol: ApiProtocol::OpenAiCompatible,
    },
    ApiPreset {
        id: "gemini",
        label: "Google Gemini",
        base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
        protocol: ApiProtocol::OpenAiCompatible,
    },
    ApiPreset {
        id: "mistral",
        label: "Mistral",
        base_url: "https://api.mistral.ai/v1",
        protocol: ApiProtocol::OpenAiCompatible,
    },
    ApiPreset {
        id: "openrouter",
        label: "OpenRouter",
        base_url: "https://openrouter.ai/api/v1",
        protocol: ApiProtocol::OpenAiCompatible,
    },
    ApiPreset {
        id: "groq",
        label: "Groq",
        base_url: "https://api.groq.com/openai/v1",
        protocol: ApiProtocol::OpenAiCompatible,
    },
];

/// The preset with this id, if it is one.
pub fn preset(id: &str) -> Option<&'static ApiPreset> {
    PRESETS.iter().find(|p| p.id == id)
}

/// Where a provider id plus the user's own URL actually point: the protocol
/// to speak and the base URL to speak it to.
///
/// For a preset the user's URL is ignored — the preset is the point. For
/// [`CUSTOM`] (or an id this version does not know) it is the user's URL,
/// spoken to as OpenAI-compatible.
pub fn endpoint(provider_id: &str, custom_base_url: &str) -> (ApiProtocol, String) {
    match preset(provider_id) {
        Some(p) => (p.protocol, p.base_url.to_string()),
        None => (
            ApiProtocol::OpenAiCompatible,
            custom_base_url.trim().trim_end_matches('/').to_string(),
        ),
    }
}

/// The provider a stored base URL belongs to, for a config written before the
/// provider list existed.
///
/// Matched on the **host**, so a URL with extra path — Anthropic's
/// `/v1/messages`, OpenAI's `/v1/chat/completions` — is still recognised. That
/// is exactly the configuration the old URL field let people get wrong.
pub fn infer_from_base_url(base_url: &str) -> &'static str {
    let host = |url: &str| -> String {
        url.trim()
            .to_ascii_lowercase()
            .split("://")
            .nth(1)
            .unwrap_or_default()
            .split(['/', ':', '?'])
            .next()
            .unwrap_or_default()
            .to_string()
    };

    let wanted = host(base_url);
    if wanted.is_empty() {
        return CUSTOM;
    }
    PRESETS
        .iter()
        .find(|p| host(p.base_url) == wanted)
        .map_or(CUSTOM, |p| p.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_never_the_custom_id() {
        for (i, a) in PRESETS.iter().enumerate() {
            assert_ne!(a.id, CUSTOM);
            for b in &PRESETS[i + 1..] {
                assert_ne!(a.id, b.id);
            }
        }
    }

    #[test]
    fn every_preset_is_https_with_no_trailing_slash() {
        for p in PRESETS {
            assert!(p.base_url.starts_with("https://"), "{}", p.id);
            assert!(!p.base_url.ends_with('/'), "{}", p.id);
        }
    }

    #[test]
    fn a_preset_ignores_the_typed_url() {
        let (protocol, base) = endpoint("anthropic", "https://example.com/whatever");
        assert_eq!(protocol, ApiProtocol::Anthropic);
        assert_eq!(base, "https://api.anthropic.com/v1");
    }

    #[test]
    fn custom_uses_the_typed_url_as_openai_compatible() {
        let (protocol, base) = endpoint(CUSTOM, " http://localhost:1234/v1/ ");
        assert_eq!(protocol, ApiProtocol::OpenAiCompatible);
        assert_eq!(base, "http://localhost:1234/v1");
    }

    #[test]
    fn an_unknown_id_behaves_as_custom() {
        // A config written by a newer version naming a provider this one lacks.
        let (protocol, base) = endpoint("some-future-provider", "https://x.example/v1");
        assert_eq!(protocol, ApiProtocol::OpenAiCompatible);
        assert_eq!(base, "https://x.example/v1");
    }

    #[test]
    fn old_configs_are_recognised_by_host() {
        // The configuration that could never work: the full Messages endpoint.
        assert_eq!(infer_from_base_url("https://api.anthropic.com/v1/messages"), "anthropic");
        assert_eq!(infer_from_base_url("https://API.OPENAI.COM/v1"), "openai");
        assert_eq!(infer_from_base_url("https://openrouter.ai/api/v1/"), "openrouter");
        assert_eq!(infer_from_base_url("http://localhost:1234/v1"), CUSTOM);
        assert_eq!(infer_from_base_url(""), CUSTOM);
    }
}
