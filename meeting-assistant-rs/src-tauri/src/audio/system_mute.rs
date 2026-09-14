//! Mute the microphone for **every** application, not just this recording.
//!
//! # What this is for, and the risk it carries
//!
//! The mute button used to silence only the recording: `write_unit` wrote zeros
//! while the stream kept running. Nobody on the call was affected, which is not
//! what people expect a mute button to do.
//!
//! Muting at the OS level is the honest version of that promise, and it is the
//! first thing this app does that changes state **outside itself**. If the
//! process dies while muted — Task Manager, a crash, a power cut — the user's
//! microphone stays muted system-wide, and days later a call where nobody can
//! hear them has no visible connection to a meeting recorder used on Tuesday.
//!
//! Three things contain that, none of which eliminate it:
//!
//! * the mute is taken **only while a recording is running**, so the window is
//!   minutes with the app in front of the user rather than hours in a tray;
//! * it is released on stop, on cancel and on quit, by the session's `Drop`
//!   rather than by remembering to call something;
//! * `Config::system_mic_muted` is written to disk **before** the device is
//!   touched, so the next launch can undo it. See `restore_after_crash`.
//!
//! What remains uncovered is a crash followed by never opening the app again.
//! That is stated plainly rather than designed around.
//!
//! # The recording still writes its own silence
//!
//! `write_unit` keeps zeroing samples. There is a gap between asking the OS to
//! mute and the device going quiet, the call can fail on a device that does not
//! support it, and the recording must be silent across both.

#[derive(Debug)]
pub enum MuteError {
    /// No device of that name is available to mute.
    NoSuchDevice(String),
    /// The device exists but will not take a mute setting — common for
    /// aggregate devices and some virtual inputs.
    Unsupported(String),
    /// The OS refused the call.
    Failed(String),
}

impl std::fmt::Display for MuteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSuchDevice(name) => write!(f, "no input device named \"{name}\""),
            Self::Unsupported(name) => write!(f, "\"{name}\" does not support muting"),
            Self::Failed(e) => write!(f, "{e}"),
        }
    }
}

/// Mute or unmute the named capture device for the whole system.
///
/// `name` came from `Config`, so it is matched with
/// [`meeting_core::config::match_saved_device_name`]: exact, then
/// case-insensitive, then tolerant of the 31-character truncation and the
/// loopback decoration Windows applies. That helper shipped with a full test
/// suite and no callers; this is the one place whose job is exactly what it
/// does.
pub fn set(name: &str, muted: bool) -> Result<(), MuteError> {
    platform::set(name, muted)
}

/// Whether the named capture device is muted system-wide right now.
///
/// Used to make the restore after a crash a no-op when the user has already
/// unmuted themselves, and to turn "the call returned Ok" into "the OS agrees"
/// in the live test — a distinction this module cannot afford to blur.
pub fn is_muted(name: &str) -> Result<bool, MuteError> {
    platform::is_muted(name)
}

/// Whether this build can mute at all. Used to decide what the UI promises.
pub fn is_supported() -> bool {
    cfg!(any(target_os = "macos", target_os = "windows"))
}

// ---------------------------------------------------------------- macOS

#[cfg(target_os = "macos")]
mod platform {
    use super::MuteError;
    use objc2_core_audio::{
        kAudioDevicePropertyMute, kAudioHardwarePropertyDevices, kAudioObjectPropertyElementMain,
        kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyScopeInput,
        kAudioObjectSystemObject, AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize,
        AudioObjectID, AudioObjectIsPropertySettable, AudioObjectPropertyAddress,
        AudioObjectSetPropertyData,
    };
    use std::ffi::c_void;
    use std::ptr::NonNull;

    fn address(selector: u32, scope: u32) -> AudioObjectPropertyAddress {
        AudioObjectPropertyAddress {
            mSelector: selector,
            mScope: scope,
            mElement: kAudioObjectPropertyElementMain,
        }
    }

