"""
Pure audio-device selection policy for Meeting Assistant.

No GUI or audio-backend dependencies are imported here, so the selection
logic can be tested without real hardware.
"""

INTERNAL_MIC_TERMS = (
    "microphone array",
    "matriz de micrófonos",
    "matriz de microfonos",
    "realtek",
    "intel smart sound",
    "intel",
    "internal",
    "integrated",
)

DEFAULT_SYSTEM_PREFERRED_TERMS = (
    "plantronics blackwire 3225 series",
)


def _unique_by_index(devices):
    result = []
    seen = set()

    for device in devices:
        if not device:
            continue

        index = device.get("index")
        if index in seen:
            continue

        seen.add(index)
        result.append(device)

    return result


def _find_by_name(devices, name):
    if not name:
        return None

    return next(
        (
            device
            for device in devices
            if device.get("name") == name
        ),
        None,
    )


def _find_by_index(devices, index):
    if index is None:
        return None

    return next(
        (
            device
            for device in devices
            if device.get("index") == index
        ),
        None,
    )


def ordered_microphone_candidates(
    devices,
    selected_name="",
    default_index=None,
    internal_terms=INTERNAL_MIC_TERMS,
):
    devices = list(devices)
    ordered = [
        _find_by_name(devices, selected_name),
        _find_by_index(devices, default_index),
    ]

    for term in internal_terms:
        term = term.lower()

        for device in devices:
            if term in str(
                device.get("name", "")
            ).lower():
                ordered.append(device)

    ordered.extend(devices)
    return _unique_by_index(ordered)


def ordered_system_candidates(
    devices,
    selected_name="",
    default_index=None,
    preferred_terms=DEFAULT_SYSTEM_PREFERRED_TERMS,
):
    devices = list(devices)
    ordered = [
        _find_by_name(devices, selected_name),
        _find_by_index(devices, default_index),
    ]

    for term in preferred_terms:
        term = term.lower()

        for device in devices:
            if term in str(
                device.get("name", "")
            ).lower():
                ordered.append(device)

    ordered.extend(devices)
    return _unique_by_index(ordered)


def choose_microphone_failover(
    devices,
    current_name="",
    configured_name="",
    default_index=None,
):
    devices = list(devices)

    current = _find_by_name(
        devices,
        current_name,
    )

    if current is not None:
        return current, False

    candidates = ordered_microphone_candidates(
        devices,
        selected_name=configured_name,
        default_index=default_index,
    )

    if not candidates:
        return None, current_name != ""

    return candidates[0], True


def choose_system_failover(
    devices,
    current_name="",
    configured_name="",
    default_index=None,
    preferred_terms=DEFAULT_SYSTEM_PREFERRED_TERMS,
):
    devices = list(devices)

    current = _find_by_name(
        devices,
        current_name,
    )

    if current is not None:
        return current, False

    candidates = ordered_system_candidates(
        devices,
        selected_name=configured_name,
        default_index=default_index,
        preferred_terms=preferred_terms,
    )

    if not candidates:
        return None, current_name != ""

    return candidates[0], True


def silence_frames_for_gap(
    gap_seconds,
    sample_rate,
):
    gap_seconds = max(
        0.0,
        float(gap_seconds),
    )
    sample_rate = max(
        1,
        int(sample_rate),
    )

    return int(
        round(
            gap_seconds * sample_rate
        )
    )
