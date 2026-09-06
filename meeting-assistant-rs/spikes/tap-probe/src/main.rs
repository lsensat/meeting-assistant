//! Diagnostic for the macOS Core Audio process tap.
//!
//! # Why this exists
//!
//! `loopback-probe` on macOS reports **zero frames, zero callbacks and zero
//! stream errors** when opening the default output device as an input stream —
//! both as a bare CLI binary and from inside an ad-hoc signed `.app` bundle
//! carrying `NSAudioCaptureUsageDescription`, and with audio actively playing.
//! Nothing appears in the unified log, so TCC is apparently never consulted.
//!
//! cpal absorbs the intermediate state: by the time you have a `Stream`, the
//! tap's `OSStatus`, the tap's negotiated format and the aggregate device's
//! channel count have all been discarded. That makes "zero callbacks" the only
//! observable, and it is consistent with several very different causes:
//!
//! * the tap was created but TCC silently gives it no audio;
//! * the tap has a valid format but the aggregate exposes **zero input
//!   channels**, so the audio unit has nothing to pull and never fires;
//! * the tap's format disagrees with what cpal set on the audio unit.
//!
//! This probe performs the same sequence cpal does, but prints the status and
//! format at every step so the three cases can be told apart. Whatever it
//! finds, this code is the starting point for `audio/macos_tap.rs` (fallback
//! F1 in the plan), which has to build the tap by hand anyway.
//!
//! # Usage
//!
//! ```text
//! cargo run            # tap the default output device
//! ```

use std::ffi::{c_void, CStr};
use std::mem::MaybeUninit;
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::AnyThread;
use objc2_core_audio::{
    kAudioAggregateDeviceNameKey, kAudioAggregateDeviceTapAutoStartKey,
    kAudioAggregateDeviceTapListKey, kAudioAggregateDeviceUIDKey, kAudioDevicePropertyDeviceUID,
    kAudioDevicePropertyStreamConfiguration, kAudioEndPointDeviceIsPrivateKey,
    kAudioHardwarePropertyDefaultOutputDevice, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyScopeInput, kAudioSubTapDriftCompensationKey,
    kAudioSubTapUIDKey, kAudioTapPropertyFormat, AudioHardwareCreateAggregateDevice,
    AudioHardwareCreateProcessTap, AudioHardwareDestroyAggregateDevice,
    AudioHardwareDestroyProcessTap, AudioObjectGetPropertyData, AudioObjectID,
    AudioObjectPropertyAddress, CATapDescription, CATapMuteBehavior,
};
use objc2_core_audio_types::{AudioBufferList, AudioStreamBasicDescription};
use objc2_core_foundation::{
    kCFAllocatorDefault, kCFTypeArrayCallBacks, kCFTypeDictionaryKeyCallBacks,
    kCFTypeDictionaryValueCallBacks, CFArray, CFDictionary, CFMutableDictionary, CFRetained,
    CFString,
};
use objc2_foundation::{NSArray, NSNumber, NSString};

const SYSTEM_OBJECT: AudioObjectID = 1; // kAudioObjectSystemObject

