//! Spike 1 — the loopback idle test. Run this on Windows before anything else.
//!
//! # What this answers
//!
//! cpal enables WASAPI loopback by opening a *render* endpoint as an input
//! stream, and it sets `AUDCLNT_STREAMFLAGS_EVENTCALLBACK | LOOPBACK` together.
//! Microsoft has historically documented event-driven loopback as unsupported,
//! and cpal issues #476 and #516 report the data callback never firing while
//! the render endpoint is idle.
//!
//! If that happens here, `system_audio.wav` would come out far shorter than the
//! meeting and the two tracks would lose alignment catastrophically — while
//! looking perfectly fine in any short test where audio plays throughout. That
//! is why this probe deliberately includes a window of true silence.
//!
//! Everything downstream depends on the answer, so do not skip it.
//!
//! # How to run it
//!
//! ```text
//! cargo run --release -- --list            # show loopback candidates
//! cargo run --release -- --device "Plantronics"   # probe one, 60s
//! cargo run --release -- --device "Plantronics" --seconds 90
//! cargo run --release -- --mic             # probe an input device instead
//! ```
//!
//! # The test protocol
//!
//! While it runs, follow this script exactly:
//!
//! * 0-15s   play music or a video
//! * 15-45s  **absolute silence** — pause everything, no notification sounds
//! * 45-60s  play music again
//!
//! PASS means the frame count keeps climbing at the full rate through the
//! silent window. FAIL means it stalls, and the port needs the `wasapi` crate
//! for the loopback path instead of cpal.
//!
//! Run it three times, because these are genuinely different cases:
//!   1. with a media player open but paused
//!   2. with *no* audio application open at all
//!   3. with only a silent browser tab open
//!
//! Case 2 is the one most likely to fail.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let want_list = args.iter().any(|a| a == "--list");
    let use_mic = args.iter().any(|a| a == "--mic");
    let filter = flag_value(&args, "--device");
    let seconds: u64 = flag_value(&args, "--seconds")
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);

    let host = cpal::default_host();
    println!("host: {}\n", host.id().name());

    if want_list || filter.is_none() {
        list_devices(&host);
        if filter.is_none() {
            println!("\nPick one with:  --device \"<substring of the name>\"");
            println!("Add --mic to probe a capture device instead of a loopback endpoint.");
            return;
        }
        println!();
    }

    let filter = filter.unwrap();

    match probe(&host, &filter, use_mic, seconds) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("\nprobe failed: {e}");
            std::process::exit(1);
        }
    }
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    let index = args.iter().position(|a| a == flag)?;
    args.get(index + 1).cloned()
}

/// List what we can see, both directions.
///
/// Loopback candidates are the *output* devices. This is the part that trips
/// people up: `input_devices()` filters on `supports_input()` and will never
/// list a render endpoint, and `default_input_config()` errors on one. The
/// format must come from `default_output_config()` instead.
fn list_devices(host: &cpal::Host) {
    println!("== loopback candidates (render endpoints, opened as input) ==");
    match host.output_devices() {
        Ok(devices) => {
            for (i, device) in devices.enumerate() {
                let name = device_name(&device);
                match device.default_output_config() {
                    Ok(config) => println!(
                        "  [{i}] {name}\n        {} Hz, {} ch, {:?}",
                        config.sample_rate(),
                        config.channels(),
                        config.sample_format()
                    ),
                    Err(e) => println!("  [{i}] {name}\n        <no default output config: {e}>"),
                }
            }
        }
        Err(e) => println!("  <enumeration failed: {e}>"),
    }

    println!("\n== capture devices (microphones) ==");
    match host.input_devices() {
        Ok(devices) => {
            for (i, device) in devices.enumerate() {
                let name = device_name(&device);
                match device.default_input_config() {
                    Ok(config) => println!(
                        "  [{i}] {name}\n        {} Hz, {} ch, {:?}",
                        config.sample_rate(),
                        config.channels(),
                        config.sample_format()
                    ),
                    Err(e) => println!("  [{i}] {name}\n        <no default input config: {e}>"),
                }
            }
        }
        Err(e) => println!("  <enumeration failed: {e}>"),
    }

    if let Some(d) = host.default_output_device() {
        println!(
            "\ndefault output (== get_default_wasapi_loopback): {}",
            device_name(&d)
        );
    }
    if let Some(d) = host.default_input_device() {
        println!("default input: {}", device_name(&d));
    }
}

