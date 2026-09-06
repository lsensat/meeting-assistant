//! Device enumeration and capture-stream opening.
//!
//! # Why there is no `macos.rs` / `windows.rs` split
//!
//! The plan called for a per-platform split here. Phase M0 measured that the
//! two platforms need **identical** cpal calls, so the split would have been
//! two copies of the same file:
//!
//! | | Windows | macOS |
//! |---|---|---|
//! | microphones | `host.input_devices()` | `host.input_devices()` |
//! | system sources | `host.output_devices()` | `host.output_devices()` |
//! | system format | `default_output_config()` | `default_output_config()` |
//! | loopback | WASAPI `AUDCLNT_STREAMFLAGS_LOOPBACK` | Core Audio process tap |
//!
//! The loopback mechanism differs, but cpal picks it internally from the device
//! it is handed: opening an input stream on a *render* endpoint enables
//! loopback on Windows and builds a process tap plus private aggregate device
//! on macOS. Neither is visible from here.
//!
//! Two asymmetries are load-bearing and are commented at their call sites:
//! render endpoints report their format through `default_output_config()`
//! (`default_input_config()` errors and `supported_input_configs()` is empty),
//! and `supports_input()` decides which path cpal takes for a system source.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, Host, Stream, StreamConfig, SupportedStreamConfig};

use meeting_core::policy;

/// Which of the two tracks a device is being enumerated or opened for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// The user's microphone: a real capture endpoint.
    Microphone,
    /// What the machine is playing. A *render* endpoint opened as input, which
    /// is what makes cpal enable loopback.
    SystemAudio,
}

impl SourceKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Microphone => "microphone",
            Self::SystemAudio => "system audio",
        }
    }
}

#[derive(Debug)]
pub enum AudioError {
    Enumeration(cpal::Error),
    NoSuchDevice(String),
    Format(String, cpal::Error),
    Build(String, cpal::Error),
    Play(String, cpal::Error),
}

impl std::fmt::Display for AudioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Enumeration(e) => write!(f, "could not enumerate audio devices: {e}"),
            Self::NoSuchDevice(id) => write!(f, "device {id} is no longer present"),
            Self::Format(name, e) => write!(f, "could not read the format of {name}: {e}"),
            Self::Build(name, e) => write!(f, "could not open a capture stream on {name}: {e}"),
            Self::Play(name, e) => write!(f, "could not start the capture stream on {name}: {e}"),
        }
    }
}

impl std::error::Error for AudioError {}

/// A cpal device plus the policy-facing description of it.
pub struct Endpoint {
    pub device: Device,
    pub info: policy::Device,
    pub config: SupportedStreamConfig,
}

/// Everything the selection policy needs about what is currently plugged in.
pub struct Snapshot {
    pub devices: Vec<policy::Device>,
    pub default_id: Option<String>,
}

impl Snapshot {
    pub fn default_id(&self) -> Option<&str> {
        self.default_id.as_deref()
    }
}

/// cpal 0.18 replaced `Device::name()` with a structured `description()`.
fn device_name(device: &Device) -> Option<String> {
    device.description().ok().map(|d| d.name().to_string())
}

/// The stable key for a device. On macOS this is the Core Audio device UID; on
/// Windows it is the WASAPI endpoint id. Both survive reboots and hotplug,
/// unlike the PortAudio integer index the Python used — which renumbers exactly
/// when the failover code runs. See Windows risk R3.
fn device_id(device: &Device) -> Option<String> {
    device.id().ok().map(|id| id.to_string())
}

/// The format a device reports for the direction we will actually open it in.
///
/// **The asymmetry is deliberate.** A render endpoint has no *input* config:
/// `default_input_config()` returns an error and `supported_input_configs()` is
/// empty on one. Its mix format comes from `default_output_config()`, and that
/// is the config we then open an input stream with. Do not "fix" this to use
/// `default_input_config()` for both.
fn config_for(device: &Device, kind: SourceKind) -> Result<SupportedStreamConfig, cpal::Error> {
    match kind {
        SourceKind::Microphone => device.default_input_config(),
        SourceKind::SystemAudio => device.default_output_config(),
    }
}