    /// Every audio object the system knows about.
    fn all_devices() -> Vec<AudioObjectID> {
        let mut addr = address(kAudioHardwarePropertyDevices, kAudioObjectPropertyScopeGlobal);
        let mut size: u32 = 0;

        // SAFETY: the address outlives the call, and `size` is a live u32.
        let status = unsafe {
            AudioObjectGetPropertyDataSize(
                kAudioObjectSystemObject as AudioObjectID,
                NonNull::from(&mut addr),
                0,
                std::ptr::null(),
                NonNull::from(&mut size),
            )
        };
        if status != 0 || size == 0 {
            return Vec::new();
        }

        let count = size as usize / std::mem::size_of::<AudioObjectID>();
        let mut ids: Vec<AudioObjectID> = vec![0; count];

        // SAFETY: `ids` has room for exactly `size` bytes, which is what the
        // call above reported and what is passed back in.
        let status = unsafe {
            AudioObjectGetPropertyData(
                kAudioObjectSystemObject as AudioObjectID,
                NonNull::from(&mut addr),
                0,
                std::ptr::null(),
                NonNull::from(&mut size),
                NonNull::new_unchecked(ids.as_mut_ptr() as *mut c_void),
            )
        };
        if status != 0 {
            return Vec::new();
        }

        ids
    }

    /// The device's name, as Core Audio reports it.
    fn name_of(id: AudioObjectID) -> Option<String> {
        // 0x6c6e616d is 'lnam', kAudioObjectPropertyName. Spelled out rather
        // than imported because the constant lives behind a different module
        // in this binding than the rest of what is used here.
        let mut addr = address(0x6c6e616d, kAudioObjectPropertyScopeGlobal);
        let mut size = std::mem::size_of::<*const c_void>() as u32;
        let mut cfstring: *const c_void = std::ptr::null();

        // SAFETY: asking for one CFStringRef into a slot sized for one pointer.
        let status = unsafe {
            AudioObjectGetPropertyData(
                id,
                NonNull::from(&mut addr),
                0,
                std::ptr::null(),
                NonNull::from(&mut size),
                NonNull::new_unchecked(&mut cfstring as *mut _ as *mut c_void),
            )
        };
        if status != 0 || cfstring.is_null() {
            return None;
        }

        // SAFETY: Core Audio hands back a **+1** CFString — this code owns a
        // reference to it. `CFRetained::from_raw` takes that ownership and
        // releases it on drop, so this neither leaks nor over-releases.
        let name = unsafe {
            let ptr = NonNull::new(cfstring as *mut objc2_core_foundation::CFString)?;
            objc2_core_foundation::CFRetained::from_raw(ptr).to_string()
        };
        Some(name)
    }

    /// The device id whose name matches `name`, by the shared matching rules.
    fn find(name: &str) -> Result<AudioObjectID, MuteError> {
        let devices: Vec<(AudioObjectID, String)> = all_devices()
            .into_iter()
            .filter_map(|id| name_of(id).map(|n| (id, n)))
            .collect();

        let names: Vec<String> = devices.iter().map(|(_, n)| n.clone()).collect();
        meeting_core::config::match_saved_device_name(name, &names)
            .map(|i| devices[i].0)
            .ok_or_else(|| MuteError::NoSuchDevice(name.to_string()))
    }

    pub fn is_muted(name: &str) -> Result<bool, MuteError> {
        let target = find(name)?;
        let mut addr = address(kAudioDevicePropertyMute, kAudioObjectPropertyScopeInput);
        let mut value: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;

        // SAFETY: one UInt32 out, into a slot sized for one UInt32.
        let status = unsafe {
            AudioObjectGetPropertyData(
                target,
                NonNull::from(&mut addr),
                0,
                std::ptr::null(),
                NonNull::from(&mut size),
                NonNull::new_unchecked(&mut value as *mut u32 as *mut c_void),
            )
        };

        if status == 0 {
            Ok(value != 0)
        } else {
            Err(MuteError::Unsupported(name.to_string()))
        }
    }

