//! Audio format conversion. Port of `audio_stream_utils.py`.
//!
//! Keeping these outside the recorder threads lets the same conversion logic
//! the real capture path uses be verified without hardware.

pub const TARGET_SAMPLE_RATE: u32 = 48_000;

/// Downmix interleaved samples to mono by averaging channels.
/// Port of `to_mono_float`.
pub fn to_mono_float(samples: &[f32], channels: u16) -> Vec<f32> {
    let channels = channels.max(1) as usize;

    if channels == 1 {
        return samples.to_vec();
    }

    // Drop a trailing partial frame, as the Python does via its `usable` slice.
    samples
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

/// Decode interleaved little-endian PCM16 bytes to mono f32 in [-1, 1).
/// Port of `pcm16_bytes_to_mono_float`.
///
/// Note the divisor is 32768, while [`f32_to_i16`] multiplies by 32767. That
/// asymmetry exists in the Python today and is deliberate here — see the note
/// on [`f32_to_i16`].
pub fn pcm16_bytes_to_mono_float(data: &[u8], channels: u16) -> Vec<f32> {
    let samples: Vec<f32> = data
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32)
        .collect();

    to_mono_float(&samples, channels)
        .iter()
        .map(|s| s / 32768.0)
        .collect()
}

/// Encode one f32 sample as PCM16, matching libsndfile's float -> PCM_16 path.
///
/// libsndfile scales by **32767** and rounds to nearest-even (it uses `lrintf`,
/// which follows the default IEEE rounding mode). Do not "fix" the mismatch
/// with [`pcm16_bytes_to_mono_float`]'s 32768 divisor into a matched pair — the
/// asymmetry is what the existing WAV files were written with, and matching
/// them up would shift every sample by one LSB against the reference files used
/// for byte-diff parity testing.
///
/// The 32767 factor should still be confirmed empirically against a real
/// `soundfile`-written WAV before any byte comparison is trusted.
pub fn f32_to_i16(sample: f32) -> i16 {
    let scaled = (sample as f64 * 32767.0).round_ties_even();
    scaled.clamp(i16::MIN as f64, i16::MAX as f64) as i16
}

/// Linearly resample mono audio. Port of `resample_mono`.
///
/// # Parity contract — do not "improve" this
///
/// Two properties are load-bearing and are relied on by the rest of the port:
///
/// 1. **No-op when the rates match.** On the common Windows setup (48 kHz mic,
///    48 kHz loopback) no resampling happens at all, which is why a higher
///    quality resampler such as `rubato` would buy nothing on this path.
/// 2. **Each call is resampled independently**, mapping the input's first and
///    last samples onto the output's first and last. Output therefore depends
///    on how the stream was chunked, not only on the samples. This introduces a
///    ~0.1% intra-chunk time stretch and a small discontinuity at every chunk
///    boundary. Sample *counts* still come out right, so there is no cumulative
///    length drift — and Whisper has evidently tolerated the artifact all along.
///
/// Because of (2), chunk size is part of the output contract. The recorder
/// re-packetises into 1024-sample units before calling this so that output can
/// be diffed against the Python app's WAVs.
pub fn resample_mono(samples: &[f32], source_rate: u32, target_rate: u32) -> Vec<f32> {
    if samples.is_empty() {
        return Vec::new();
    }

    if source_rate == target_rate {
        return samples.to_vec();
    }

    let n = samples.len();
    let target_length =
        ((n as f64 * target_rate as f64 / source_rate as f64).round_ties_even() as usize).max(1);

    // A single input sample has nothing to interpolate between; NumPy's
    // linspace(0, 0, m) yields all zeros, so every output takes samples[0].
    if n == 1 {
        return vec![samples[0]; target_length];
    }

    // np.linspace(0, n - 1, target_length) evaluated against np.interp over
    // integer source positions 0..n-1.
    let last = (n - 1) as f64;
    let step = if target_length > 1 {
        last / (target_length - 1) as f64
    } else {
        0.0
    };

    (0..target_length)
        .map(|i| {
            let pos = (i as f64 * step).min(last);
            let left = pos.floor() as usize;

            if left >= n - 1 {
                return samples[n - 1];
            }

            let frac = (pos - left as f64) as f32;
            samples[left] + frac * (samples[left + 1] - samples[left])
        })
        .collect()
}