fn describe(device: &Device, kind: SourceKind) -> Option<policy::Device> {
    let name = device_name(device)?;
    let id = device_id(device)?;
    let config = config_for(device, kind).ok()?;

    Some(policy::Device::new(
        id,
        name,
        config.sample_rate(),
        config.channels(),
    ))
}

fn host() -> Host {
    cpal::default_host()
}

/// Enumerate what can currently be recorded for `kind`, plus the OS default.
///
/// Devices whose name, id or format cannot be read are skipped rather than
/// failing the whole enumeration: this runs once a second during recording, and
/// a device being torn down mid-enumeration is normal, not exceptional.
pub fn snapshot(kind: SourceKind) -> Result<Snapshot, AudioError> {
    let host = host();

    let devices: Vec<Device> = match kind {
        SourceKind::Microphone => host.input_devices().map_err(AudioError::Enumeration)?.collect(),
        SourceKind::SystemAudio => host
            .output_devices()
            .map_err(AudioError::Enumeration)?
            .collect(),
    };

    let infos = devices.iter().filter_map(|d| describe(d, kind)).collect();

    let default_device = match kind {
        SourceKind::Microphone => host.default_input_device(),
        SourceKind::SystemAudio => host.default_output_device(),
    };

    Ok(Snapshot {
        devices: infos,
        default_id: default_device.as_ref().and_then(device_id),
    })
}

/// Re-acquire a cpal `Device` from the stable id the policy chose.
pub fn find(id: &str, kind: SourceKind) -> Result<Endpoint, AudioError> {
    let host = host();

    let devices: Vec<Device> = match kind {
        SourceKind::Microphone => host.input_devices().map_err(AudioError::Enumeration)?.collect(),
        SourceKind::SystemAudio => host
            .output_devices()
            .map_err(AudioError::Enumeration)?
            .collect(),
    };

    let device = devices
        .into_iter()
        .find(|d| device_id(d).as_deref() == Some(id))
        .ok_or_else(|| AudioError::NoSuchDevice(id.to_string()))?;

    let name = device_name(&device).unwrap_or_else(|| id.to_string());
    let config = config_for(&device, kind).map_err(|e| AudioError::Format(name.clone(), e))?;

    let info = policy::Device::new(id, name, config.sample_rate(), config.channels());

    Ok(Endpoint {
        device,
        info,
        config,
    })
}

/// True when cpal will record this system source's *microphone* instead of the
/// audio it is playing.
///
/// cpal decides between loopback and ordinary capture with
/// `if self.supports_input()` (cpal 0.18.2 `coreaudio/macos/device.rs:727`,
/// and the same shape on WASAPI). A device exposing both directions therefore
/// silently yields the wrong audio: full-length file, plausible waveform, no
/// error. That is risk R-M2.
///
/// Phase M0 measured that macOS splits a USB headset into two separate
/// endpoints, so this has not been observed in practice — but it is one call
/// and it turns a silent catastrophe into a loud log line, so it stays.
pub fn would_capture_microphone_instead(endpoint: &Endpoint) -> bool {
    endpoint.device.supports_input()
}