fn main() {
    println!("== Core Audio process tap diagnostic ==\n");

    let output_device = match default_output_device() {
        Some(id) => id,
        None => {
            eprintln!("FAIL: could not read the default output device");
            std::process::exit(1);
        }
    };
    println!("default output device id : {output_device}");

    let uid = match device_uid(output_device) {
        Ok(uid) => uid,
        Err(status) => {
            eprintln!("FAIL: could not read device UID (OSStatus {status})");
            std::process::exit(1);
        }
    };
    println!("default output device UID: {uid}\n");

    // --- 1. Create the tap, exactly as cpal does -------------------------
    //
    // An empty process list combined with `setExclusive(true)` means "exclude
    // nothing", i.e. tap every process. This is the documented idiom, not an
    // oversight.
    let processes = NSArray::new();
    let tap_desc = unsafe {
        CATapDescription::initWithProcesses_andDeviceUID_withStream(
            CATapDescription::alloc(),
            &processes,
            &NSString::from_str(&uid),
            0,
        )
    };
    unsafe {
        tap_desc.setMuteBehavior(CATapMuteBehavior::Unmuted);
        tap_desc.setName(&NSString::from_str("meeting-assistant tap probe"));
        tap_desc.setPrivate(true);
        tap_desc.setExclusive(true);
    }

    let mut tap_id: MaybeUninit<AudioObjectID> = MaybeUninit::uninit();
    let status = unsafe { AudioHardwareCreateProcessTap(Some(tap_desc.as_ref()), tap_id.as_mut_ptr()) };
    println!("AudioHardwareCreateProcessTap -> OSStatus {status} ({})", describe(status));
    if status != 0 {
        eprintln!("\nFAIL: tap creation itself failed. This is a permission or API error,");
        eprintln!("      not a format problem.");
        std::process::exit(1);
    }
    let tap_id = unsafe { tap_id.assume_init() };
    println!("tap object id            : {tap_id}");

    // --- 2. What format did the tap actually negotiate? ------------------
    //
    // This is the number cpal throws away. A tap that TCC has silenced still
    // reports a plausible format, but a tap with **0 channels** explains zero
    // callbacks completely: the audio unit has nothing to pull.
    match tap_format(tap_id) {
        Ok(asbd) => {
            println!(
                "tap format               : {} Hz, {} ch, {} bits, format id {:#x}, flags {:#x}",
                asbd.mSampleRate,
                asbd.mChannelsPerFrame,
                asbd.mBitsPerChannel,
                asbd.mFormatID,
                asbd.mFormatFlags
            );
            if asbd.mChannelsPerFrame == 0 {
                println!("  >> ZERO CHANNELS. This alone explains zero callbacks.");
            }
        }
        Err(s) => println!("tap format               : <failed, OSStatus {s} ({})>", describe(s)),
    }

    // --- 3. Wrap it in an aggregate device -------------------------------
    let props = aggregate_properties(
        unsafe { tap_desc.UUID().UUIDString() },
        "com.meetingassistant.tapprobe.aggregate",
        "Meeting Assistant tap probe aggregate",
    );
    let mut aggregate_id: AudioObjectID = 0;
    let status = unsafe {
        AudioHardwareCreateAggregateDevice(props.as_ref(), NonNull::from(&mut aggregate_id))
    };
    println!("\nAudioHardwareCreateAggregateDevice -> OSStatus {status} ({})", describe(status));
    if status == 0 {
        println!("aggregate device id      : {aggregate_id}");

        // --- 4. The decisive number: input channels on the aggregate -----
        match input_channel_count(aggregate_id) {
            Ok((buffers, channels)) => {
                println!("aggregate input streams  : {buffers} buffer(s), {channels} channel(s)");
                if channels == 0 {
                    println!("  >> ZERO INPUT CHANNELS on the aggregate.");
                    println!("  >> cpal builds its input AudioUnit on this device, so it can never");
                    println!("  >> receive data. This is the direct cause of 0 callbacks / 0 errors.");
                } else {
                    println!("  >> Aggregate looks healthy. If cpal still gets no callbacks, the");
                    println!("  >> fault is in the AudioUnit format negotiation, not the tap.");
                }
            }
            Err(s) => println!("aggregate input streams  : <failed, OSStatus {s} ({})>", describe(s)),
        }

        unsafe { AudioHardwareDestroyAggregateDevice(aggregate_id) };
    }

    unsafe { AudioHardwareDestroyProcessTap(tap_id) };
    println!("\ncleaned up tap and aggregate.");
}

/// Decode the four-character OSStatus codes Core Audio actually returns.
fn describe(status: i32) -> String {
    match status {
        0 => "ok".into(),
        560947818 => "!obj — kAudioHardwareBadObjectError".into(),
        561211770 => "!dev — kAudioHardwareBadDeviceError".into(),
        1852797029 => "nope — kAudioHardwareIllegalOperationError".into(),
        561015905 => "!dat — kAudioHardwareUnspecifiedError".into(),
        2003332927 => "who? — kAudioHardwareUnknownPropertyError".into(),
        1937010544 => "!siz — kAudioHardwareBadPropertySizeError".into(),
        -1 => "kAudioHardwareNotRunningError".into(),
        other => {
            // Core Audio errors are usually packed four-char codes.
            let bytes = (other as u32).to_be_bytes();
            if bytes.iter().all(|b| b.is_ascii_graphic()) {
                format!("'{}'", String::from_utf8_lossy(&bytes))
            } else {
                "unknown".into()
            }
        }
    }
}

fn default_output_device() -> Option<AudioObjectID> {
    let address = AudioObjectPropertyAddress {
        mSelector: kAudioHardwarePropertyDefaultOutputDevice,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut id: AudioObjectID = 0;
    let mut size = size_of::<AudioObjectID>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            SYSTEM_OBJECT,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut id).cast(),
        )
    };
    (status == 0 && id != 0).then_some(id)
}

fn device_uid(device: AudioObjectID) -> Result<String, i32> {
    let address = AudioObjectPropertyAddress {
        mSelector: kAudioDevicePropertyDeviceUID,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut cfstring: *const CFString = std::ptr::null();
    let mut size = size_of::<*const CFString>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            device,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut cfstring).cast(),
        )
    };
    if status != 0 || cfstring.is_null() {
        return Err(status);
    }
    Ok(unsafe { &*cfstring }.to_string())
}