    pub fn set(name: &str, muted: bool) -> Result<(), MuteError> {
        let target = find(name)?;

        let mut addr = address(kAudioDevicePropertyMute, kAudioObjectPropertyScopeInput);

        // Ask before telling. Many inputs — aggregate devices especially —
        // carry the property but refuse to set it, and the failure is an
        // opaque OSStatus that would be reported as "the OS refused".
        // Core Foundation's `Boolean` is a `u8`, not Rust's `bool`; the two are
        // not interchangeable across the FFI boundary.
        let mut settable: u8 = 0;
        // SAFETY: both pointers are live for the call.
        let status = unsafe {
            AudioObjectIsPropertySettable(
                target,
                NonNull::from(&mut addr),
                NonNull::from(&mut settable),
            )
        };
        if status != 0 || settable == 0 {
            return Err(MuteError::Unsupported(name.to_string()));
        }

        let value: u32 = u32::from(muted);
        // SAFETY: the property takes a single UInt32, which is what is passed.
        let status = unsafe {
            AudioObjectSetPropertyData(
                target,
                NonNull::from(&mut addr),
                0,
                std::ptr::null(),
                std::mem::size_of::<u32>() as u32,
                NonNull::new_unchecked(&value as *const u32 as *mut c_void),
            )
        };

        if status == 0 {
            Ok(())
        } else {
            Err(MuteError::Failed(format!("Core Audio returned {status}")))
        }
    }
}

// -------------------------------------------------------------- Windows

#[cfg(target_os = "windows")]
mod platform {
    use super::MuteError;
    use windows::core::GUID;
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    use windows::Win32::Media::Audio::{eCapture, IMMDeviceEnumerator, MMDeviceEnumerator, DEVICE_STATE_ACTIVE};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ,
    };
    // `Win32::Foundation`, not `UI::Shell::PropertiesSystem` where the rest of
    // the property-store API lives. Wrong in the first draft, and caught by
    // compiling this module for a Windows target from the Mac.
    use windows::Win32::Foundation::PROPERTYKEY;

    /// `PKEY_Device_FriendlyName` — the name the user sees in Sound settings,
    /// and the one stored in `Config`.
    const PKEY_DEVICE_FRIENDLY_NAME: PROPERTYKEY = PROPERTYKEY {
        fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
        pid: 14,
    };

    pub fn is_muted(name: &str) -> Result<bool, MuteError> {
        with_endpoint(name, |volume| {
            // SAFETY: the interface is live for the closure.
            unsafe { volume.GetMute() }
                .map(|b| b.as_bool())
                .map_err(|e| MuteError::Failed(e.to_string()))
        })
    }

    pub fn set(name: &str, muted: bool) -> Result<(), MuteError> {
        with_endpoint(name, |volume| {
            // SAFETY: as above; the null context means "no client to exclude
            // from the change notification", which is what we want.
            unsafe { volume.SetMute(muted, std::ptr::null()) }
                .map_err(|e| MuteError::Failed(e.to_string()))
        })
    }

    /// Find the capture endpoint named `name` and hand its volume interface to
    /// `f`. One lookup, so the getter and the setter cannot disagree about
    /// which device they mean.
    fn with_endpoint<T>(
        name: &str,
        f: impl FnOnce(&IAudioEndpointVolume) -> Result<T, MuteError>,
    ) -> Result<T, MuteError> {
        // SAFETY: initialising COM for this thread. A repeat call on an
        // already-initialised thread returns S_FALSE, which is not an error and
        // is why the result is discarded rather than checked.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }

        // SAFETY: each call below is a documented COM call whose arguments are
        // live for its duration; failures come back as `Err`, not as UB.
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .map_err(|e| MuteError::Failed(e.to_string()))?;

            let collection = enumerator
                .EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)
                .map_err(|e| MuteError::Failed(e.to_string()))?;

            let count = collection
                .GetCount()
                .map_err(|e| MuteError::Failed(e.to_string()))?;

            // Collect the names first, then let the shared matcher choose, so
            // the same rules apply here as on macOS instead of two hand-rolled
            // comparisons drifting apart.
            let mut endpoints: Vec<(u32, String)> = Vec::new();
            for i in 0..count {
                let Ok(device) = collection.Item(i) else {
                    continue;
                };
                let Ok(store) = device.OpenPropertyStore(STGM_READ) else {
                    continue;
                };
                let Ok(value) = store.GetValue(&PKEY_DEVICE_FRIENDLY_NAME) else {
                    continue;
                };
                endpoints.push((i, value.to_string()));
            }

            let names: Vec<String> = endpoints.iter().map(|(_, n)| n.clone()).collect();
            let Some(index) = meeting_core::config::match_saved_device_name(name, &names) else {
                return Err(MuteError::NoSuchDevice(name.to_string()));
            };

            let device = collection
                .Item(endpoints[index].0)
                .map_err(|e| MuteError::Failed(e.to_string()))?;

            let volume: IAudioEndpointVolume = device
                .Activate(CLSCTX_ALL, None)
                .map_err(|e| MuteError::Unsupported(format!("{name}: {e}")))?;

            f(&volume)
        }
    }
}