/// The only sample rate whisper.cpp accepts.
pub const WHISPER_SAMPLE_RATE: u32 = 16_000;

/// Resample audio for whisper.cpp, low-passing first so nothing aliases.
///
/// # This function has no Python counterpart, and that is the point
///
/// faster-whisper took a *file path* and resampled internally. whisper.cpp does
/// not: `full()` assumes the slice it is given is already 16 kHz mono. Hand it
/// 48 kHz and there is no crash and no error — you get a fluent, confidently
/// wrong transcript with plausible timestamps. That is Windows risk R2, and it
/// is the most likely silent-wrong-output bug in the whole port.
///
/// # Why the low-pass is not optional
///
/// Going from 48 kHz to 16 kHz is 3:1 decimation. Anything above the new
/// Nyquist of 8 kHz folds back down into the audible band — a 12 kHz sibilant
/// or a 10 kHz hiss lands at 4 kHz and 6 kHz respectively, right on top of
/// speech. Just taking every third sample is therefore not "simpler", it is
/// wrong, and it degrades transcription accuracy in a way that looks like the
/// model being bad rather than the audio being broken.
///
/// A windowed-sinc FIR removes those components before they can fold. The
/// guard test is [`tests::content_above_the_new_nyquist_is_attenuated`], which
/// feeds a 12 kHz tone and asserts it does not survive.
pub fn resample_for_whisper(samples: &[f32], source_rate: u32) -> Vec<f32> {
    if samples.is_empty() {
        return Vec::new();
    }

    if source_rate == WHISPER_SAMPLE_RATE {
        return samples.to_vec();
    }

    // Upsampling cannot alias: there is no content above the new Nyquist to
    // fold, so interpolation alone is correct.
    if source_rate < WHISPER_SAMPLE_RATE {
        return resample_mono(samples, source_rate, WHISPER_SAMPLE_RATE);
    }

    // Cut a little below the new Nyquist so the filter's transition band lands
    // in the stopband rather than straddling 8 kHz.
    let cutoff_hz = 0.45 * WHISPER_SAMPLE_RATE as f64;
    let filtered = low_pass(samples, source_rate, cutoff_hz);

    resample_mono(&filtered, source_rate, WHISPER_SAMPLE_RATE)
}

/// Number of FIR taps. Odd so the filter has an exact integer group delay,
/// which is then compensated by shifting the output — otherwise every
/// transcript timestamp would be late by half the filter length.
const FIR_TAPS: usize = 101;

/// Windowed-sinc low-pass, zero-phase after group-delay compensation.
fn low_pass(samples: &[f32], sample_rate: u32, cutoff_hz: f64) -> Vec<f32> {
    let normalized = cutoff_hz / sample_rate as f64;
    let half = (FIR_TAPS / 2) as isize;

    // Blackman-windowed sinc, normalized to unity DC gain so overall loudness
    // is unchanged.
    let mut kernel = Vec::with_capacity(FIR_TAPS);
    for i in 0..FIR_TAPS {
        let n = i as isize - half;
        let sinc = if n == 0 {
            2.0 * normalized
        } else {
            let x = 2.0 * std::f64::consts::PI * normalized * n as f64;
            (2.0 * normalized) * (x.sin() / x)
        };

        let ratio = i as f64 / (FIR_TAPS - 1) as f64;
        let window = 0.42 - 0.5 * (2.0 * std::f64::consts::PI * ratio).cos()
            + 0.08 * (4.0 * std::f64::consts::PI * ratio).cos();

        kernel.push(sinc * window);
    }

    let sum: f64 = kernel.iter().sum();
    if sum != 0.0 {
        for tap in &mut kernel {
            *tap /= sum;
        }
    }

    // Convolve, compensating the group delay so the output stays time-aligned
    // with the input. Edges clamp to the first and last sample rather than to
    // zero, which would otherwise ring at the file boundaries.
    let len = samples.len() as isize;
    (0..samples.len())
        .map(|i| {
            let center = i as isize + half;
            let mut acc = 0.0f64;
            for (k, tap) in kernel.iter().enumerate() {
                let index = (center - k as isize).clamp(0, len - 1);
                acc += samples[index as usize] as f64 * tap;
            }
            acc as f32
        })
        .collect()
}