fn tap_format(tap: AudioObjectID) -> Result<AudioStreamBasicDescription, i32> {
    let address = AudioObjectPropertyAddress {
        mSelector: kAudioTapPropertyFormat,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut asbd: AudioStreamBasicDescription = unsafe { std::mem::zeroed() };
    let mut size = size_of::<AudioStreamBasicDescription>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            tap,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut asbd).cast(),
        )
    };
    if status == 0 {
        Ok(asbd)
    } else {
        Err(status)
    }
}

/// Returns (buffer count, total channels) on the device's **input** scope.
fn input_channel_count(device: AudioObjectID) -> Result<(u32, u32), i32> {
    let address = AudioObjectPropertyAddress {
        mSelector: kAudioDevicePropertyStreamConfiguration,
        mScope: kAudioObjectPropertyScopeInput,
        mElement: kAudioObjectPropertyElementMain,
    };

    // AudioBufferList is variable-length; ask for the size first.
    let mut size: u32 = 0;
    let status = unsafe {
        objc2_core_audio::AudioObjectGetPropertyDataSize(
            device,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
        )
    };
    if status != 0 {
        return Err(status);
    }

    let mut buffer = vec![0u8; size as usize];
    let status = unsafe {
        AudioObjectGetPropertyData(
            device,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::new(buffer.as_mut_ptr().cast::<c_void>()).unwrap(),
        )
    };
    if status != 0 {
        return Err(status);
    }

    let list = unsafe { &*(buffer.as_ptr() as *const AudioBufferList) };
    let count = list.mNumberBuffers;
    let buffers =
        unsafe { std::slice::from_raw_parts(list.mBuffers.as_ptr(), count as usize) };
    let channels = buffers.iter().map(|b| b.mNumberChannels).sum();
    Ok((count, channels))
}

fn to_cfstring(cstr: &'static CStr) -> CFRetained<CFString> {
    unsafe { CFString::with_c_string(kCFAllocatorDefault, cstr.as_ptr(), 0x0800_0100) }.unwrap()
}

/// Byte-for-byte the same dictionary cpal builds in its `loopback.rs`.
fn aggregate_properties(
    tap_uid: Retained<NSString>,
    agg_uid: &str,
    agg_name: &str,
) -> CFRetained<CFDictionary> {
    unsafe {
        let tap_inner = CFMutableDictionary::new(
            kCFAllocatorDefault,
            2,
            &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks,
        )
        .unwrap();
        CFMutableDictionary::set_value(
            Some(tap_inner.as_ref()),
            &*to_cfstring(kAudioSubTapUIDKey) as *const _ as *const c_void,
            &*tap_uid as *const _ as *const c_void,
        );
        CFMutableDictionary::set_value(
            Some(tap_inner.as_ref()),
            &*to_cfstring(kAudioSubTapDriftCompensationKey) as *const _ as *const c_void,
            &*NSNumber::initWithBool(NSNumber::alloc(), true) as *const _ as *const c_void,
        );

        let taps_list = [tap_inner];
        let taps = CFArray::new(
            kCFAllocatorDefault,
            taps_list.as_ptr() as *mut *const c_void,
            taps_list.len() as _,
            &kCFTypeArrayCallBacks,
        )
        .unwrap();

        let dict = CFMutableDictionary::new(
            kCFAllocatorDefault,
            5,
            &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks,
        )
        .unwrap();
        CFMutableDictionary::set_value(
            Some(dict.as_ref()),
            &*to_cfstring(kAudioAggregateDeviceNameKey) as *const _ as *const c_void,
            &*CFString::from_str(agg_name) as *const _ as *const c_void,
        );
        CFMutableDictionary::set_value(
            Some(dict.as_ref()),
            &*to_cfstring(kAudioAggregateDeviceUIDKey) as *const _ as *const c_void,
            &*CFString::from_str(agg_uid) as *const _ as *const c_void,
        );
        CFMutableDictionary::set_value(
            Some(dict.as_ref()),
            &*to_cfstring(kAudioAggregateDeviceTapListKey) as *const _ as *const c_void,
            &*taps as *const _ as *const c_void,
        );
        CFMutableDictionary::set_value(
            Some(dict.as_ref()),
            &*to_cfstring(kAudioAggregateDeviceTapAutoStartKey) as *const _ as *const c_void,
            &*NSNumber::initWithBool(NSNumber::alloc(), true) as *const _ as *const c_void,
        );
        CFMutableDictionary::set_value(
            Some(dict.as_ref()),
            &*to_cfstring(kAudioEndPointDeviceIsPrivateKey) as *const _ as *const c_void,
            &*NSNumber::initWithBool(NSNumber::alloc(), true) as *const _ as *const c_void,
        );

        CFRetained::cast_unchecked::<CFDictionary>(dict)
    }
}