fn probe(
    host: &cpal::Host,
    filter: &str,
    use_mic: bool,
    seconds: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let needle = filter.to_lowercase();

    let devices: Vec<cpal::Device> = if use_mic {
        host.input_devices()?.collect()
    } else {
        host.output_devices()?.collect()
    };

    let device = devices
        .into_iter()
        .find(|d| {
            device_name(d).to_lowercase().contains(&needle)
        })
        .ok_or_else(|| format!("no {} device matching {filter:?}", if use_mic { "input" } else { "output" }))?;

    let name = device_name(&device);

    // A render endpoint has no *input* config; ask for its output config and
    // build the input stream from that. This asymmetry is the whole trick
    // behind cpal's loopback support.
    let supported = if use_mic {
        device.default_input_config()?
    } else {
        device.default_output_config()?
    };

    let sample_rate = supported.sample_rate();
    let channels = supported.channels();

    println!("device : {name}");
    println!("format : {sample_rate} Hz, {channels} ch, {:?}", supported.sample_format());
    println!("mode   : {}", if use_mic { "capture" } else { "LOOPBACK (render endpoint as input)" });
    println!("running for {seconds}s\n");
    println!("  follow the script: 0-15s audio, 15-45s SILENCE, 45-60s audio\n");

    let frames = Arc::new(AtomicU64::new(0));
    let callbacks = Arc::new(AtomicU64::new(0));
    let errors = Arc::new(AtomicUsize::new(0));

    let config: cpal::StreamConfig = supported.into();

    let cb_frames = Arc::clone(&frames);
    let cb_calls = Arc::clone(&callbacks);
    let cb_errors = Arc::clone(&errors);
    let ch = channels as u64;

    // Request f32 regardless of the endpoint's native format. The loopback mix
    // format is normally IEEE float rather than int16 (pyaudiowpatch was
    // silently converting), and letting cpal convert keeps one code path.
    let stream = device.build_input_stream(
        config,
        move |data: &[f32], _: &cpal::InputCallbackInfo| {
            cb_calls.fetch_add(1, Ordering::Relaxed);
            cb_frames.fetch_add(data.len() as u64 / ch, Ordering::Relaxed);
        },
        move |err| {
            cb_errors.fetch_add(1, Ordering::Relaxed);
            eprintln!("  !! stream error: {err}");
        },
        None,
    )?;

    // cpal 0.18 no longer auto-starts streams. Forgetting this records nothing,
    // silently — exactly the failure mode risk R4 warns about.
    stream.play()?;

    let started = Instant::now();
    let mut previous = 0u64;
    let mut silent_window_frames = 0u64;

    for tick in 1..=seconds {
        std::thread::sleep(Duration::from_secs(1));

        let total = frames.load(Ordering::Relaxed);
        let this_second = total - previous;
        previous = total;

        // 15..45s is the designated silence window in the test protocol.
        let in_silence = (15..45).contains(&tick);
        if in_silence {
            silent_window_frames += this_second;
        }

        let pct = this_second as f64 / sample_rate as f64 * 100.0;
        let marker = if in_silence { " <- SILENCE WINDOW" } else { "" };
        println!("  t={tick:3}s  frames/s={this_second:7}  ({pct:5.1}% of realtime)  total={total}{marker}");
    }

    drop(stream);

    let elapsed = started.elapsed().as_secs_f64();
    let total = frames.load(Ordering::Relaxed);
    let expected = (elapsed * sample_rate as f64) as u64;
    let ratio = total as f64 / expected as f64;

    // 30 seconds of the run were the silence window.
    let silent_expected = 30.0 * sample_rate as f64;
    let silent_ratio = silent_window_frames as f64 / silent_expected;

    println!("\n----------------------------------------------------------");
    println!("elapsed          : {elapsed:.2}s");
    println!("frames captured  : {total}");
    println!("frames expected  : {expected}");
    println!("overall ratio    : {:.1}%", ratio * 100.0);
    println!("callbacks        : {}", callbacks.load(Ordering::Relaxed));
    println!("stream errors    : {}", errors.load(Ordering::Relaxed));
    println!("\nsilence window (t=15..45s):");
    println!("  frames         : {silent_window_frames}");
    println!("  expected       : {silent_expected:.0}");
    println!("  ratio          : {:.1}%", silent_ratio * 100.0);
    println!("----------------------------------------------------------");

    // The silence window is the actual verdict. A healthy overall ratio can
    // hide a stall if audio was playing for most of the run.
    if silent_ratio > 0.95 {
        println!("VERDICT: PASS — capture continued through silence.");
        println!("         cpal is viable for the loopback path. Proceed with the plan as written.");
    } else if silent_ratio > 0.5 {
        println!("VERDICT: PARTIAL — capture degraded during silence ({:.1}%).", silent_ratio * 100.0);
        println!("         Re-run to confirm. If it reproduces, treat as FAIL.");
    } else {
        println!("VERDICT: FAIL — capture stalled during silence ({:.1}%).", silent_ratio * 100.0);
        println!("         cpal cannot carry the loopback path. Switch to plan B:");
        println!("         the `wasapi` crate with a polling GetBuffer loop, which also");
        println!("         matches the Python structure more closely. Mic capture can stay on cpal.");
    }

    if !use_mic && total == 0 {
        println!("\nNOTE: zero frames overall. Confirm this endpoint is the one actually");
        println!("      playing audio, and that you passed a render endpoint, not a capture one.");
    }

    Ok(())
}

/// cpal 0.18 replaced `Device::name()` with a structured `description()`.
/// Everything here only needs the human-readable name.
fn device_name(device: &cpal::Device) -> String {
    device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|e| format!("<name error: {e}>"))
}
