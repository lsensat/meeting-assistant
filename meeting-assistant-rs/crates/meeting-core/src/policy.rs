//! Audio-device selection policy. Port of `audio_policy.py`.
//!
//! No audio backend is referenced here, so the selection logic is testable
//! without real hardware — same rationale as the Python module's docstring.
//!
//! The one structural change from Python: devices are keyed by a string `id`
//! rather than a PortAudio integer index. PortAudio indices renumber on
//! hotplug, which is exactly when this code runs; cpal's `DeviceId` is stable.
//! Tests pass stringified integers so they read like the Python originals.

/// Substrings that mark a built-in microphone. Preferred over arbitrary inputs
/// when the configured and default devices are both unavailable.
///
/// Locale-coupled on purpose: the first group matches Spanish Windows device
/// names, which is what the Windows target machines run. Ported verbatim from
/// `audio_policy.py`.
///
/// The macOS group is appended rather than kept in a separate list. Matching is
/// substring-based and these strings cannot occur in a Windows endpoint name,
/// so one shared list stays correct on both platforms and `ordered_candidates`
/// needs no `cfg`. Names verified against real hardware on macOS 26.6:
/// `MacBook Air Microphone`, `EarPods Microphone`.
pub const INTERNAL_MIC_TERMS: &[&str] = &[
    // Windows
    "microphone array",
    "matriz de micrófonos",
    "matriz de microfonos",
    "onboard",
    "intel smart sound",
    "intel",
    "internal",
    "integrated",
    // macOS
    "macbook air microphone",
    "macbook pro microphone",
    "micrófono del macbook",
    "microfono del macbook",
    "built-in",
];

/// Loopback devices preferred when nothing is configured.
///
/// Hardcodes one specific headset — see deferred fix #8. Kept as-is for parity.
pub const DEFAULT_SYSTEM_PREFERRED_TERMS: &[&str] = &["acme headset 3225 series"];

/// An audio endpoint, either a capture device or a loopback-capable render device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// Stable key. On Windows this is cpal's `DeviceId`.
    pub id: String,
    /// The name Windows reports. Used for display and for config matching.
    pub name: String,
    pub sample_rate: u32,
    pub channels: u16,
}

impl Device {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        sample_rate: u32,
        channels: u16,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            sample_rate,
            channels,
        }
    }
}

/// Deduplicate by id, preserving first-seen order. Port of `_unique_by_index`.
fn unique_by_id(devices: Vec<&Device>) -> Vec<Device> {
    let mut seen: Vec<&str> = Vec::new();
    let mut result = Vec::new();

    for device in devices {
        if seen.contains(&device.id.as_str()) {
            continue;
        }
        seen.push(&device.id);
        result.push(device.clone());
    }

    result
}

/// Port of `_find_by_name`. An empty name never matches, mirroring Python's
/// `if not name: return None` — otherwise a device with an empty name would.
fn find_by_name<'a>(devices: &'a [Device], name: &str) -> Option<&'a Device> {
    if name.is_empty() {
        return None;
    }
    devices.iter().find(|d| d.name == name)
}

/// Port of `_find_by_index`.
fn find_by_id<'a>(devices: &'a [Device], id: Option<&str>) -> Option<&'a Device> {
    let id = id?;
    devices.iter().find(|d| d.id == id)
}

/// Build the ordered preference list, shared by both the mic and system paths.
/// They differ only in which substrings get promoted, so Python's two
/// near-identical functions collapse into one here.
fn ordered_candidates(
    devices: &[Device],
    selected_name: &str,
    default_id: Option<&str>,
    preferred_terms: &[&str],
) -> Vec<Device> {
    let mut ordered: Vec<&Device> = Vec::new();

    if let Some(d) = find_by_name(devices, selected_name) {
        ordered.push(d);
    }
    if let Some(d) = find_by_id(devices, default_id) {
        ordered.push(d);
    }

    for term in preferred_terms {
        let term = term.to_lowercase();
        for device in devices {
            if device.name.to_lowercase().contains(&term) {
                ordered.push(device);
            }
        }
    }

    ordered.extend(devices.iter());
    unique_by_id(ordered)
}

