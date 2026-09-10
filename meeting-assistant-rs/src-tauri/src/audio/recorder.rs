//! One generic recorder, instantiated twice.
//!
//! Port of `record_microphone` (`app.py:1470`) and `record_system_audio`
//! (`app.py:1651`), which are ~360 lines of near-duplicate Python differing
//! only in which enumerator they call and which events they emit. Here that
//! difference is a [`SourceKind`] and an [`Events`] mapping, so there is one
//! loop to reason about instead of two that must be kept in sync.
//!
//! # Threading
//!
//! Two threads per track, not one:
//!
//! * the **driver's realtime thread**, owned by cpal, which runs the data
//!   callback. It downmixes and `try_send`s into a bounded channel. It never
//!   allocates, locks, or blocks — a blocked realtime callback is a glitch.
//! * the **writer thread**, spawned here, which owns the `Stream`, the
//!   [`TrackWriter`] and the gap bookkeeping.
//!
//! The `Stream` lives on the writer thread because cpal's `Stream` is `!Send`
//! on some backends and must be created and dropped on the same thread.
//!
//! # Why the config is snapshotted
//!
//! The Python recorder threads call `settings_mic_var.get()` (`app.py:1471`) —
//! a Tcl call from a non-main thread — and read the global `config` while the
//! main thread may be running `config.clear()` in `write_config()`
//! (`app.py:655`). Deferred fixes #1 and #2. Here every thread is handed an
//! owned [`RecorderConfig`] at start and never reads live UI state again.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};

use meeting_core::convert::{resample_mono, TARGET_SAMPLE_RATE};
use meeting_core::policy;

use super::devices::{self, AudioError, SourceKind};
use super::wav::{TrackWriter, WavError};

/// Verbatim parity with the Python constants (`app.py:65-67`).
const CHUNK: usize = 1024;
const HOTPLUG_CHECK: Duration = Duration::from_secs(1);
const HOTPLUG_RETRY: Duration = Duration::from_millis(250);

/// No data for this long means the device is wedged. New behaviour with no
/// Python counterpart — the Python poll could only run *after* a successful
/// blocking read, so a hung device also hung the detector.
const DATA_WATCHDOG: Duration = Duration::from_secs(2);

/// Bounded so a stalled writer cannot grow memory without limit. Sized for
/// ~1 s of 512-frame callbacks (the cadence Phase M0 measured on macOS).
const QUEUE_DEPTH: usize = 128;

/// Mirrors `threading.Event`: settable once, waitable with a timeout.
pub struct StopEvent {
    flag: Mutex<bool>,
    condvar: Condvar,
}

impl Default for StopEvent {
    fn default() -> Self {
        Self::new()
    }
}

impl StopEvent {
    pub fn new() -> Self {
        Self {
            flag: Mutex::new(false),
            condvar: Condvar::new(),
        }
    }

    pub fn set(&self) {
        let mut flag = self.flag.lock().expect("stop flag poisoned");
        *flag = true;
        self.condvar.notify_all();
    }

    pub fn is_set(&self) -> bool {
        *self.flag.lock().expect("stop flag poisoned")
    }

    /// Port of `stop_event.wait(timeout)`. Returns true if stop was set.
    pub fn wait(&self, timeout: Duration) -> bool {
        let flag = self.flag.lock().expect("stop flag poisoned");
        if *flag {
            return true;
        }
        let (flag, _) = self
            .condvar
            .wait_timeout(flag, timeout)
            .expect("stop flag poisoned");
        *flag
    }
}

/// UI-facing events. One-to-one with the Python queue tags so the port can be
/// diffed against current behaviour.
#[derive(Debug, Clone)]
pub enum Event {
    /// Recording from this device; no failover happened.
    Device(SourceKind, String),
    /// Recording from this device, but it is not the configured one.
    Fallback(SourceKind, String),
    Log(String),
    Error(String),
}

pub type Events = Sender<Event>;

/// Everything a recorder thread needs, owned. Snapshotted before the thread
/// starts; never read from live UI state.
#[derive(Debug, Clone)]
pub struct RecorderConfig {
    pub kind: SourceKind,
    pub configured_name: String,
    pub output_file: PathBuf,
}

