//! The seam between the pipeline and whichever model generates the summary.
//!
//! `pipeline.rs` calls [`chat`] twice — once per transcript chunk, once for the
//! final synthesis — and does not care which provider answers. Adding a
//! provider means adding a variant here, not touching the pipeline.
//!
//! # No silent fallback
//!
//! If the configured provider fails, this returns the error. It never retries
//! against the other one. Falling back from a failed local Ollama to a remote
//! API would send the transcript off the machine because of a transient local
//! failure, which is precisely the thing the user did not agree to.

use meeting_core::config::{Config, SummaryProvider, KEYCHAIN_ACCOUNT, KEYCHAIN_SERVICE};
use meeting_core::providers::ApiProtocol;

use crate::{anthropic, ollama, openai};

#[derive(Debug)]
pub enum SummaryError {
    Ollama(ollama::OllamaError),
    OpenAi(openai::OpenAiError),
    /// The OS keychain could not be read. Distinct from "no key stored":
    /// this means the store itself is unavailable.
    Keychain(String),
}

impl std::fmt::Display for SummaryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ollama(e) => write!(f, "{e}"),
            Self::OpenAi(e) => write!(f, "{e}"),
            Self::Keychain(e) => write!(
                f,
                "Could not read the API key from the system keychain: {e}"
            ),
        }
    }
}

impl std::error::Error for SummaryError {}

impl From<ollama::OllamaError> for SummaryError {
    fn from(e: ollama::OllamaError) -> Self {
        Self::Ollama(e)
    }
}
impl From<openai::OpenAiError> for SummaryError {
    fn from(e: openai::OpenAiError) -> Self {
        Self::OpenAi(e)
    }
}

/// Everything the summary step needs, resolved from `Config` at the point the
/// pipeline starts, so a settings change mid-run cannot switch providers
/// underneath it.
#[derive(Debug, Clone)]
pub struct ProviderConfig {
    pub provider: SummaryProvider,
    pub ollama_model: String,
    /// How to talk to the remote provider, resolved from its id.
    pub api_protocol: ApiProtocol,
    /// The provider's base URL — the preset's own, or the user's for "Other".
    pub api_base_url: String,
    pub api_model: String,
}

impl ProviderConfig {
    pub fn from_config(config: &Config) -> Self {
        let (api_protocol, api_base_url) = config.api_endpoint();
        Self {
            provider: config.summary_provider,
            ollama_model: config.ollama_model.clone(),
            api_protocol,
            api_base_url,
            api_model: config.api_model.clone(),
        }
    }

    /// True when this configuration sends the transcript off the machine.
    /// Used by the UI to show the warning at the point of choosing.
    pub fn is_remote(&self) -> bool {
        matches!(self.provider, SummaryProvider::OpenAiCompatible)
    }
}

pub fn chat(config: &ProviderConfig, system: &str, user: &str) -> Result<String, SummaryError> {
    match config.provider {
        SummaryProvider::Ollama => Ok(ollama::chat(&config.ollama_model, system, user)?),
        SummaryProvider::OpenAiCompatible => {
            let key = read_api_key()?;
            let chat = match config.api_protocol {
                ApiProtocol::Anthropic => anthropic::chat,
                ApiProtocol::OpenAiCompatible => openai::chat,
            };
            Ok(chat(&config.api_base_url, &key, &config.api_model, system, user)?)
        }
    }
}

/// Read the API key from the OS keychain.
///
/// A missing entry is not an error here — it comes back as an empty string and
/// `openai::chat` turns it into `MissingKey`, which carries a message the user
/// can act on. Only a broken keychain is an error.
fn read_api_key() -> Result<String, SummaryError> {
    let entry = keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT)
        .map_err(|e| SummaryError::Keychain(e.to_string()))?;

    match entry.get_password() {
        Ok(key) => Ok(key),
        Err(keyring::Error::NoEntry) => Ok(String::new()),
        Err(e) => Err(SummaryError::Keychain(e.to_string())),
    }
}

/// Store the API key. Called from the settings and setup commands.
pub fn store_api_key(key: &str) -> Result<(), SummaryError> {
    let entry = keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT)
        .map_err(|e| SummaryError::Keychain(e.to_string()))?;

    if key.trim().is_empty() {
        // Clearing is a legitimate action; a missing entry is not a failure.
        return match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(SummaryError::Keychain(e.to_string())),
        };
    }

    entry
        .set_password(key)
        .map_err(|e| SummaryError::Keychain(e.to_string()))
}

/// Whether a key is stored, without revealing it. The UI shows "key saved"
/// rather than the value.
pub fn has_api_key() -> bool {
    read_api_key().map(|k| !k.is_empty()).unwrap_or(false)
}

/// The models the chosen provider offers, which doubles as a test of the key.
///
/// `typed_key` is what the user has typed but not saved yet; empty means use
/// the stored one. The URL is resolved exactly as a summary would resolve it,
/// so a list that loads here is a configuration that will summarise.
pub fn list_models(
    provider_id: &str,
    custom_base_url: &str,
    typed_key: &str,
) -> Result<Vec<String>, SummaryError> {
    let key = if typed_key.trim().is_empty() {
        read_api_key()?
    } else {
        typed_key.trim().to_string()
    };
    let (protocol, base_url) = meeting_core::providers::endpoint(provider_id, custom_base_url);
    Ok(match protocol {
        ApiProtocol::Anthropic => anthropic::list_models(&base_url, &key)?,
        ApiProtocol::OpenAiCompatible => openai::list_models(&base_url, &key)?,
    })
}

/// The last four characters of the stored key, so Settings can show which key
/// is saved ("••••••••a1B2") without the key ever reaching the window.
///
/// `None` when no key is stored. Keys too short to spare four characters get
/// no hint at all rather than most of themselves.
pub fn api_key_hint() -> Option<String> {
    let key = read_api_key().ok()?;
    key_hint(&key)
}

fn key_hint(key: &str) -> Option<String> {
    let chars: Vec<char> = key.trim().chars().collect();
    match chars.len() {
        0 => None,
        n if n < 16 => Some(String::new()),
        n => Some(chars[n - 4..].iter().collect()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn config_with(provider: SummaryProvider) -> Config {
        Config {
            summary_provider: provider,
            ..Config::defaults(Path::new("/tmp"))
        }
    }

    #[test]
    fn the_key_hint_is_the_last_four_characters_only() {
        assert_eq!(key_hint(""), None);
        assert_eq!(key_hint("sk-ant-api03-abcdefghijklmnop-WXYZ").as_deref(), Some("WXYZ"));
        // Too short to show any of it safely: saved, but no characters.
        assert_eq!(key_hint("short-key").as_deref(), Some(""));
    }

    #[test]
    fn an_anthropic_config_speaks_the_messages_api() {
        let mut config = config_with(SummaryProvider::OpenAiCompatible);
        config.api_provider = "anthropic".into();
        let provider = ProviderConfig::from_config(&config);
        assert_eq!(provider.api_protocol, ApiProtocol::Anthropic);
        assert_eq!(provider.api_base_url, "https://api.anthropic.com/v1");
    }

    #[test]
    fn a_default_config_is_local() {
        let provider = ProviderConfig::from_config(&config_with(SummaryProvider::Ollama));
        assert!(!provider.is_remote(), "the default must never be remote");
    }

    #[test]
    fn the_remote_provider_reports_itself_as_remote() {
        let provider = ProviderConfig::from_config(&config_with(SummaryProvider::OpenAiCompatible));
        assert!(provider.is_remote());
    }
}
