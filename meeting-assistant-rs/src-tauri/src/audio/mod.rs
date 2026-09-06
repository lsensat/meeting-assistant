//! Audio capture: enumeration, the recorder threads and the WAV writers.
//!
//! Everything platform-specific is confined to [`devices`], and Phase M0
//! measured that even that needs no `cfg` split — see the module docs there.

pub mod devices;
pub mod recorder;
pub mod wav;