/// What a finished track reports back.
#[derive(Debug, Clone)]
pub struct TrackSummary {
    pub kind: SourceKind,
    pub path: PathBuf,
    pub frames: u64,
    pub duration_seconds: f64,
    /// True if the recording ever ran on a device other than the configured
    /// one. Drives the "(automatic)" suffix in the UI.
    pub automatic_fallback: bool,
    pub final_device: String,
    /// Realtime callbacks dropped because the writer could not keep up.
    /// Non-zero means audio was lost.
    pub overflows: u64,
    /// Silence inserted to cover device **outages**, in samples.
    ///
    /// Non-zero means the device went away mid-recording and that stretch of
    /// the meeting is silence. Deliberately excludes the opening silence — see
    /// `lead_in_frames`.
    pub gap_frames: u64,
    /// Silence covering the first device open, in samples.
    ///
    /// Every recording has some: opening a capture device takes a couple of
    /// hundred milliseconds on macOS, and longer on Windows where device
    /// enumeration and the WASAPI loopback open both happen inside this window.
    /// The silence is what makes the two tracks start at the same instant.
    ///
    /// It is counted separately because folding it into `gap_frames` made every
    /// healthy recording look damaged: a damage check that fires on all of them
    /// guarantees the one real failure is ignored.
    pub lead_in_frames: u64,
}

#[derive(Debug)]
pub enum RecorderError {
    Wav(WavError),
    /// No usable device was ever found, so the file would be empty.
    NoDevice(SourceKind),
}

impl std::fmt::Display for RecorderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Wav(e) => write!(f, "{e}"),
            Self::NoDevice(kind) => write!(f, "no {} device could be opened", kind.label()),
        }
    }
}

impl std::error::Error for RecorderError {}

impl From<WavError> for RecorderError {
    fn from(e: WavError) -> Self {
        Self::Wav(e)
    }
}

/// Handles for a running track.
pub struct Recorder {
    pub handle: JoinHandle<Result<TrackSummary, RecorderError>>,
}

/// Accumulates arbitrary-sized device chunks and emits exactly `CHUNK`-sized
/// units.
///
/// # Why this exists
///
/// `resample_mono` resamples **each call independently**, mapping the input's
/// first and last sample onto the output's first and last. Its output therefore
/// depends on how the stream was chunked, not only on the samples. The Python
/// always fed it exactly 1024 frames (`stream.read(CHUNK)`), so feeding it the
/// 512-frame buffers macOS actually delivers would produce a different file
/// from the same audio and make byte-diff parity testing impossible.
///
/// Re-packetising removes chunk size as a confound. It is the reason the plan
/// insists `resample_mono` must not be "fixed" to be stateful.
struct Repacketiser {
    pending: VecDeque<f32>,
    scratch: Vec<f32>,
}

impl Repacketiser {
    fn new() -> Self {
        Self {
            pending: VecDeque::with_capacity(CHUNK * 4),
            scratch: Vec::with_capacity(CHUNK),
        }
    }

    fn push(&mut self, samples: &[f32]) {
        self.pending.extend(samples.iter().copied());
    }

    /// Take the next full 1024-sample unit, or `None` if not enough yet.
    fn next_chunk(&mut self) -> Option<&[f32]> {
        if self.pending.len() < CHUNK {
            return None;
        }
        self.scratch.clear();
        self.scratch.extend(self.pending.drain(..CHUNK));
        Some(&self.scratch)
    }

    /// Whatever is left at stop. The Python's final partial read is written
    /// too, so dropping this would truncate every recording by up to 1023
    /// samples.
    fn drain_remainder(&mut self) -> Option<&[f32]> {
        if self.pending.is_empty() {
            return None;
        }
        self.scratch.clear();
        self.scratch.extend(self.pending.drain(..));
        Some(&self.scratch)
    }
}

/// Spawn one track's recorder.
///
/// `started` is stamped **once by the caller, before either track is spawned**,
/// and passed to both. The Python stamps `start_times` inside each thread
/// (`app.py:1374-1393`), baking thread-spawn jitter into the alignment between
/// the two files; one shared instant removes that nondeterminism.
pub fn spawn(
    config: RecorderConfig,
    stop: Arc<StopEvent>,
    muted: Arc<AtomicBool>,
    events: Events,
    started: Instant,
) -> Recorder {
    let handle = std::thread::Builder::new()
        .name(format!("recorder-{}", config.kind.label().replace(' ', "-")))
        .spawn(move || run(config, stop, muted, events, started))
        .expect("failed to spawn recorder thread");

    Recorder { handle }
}