/// A block of silence covering `duration_seconds`. Port of `silent_samples`.
/// The loudest sample in a buffer, as an absolute amplitude in 0.0..=1.0.
///
/// Peak rather than average on purpose: this is used to decide whether a track
/// is worth transcribing at all, and a single spoken word in an otherwise empty
/// hour must keep the track. An average would drown it.
pub fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0_f32, |loudest, s| loudest.max(s.abs()))
}

pub fn silent_samples(duration_seconds: f64, sample_rate: u32) -> Vec<f32> {
    let duration_seconds = duration_seconds.max(0.0);
    let count = (duration_seconds * sample_rate as f64).round_ties_even() as usize;
    vec![0.0; count]
}

#[cfg(test)]
mod whisper_resample_tests {
    use super::*;

    /// Root-mean-square amplitude, ignoring the filter's settling region at
    /// each end.
    fn rms(samples: &[f32]) -> f64 {
        let skip = samples.len() / 10;
        let body = &samples[skip..samples.len() - skip];
        let sum: f64 = body.iter().map(|s| (*s as f64) * (*s as f64)).sum();
        (sum / body.len() as f64).sqrt()
    }

    fn tone(freq_hz: f64, sample_rate: u32, seconds: f64) -> Vec<f32> {
        let count = (sample_rate as f64 * seconds) as usize;
        (0..count)
            .map(|i| {
                let t = i as f64 / sample_rate as f64;
                (2.0 * std::f64::consts::PI * freq_hz * t).sin() as f32
            })
            .collect()
    }

    #[test]
    fn audio_already_at_16k_is_returned_untouched() {
        let source = vec![0.1, -0.2, 0.3];
        assert_eq!(resample_for_whisper(&source, WHISPER_SAMPLE_RATE), source);
    }

    #[test]
    fn empty_input_stays_empty() {
        assert!(resample_for_whisper(&[], 48_000).is_empty());
    }

    /// 48 kHz to 16 kHz must produce exactly a third of the samples, or every
    /// timestamp whisper.cpp reports is scaled wrong.
    #[test]
    fn output_length_matches_the_rate_ratio() {
        let source = tone(440.0, 48_000, 1.0);
        let result = resample_for_whisper(&source, 48_000);
        assert_eq!(result.len(), 16_000);
    }

    /// **The R2 guard.** A 12 kHz tone is above the 8 kHz Nyquist of the target
    /// rate. Without a low-pass it does not disappear — it folds down to 4 kHz
    /// and lands in the middle of the speech band, quietly corrupting the
    /// transcript. If this test fails, transcription accuracy is degraded in a
    /// way that looks like a bad model rather than bad audio.
    #[test]
    fn content_above_the_new_nyquist_is_attenuated() {
        let source = tone(12_000.0, 48_000, 1.0);
        let result = resample_for_whisper(&source, 48_000);

        let before = rms(&source);
        let after = rms(&result);

        assert!(
            after < before * 0.05,
            "12 kHz tone survived decimation: {before:.4} -> {after:.4}. \
             It has aliased to 4 kHz, on top of speech."
        );
    }

    /// The other half of the contract: the filter must not eat actual speech.
    /// 500 Hz is well inside the passband and should come through close to
    /// unchanged.
    #[test]
    fn speech_band_content_survives() {
        let source = tone(500.0, 48_000, 1.0);
        let result = resample_for_whisper(&source, 48_000);

        let before = rms(&source);
        let after = rms(&result);

        assert!(
            after > before * 0.9,
            "500 Hz tone was over-attenuated: {before:.4} -> {after:.4}"
        );
    }

    /// A device running below 16 kHz (AirPods with the mic active drop to
    /// 16 kHz, and some fall lower) must upsample, not silently truncate.
    #[test]
    fn lower_rates_are_upsampled() {
        let source = tone(300.0, 8_000, 1.0);
        let result = resample_for_whisper(&source, 8_000);
        assert_eq!(result.len(), 16_000);
        assert!(rms(&result) > 0.5, "upsampled tone lost its amplitude");
    }