// ---------------------------------------------------------------- other

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod platform {
    use super::MuteError;

    pub fn set(name: &str, _muted: bool) -> Result<(), MuteError> {
        Err(MuteError::Unsupported(name.to_string()))
    }

    pub fn is_muted(name: &str) -> Result<bool, MuteError> {
        Err(MuteError::Unsupported(name.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_device_is_reported_by_name() {
        // Not a platform test: whatever the OS does, asking to mute something
        // that is not there must name what was not found, because that string
        // is what reaches the log and the user.
        let err = set("No Such Microphone 12345", true).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("No Such Microphone 12345"),
            "the error must name the device: {message}"
        );
    }

    #[test]
    fn muting_is_available_on_the_platforms_that_ship() {
        assert!(is_supported(), "both shipped platforms can mute");
    }
}

#[cfg(test)]
mod live {
    use super::*;

    /// Mute a real device and read the state back from the OS.
    ///
    /// Ignored by default: it changes global state, so it has no business in a
    /// build gate. It exists because every claim in this module is a claim
    /// about the OS, and the only way to check one is to make it.
    ///
    /// ```text
    /// MUTE_TEST_DEVICE="MacBook Air Microphone" cargo test -p meeting-assistant \
    ///     --lib audio::system_mute::live -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "changes the system mute state; set MUTE_TEST_DEVICE"]
    fn muting_a_real_device_takes_effect() {
        let device = std::env::var("MUTE_TEST_DEVICE").expect("set MUTE_TEST_DEVICE");

        let before = is_muted(&device).expect("read the mute state");
        println!("before   muted={before}");

        set(&device, true).expect("mute");
        assert!(
            is_muted(&device).expect("read back"),
            "the OS must agree that it is muted, not merely accept the call"
        );
        println!("muted    the OS agrees");

        set(&device, false).expect("unmute");
        assert!(!is_muted(&device).expect("read back"), "must unmute");
        println!("unmuted  the OS agrees");

        // Leave the machine as it was found, whatever that was.
        set(&device, before).expect("restore");
    }

    /// The child half of the crash test: mute, then die without unwinding.
    ///
    /// Runs only when the parent asks, because its whole job is to kill the test
    /// process. `process::abort` rather than a panic: a panic would unwind and
    /// run `Drop`, which is the exact thing this has to prevent.
    #[test]
    #[ignore = "child of crash_while_muted_leaves_the_device_muted; kills its own process"]
    fn dies_while_holding_the_mute() {
        if std::env::var("MUTE_TEST_DIE").is_err() {
            return;
        }
        let device = std::env::var("MUTE_TEST_DEVICE").expect("set MUTE_TEST_DEVICE");
        let flag = std::env::var("MUTE_TEST_FLAG").expect("set MUTE_TEST_FLAG");

        let guard = MuteGuard::acquire(&device, move |d| {
            // Stands in for `Config::system_mic_muted`: written before the
            // device is touched, cleared on release. The real one goes through
            // `AppState::remember_system_mute`, which this crate cannot build
            // inside a test.
            match d {
                Some(name) => std::fs::write(&flag, name).expect("write the flag"),
                None => {
                    std::fs::remove_file(&flag).ok();
                }
            }
        })
        .expect("acquire");

        assert!(is_muted(&device).expect("read back"), "the child must mute");

        // Not `drop(guard)`, not a panic, not an early return: a hard kill.
        std::mem::forget(guard);
        std::process::abort();
    }

    /// The crash path, by actually crashing.
    ///
    /// The plan called for `kill -9` on the running app. This is the same event
    /// with the GUI removed: a process holding a live `MuteGuard` dies without
    /// running destructors, and what has to be true afterwards is that the
    /// device is still muted, the flag on disk says which device, and the next
    /// launch's `restore_after_crash` puts it back.
    ///
    /// Worth having as a test rather than a manual run because it is the one
    /// path no ordinary use exercises, and the one the user was rightly worried
    /// about.
    #[test]
    #[ignore = "changes the system mute state; set MUTE_TEST_DEVICE"]
    fn crash_while_muted_leaves_the_device_muted() {
        let device = std::env::var("MUTE_TEST_DEVICE").expect("set MUTE_TEST_DEVICE");
        let before = is_muted(&device).expect("read the mute state");

        let flag = std::env::temp_dir().join(format!("ma-mute-flag-{}", std::process::id()));
        std::fs::remove_file(&flag).ok();

        let status = std::process::Command::new(std::env::current_exe().expect("current exe"))
            .args([
                "--exact",
                "audio::system_mute::live::dies_while_holding_the_mute",
                "--ignored",
                "--nocapture",
            ])
            .env("MUTE_TEST_DIE", "1")
            .env("MUTE_TEST_DEVICE", &device)
            .env("MUTE_TEST_FLAG", &flag)
            .status()
            .expect("spawn the child");

        assert!(
            !status.success(),
            "the child was supposed to abort, not exit cleanly"
        );
        println!("crashed  child died with {status}");

        // The state a user would find: microphone dead, and a record of it.
        assert!(
            is_muted(&device).expect("read back"),
            "a hard kill must leave the device muted — otherwise there is nothing to restore"
        );
        assert_eq!(
            std::fs::read_to_string(&flag).expect("the flag must survive the crash"),
            device,
            "the flag must name the device, since the configured microphone may change"
        );
        println!("stranded the OS agrees, and the flag names the device");

        // What the next launch does.
        assert!(
            restore_after_crash(&device),
            "the restore must report that it acted"
        );
        assert!(
            !is_muted(&device).expect("read back"),
            "the next launch must unmute"
        );
        println!("restored the OS agrees");

        // And is a no-op the second time, which is what protects a user who has
        // already unmuted themselves from being overridden.
        assert!(
            !restore_after_crash(&device),
            "nothing to undo must report nothing done"
        );

        std::fs::remove_file(&flag).ok();
        set(&device, before).expect("restore");
    }

    /// The guard hands the microphone back when it is dropped.
    ///
    /// This is the property the whole design rests on: every way out of a
    /// recording drops the session, and dropping the session must unmute. A
    /// test that only checked `set` would not have covered it.
    #[test]
    #[ignore = "changes the system mute state; set MUTE_TEST_DEVICE"]
    fn dropping_the_guard_unmutes() {
        let device = std::env::var("MUTE_TEST_DEVICE").expect("set MUTE_TEST_DEVICE");
        let before = is_muted(&device).expect("read the mute state");

        let remembered: std::sync::Arc<std::sync::Mutex<Vec<Option<String>>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = std::sync::Arc::clone(&remembered);

        {
            let _guard = MuteGuard::acquire(&device, move |d| {
                sink.lock().unwrap().push(d.map(str::to_string));
            })
            .expect("acquire");

            assert!(is_muted(&device).expect("read back"), "the guard must mute");
            println!("held     the OS agrees");
        }

        assert!(
            !is_muted(&device).expect("read back"),
            "dropping the guard must unmute"
        );
        println!("dropped  the OS agrees");

        // The flag is written before the device is touched and cleared after it
        // is released, so the file is never more optimistic than the device.
        let calls = remembered.lock().unwrap().clone();
        assert_eq!(
            calls,
            vec![Some(device.clone()), None],
            "the flag must be set before muting and cleared after releasing"
        );

        set(&device, before).expect("restore");
    }
}

/// Holds a system-wide mute for as long as this value lives.
///
/// # Why a guard and not "remember to unmute"
///
/// The same reasoning as `queue::RecordingHold`, which this copies. Every way
/// out of a recording has to release the mute: the `?` returns in
/// `stop_recording` and `cancel_recording`, an early return, a panic in
/// whichever thread owns the session. Written as a rule to follow, that is one
/// refactor away from leaving a microphone dead.
///
/// Owned by `RecordingSession`, so the mute's lifetime **is** the recording's.
///
/// Taking the guard is what writes `Config::system_mic_muted`; dropping it is
/// what clears it. The file is therefore never more optimistic than the device:
/// it is written before the mute is taken and cleared after it is released, so
/// a crash at any point leaves a flag that says "possibly muted", which the next
/// launch verifies rather than trusts.
/// How the guard reports what it is holding, so this module needs to know
/// nothing about `Config`. Called with the device name before the mute is
/// taken, and with `None` once it has been released.
type Remember = Box<dyn Fn(Option<&str>) + Send + Sync>;

pub struct MuteGuard {
    device: String,
    on_release: Remember,
}

impl MuteGuard {
    /// Mute `device` system-wide, and record that we did.
    ///
    /// `remember` is called with the device name before the device is touched,
    /// and with `None` once it has been released — the app supplies a closure
    /// that persists it, so this module needs to know nothing about `Config`.
    ///
    /// Returns `Err` with the device left alone if the OS refuses; the caller
    /// falls back to silencing the recording only.
    pub fn acquire(
        device: &str,
        remember: impl Fn(Option<&str>) + Send + Sync + 'static,
    ) -> Result<Self, MuteError> {
        // Written first. A flag claiming a mute that was never taken costs one
        // redundant check at the next launch; a mute with no flag is a
        // microphone nobody knows to restore.
        remember(Some(device));

        match set(device, true) {
            Ok(()) => Ok(Self {
                device: device.to_string(),
                on_release: Box::new(remember),
            }),
            Err(e) => {
                remember(None);
                Err(e)
            }
        }
    }

    /// Which device this guard is holding muted.
    ///
    /// Load-bearing, not a convenience: the recording can change microphones
    /// underneath it — the device-follow path switches on a disconnect — and a
    /// mute held on the device that went away is a mute nobody can hear the
    /// effect of. The session compares this against the device now capturing.
    pub fn device(&self) -> &str {
        &self.device
    }
}

impl Drop for MuteGuard {
    fn drop(&mut self) {
        let _ = set(&self.device, false);
        (self.on_release)(None);
    }
}

/// Undo a mute left behind by a previous run.
///
/// Checks before acting: someone who has already unmuted themselves — from
/// Sound settings, from Control Center, from the headset's own button — should
/// not have the app assert itself over that. Returns whether it actually
/// unmuted anything, for the log line.
pub fn restore_after_crash(device: &str) -> bool {
    match is_muted(device) {
        Ok(true) => set(device, false).is_ok(),
        _ => false,
    }
}
