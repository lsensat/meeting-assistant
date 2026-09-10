//! Meeting Assistant — the platform-dependent half.
//!
//! Hardware-independent logic (device-selection policy, resampling, progress
//! math, config, i18n, prompts) lives in the `meeting-core` crate and is shared
//! unchanged between Windows and macOS. This crate adds audio capture, and in
//! later phases transcription, the Ollama client and the Tauri shell.

pub mod audio;
pub mod commands;
pub mod diagnostics;
pub mod ollama;
pub mod openai;
pub mod pipeline;
pub mod queue;
pub mod platform;
pub mod session;
pub mod state;
pub mod summary;
pub mod tray;
pub mod whisper;