/// Open a capture stream, handing each mono-downmixed chunk to `on_chunk`.
///
/// `on_chunk` runs on the driver's **realtime thread**. It must not allocate,
/// lock or block — see `recorder.rs`, where it is a `try_send` into a bounded
/// channel and nothing else.
pub fn open_capture<C, E>(
    endpoint: &Endpoint,
    kind: SourceKind,
    mut on_chunk: C,
    on_error: E,
) -> Result<Stream, AudioError>
where
    C: FnMut(&[f32]) + Send + 'static,
    E: FnMut(cpal::Error) + Send + 'static,
{
    let name = endpoint.info.name.clone();

    // Hand cpal exactly the config the device reported, unmodified.
    //
    // On macOS `build_input_stream_raw` calls `set_physical_format` on the real
    // output device *before* creating the process tap (cpal 0.18.2
    // `coreaudio/macos/device.rs:712-725`). Passing anything other than the
    // device's own reported format can therefore change the sample rate of the
    // user's speakers as a side effect of starting a recording. Resampling to
    // 48 kHz happens later, on our side. Never pass a rate we chose.
    let config: StreamConfig = endpoint.config.into();
    let channels = endpoint.config.channels();

    // Request f32 regardless of the endpoint's native format and let cpal
    // convert. Both the WASAPI loopback mix format and the Core Audio tap
    // format are normally float, and this keeps one code path.
    // Downmix scratch, reused across callbacks. `to_mono_float` returns a fresh
    // Vec, and allocating on the driver's realtime thread risks a glitch, so
    // the equivalent is written into a buffer that only grows once.
    // `downmix_into` is asserted equal to `to_mono_float` in the tests below.
    let mut mono: Vec<f32> = Vec::new();

    let stream = endpoint
        .device
        .build_input_stream(
            config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                // Downmix on the realtime thread so the channel carries mono
                // and the writer never needs to know the device layout.
                downmix_into(&mut mono, data, channels);
                on_chunk(&mono);
            },
            on_error,
            None,
        )
        .map_err(|e| AudioError::Build(name.clone(), e))?;

    // cpal 0.18 no longer auto-starts streams. Forgetting this records silence
    // with no error whatsoever — Windows risk R4.
    stream.play().map_err(|e| AudioError::Play(name, e))?;

    let _ = kind;
    Ok(stream)
}

/// Allocation-free equivalent of [`meeting_core::convert::to_mono_float`].
///
/// Averages the channels of an interleaved buffer into `out`, reusing `out`'s
/// capacity. Kept byte-for-byte equivalent to the reference implementation by
/// [`tests::downmix_into_matches_the_reference`] — if the reference ever
/// changes, that test fails rather than the two silently diverging.
fn downmix_into(out: &mut Vec<f32>, data: &[f32], channels: u16) {
    out.clear();

    if channels <= 1 {
        out.extend_from_slice(data);
        return;
    }

    let channels = channels as usize;
    out.reserve(data.len() / channels);

    for frame in data.chunks_exact(channels) {
        let sum: f32 = frame.iter().sum();
        out.push(sum / channels as f32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use meeting_core::convert::to_mono_float;

    /// The realtime path must produce exactly what the tested, allocating
    /// reference produces. Any divergence here is a silent audio bug.
    #[test]
    fn downmix_into_matches_the_reference() {
        let cases: &[(&[f32], u16)] = &[
            (&[], 1),
            (&[], 2),
            (&[0.5, -0.5, 1.0], 1),
            (&[0.5, -0.5, 1.0, 0.0], 2),
            (&[0.25, 0.75, -1.0, 1.0, 0.0, 0.5], 2),
            (&[0.3, 0.6, 0.9, -0.3, -0.6, -0.9], 3),
        ];

        let mut out = Vec::new();
        for (data, channels) in cases {
            downmix_into(&mut out, data, *channels);
            assert_eq!(
                out,
                to_mono_float(data, *channels),
                "mismatch for {channels} channels on {data:?}"
            );
        }
    }

    /// The buffer is reused across callbacks; a shorter chunk after a longer
    /// one must not leave stale samples behind.
    #[test]
    fn reused_buffer_does_not_leak_previous_chunk() {
        let mut out = Vec::new();
        downmix_into(&mut out, &[1.0, 1.0, 1.0, 1.0], 2);
        assert_eq!(out.len(), 2);
        downmix_into(&mut out, &[0.5, 0.5], 2);
        assert_eq!(out, vec![0.5]);
    }
}
