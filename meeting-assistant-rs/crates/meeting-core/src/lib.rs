//! Hardware-independent logic for Meeting Assistant.
//!
//! Port of the Python modules `audio_policy.py`, `audio_stream_utils.py` and
//! `progress_utils.py`. These are the parts that were already pure and unit
//! tested on the Python side, so they port first and give the rest of the
//! migration a green test suite to build against.
//!
//! # Parity note: rounding
//!
//! Python's built-in `round()` is round-half-to-even ("banker's rounding"), so
//! `round(62.5) == 62`, not 63. Rust's `f64::round` rounds half away from zero
//! and would give 63. Every rounding site ported from Python therefore uses
//! [`f64::round_ties_even`], which matches Python exactly. At least one existing
//! test (`test_progress_is_weighted_when_track_lengths_differ`) depends on this.

pub mod config;
pub mod convert;
pub mod devices;
pub mod i18n;
pub mod policy;
pub mod progress;
pub mod prompts;
pub mod text;
