//! Hardware-independent logic for Meeting Assistant.
//!
//! Everything here is pure: device-selection policy, audio conversion, progress
//! arithmetic, prompts and text handling. No audio backend, no filesystem, no
//! Tauri — so all of it is testable without hardware, and the crate builds and
//! its tests run on any platform.
//!
//! # Rounding
//!
//! Rounding sites use [`f64::round_ties_even`] — round-half-to-even — rather
//! than `f64::round`, which rounds half away from zero. Exact halves are common
//! here because durations and percentages divide evenly far more often than
//! arbitrary measurements do, and at least one test depends on the choice.

pub mod config;
pub mod convert;
pub mod devices;
pub mod i18n;
pub mod policy;
pub mod progress;
pub mod prompts;
pub mod text;