    /// DC gain of the filter is normalized, so a constant signal keeps its
    /// level rather than being scaled by the kernel sum.
    #[test]
    fn constant_signal_keeps_its_level() {
        let source = vec![0.5f32; 48_000];
        let result = resample_for_whisper(&source, 48_000);
        for sample in &result {
            assert!(
                (*sample - 0.5).abs() < 1e-3,
                "DC level shifted: {sample} != 0.5"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    // (peak's own tests are grouped with the rest below)
    use super::*;

    // --- ported from tests/test_audio_stream_utils.py ---------------------

    #[test]
    fn block_at_44100_resamples_to_48000_duration() {
        let source = vec![0.0f32; 441];
        let result = resample_mono(&source, 44_100, 48_000);
        assert_eq!(result.len(), 480);
    }

    #[test]
    fn stereo_pcm16_is_mixed_to_mono() {
        // [[10000, -10000], [20000, 10000]] as little-endian i16
        let stereo: Vec<i16> = vec![10_000, -10_000, 20_000, 10_000];
        let bytes: Vec<u8> = stereo.iter().flat_map(|s| s.to_le_bytes()).collect();

        let result = pcm16_bytes_to_mono_float(&bytes, 2);

        assert_eq!(result.len(), 2);
        assert!(result[0].abs() < 1e-6);
        assert!(result[1] > 0.0);
    }

    #[test]
    fn silent_samples_matches_gap_duration() {
        let result = silent_samples(0.7, TARGET_SAMPLE_RATE);
        assert_eq!(result.len(), 33_600);
        assert!(result.iter().all(|s| *s == 0.0));
    }

    // --- additional coverage for the parity contract ----------------------

    #[test]
    fn matching_rates_are_a_no_op() {
        let source = vec![0.1, -0.2, 0.3, -0.4];
        assert_eq!(resample_mono(&source, 48_000, 48_000), source);
    }

    #[test]
    fn empty_input_stays_empty() {
        assert!(resample_mono(&[], 44_100, 48_000).is_empty());
    }

    #[test]
    fn endpoints_are_preserved() {
        // The chunk-independent mapping pins first and last samples in place.
        let source = vec![1.0, 0.0, 0.0, -1.0];
        let result = resample_mono(&source, 44_100, 48_000);
        assert_eq!(result.first().copied(), Some(1.0));
        assert_eq!(result.last().copied(), Some(-1.0));
    }

    #[test]
    fn single_sample_is_held() {
        let result = resample_mono(&[0.5], 16_000, 48_000);
        assert_eq!(result.len(), 3);
        assert!(result.iter().all(|s| *s == 0.5));
    }

    #[test]
    fn interpolates_midpoints() {
        // 2 samples -> 3 samples places one exactly halfway.
        let result = resample_mono(&[0.0, 1.0], 2, 3);
        assert_eq!(result.len(), 3);
        assert!((result[1] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn mono_passthrough_does_not_average() {
        let samples = vec![0.25, -0.5];
        assert_eq!(to_mono_float(&samples, 1), samples);
    }

    #[test]
    fn partial_trailing_frame_is_dropped() {
        // Three samples across two channels: the lone third sample is discarded.
        let result = to_mono_float(&[1.0, 1.0, 1.0], 2);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn f32_to_i16_scales_by_32767_and_clamps() {
        assert_eq!(f32_to_i16(0.0), 0);
        assert_eq!(f32_to_i16(1.0), 32_767);
        assert_eq!(f32_to_i16(-1.0), -32_767);
        // Beyond full scale must clamp rather than wrap.
        assert_eq!(f32_to_i16(2.0), 32_767);
        assert_eq!(f32_to_i16(-2.0), -32_768);
    }

    #[test]
    fn peak_of_digital_silence_is_zero() {
        assert_eq!(peak(&[0.0; 1000]), 0.0);
        // An empty buffer is silent, not a panic.
        assert_eq!(peak(&[]), 0.0);
    }

    #[test]
    fn peak_finds_the_loudest_sample_either_side_of_zero() {
        assert_eq!(peak(&[0.1, -0.8, 0.3]), 0.8);
        assert_eq!(peak(&[-0.05, 0.02]), 0.05);
    }

    #[test]
    fn one_loud_sample_carries_the_whole_buffer() {
        // The case peak exists for. A single word in an otherwise empty hour
        // must keep the track; an average would put this far below any
        // threshold and the speech would be thrown away.
        let mut samples = vec![0.0_f32; 100_000];
        samples[42] = 0.6;
        assert_eq!(peak(&samples), 0.6);
    }
}