/// Microphone preference order: configured device, then OS default, then any
/// built-in mic, then everything else. Port of `ordered_microphone_candidates`.
pub fn ordered_microphone_candidates(
    devices: &[Device],
    selected_name: &str,
    default_id: Option<&str>,
) -> Vec<Device> {
    ordered_candidates(devices, selected_name, default_id, INTERNAL_MIC_TERMS)
}

/// Loopback preference order. Port of `ordered_system_candidates`.
pub fn ordered_system_candidates(
    devices: &[Device],
    selected_name: &str,
    default_id: Option<&str>,
) -> Vec<Device> {
    ordered_candidates(
        devices,
        selected_name,
        default_id,
        DEFAULT_SYSTEM_PREFERRED_TERMS,
    )
}

/// Outcome of a failover decision.
///
/// `changed` drives the UI's "(automatic)" suffix: it means the recorder is on
/// a device the user did not pick. Note the Python quirk this preserves — when
/// no device at all is available, `changed` is `true` only if we *had* a device
/// before, so a meeting that never acquired one does not claim it fell back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failover {
    pub device: Option<Device>,
    pub changed: bool,
}

fn choose_failover(
    devices: &[Device],
    current_name: &str,
    configured_name: &str,
    default_id: Option<&str>,
    preferred_terms: &[&str],
) -> Failover {
    // The device we are already on wins if it is still present. This is what
    // makes the policy stable: after a fallback we do not switch back the
    // moment the original device reappears mid-meeting.
    if let Some(current) = find_by_name(devices, current_name) {
        return Failover {
            device: Some(current.clone()),
            changed: false,
        };
    }

    let candidates = ordered_candidates(devices, configured_name, default_id, preferred_terms);

    match candidates.into_iter().next() {
        Some(device) => Failover {
            device: Some(device),
            changed: true,
        },
        None => Failover {
            device: None,
            changed: !current_name.is_empty(),
        },
    }
}

/// Pick a microphone during recording. Port of `choose_microphone_failover`.
pub fn choose_microphone_failover(
    devices: &[Device],
    current_name: &str,
    configured_name: &str,
    default_id: Option<&str>,
) -> Failover {
    choose_failover(
        devices,
        current_name,
        configured_name,
        default_id,
        INTERNAL_MIC_TERMS,
    )
}

/// Pick a loopback device during recording. Port of `choose_system_failover`.
pub fn choose_system_failover(
    devices: &[Device],
    current_name: &str,
    configured_name: &str,
    default_id: Option<&str>,
) -> Failover {
    choose_failover(
        devices,
        current_name,
        configured_name,
        default_id,
        DEFAULT_SYSTEM_PREFERRED_TERMS,
    )
}

/// Whether the device now in use counts as an automatic fallback.
///
/// # Why this is not "did the policy have to choose"
///
/// The obvious test — [`Failover::changed`] — is wrong, and was shipped.
/// `changed` is true whenever the device we were on could not be found in the
/// enumeration, **including when it reappears a moment later and we reopen the
/// very same one**. A device that blips reacquires itself, and reporting that as
/// a fallback told the user "Not the device you chose" while showing the device
/// they had chosen.
///
/// It compounds: the recorder's flag is sticky, so a single blip labels the
/// whole meeting, and the data watchdog produces a stream of them.
///
/// The honest question is where we *landed*:
///
/// * the user named a device — are we on it?
/// * the user named nothing — have we drifted off the one we started on?
///
/// `previous_name` is empty on the first acquisition, which is why a first open
/// with no configured device is never a fallback.
pub fn is_automatic_fallback(configured_name: &str, previous_name: &str, chosen_name: &str) -> bool {
    if configured_name.is_empty() {
        !previous_name.is_empty() && chosen_name != previous_name
    } else {
        chosen_name != configured_name
    }
}

