from audio_policy import (
    choose_microphone_failover,
    choose_system_failover,
    ordered_microphone_candidates,
    ordered_system_candidates,
    silence_frames_for_gap,
)


def dev(index, name):
    return {
        "index": index,
        "name": name,
        "samplerate": 48000,
        "channels": 2,
    }


def test_selected_microphone_wins():
    devices = [
        dev(1, "Microphone Array (Intel)"),
        dev(2, "Plantronics Blackwire 3225 Series"),
    ]
    result = ordered_microphone_candidates(
        devices,
        selected_name="Plantronics Blackwire 3225 Series",
        default_index=1,
    )
    assert result[0]["index"] == 2


def test_default_microphone_fallback():
    devices = [
        dev(1, "Microphone Array (Intel)"),
        dev(3, "USB Microphone"),
    ]
    result = ordered_microphone_candidates(
        devices,
        selected_name="Missing",
        default_index=1,
    )
    assert result[0]["index"] == 1


def test_internal_microphone_precedes_arbitrary_input():
    devices = [
        dev(3, "USB Microphone"),
        dev(4, "Microphone Array (Intel Smart Sound)"),
    ]
    result = ordered_microphone_candidates(
        devices,
        selected_name="Missing",
        default_index=None,
    )
    assert result[0]["index"] == 4


def test_microphone_candidates_are_unique():
    devices = [
        dev(1, "Microphone Array (Intel)"),
        dev(2, "Plantronics Blackwire 3225 Series"),
    ]
    result = ordered_microphone_candidates(
        devices,
        selected_name="Plantronics Blackwire 3225 Series",
        default_index=2,
    )
    assert [x["index"] for x in result].count(2) == 1


def test_selected_loopback_wins():
    devices = [
        dev(19, "Plantronics Blackwire 3225 Series [Loopback]"),
        dev(20, "Speakers (Realtek) [Loopback]"),
    ]
    result = ordered_system_candidates(
        devices,
        selected_name="Speakers (Realtek) [Loopback]",
    )
    assert result[0]["index"] == 20


def test_plantronics_is_default_preference_without_saved_loopback():
    devices = [
        dev(20, "Speakers (Realtek) [Loopback]"),
        dev(19, "Plantronics Blackwire 3225 Series [Loopback]"),
    ]
    result = ordered_system_candidates(devices)
    assert result[0]["index"] == 19


def test_supplied_default_loopback_can_win():
    devices = [
        dev(19, "Plantronics Blackwire 3225 Series [Loopback]"),
        dev(20, "Speakers (Realtek) [Loopback]"),
    ]
    result = ordered_system_candidates(
        devices,
        selected_name="Missing",
        default_index=20,
    )
    assert result[0]["index"] == 20


def test_runtime_mic_keeps_current_fallback_after_headset_returns():
    devices = [
        dev(1, "Microphone Array (Intel)"),
        dev(2, "Plantronics Blackwire 3225 Series"),
    ]
    chosen, changed = choose_microphone_failover(
        devices,
        current_name="Microphone Array (Intel)",
        configured_name="Plantronics Blackwire 3225 Series",
        default_index=1,
    )
    assert chosen["index"] == 1
    assert changed is False


def test_runtime_mic_switches_when_current_disappears():
    devices = [
        dev(1, "Microphone Array (Intel)"),
        dev(3, "USB Microphone"),
    ]
    chosen, changed = choose_microphone_failover(
        devices,
        current_name="Plantronics Blackwire 3225 Series",
        configured_name="Plantronics Blackwire 3225 Series",
        default_index=1,
    )
    assert chosen["index"] == 1
    assert changed is True


def test_runtime_system_switches_to_realtek():
    devices = [
        dev(20, "Speakers (Realtek) [Loopback]"),
    ]
    chosen, changed = choose_system_failover(
        devices,
        current_name="Plantronics Blackwire 3225 Series [Loopback]",
        configured_name="Plantronics Blackwire 3225 Series [Loopback]",
        default_index=20,
    )
    assert chosen["index"] == 20
    assert changed is True


def test_runtime_failover_handles_no_device():
    chosen, changed = choose_system_failover(
        [],
        current_name="Plantronics Blackwire 3225 Series [Loopback]",
    )
    assert chosen is None
    assert changed is True


def test_gap_to_silence_frames():
    assert silence_frames_for_gap(0.7, 48000) == 33600
    assert silence_frames_for_gap(-1, 48000) == 0