/// The recorder loop. Structure follows `record_microphone` closely on purpose,
/// so the two can be read side by side during review.
fn run(
    config: RecorderConfig,
    stop: Arc<StopEvent>,
    muted: Arc<AtomicBool>,
    events: Events,
    started: Instant,
) -> Result<TrackSummary, RecorderError> {
    let kind = config.kind;
    let mut writer = TrackWriter::create(&config.output_file)?;

    // Owned exclusively by this thread, alongside the writer. See wav.rs.
    let mut gap_started_at: Option<Instant> = None;
    let mut current: Option<OpenStream> = None;
    let mut current_name = String::new();
    let mut automatic_fallback = false;
    let mut ever_opened = false;
    let mut last_device_check = Instant::now();
    // What the OS default was when the current stream was opened. Detector 4
    // compares against this so it fires on the *transition*, not on the state.
    let mut known_default_id: Option<String> = None;
    // Needed to resample the repacketiser tail at stop even if the device is
    // gone by then and the stream's own `source_rate` is no longer reachable.
    let mut last_source_rate = TARGET_SAMPLE_RATE;
    let mut overflows_reported = 0u64;
    // Consecutive failed opens, for the backoff below.
    let mut consecutive_failures = 0u32;
    // Whether the configured device's current reappearance has already been
    // acted on. Reset when it goes away, so each return is attempted once.
    let mut returned_to_configured = false;
    let mut gap_frames = 0u64;
    // Silence written to cover the FIRST device open, kept apart from
    // `gap_frames` because it is alignment, not damage — see `lead_in_frames`
    // on `TrackSummary`.
    let mut lead_in_frames = 0u64;
    let mut gap_is_lead_in = false;
    let mut repacketiser = Repacketiser::new();

    // The very first open covers the time between `started` and now, so the two
    // tracks begin at the same instant even though the threads start slightly
    // apart.
    let mut pending_lead_in = Some(started);

    while !stop.is_set() {
        // --- acquire a device ------------------------------------------
        if current.is_none() {
            let snapshot = match devices::snapshot(kind) {
                Ok(s) => s,
                Err(e) => {
                    let _ = events.send(Event::Log(format!("{e}")));
                    if stop.wait(HOTPLUG_RETRY) {
                        break;
                    }
                    continue;
                }
            };

            let failover = match kind {
                SourceKind::Microphone => policy::choose_microphone_failover(
                    &snapshot.devices,
                    &current_name,
                    &config.configured_name,
                    snapshot.default_id(),
                ),
                SourceKind::SystemAudio => policy::choose_system_failover(
                    &snapshot.devices,
                    &current_name,
                    &config.configured_name,
                    snapshot.default_id(),
                ),
            };

            let Some(chosen) = failover.device else {
                if stop.wait(HOTPLUG_RETRY) {
                    break;
                }
                continue;
            };

            let opened = match open(&chosen.id, kind, &events) {
                Ok(opened) => opened,
                Err(e) => {
                    let _ = events.send(Event::Log(format!("{e}")));
                    if stop.wait(HOTPLUG_RETRY) {
                        break;
                    }
                    continue;
                }
            };

            // Cover the outage *before* any new audio lands, so the samples
            // that follow sit at the right file position. This is the only
            // thing keeping the two tracks aligned across a disconnect, and it
            // only ever manifests on a disconnect (`app.py:1557-1562`).
            // The silence is deliberately NOT written here — it is written when
            // the first sample actually arrives. See the pump branch below.
            if gap_started_at.is_none() {
                gap_started_at = pending_lead_in.take();
                // Only the first acquisition draws from `pending_lead_in`, so
                // this is exactly the opening silence and never an outage.
                gap_is_lead_in = gap_started_at.is_some();
            }

            // `failover.changed` is also true on the very FIRST acquisition:
            // `current_name` starts empty, so `choose_*_failover` cannot find
            // "the device we are already on" and reports a change. Taking that
            // at face value marked every recording as a fallback and put
            // "(automatic)" in the UI permanently, even when the configured
            // device was used. Only trust it once we have actually been on a
            // device.
            //
            // Judged by **where we landed**, not by whether the policy had to
            // make a choice.
            //
            // `failover.changed` is true whenever the device we were on could
            // not be found in the enumeration — including when it reappears a
            // moment later and we reopen the very same one. Treating that as a
            // fallback told the user "Not the device you chose" while showing
            // the device they had chosen, and because the flag is sticky, one
            // transient blip labelled the whole meeting. A recording that
            // reacquires its own device many times — which is what the data
            // watchdog produces — is marked permanently.
            //
            // The honest question is simpler: are we on the device the user
            // asked for? If they asked for nothing, have we drifted off the one
            // we started on?
            automatic_fallback |= policy::is_automatic_fallback(
                &config.configured_name,
                &current_name,
                &chosen.name,
            );

            // A successful open clears the backoff, so a device that recovers
            // is not punished for having failed earlier.
            consecutive_failures = 0;
            current_name = chosen.name.clone();
            last_device_check = Instant::now();
            ever_opened = true;

            // Re-baseline detector 4 on every open, including the first. If we
            // could not move to the new default, this records the default we
            // *saw* rather than the device we got, so the detector stays quiet
            // until the user changes output again instead of retrying forever.
            known_default_id = snapshot.default_id().map(str::to_string);
            last_source_rate = opened.source_rate;

            let event = if automatic_fallback {
                Event::Fallback(kind, current_name.clone())
            } else {
                Event::Device(kind, current_name.clone())
            };
            let _ = events.send(event);

            let _ = events.send(Event::Log(format!(
                "{} capture on \"{}\" ({} Hz, {} ch, id {})",
                kind.label(),
                chosen.name,
                opened.source_rate,
                chosen.channels,
                chosen.id
            )));

            current = Some(opened);
        }

        // --- pump audio -------------------------------------------------
        let Some(open_stream) = current.as_mut() else {
            continue;
        };

        match open_stream.chunks.recv_timeout(HOTPLUG_RETRY) {
            Ok(chunk) => {
                // Close the outage against the first sample that actually
                // arrives, not against the moment `open()` returned.
                //
                // `stream.play()` returns well before the device starts
                // delivering, and how long that takes differs sharply per
                // device — a Core Audio tap has to spin up an aggregate device,
                // a microphone does not. Ending the gap at `open()` therefore
                // under-counts the outage by the startup latency, and the track
                // comes out short by that much on every transition.
                //
                // Measured on macOS: ~0.23 s lost per device transition, 0.457 s
                // of skew across a plug-in plus an unplug in one 60 s run.
                //
                // This is a deliberate deviation from the Python, which writes
                // the silence at open time (`app.py:1557-1562`). It is a more
                // faithful implementation of that code's *intent* — advance the
                // file by exactly the wall-clock time no audio was arriving —
                // and the Python only gets away with the simpler version
                // because its blocking `stream.read()` starts returning almost
                // immediately after open.
                if let Some(gap_start) = gap_started_at.take() {
                    let seconds = gap_start.elapsed().as_secs_f64();
                    let frames = policy::silence_frames_for_gap(seconds, TARGET_SAMPLE_RATE);
                    if frames > 0 {
                        writer.write_silence(frames)?;
                        // Counted apart. Opening a device takes a couple of
                        // hundred milliseconds on macOS and longer on Windows,
                        // where enumeration and the WASAPI loopback open both
                        // sit inside this window. That silence is how the two
                        // tracks come to start at the same instant; calling it
                        // damage would report every healthy meeting as broken.
                        if std::mem::take(&mut gap_is_lead_in) {
                            // Only the part of it that a device open can
                            // account for. An idle-gated tap delivering
                            // nothing for its first twelve seconds is not
                            // lead-in, and calling it that is what let a
                            // meeting missing its opening pass as intact.
                            let (lead_in, over) =
                                policy::split_lead_in(frames, TARGET_SAMPLE_RATE);
                            lead_in_frames += lead_in;
                            gap_frames += over;
                        } else {
                            gap_frames += frames as u64;
                        }
                    }
                }

                open_stream.last_data = Instant::now();
                repacketiser.push(&chunk);

                while let Some(unit) = repacketiser.next_chunk() {
                    write_unit(&mut writer, unit, open_stream.source_rate, &muted)?;
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                // No data. Either the device is wedged or we are simply between
                // callbacks; the watchdog below decides which.
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                // cpal dropped the callback, which is what an unplug looks like
                // from here. This used to be the one teardown path with no log
                // line, so a real disconnect vanished from the logs entirely
                // while the three noisy detectors stayed quiet.
                let _ = events.send(Event::Log(format!(
                    "{} stream closed by the driver (device unplugged?); reopening",
                    kind.label()
                )));
                close_stream(
                    &mut current,
                    &mut writer,
                    &mut repacketiser,
                    &muted,
                    &mut gap_started_at,
                )?;
                continue;
            }
        }

        // Report dropped callbacks once per occurrence rather than per chunk.
        let overflows = open_stream.overflows.load(Ordering::Relaxed);
        if overflows > overflows_reported {
            overflows_reported = overflows;
            let _ = events.send(Event::Log(format!("{} input overflow", kind.label())));
        }

        // --- detector 1: the stream errored out -------------------------
        if open_stream.failed.load(Ordering::Relaxed) {
            let detail = open_stream
                .last_error
                .lock()
                .ok()
                .and_then(|slot| slot.clone())
                .unwrap_or_else(|| "no detail from cpal".to_string());
            consecutive_failures = consecutive_failures.saturating_add(1);
            let wait = Duration::from_millis(policy::reopen_backoff_ms(consecutive_failures));
            let _ = events.send(Event::Log(format!(
                "{} stream reported an error ({consecutive_failures} in a row); \
                 reopening in {}ms — {detail}",
                kind.label(),
                wait.as_millis()
            )));
            close_stream(
                    &mut current,
                    &mut writer,
                    &mut repacketiser,
                    &muted,
                    &mut gap_started_at,
                )?;

            // Backed off, because reopening a device that keeps failing is not
            // free. A Plantronics headset opened for capture while the same
            // headset was open for loopback produced "a buffer underrun or
            // overrun occurred" **35 times in 4.4 seconds** — an open every
            // 120ms, each costing 60-70ms of device work, on the machine that
            // was supposed to be recording a meeting.
            //
            // The silence written to cover the outage is the same either way:
            // it is measured from wall-clock time, not from how often we tried.
            if stop.wait(wait) {
                break;
            }
            continue;
        }

        // --- detector 2: no data for too long ---------------------------
        //
        // New behaviour. Log loudly so a watchdog gap is never mistaken for a
        // real disconnect when reading the logs afterwards.
        if open_stream.last_data.elapsed() > DATA_WATCHDOG {
            let _ = events.send(Event::Log(format!(
                "WATCHDOG: no {} data for {:.1}s; treating as a disconnect",
                kind.label(),
                open_stream.last_data.elapsed().as_secs_f64()
            )));
            close_stream(
                    &mut current,
                    &mut writer,
                    &mut repacketiser,
                    &muted,
                    &mut gap_started_at,
                )?;
            continue;
        }

        // --- detector 3: the device left the enumeration ----------------
        //
        // Verbatim parity with HOTPLUG_CHECK_SECONDS (`app.py:1614`).
        if last_device_check.elapsed() >= HOTPLUG_CHECK {
            last_device_check = Instant::now();

            if let Ok(snapshot) = devices::snapshot(kind) {
                let still_present = snapshot.devices.iter().any(|d| d.name == current_name);
                if !still_present {
                    let _ = events.send(Event::Log(format!(
                        "{} device \"{}\" disappeared",
                        kind.label(),
                        current_name
                    )));
                    close_stream(
                    &mut current,
                    &mut writer,
                    &mut repacketiser,
                    &muted,
                    &mut gap_started_at,
                )?;
                    continue;
                }

                // --- detector 3b: the configured device came back --------
                //
                // A headset unplugged at 5s and plugged back in at 36s left the
                // remaining fifteen seconds recorded on the laptop's built-in
                // microphone, while the user was wearing the headset. The
                // policy is only consulted when the current stream has already
                // died, so a healthy fallback is never reconsidered.
                //
                // Edge-triggered, for the reason spelled out on detector 4
                // below: a device that enumerates but will not open would
                // otherwise tear the stream down every second, forever.
                // `returned_to_configured` latches until the device goes away
                // again, so each reappearance is worth exactly one attempt.
                let configured_present = !config.configured_name.is_empty()
                    && snapshot
                        .devices
                        .iter()
                        .any(|d| d.name == config.configured_name);

                if !configured_present {
                    returned_to_configured = false;
                }

                if !returned_to_configured
                    && policy::should_return_to_configured(
                        &config.configured_name,
                        &current_name,
                        configured_present,
                    )
                {
                    returned_to_configured = true;
                    let _ = events.send(Event::Log(format!(
                        "{}: \"{}\" is back; returning to it from \"{}\"",
                        kind.label(),
                        config.configured_name,
                        current_name
                    )));

                    // Cleared for the same reason detector 4 clears it:
                    // `choose_failover` keeps the device it is already on, so
                    // without this the reopen re-selects the fallback and the
                    // switch silently never happens.
                    current_name.clear();
                    close_stream(
                        &mut current,
                        &mut writer,
                        &mut repacketiser,
                        &muted,
                        &mut gap_started_at,
                    )?;
                    continue;
                }

                // --- detector 4: macOS only in practice -----------------
                //
                // A tap only captures audio routed to the device it taps
                // (risk R-M8). If the user switches output mid-meeting the tap
                // keeps running and records pure silence, with no error
                // anywhere. Treat it exactly like a disconnect.
                //
                // # Two things here are not optional
                //
                // **Edge-triggered, not level-triggered.** `known_default_id`
                // is what the default was when this stream was opened, so this
                // fires once per actual change. A level-triggered version —
                // "default differs from the device I am on" — stays true
                // forever whenever we cannot move to the new default, and
                // re-tears the stream down every poll.
                //
                // **`current_name` must be cleared.** `choose_system_failover`
                // deliberately keeps the device it is already on when that
                // device is still present, which is what stops it flapping
                // back mid-meeting. Tearing down without clearing therefore
                // re-selects the *same* device and the switch silently never
                // happens.
                //
                // Both were real: measured together they produced 17 teardowns
                // in 60 s, never once switching device, and cost 21.7 s of the
                // system track. Do not "simplify" either one away.
                if kind == SourceKind::SystemAudio
                    && config.configured_name.is_empty()
                    && snapshot.default_id() != known_default_id.as_deref()
                {
                    let new_default = snapshot
                        .devices
                        .iter()
                        .find(|d| Some(d.id.as_str()) == snapshot.default_id())
                        .map(|d| d.name.as_str())
                        .unwrap_or("<unknown>");

                    let _ = events.send(Event::Log(format!(
                        "default output changed from \"{current_name}\" to \
                         \"{new_default}\"; re-opening so system audio keeps \
                         being captured"
                    )));

                    known_default_id = snapshot.default_id().map(str::to_string);
                    current_name.clear();
                    close_stream(
                    &mut current,
                    &mut writer,
                    &mut repacketiser,
                    &muted,
                    &mut gap_started_at,
                )?;
                    continue;
                }
            }
        }
    }

    // --- stop -----------------------------------------------------------
    //
    // Order matters: stop the callback first, *then* drain.
    //
    // Dropping the stream ends the realtime callback, so after this point the
    // channel holds exactly what was captured and nothing more will arrive.
    // Draining first instead would keep pulling newly captured audio for as
    // long as the drain ran, writing past the moment the user pressed stop.
    //
    // The drain itself is parity, not a new behaviour: the Python read the
    // device with a *blocking* `stream.read(CHUNK)`, so everything produced up
    // to the stop was consumed by construction. Our callback-plus-channel model
    // can strand chunks that the realtime thread delivered microseconds before
    // the stop flag was set, and dropping them would silently truncate every
    // recording by that much.
    //
    // The stream is dropped on this thread, the one that created it, because
    // cpal's `Stream` is `!Send` on some backends.
    if let Some(open_stream) = current.take() {
        let OpenStream {
            _stream,
            chunks,
            source_rate,
            ..
        } = open_stream;

        drop(_stream);

        while let Ok(chunk) = chunks.try_recv() {
            repacketiser.push(&chunk);
            while let Some(unit) = repacketiser.next_chunk() {
                write_unit(&mut writer, unit, source_rate, &muted)?;
            }
        }
    }

    // Outside the block above on purpose: when the device disconnected and
    // never came back, `current` is None but the repacketiser can still hold a
    // sub-1024-sample tail from before the disconnect. Leaving it inside would
    // strand those samples in exactly the case where the track is already
    // damaged.
    if let Some(rest) = repacketiser.drain_remainder() {
        let resampled = resample_mono(rest, last_source_rate, TARGET_SAMPLE_RATE);
        writer.write_samples(&resampled)?;
    }

    // A gap still open at stop is written too, so a track that lost its device
    // and never got it back still ends at the right length (`app.py:1645`).
    if let Some(gap_start) = gap_started_at.take() {
        let frames =
            policy::silence_frames_for_gap(gap_start.elapsed().as_secs_f64(), TARGET_SAMPLE_RATE);
        if frames > 0 {
            writer.write_silence(frames)?;
            gap_frames += frames as u64;
        }
    }

    if !ever_opened {
        return Err(RecorderError::NoDevice(kind));
    }

    let path = writer.path().to_path_buf();
    let duration_seconds = writer.duration_seconds();
    let frames = writer.finalize()?;

    Ok(TrackSummary {
        kind,
        path,
        frames,
        duration_seconds,
        automatic_fallback,
        final_device: current_name,
        overflows: overflows_reported,
        gap_frames,
        lead_in_frames,
    })
}

/// Mute is applied to the samples, not by pausing the stream, so the file keeps
/// advancing at realtime and the tracks stay aligned (`app.py:1600`).
fn write_unit(
    writer: &mut TrackWriter,
    unit: &[f32],
    source_rate: u32,
    muted: &AtomicBool,
) -> Result<(), WavError> {
    if muted.load(Ordering::Relaxed) {
        let resampled_len = resample_mono(unit, source_rate, TARGET_SAMPLE_RATE).len();
        return writer.write_silence(resampled_len);
    }

    let resampled = resample_mono(unit, source_rate, TARGET_SAMPLE_RATE);
    writer.write_samples(&resampled)
}

/// Close the stream, recover everything it already captured, and start owing
/// silence from that point.
///
/// # Why the drain is here and not skipped
///
/// The realtime callback runs ahead of the writer, so at any instant the
/// channel holds audio that was captured but not yet written. Dropping the
/// stream without draining discards it — *and* the gap clock then starts at
/// teardown time, so the lost span is not covered by silence either. The track
/// comes out short by that much on every device transition, which is exactly
/// the alignment the silence-gap machinery exists to protect.
///
/// Measured: two transitions in a 60 s run cost 0.488 s of skew before this
/// drain existed.
///
/// Order matters for the same reason it does at stop: drop the stream first so
/// the callback is dead, then drain, so this cannot sit here consuming newly
/// captured audio.
/// Close the current stream, writing whatever it left behind.
///
/// # Why this no longer touches `automatic_fallback`
///
/// It used to end with `*automatic_fallback = true`, unconditionally. So **any**
/// stream close — a watchdog trip, an error, a device change — marked the
/// recording as having fallen back, no matter which device was opened next.
///
/// A device that blips and comes straight back therefore reported "the device
/// set in Settings was not available" while the panel showed the device from
/// Settings. On a machine where the watchdog fires repeatedly it was permanent.
///
/// Closing a stream is not falling back. Whether we fell back is knowable only
/// at the *open*, where the device we landed on is known, and that is where
/// `policy::is_automatic_fallback` decides it.
fn close_stream(
    current: &mut Option<OpenStream>,
    writer: &mut TrackWriter,
    repacketiser: &mut Repacketiser,
    muted: &AtomicBool,
    gap_started_at: &mut Option<Instant>,
) -> Result<(), WavError> {
    let mut outage_began = Instant::now();

    if let Some(open_stream) = current.take() {
        let OpenStream {
            _stream,
            chunks,
            source_rate,
            last_data,
            ..
        } = open_stream;

        drop(_stream);

        let mut last_data = last_data;
        while let Ok(chunk) = chunks.try_recv() {
            last_data = Instant::now();
            repacketiser.push(&chunk);
            while let Some(unit) = repacketiser.next_chunk() {
                write_unit(writer, unit, source_rate, muted)?;
            }
        }

        // The outage began when audio stopped arriving, NOT when we noticed.
        //
        // Every detector has latency: the error flag and the channel are only
        // polled every 250 ms, the enumeration poll runs at 1 s, the watchdog at
        // 2 s. Dating the gap from detection silently swallows that latency —
        // the samples were never captured and no silence is credited for them,
        // so the track ends short by the detector's own reaction time.
        //
        // Measured: this was the last ~0.14 s per device transition remaining
        // after the first-sample fix below.
        outage_began = last_data;
    }

    if gap_started_at.is_none() {
        *gap_started_at = Some(outage_began);
    }
    Ok(())
}

/// A live capture stream plus the state the writer needs about it.
struct OpenStream {
    /// Dropped on teardown, which stops the realtime callback.
    /// The capture stream, and on Windows loopback the silent render stream
    /// that keeps the audio engine delivering. Both must outlive the capture.
    _stream: devices::Capture,
    chunks: Receiver<Vec<f32>>,
    source_rate: u32,
    last_data: Instant,
    overflows: Arc<AtomicU64>,
    failed: Arc<AtomicBool>,
    /// What cpal said when the stream errored, if it did.
    last_error: Arc<Mutex<Option<String>>>,
}

fn open(id: &str, kind: SourceKind, events: &Events) -> Result<OpenStream, AudioError> {
    let endpoint = devices::find(id, kind)?;

    // The format the device reports, against the f32 the stream asks for.
    //
    // This is the single most useful line in the diagnostics file. `cpal`'s
    // `build_input_stream::<f32>` does **not** convert: it passes `F32` down and
    // the callback does `.expect("host supplied incorrect sample type")`. A
    // device whose shared-mode format is not float therefore either fails to
    // open or panics on the realtime thread — and a panic there kills the
    // callback silently, which is indistinguishable from "the device
    // disappeared" once the watchdog notices.
    //
    // Whether that is what happens on a given machine had to be guessed at,
    // repeatedly, because nothing recorded it.
    let _ = events.send(Event::Log(format!(
        "{}: opening \"{}\" — device reports {} Hz, {} ch, {:?}; requesting f32",
        kind.label(),
        endpoint.info.name,
        endpoint.config.sample_rate(),
        endpoint.config.channels(),
        endpoint.config.sample_format(),
    )));

    if kind == SourceKind::SystemAudio && devices::would_capture_microphone_instead(&endpoint) {
        // Risk R-M2. Not observed on macOS, where headsets enumerate as two
        // separate endpoints, but if it ever happens the recording is silently
        // wrong, so say so rather than producing a plausible bad file.
        let _ = events.send(Event::Log(format!(
            "WARNING: \"{}\" reports input channels, so this may capture its \
             microphone instead of system audio",
            endpoint.info.name
        )));
    }

    // A fresh channel per stream open. A generation counter would also work,
    // but this makes it structurally impossible for an in-flight callback from
    // a dead stream to write stale audio after a gap has been inserted.
    let (tx, rx) = bounded::<Vec<f32>>(QUEUE_DEPTH);

    let overflows = Arc::new(AtomicU64::new(0));
    let failed = Arc::new(AtomicBool::new(false));
    // The error itself, not merely that there was one. Discarding it left the
    // watchdog to report "the device disappeared" for a stream that had told us
    // exactly what went wrong.
    let last_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    let cb_overflows = Arc::clone(&overflows);
    let cb_failed = Arc::clone(&failed);
    let cb_error = Arc::clone(&last_error);

    let source_rate = endpoint.info.sample_rate;

    let open_started = Instant::now();
    let stream = devices::open_capture(
        &endpoint,
        kind,
        move |mono| {
            // Realtime thread. `try_send`, never `send`: blocking here stalls
            // the driver and glitches the audio for every app on the machine.
            match tx.try_send(mono.to_vec()) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                    cb_overflows.fetch_add(1, Ordering::Relaxed);
                }
            }
        },
        move |err| {
            // Fastest of the four detectors. The writer polls this flag.
            if let Ok(mut slot) = cb_error.lock() {
                *slot = Some(err.to_string());
            }
            cb_failed.store(true, Ordering::Relaxed);
        },
    )?;

    let _ = events.send(Event::Log(format!(
        "{}: stream open took {:.0}ms",
        kind.label(),
        open_started.elapsed().as_secs_f64() * 1000.0
    )));

    Ok(OpenStream {
        _stream: stream,
        chunks: rx,
        source_rate,
        last_data: Instant::now(),
        overflows,
        failed,
        last_error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Closing a stream must not, by itself, claim a fallback.
    ///
    /// `close_stream` used to end with `*automatic_fallback = true`,
    /// unconditionally — so a watchdog trip that reopened the *same* device
    /// still told the user "the device set in Settings was not available".
    /// On a machine where the watchdog fires repeatedly it was permanent, and
    /// it silently defeated `policy::is_automatic_fallback`.
    #[test]
    fn closing_a_stream_does_not_assert_a_fallback() {
        use meeting_core::policy::is_automatic_fallback;

        // What a watchdog cycle looks like: the same device, closed and
        // reopened. Nothing here is a fallback.
        assert!(!is_automatic_fallback("Headset", "Headset", "Headset"));

        // And the signature no longer offers a way to say otherwise: if
        // `close_stream` regrows an `automatic_fallback` parameter, this stops
        // compiling rather than silently regressing.
        /// Exactly what `close_stream` may take. Regrowing an
        /// `automatic_fallback` parameter stops this compiling.
        type CloseStream = fn(
            &mut Option<OpenStream>,
            &mut TrackWriter,
            &mut Repacketiser,
            &AtomicBool,
            &mut Option<Instant>,
        ) -> Result<(), WavError>;

        let _: CloseStream = close_stream;
    }

    #[test]
    fn repacketiser_emits_exactly_chunk_sized_units() {
        let mut r = Repacketiser::new();

        // Feed the 512-frame buffers macOS actually delivers.
        r.push(&vec![0.1; 512]);
        assert!(r.next_chunk().is_none(), "512 is not yet a full unit");

        r.push(&vec![0.2; 512]);
        let unit = r.next_chunk().expect("1024 available");
        assert_eq!(unit.len(), CHUNK);
        assert!(r.next_chunk().is_none());
    }

    #[test]
    fn repacketiser_preserves_sample_order_across_boundaries() {
        let mut r = Repacketiser::new();
        let input: Vec<f32> = (0..CHUNK * 2).map(|i| i as f32).collect();

        r.push(&input[..700]);
        r.push(&input[700..]);

        let first: Vec<f32> = r.next_chunk().expect("first").to_vec();
        let second: Vec<f32> = r.next_chunk().expect("second").to_vec();

        assert_eq!(first, input[..CHUNK]);
        assert_eq!(second, input[CHUNK..]);
    }

    /// The tail matters: dropping it truncates every recording by up to 1023
    /// samples, which is small enough to look like nothing and still shift a
    /// byte-diff parity test.
    #[test]
    fn repacketiser_drains_the_partial_tail() {
        let mut r = Repacketiser::new();
        r.push(&vec![0.5; CHUNK + 7]);

        assert_eq!(r.next_chunk().expect("full unit").len(), CHUNK);
        assert_eq!(r.drain_remainder().expect("tail").len(), 7);
        assert!(r.drain_remainder().is_none());
    }

    #[test]
    fn stop_event_wakes_waiters_immediately() {
        let stop = Arc::new(StopEvent::new());
        assert!(!stop.is_set());

        let waiter = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            let started = Instant::now();
            let set = waiter.wait(Duration::from_secs(30));
            (set, started.elapsed())
        });

        std::thread::sleep(Duration::from_millis(50));
        stop.set();

        let (set, elapsed) = handle.join().expect("join");
        assert!(set, "wait must report the flag as set");
        assert!(
            elapsed < Duration::from_secs(5),
            "wait should return on notify, not on timeout (took {elapsed:?})"
        );
    }

    #[test]
    fn stop_event_returns_false_on_timeout() {
        let stop = StopEvent::new();
        assert!(!stop.wait(Duration::from_millis(10)));
    }
}