/// How many samples of silence cover a gap of `gap_seconds`.
///
/// This is what keeps the two tracks time-aligned across a device disconnect.
/// Port of `silence_frames_for_gap`; negative gaps clamp to zero.
pub fn silence_frames_for_gap(gap_seconds: f64, sample_rate: u32) -> usize {
    let gap_seconds = gap_seconds.max(0.0);
    let sample_rate = sample_rate.max(1) as f64;

    (gap_seconds * sample_rate).round_ties_even() as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mirrors the `dev()` helper in tests/test_audio_policy.py.
    fn dev(index: u32, name: &str) -> Device {
        Device::new(index.to_string(), name, 48_000, 2)
    }

    /// The bug: a device that vanishes and comes back is not a fallback.
    #[test]
    fn reacquiring_the_same_device_is_not_a_fallback() {
        // The watchdog fires, the device is briefly absent from the
        // enumeration, and we reopen the very same one. `Failover::changed` is
        // true here, which is exactly what the old code trusted — so the user
        // was told "Not the device you chose" while looking at the device they
        // had chosen.
        assert!(
            !is_automatic_fallback("Headset", "Headset", "Headset"),
            "landing back on the configured device is not falling back"
        );
        assert!(
            !is_automatic_fallback("", "Headset", "Headset"),
            "with nothing configured, staying put is not falling back either"
        );
    }

    #[test]
    fn landing_somewhere_else_is_a_fallback() {
        assert!(is_automatic_fallback("Headset", "Headset", "Built-in"));
        // A configured device we could not get on the first open.
        assert!(is_automatic_fallback("Headset", "", "Built-in"));
        // Nothing configured, but we drifted off what we started on.
        assert!(is_automatic_fallback("", "Headset", "Built-in"));
    }

    #[test]
    fn the_first_open_with_no_configured_device_is_never_a_fallback() {
        // An empty `previous_name` means nothing has been opened yet.
        // Reporting a fallback here put "(automatic)" in the UI permanently.
        assert!(!is_automatic_fallback("", "", "Built-in"));
    }

    #[test]
    fn selected_microphone_wins() {
        let devices = vec![
            dev(1, "Microphone Array (Intel)"),
            dev(2, "Acme Headset 3225 Series"),
        ];
        let result =
            ordered_microphone_candidates(&devices, "Acme Headset 3225 Series", Some("1"));
        assert_eq!(result[0].id, "2");
    }

    #[test]
    fn default_microphone_fallback() {
        let devices = vec![dev(1, "Microphone Array (Intel)"), dev(3, "USB Microphone")];
        let result = ordered_microphone_candidates(&devices, "Missing", Some("1"));
        assert_eq!(result[0].id, "1");
    }

    #[test]
    fn internal_microphone_precedes_arbitrary_input() {
        let devices = vec![
            dev(3, "USB Microphone"),
            dev(4, "Microphone Array (Intel Smart Sound)"),
        ];
        let result = ordered_microphone_candidates(&devices, "Missing", None);
        assert_eq!(result[0].id, "4");
    }

    /// Same rule as `internal_microphone_precedes_arbitrary_input`, but with
    /// the real device names macOS reports. Without the macOS entries in
    /// `INTERNAL_MIC_TERMS` the built-in mic is indistinguishable from any
    /// other input and the failover picks whatever enumerated first.
    #[test]
    fn macos_builtin_microphone_precedes_arbitrary_input() {
        let devices = vec![
            dev(1, "Luis’s iPhone Microphone"),
            dev(2, "MacBook Air Microphone"),
        ];
        let result = ordered_microphone_candidates(&devices, "Missing", None);
        assert_eq!(result[0].id, "2");
    }

    /// A USB headset must not be mistaken for the built-in microphone: it is a
    /// legitimate choice, but only after the internal one when neither is
    /// configured.
    #[test]
    fn macos_headset_microphone_is_not_treated_as_internal() {
        let devices = vec![
            dev(1, "EarPods Microphone"),
            dev(2, "MacBook Air Microphone"),
        ];
        let result = ordered_microphone_candidates(&devices, "", None);
        assert_eq!(result[0].id, "2");
    }

    #[test]
    fn microphone_candidates_are_unique() {
        let devices = vec![
            dev(1, "Microphone Array (Intel)"),
            dev(2, "Acme Headset 3225 Series"),
        ];
        let result =
            ordered_microphone_candidates(&devices, "Acme Headset 3225 Series", Some("2"));
        assert_eq!(result.iter().filter(|d| d.id == "2").count(), 1);
    }

    #[test]
    fn selected_loopback_wins() {
        let devices = vec![
            dev(19, "Acme Headset 3225 Series [Loopback]"),
            dev(20, "Speakers (Onboard) [Loopback]"),
        ];
        let result = ordered_system_candidates(&devices, "Speakers (Onboard) [Loopback]", None);
        assert_eq!(result[0].id, "20");
    }

    #[test]
    fn acme_is_default_preference_without_saved_loopback() {
        let devices = vec![
            dev(20, "Speakers (Onboard) [Loopback]"),
            dev(19, "Acme Headset 3225 Series [Loopback]"),
        ];
        let result = ordered_system_candidates(&devices, "", None);
        assert_eq!(result[0].id, "19");
    }

    #[test]
    fn supplied_default_loopback_can_win() {
        let devices = vec![
            dev(19, "Acme Headset 3225 Series [Loopback]"),
            dev(20, "Speakers (Onboard) [Loopback]"),
        ];
        let result = ordered_system_candidates(&devices, "Missing", Some("20"));
        assert_eq!(result[0].id, "20");
    }

    #[test]
    fn runtime_mic_keeps_current_fallback_after_headset_returns() {
        let devices = vec![
            dev(1, "Microphone Array (Intel)"),
            dev(2, "Acme Headset 3225 Series"),
        ];
        let result = choose_microphone_failover(
            &devices,
            "Microphone Array (Intel)",
            "Acme Headset 3225 Series",
            Some("1"),
        );
        assert_eq!(result.device.unwrap().id, "1");
        assert!(!result.changed);
    }

    #[test]
    fn runtime_mic_switches_when_current_disappears() {
        let devices = vec![dev(1, "Microphone Array (Intel)"), dev(3, "USB Microphone")];
        let result = choose_microphone_failover(
            &devices,
            "Acme Headset 3225 Series",
            "Acme Headset 3225 Series",
            Some("1"),
        );
        assert_eq!(result.device.unwrap().id, "1");
        assert!(result.changed);
    }

    #[test]
    fn runtime_system_switches_to_onboard() {
        let devices = vec![dev(20, "Speakers (Onboard) [Loopback]")];
        let result = choose_system_failover(
            &devices,
            "Acme Headset 3225 Series [Loopback]",
            "Acme Headset 3225 Series [Loopback]",
            Some("20"),
        );
        assert_eq!(result.device.unwrap().id, "20");
        assert!(result.changed);
    }

    #[test]
    fn runtime_failover_handles_no_device() {
        let result = choose_system_failover(
            &[],
            "Acme Headset 3225 Series [Loopback]",
            "",
            None,
        );
        assert!(result.device.is_none());
        assert!(result.changed);
    }

    #[test]
    fn gap_to_silence_frames() {
        assert_eq!(silence_frames_for_gap(0.7, 48_000), 33_600);
        assert_eq!(silence_frames_for_gap(-1.0, 48_000), 0);
    }
}

