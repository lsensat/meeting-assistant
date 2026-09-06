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

use crate::{ollama, openai};

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
    pub api_base_url: String,
    pub api_model: String,
}

impl ProviderConfig {
    pub fn from_config(config: &Config) -> Self {
        Self {
            provider: config.summary_provider,
            ollama_model: config.ollama_model.clone(),
            api_base_url: config.api_base_url.clone(),
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
            Ok(openai::chat(
                &config.api_base_url,
                &key,
                &config.api_model,
                system,
                user,
            )?)
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