// --- was the recording actually captured? -------------------------------

/// How a recorded track can be damaged.
///
/// # Why this exists at all
///
/// A track that never opened is **not** represented here: there is no
/// `TrackSummary` for it at all, so it is handled by the caller, which sees the
/// `Err` arm directly.
///
/// A meeting was recorded while another was being processed. The audio came out
/// wrong, the transcript was empty and the summary useless — and the app said
/// nothing, because the counters that already knew went to a log line the
/// frontend forwarded to `console.log`, which a release build cannot open. The
/// user found out from an empty summary.
///
/// So damage is assessed the moment capture stops, from what the recorder
/// already measured, and is reported before any model runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackDamage {
    /// Realtime callbacks were dropped because the writer could not keep up.
    /// Audio is missing and cannot be recovered.
    Dropped { callbacks: u64 },
    /// The device went away and silence was written to keep the timeline
    /// aligned. The file is full length; that length is partly not a recording.
    Silence { seconds: u64 },
    /// The file is materially shorter than the recording. A backstop for loss
    /// that neither counter above saw.
    Short { missing_seconds: u64 },
}

/// One track's measurements, as the recorder reports them.
#[derive(Debug, Clone, Copy)]
pub struct TrackFacts {
    /// Seconds of audio in the file.
    pub duration_seconds: f64,
    /// Realtime callbacks dropped.
    pub overflows: u64,
    /// Samples of silence inserted to cover an outage.
    pub gap_frames: u64,
    pub sample_rate: u32,
}

/// Systematic shortfall from resampling, as a fraction.
///
/// `convert::resample_mono` rounds to nearest once per 1024-sample chunk
/// (`round_ties_even`). Going from 44.1 kHz to 48 kHz it wants 1114.2857
/// samples and writes 1114 — **0.026%**, about 0.92 s per hour. The direction
/// depends on the rate pair: it happens to lose here, and another pair could
/// gain, which is why the comparison below is signed and only a shortfall
/// counts. 48 kHz and 16 kHz return early and are exact.
///
/// The test derives this fraction from the resampler rather than from this
/// comment, so the two cannot drift apart.
///
/// The tolerance has to clear this or every long recording from a 44.1 kHz
/// device is reported as damaged. `0.2%` leaves roughly 8x headroom.
const LENGTH_TOLERANCE: f64 = 0.002;

/// Absolute floor on the tolerance, for recordings too short for a percentage
/// to mean anything.
///
/// `elapsed_seconds` is sampled the instant the stop flag is set, while recorder
/// threads may still be inside a 250 ms `recv_timeout`, and the closing gap
/// silence is measured from a later instant. A few hundred milliseconds either
/// way is normal.
const LENGTH_FLOOR_SECONDS: f64 = 0.5;

/// How much outage silence is worth telling the user about.
///
/// `gap_frames` counts only genuine outages — the opening device-open silence
/// is `lead_in_frames` and never reaches here. Even so, a device switch
/// mid-meeting (headphones going in) is handled gracefully and costs a fraction
/// of a second, which is not the kind of thing to interrupt someone about.
///
/// A second is the point where a listener would notice a hole.
const GAP_REPORT_SECONDS: f64 = 1.0;

/// Assess one track against how long the recording actually ran.
///
/// `elapsed_seconds` is wall-clock; `facts` is what reached the file.
///
/// # Order matters
///
/// `overflows` and `gap_frames` are **exact**, and each names a specific
/// mechanism. The length comparison is a backstop and is reported only when
/// neither counter fired, because it cannot distinguish causes and, for gaps,
/// cannot see the problem at all: `TrackWriter::write_silence` advances the
/// frame count, so a track that lost its device for twenty seconds is *full
/// length*. Reporting the length first would describe the symptom while the
/// exact cause sat unused.
pub fn assess_track(elapsed_seconds: f64, facts: TrackFacts) -> Option<TrackDamage> {
    if facts.overflows > 0 {
        return Some(TrackDamage::Dropped {
            callbacks: facts.overflows,
        });
    }

    let rate = if facts.sample_rate == 0 {
        1
    } else {
        facts.sample_rate
    };
    let gap_seconds = facts.gap_frames as f64 / rate as f64;
    if gap_seconds >= GAP_REPORT_SECONDS {
        return Some(TrackDamage::Silence {
            seconds: gap_seconds.round().max(1.0) as u64,
        });
    }

    // Only a shortfall counts. A track can legitimately run slightly *longer*
    // than the measured elapsed time — see LENGTH_FLOOR_SECONDS.
    let missing = elapsed_seconds - facts.duration_seconds;
    let tolerance = LENGTH_FLOOR_SECONDS.max(elapsed_seconds * LENGTH_TOLERANCE);
    if missing > tolerance {
        return Some(TrackDamage::Short {
            missing_seconds: missing.round().max(1.0) as u64,
        });
    }

    None
}

#[cfg(test)]
mod damage_tests {
    use super::*;

    /// Note `gap_frames` here means **outage** silence. The opening device-open
    /// silence is counted separately by the recorder, precisely so a healthy
    /// recording reaches this function with `gap_frames == 0`.
    fn facts(duration: f64, overflows: u64, gap_frames: u64) -> TrackFacts {
        TrackFacts {
            duration_seconds: duration,
            overflows,
            gap_frames,
            sample_rate: 48_000,
        }
    }

    #[test]
    fn a_healthy_recording_is_not_reported() {
        assert_eq!(assess_track(600.0, facts(600.0, 0, 0)), None);
        // Slightly long is normal: elapsed is sampled before the threads finish.
        assert_eq!(assess_track(600.0, facts(600.3, 0, 0)), None);
    }

    /// The bug this whole feature exists for.
    #[test]
    fn dropped_callbacks_are_reported_exactly() {
        assert_eq!(
            assess_track(600.0, facts(480.0, 37, 0)),
            Some(TrackDamage::Dropped { callbacks: 37 }),
            "overflow is the exact signal and must win over the length"
        );
    }

    /// The case the length check cannot see: silence is written to keep the
    /// tracks aligned, so the file is full length while the audio is missing.
    /// A device blip is not worth a warning, and warning about all of them
    /// would train the user to ignore the one that matters.
    #[test]
    fn a_sub_second_outage_is_not_reported() {
        let blip = facts(600.0, 0, (0.4 * 48_000.0) as u64);
        assert_eq!(assess_track(600.0, blip), None);
    }

    #[test]
    fn inserted_silence_is_reported_even_though_the_file_is_full_length() {
        let full_length = facts(600.0, 0, 20 * 48_000);
        assert_eq!(
            assess_track(600.0, full_length),
            Some(TrackDamage::Silence { seconds: 20 }),
            "a device outage pads the file, so only gap_frames can see it"
        );
    }

    #[test]
    fn a_short_file_is_the_backstop_when_no_counter_fired() {
        assert_eq!(
            assess_track(600.0, facts(500.0, 0, 0)),
            Some(TrackDamage::Short {
                missing_seconds: 100
            })
        );
    }

    /// Without this the tolerance would flag every long recording made on a
    /// 44.1 kHz device as damaged.
    /// The shortfall is taken from the resampler, not from a constant.
    ///
    /// An earlier version hardcoded `1.0 - 0.00026` — the number written in the
    /// comment above `LENGTH_TOLERANCE`. That asserted the tolerance clears a
    /// figure someone typed, not the figure the code produces, and would have
    /// gone on passing if the rounding changed.
    #[test]
    fn resampling_from_44_1khz_does_not_look_like_damage() {
        let chunk = vec![0.0f32; 1024];
        let out = crate::convert::resample_mono(&chunk, 44_100, 48_000).len() as f64;
        let want = 1024.0 * 48_000.0 / 44_100.0;
        // Whatever the rounding does, this is the fraction actually lost.
        let shortfall = (want - out) / want;
        assert!(
            shortfall < LENGTH_TOLERANCE,
            "the resampler loses {shortfall:.5} per chunk, more than the {LENGTH_TOLERANCE} \
             tolerance — every long recording from a 44.1kHz device would be called damaged"
        );

        let hour = 3600.0;
        let resampled = hour * (1.0 - shortfall);
        assert_eq!(
            assess_track(hour, facts(resampled, 0, 0)),
            None,
            "0.92s lost per hour is arithmetic, not damage"
        );

        // Ten hours: still arithmetic, and well inside a proportional tolerance.
        let ten = 36_000.0;
        assert_eq!(assess_track(ten, facts(ten * (1.0 - shortfall), 0, 0)), None);
    }

    #[test]
    fn a_short_recording_uses_the_floor_not_the_percentage() {
        // 0.2% of 5s is 10ms, which every recording would breach.
        assert_eq!(assess_track(5.0, facts(4.7, 0, 0)), None);
        assert_eq!(
            assess_track(5.0, facts(3.0, 0, 0)),
            Some(TrackDamage::Short { missing_seconds: 2 })
        );
    }

    #[test]
    fn a_zero_sample_rate_does_not_divide_by_zero() {
        let broken = TrackFacts {
            duration_seconds: 10.0,
            overflows: 0,
            gap_frames: 480,
            sample_rate: 0,
        };
        assert!(matches!(
            assess_track(10.0, broken),
            Some(TrackDamage::Silence { .. })
        ));
    }
}

/// How long to wait before reopening a capture device that just failed.
///
/// # Why a device that fails needs to be left alone for a moment
///
/// Reopening on failure with no delay is a storm. Measured on a real machine: a
/// USB headset opened for capture while the *same* headset was open for
/// loopback reported "a buffer underrun or overrun occurred" **35 times in 4.4
/// seconds** — an open every 120 ms, each costing 60-70 ms of device work, on
/// the machine that was supposed to be recording a meeting.
///
/// Nothing was gained by trying that often. The silence written to cover an
/// outage is measured from wall-clock time, so a track is no shorter for having
/// been retried less.
///
/// Doubling from 250 ms, capped at 4 s: quick enough that a device which
/// recovers is picked up almost immediately, slow enough that one which cannot
/// is checked fifteen times a minute rather than five hundred.
pub fn reopen_backoff_ms(consecutive_failures: u32) -> u64 {
    const BASE_MS: u64 = 250;
    const CAP_MS: u64 = 4_000;

    if consecutive_failures <= 1 {
        return BASE_MS;
    }
    // `saturating_sub` keeps the shift in range; 5 doublings reaches the cap.
    let shift = (consecutive_failures - 1).min(5);
    (BASE_MS << shift).min(CAP_MS)
}

#[cfg(test)]
mod backoff_tests {
    use super::*;

    #[test]
    fn the_first_failure_retries_promptly() {
        // A one-off blip should not cost the user a pause.
        assert_eq!(reopen_backoff_ms(0), 250);
        assert_eq!(reopen_backoff_ms(1), 250);
    }

    #[test]
    fn repeated_failures_back_off_and_then_stop_growing() {
        assert_eq!(reopen_backoff_ms(2), 500);
        assert_eq!(reopen_backoff_ms(3), 1_000);
        assert_eq!(reopen_backoff_ms(4), 2_000);
        assert_eq!(reopen_backoff_ms(5), 4_000);
        assert_eq!(reopen_backoff_ms(6), 4_000, "capped");
        assert_eq!(reopen_backoff_ms(u32::MAX), 4_000, "still capped");
    }

    /// The storm this exists to prevent, in numbers.
    #[test]
    fn a_failing_device_is_retried_far_less_than_before() {
        // The observed failure: 35 opens in 4.4s, i.e. roughly every 125ms.
        let mut elapsed = 0u64;
        let mut attempts = 0u32;
        while elapsed < 4_400 {
            attempts += 1;
            elapsed += reopen_backoff_ms(attempts);
        }
        assert!(
            attempts <= 6,
            "still storming: {attempts} opens in 4.4s, was 35"
        );
    }
}
