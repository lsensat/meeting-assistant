import os
import json
import time
import wave
import queue
import threading
import re
import subprocess
import shutil
import tempfile
import base64
import io
import ctypes
from pathlib import Path
from datetime import datetime
import tkinter as tk
from tkinter import filedialog, messagebox, simpledialog

import customtkinter as ctk
import sounddevice as sd
import soundfile as sf
import pyaudiowpatch as pyaudio
import ollama
from PIL import Image

from faster_whisper import WhisperModel

from audio_policy import (
    choose_microphone_failover,
    choose_system_failover,
    ordered_microphone_candidates,
    ordered_system_candidates,
)
from audio_stream_utils import (
    TARGET_SAMPLE_RATE,
    pcm16_bytes_to_mono_float,
    resample_mono,
    silent_samples,
)
from progress_utils import transcription_percent

try:
    from huggingface_hub import scan_cache_dir
except Exception:
    scan_cache_dir = None


# ============================================================
# RUTAS Y CONFIGURACION
# ============================================================

APP_FOLDER = Path(__file__).resolve().parent
CONFIG_FILE = APP_FOLDER / "config.json"

WHISPER_REPOS = {
    "tiny": "Systran/faster-whisper-tiny",
    "base": "Systran/faster-whisper-base",
    "small": "Systran/faster-whisper-small",
    "medium": "Systran/faster-whisper-medium",
    "large-v3": "Systran/faster-whisper-large-v3",
}

DEFAULT_WHISPER_MODEL = "small"
WHISPER_LANGUAGE = None
CHUNK = 1024
HOTPLUG_CHECK_SECONDS = 1.0
HOTPLUG_RETRY_SECONDS = 0.25

SUMMARY_TYPE_IDS = [
    "meeting_minutes",
    "executive",
    "actions",
    "brief",
    "custom",
]

SUMMARY_TYPE_LABELS = {
    "meeting_minutes": {
        "en": "Meeting minutes",
        "es": "Acta de reunión",
    },
    "executive": {
        "en": "Executive summary",
        "es": "Resumen ejecutivo",
    },
    "actions": {
        "en": "Actions and decisions",
        "es": "Acciones y decisiones",
    },
    "brief": {
        "en": "Brief summary",
        "es": "Resumen breve",
    },
    "custom": {
        "en": "Custom",
        "es": "Personalizado",
    },
}

APP_LANGUAGE_LABELS = {
    "en": "English",
    "es": "Español",
}

TRANSCRIPTION_LANGUAGE_LABELS = {
    "auto": {
        "en": "Auto-detect",
        "es": "Detección automática",
    },
    "en": {
        "en": "English",
        "es": "Inglés",
    },
    "es": {
        "en": "Spanish",
        "es": "Español",
    },
}

TEXTS = {
    "en": {
        "settings": "Settings",
        "language": "Language",
        "application_language": "Application language",
        "transcription_language": "Transcription language",
        "processing": "Processing",
        "whisper_model": "Whisper model",
        "ai_model": "AI model",
        "summary_type": "Summary type",
        "audio": "Audio",
        "microphone": "Microphone",
        "computer_audio": "Computer audio",
        "update_devices": "Update devices",
        "files_privacy": "Files and privacy",
        "save_meetings_in": "Save meetings in",
        "keep_audio": "Keep audio after processing",
        "custom_prompt": "Custom prompt",
        "save_settings": "Save settings",

        "ready": "Ready",
        "ready_ollama_started": "Ready · Ollama started automatically",
        "checking_environment": "Checking environment...",
        "checking_ollama": "Checking Ollama...",
        "checking_folder": "Checking working folder...",
        "review_required": "Review required",
        "recording": "● Recording",
        "recording_muted": "● Recording · microphone muted",
        "finalizing_recording": "Finishing recording...",
        "saving_audio": "Saving audio...",
        "loading_whisper": "Loading Whisper {model}...",
        "downloading_whisper": "Downloading Whisper {model} for the first time...",
        "transcribing": "Transcribing {speaker}...",
        "transcribing_progress": "Transcribing {speaker}... {percent}%",
        "summarizing": "Summarizing with {model}...",
        "summary_block": "Summary: block {index}/{total} with {model}...",
        "generating_final_summary": "Generating final summary...",
        "processed_ok": "✓ Meeting processed successfully",
        "done": "DONE",
        "error_occurred": "An error occurred",

        "whisper_installed": "{model}  installed",
        "whisper_download": "{model}  · downloads when first used",
        "whisper_available": "Available locally",
        "whisper_first_use": "It will be downloaded the first time it is used",

        "ollama_unavailable": "Ollama unavailable",
        "ollama_not_responding": "Ollama is not responding",
        "ollama_no_models": "No models installed",
        "ollama_active_install": "Ollama active · install at least one model",
        "ollama_active_models": "Ollama active · {count} model(s)",
        "searching_models": "Searching models...",
        "searching_ollama": "Checking Ollama...",

        "microphone_loading": "Microphone: loading...",
        "computer_audio_loading": "Computer audio: loading...",
        "microphone_connecting": "Microphone: connecting...",
        "computer_audio_connecting": "Computer audio: connecting...",
        "automatic": "automatic",

        "stage_audio": "Audio",
        "stage_whisper": "Whisper",
        "stage_summary": "Summary",

        "tooltip_start": "Start meeting",
        "tooltip_stop": "Finish meeting",
        "tooltip_mute": "Mute / unmute microphone",
        "tooltip_transcript": "Open transcript",
        "tooltip_folder": "Open folder",
        "tooltip_settings": "Settings",
        "tooltip_refresh_models": "Refresh models",
        "tooltip_change_folder": "Change folder",

        "save_meeting_title": "Save meeting",
        "meeting_name_optional": "Meeting name (optional):",
        "custom_prompt_title": "Custom prompt",
        "custom_prompt_required": "Enter a custom prompt before saving.",

        "ollama_missing_title": "Ollama unavailable",
        "ollama_stopped": (
            "Ollama stopped after Meeting Assistant was opened. "
            "Close and reopen Meeting Assistant so it can start Ollama automatically."
        ),
        "ollama_model_title": "Ollama model",
        "ollama_model_missing": "No Ollama model is available to generate the summary.",
        "microphone_title": "Microphone",
        "microphone_missing": "Windows does not detect an available microphone.",
        "computer_audio_title": "Computer audio",
        "computer_audio_missing": "No computer-audio loopback device was found.",
        "meeting_audio_title": "Meeting audio",
        "meeting_audio_select": "Select a valid computer-audio device in Settings.",
        "folder_title": "Folder unavailable",
        "folder_unavailable": "The selected destination folder cannot be created or used.",

        "startup_title": "Startup check",
        "startup_no_models": "Ollama is running, but no model is installed.",
        "startup_bad_folder": "The destination folder cannot be used: {detail}",
        "startup_no_mic": "Windows does not detect an available microphone.",
        "startup_no_loopback": "No loopback device was found to capture computer audio.",

        "meeting_in_progress_title": "Meeting in progress",
        "meeting_in_progress_close": "Finish the meeting before closing the application.",
        "processing_title": "Processing",
        "processing_close": "Wait for processing to finish before closing the application.",

        "speaker_me": "ME",
        "speaker_meeting": "MEETING",

        "error_no_mic": "No microphone is available in Windows.",
        "error_no_mic_start": (
            "No microphone could be started. Meeting Assistant tried the selected "
            "device, the Windows default and the other available inputs."
        ),
        "error_no_system_selected": (
            "No meeting-audio device is selected. Open Settings and select one."
        ),
        "error_system_gone": "The selected computer-audio device is no longer available.",
        "error_system_capture": (
            "Computer audio could not be captured. Check that the selected device "
            "is the one you are using to hear the meeting."
        ),
        "error_audio_sources": "Both audio sources could not be started.",
        "error_whisper_load": (
            "Whisper could not be loaded. If the model was not downloaded, "
            "check the Internet connection and try again."
        ),
        "error_no_voice": (
            "Whisper did not detect speech. Check the audio devices and try again."
        ),
        "error_summary": (
            "The summary could not be generated. Check that Ollama is running "
            "and that the selected model is still available."
        ),
    },

    "es": {
        "settings": "Configuración",
        "language": "Idioma",
        "application_language": "Idioma de la aplicación",
        "transcription_language": "Idioma de transcripción",
        "processing": "Procesamiento",
        "whisper_model": "Modelo Whisper",
        "ai_model": "Modelo IA",
        "summary_type": "Tipo de resumen",
        "audio": "Audio",
        "microphone": "Micrófono",
        "computer_audio": "Audio del PC",
        "update_devices": "Actualizar dispositivos",
        "files_privacy": "Archivos y privacidad",
        "save_meetings_in": "Guardar reuniones en",
        "keep_audio": "Conservar audio después de procesar",
        "custom_prompt": "Prompt personalizado",
        "save_settings": "Guardar configuración",

        "ready": "Preparado",
        "ready_ollama_started": "Preparado · Ollama iniciado automáticamente",
        "checking_environment": "Comprobando entorno...",
        "checking_ollama": "Comprobando Ollama...",
        "checking_folder": "Comprobando carpeta de trabajo...",
        "review_required": "Revisión necesaria",
        "recording": "● Grabando",
        "recording_muted": "● Grabando · micrófono silenciado",
        "finalizing_recording": "Finalizando grabación...",
        "saving_audio": "Guardando audio...",
        "loading_whisper": "Cargando Whisper {model}...",
        "downloading_whisper": "Descargando Whisper {model} por primera vez...",
        "transcribing": "Transcribiendo {speaker}...",
        "transcribing_progress": "Transcribiendo {speaker}... {percent}%",
        "summarizing": "Resumiendo con {model}...",
        "summary_block": "Resumen: bloque {index}/{total} con {model}...",
        "generating_final_summary": "Generando resumen final...",
        "processed_ok": "✓ Reunión procesada correctamente",
        "done": "TERMINADO",
        "error_occurred": "Se ha producido un error",

        "whisper_installed": "{model}  instalado",
        "whisper_download": "{model}  · se descargará al usar",
        "whisper_available": "Disponible localmente",
        "whisper_first_use": "Se descargará la primera vez que se use",

        "ollama_unavailable": "Ollama no disponible",
        "ollama_not_responding": "Ollama no responde",
        "ollama_no_models": "Sin modelos instalados",
        "ollama_active_install": "Ollama activo · instala al menos un modelo",
        "ollama_active_models": "Ollama activo · {count} modelo(s)",
        "searching_models": "Buscando modelos...",
        "searching_ollama": "Buscando Ollama...",

        "microphone_loading": "Micrófono: cargando...",
        "computer_audio_loading": "Audio PC: cargando...",
        "microphone_connecting": "Micrófono: conectando...",
        "computer_audio_connecting": "Audio PC: conectando...",
        "automatic": "automático",

        "stage_audio": "Audio",
        "stage_whisper": "Whisper",
        "stage_summary": "Resumen",

        "tooltip_start": "Iniciar reunión",
        "tooltip_stop": "Finalizar reunión",
        "tooltip_mute": "Silenciar / activar micrófono",
        "tooltip_transcript": "Abrir transcripción",
        "tooltip_folder": "Abrir carpeta",
        "tooltip_settings": "Configuración",
        "tooltip_refresh_models": "Actualizar modelos",
        "tooltip_change_folder": "Cambiar carpeta",

        "save_meeting_title": "Guardar reunión",
        "meeting_name_optional": "Nombre de la reunión (opcional):",
        "custom_prompt_title": "Prompt personalizado",
        "custom_prompt_required": "Escribe un prompt personalizado antes de guardar.",

        "ollama_missing_title": "Ollama no disponible",
        "ollama_stopped": (
            "Ollama se ha detenido desde que se abrió la aplicación. "
            "Cierra y vuelve a abrir Meeting Assistant para reiniciarlo automáticamente."
        ),
        "ollama_model_title": "Modelo Ollama",
        "ollama_model_missing": "No hay ningún modelo Ollama disponible para generar el resumen.",
        "microphone_title": "Micrófono",
        "microphone_missing": "Windows no detecta ningún micrófono disponible.",
        "computer_audio_title": "Audio del PC",
        "computer_audio_missing": "No se ha encontrado ningún dispositivo de audio del PC.",
        "meeting_audio_title": "Audio de reunión",
        "meeting_audio_select": "Selecciona un dispositivo de audio del PC válido en Configuración.",
        "folder_title": "Carpeta no disponible",
        "folder_unavailable": "No se puede crear o utilizar la carpeta de destino seleccionada.",

        "startup_title": "Comprobación inicial",
        "startup_no_models": "Ollama está iniciado, pero no hay ningún modelo instalado.",
        "startup_bad_folder": "La carpeta de destino no se puede utilizar: {detail}",
        "startup_no_mic": "Windows no detecta ningún micrófono disponible.",
        "startup_no_loopback": "No se ha encontrado ningún dispositivo loopback para capturar el audio del PC.",

        "meeting_in_progress_title": "Reunión en curso",
        "meeting_in_progress_close": "Finaliza la reunión antes de cerrar la aplicación.",
        "processing_title": "Procesando",
        "processing_close": "Espera a que termine el procesamiento antes de cerrar.",

        "speaker_me": "YO",
        "speaker_meeting": "REUNION",

        "error_no_mic": "No se ha encontrado ningún micrófono disponible en Windows.",
        "error_no_mic_start": (
            "No se ha podido iniciar ningún micrófono. Se ha intentado el seleccionado, "
            "el predeterminado de Windows y las demás entradas disponibles."
        ),
        "error_no_system_selected": (
            "No hay un dispositivo de audio de reunión seleccionado. "
            "Abre Configuración y selecciona uno."
        ),
        "error_system_gone": "El dispositivo de audio seleccionado ya no está disponible.",
        "error_system_capture": (
            "No se ha podido capturar el audio del PC. Comprueba que el dispositivo "
            "seleccionado es el que estás usando para escuchar la reunión."
        ),
        "error_audio_sources": "No se pudieron iniciar las dos fuentes de audio.",
        "error_whisper_load": (
            "No se pudo cargar Whisper. Si el modelo no estaba descargado, "
            "comprueba la conexión a Internet y vuelve a intentarlo."
        ),
        "error_no_voice": (
            "Whisper no ha detectado voz. Revisa los dispositivos de audio y prueba de nuevo."
        ),
        "error_summary": (
            "No se pudo generar el resumen. Comprueba que Ollama está iniciado "
            "y que el modelo seleccionado sigue disponible."
        ),
    },
}

DEFAULT_CONFIG = {
    "language": "en",
    "transcription_language": "auto",
    "whisper_model": DEFAULT_WHISPER_MODEL,
    "ollama_model": "",
    "summary_type": "meeting_minutes",
    "output_folder": str(APP_FOLDER / "meetings"),
    "keep_audio": True,
    "microphone_name": "",
    "system_audio_name": "",
    "custom_summary_prompt": (
        "Summarize the meeting clearly. Include only information present in "
        "the transcript and do not invent owners, dates, decisions or actions."
    ),
}


# ============================================================
# ESTADO GLOBAL
# ============================================================

stop_event = None
mic_thread = None
system_thread = None

recording = False
processing = False
recording_started_at = None
start_times = {}

mic_muted = False
pending_meeting_title = ""

current_folder = None
transcript_file = None
summary_file = None

selected_whisper_model = None
selected_ollama_model = None
selected_summary_type = None
selected_transcription_language = None

messages = queue.Queue()

mic_device_map = {}
system_device_map = {}

current_mic_display = ""
current_system_display = ""
current_mic_is_fallback = False

stage_state = {
    "audio": "pending",
    "whisper": "pending",
    "summary": "pending",
}


# ============================================================
# CONFIG
# ============================================================

def load_config():
    data = DEFAULT_CONFIG.copy()

    if CONFIG_FILE.exists():
        try:
            saved = json.loads(
                CONFIG_FILE.read_text(encoding="utf-8")
            )

            old_summary_map = {
                "Acta de reunión": "meeting_minutes",
                "Resumen ejecutivo": "executive",
                "Acciones y decisiones": "actions",
                "Resumen breve": "brief",
                "Personalizado": "custom",
                "Meeting minutes": "meeting_minutes",
                "Executive summary": "executive",
                "Actions and decisions": "actions",
                "Brief summary": "brief",
                "Custom": "custom",
            }

            if "summary_type" in saved:
                saved["summary_type"] = old_summary_map.get(
                    saved["summary_type"],
                    saved["summary_type"],
                )

            data.update(saved)

        except Exception:
            pass

    if data.get("language") not in ("en", "es"):
        data["language"] = "en"

    if data.get("transcription_language") not in (
        "auto",
        "en",
        "es",
    ):
        data["transcription_language"] = "auto"

    if data.get("summary_type") not in SUMMARY_TYPE_IDS:
        data["summary_type"] = "meeting_minutes"

    return data

# Load persisted settings before any translated UI text is evaluated.
# This must stay above current_language()/tr() and before the UI is built.
config = load_config()


def current_language():
    return globals().get("config", DEFAULT_CONFIG).get("language", "en")


def tr(key, **kwargs):
    language = current_language()
    value = TEXTS.get(
        language,
        TEXTS["en"],
    ).get(
        key,
        TEXTS["en"].get(key, key),
    )

    if kwargs:
        try:
            return value.format(**kwargs)
        except Exception:
            return value

    return value


def language_code_from_display(value):
    for code, label in APP_LANGUAGE_LABELS.items():
        if value == label:
            return code
    return "en"


def language_display(code):
    return APP_LANGUAGE_LABELS.get(
        code,
        APP_LANGUAGE_LABELS["en"],
    )


def summary_type_id_from_display(value):
    if value in SUMMARY_TYPE_IDS:
        return value

    for summary_id, labels in SUMMARY_TYPE_LABELS.items():
        if value in labels.values():
            return summary_id

    return "meeting_minutes"


def summary_type_display(summary_id, language=None):
    language = language or current_language()
    labels = SUMMARY_TYPE_LABELS.get(
        summary_id,
        SUMMARY_TYPE_LABELS["meeting_minutes"],
    )
    return labels.get(language, labels["en"])


def summary_type_display_values(language=None):
    language = language or current_language()
    return [
        summary_type_display(summary_id, language)
        for summary_id in SUMMARY_TYPE_IDS
    ]


def transcription_language_id_from_display(value):
    if value in ("auto", "en", "es"):
        return value

    for language_id, labels in (
        TRANSCRIPTION_LANGUAGE_LABELS.items()
    ):
        if value in labels.values():
            return language_id

    return "auto"


def transcription_language_display(
    language_id,
    ui_language=None,
):
    ui_language = ui_language or current_language()
    labels = TRANSCRIPTION_LANGUAGE_LABELS.get(
        language_id,
        TRANSCRIPTION_LANGUAGE_LABELS["auto"],
    )
    return labels.get(ui_language, labels["en"])


def transcription_language_display_values(
    ui_language=None,
):
    ui_language = ui_language or current_language()
    return [
        transcription_language_display(
            language_id,
            ui_language,
        )
        for language_id in ("auto", "en", "es")
    ]


def write_config():
    data = {
        "language": language_code_from_display(
            app_language_var.get()
        ),
        "transcription_language": (
            transcription_language_id_from_display(
                transcription_language_var.get()
            )
        ),
        "whisper_model": parse_whisper_value(
            whisper_var.get()
        ),
        "ollama_model": ollama_var.get(),
        "summary_type": summary_type_id_from_display(
            summary_type_var.get()
        ),
        "output_folder": settings_output_var.get(),
        "keep_audio": bool(
            settings_keep_audio_var.get()
        ),
        "microphone_name": selected_device_name(
            settings_mic_var.get(),
            mic_device_map,
        ),
        "system_audio_name": selected_device_name(
            settings_system_var.get(),
            system_device_map,
        ),
        "custom_summary_prompt": (
            settings_custom_prompt.get(
                "1.0",
                "end",
            ).strip()
        ),
    }

    CONFIG_FILE.write_text(
        json.dumps(
            data,
            indent=2,
            ensure_ascii=False,
        ),
        encoding="utf-8",
    )

    config.clear()
    config.update(data)

# ============================================================
# UTILIDADES
# ============================================================

def format_time(seconds):
    seconds = max(0, seconds)
    hours = int(seconds // 3600)
    minutes = int((seconds % 3600) // 60)
    secs = int(seconds % 60)
    return f"{hours:02d}:{minutes:02d}:{secs:02d}"


def sanitize_name(value):
    value = value.strip()
    if not value:
        return ""

    value = re.sub(r'[<>:"/\\|?*]', "", value)
    value = re.sub(r"\s+", "_", value)
    return value[:80].strip("._-")


def selected_device_name(label, mapping):
    info = mapping.get(label)
    if not info:
        return ""
    return info.get("name", "")


def open_path(path):
    if path and Path(path).exists():
        os.startfile(str(path))


# ============================================================
# WHISPER: ESTADO DE MODELOS
# ============================================================

def installed_whisper_models():
    installed = set()

    if scan_cache_dir is None:
        return installed

    try:
        cache = scan_cache_dir()
        repo_ids = {repo.repo_id for repo in cache.repos}

        for model, repo_id in WHISPER_REPOS.items():
            if repo_id in repo_ids:
                installed.add(model)
    except Exception:
        pass

    return installed


def whisper_display_values():
    installed = installed_whisper_models()
    values = []

    for model in WHISPER_REPOS:
        if model in installed:
            values.append(
                tr("whisper_installed", model=model)
            )
        else:
            values.append(
                tr("whisper_download", model=model)
            )

    return values

def parse_whisper_value(value):
    return value.split()[0].strip()


def refresh_whisper_models():
    current = parse_whisper_value(
        whisper_var.get() or config.get("whisper_model", DEFAULT_WHISPER_MODEL)
    )

    values = whisper_display_values()
    whisper_menu.configure(values=values)

    match = next(
        (item for item in values if parse_whisper_value(item) == current),
        values[0],
    )

    whisper_var.set(match)
    update_whisper_note()


def update_whisper_note(*_):
    model = parse_whisper_value(
        whisper_var.get()
    )
    installed = model in installed_whisper_models()

    whisper_status.configure(
        text=(
            tr("whisper_available")
            if installed
            else tr("whisper_first_use")
        )
    )

# ============================================================
# OLLAMA
# ============================================================

def find_ollama_executable():
    """Localiza ollama.exe en PATH y en ubicaciones habituales de Windows."""
    found = shutil.which("ollama")
    if found:
        return found

    candidates = []

    local_appdata = os.environ.get("LOCALAPPDATA")
    program_files = os.environ.get("ProgramFiles")

    if local_appdata:
        candidates.extend(
            [
                Path(local_appdata) / "Programs" / "Ollama" / "ollama.exe",
                Path(local_appdata) / "Ollama" / "ollama.exe",
            ]
        )

    if program_files:
        candidates.append(
            Path(program_files) / "Ollama" / "ollama.exe"
        )

    for candidate in candidates:
        if candidate.exists():
            return str(candidate)

    return None


def ollama_runtime_status():
    """
    Devuelve (running, models, error).
    running=True también cuando Ollama está activo pero no hay modelos.
    """
    try:
        response = ollama.list()
        items = (
            response.models
            if hasattr(response, "models")
            else response.get("models", [])
        )

        models = []

        for item in items:
            name = None

            if hasattr(item, "model"):
                name = item.model
            elif hasattr(item, "name"):
                name = item.name
            elif isinstance(item, dict):
                name = item.get("model") or item.get("name")

            if name:
                models.append(name)

        return True, sorted(set(models)), None

    except Exception as e:
        return False, [], e


def ensure_ollama_running(timeout=15):
    running, models, _ = ollama_runtime_status()

    is_es = current_language() == "es"

    if running:
        return {
            "ok": True,
            "started": False,
            "models": models,
            "message": (
                "Ollama ya estaba activo."
                if is_es
                else "Ollama was already running."
            ),
        }

    executable = find_ollama_executable()

    if not executable:
        return {
            "ok": False,
            "started": False,
            "models": [],
            "message": (
                "Ollama no está instalado o no se encuentra ollama.exe."
                if is_es
                else "Ollama is not installed or ollama.exe could not be found."
            ),
        }

    try:
        creationflags = getattr(
            subprocess,
            "CREATE_NO_WINDOW",
            0,
        )

        subprocess.Popen(
            [executable, "serve"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            stdin=subprocess.DEVNULL,
            creationflags=creationflags,
            close_fds=True,
        )

    except Exception as e:
        return {
            "ok": False,
            "started": False,
            "models": [],
            "message": (
                f"No se ha podido iniciar Ollama: {e}"
                if is_es
                else f"Ollama could not be started: {e}"
            ),
        }

    deadline = time.time() + timeout

    while time.time() < deadline:
        time.sleep(0.5)

        running, models, _ = ollama_runtime_status()

        if running:
            return {
                "ok": True,
                "started": True,
                "models": models,
                "message": (
                    "Ollama se ha iniciado automáticamente."
                    if is_es
                    else "Ollama was started automatically."
                ),
            }

    return {
        "ok": False,
        "started": False,
        "models": [],
        "message": (
            (
                "Ollama está instalado, pero no ha respondido después "
                f"de {timeout} segundos."
            )
            if is_es
            else (
                "Ollama is installed but did not respond after "
                f"{timeout} seconds."
            )
        ),
    }

def get_ollama_models():
    running, models, _ = ollama_runtime_status()

    if not running:
        return []

    return models


def refresh_ollama_models():
    running, models, _ = ollama_runtime_status()

    if not running:
        value = tr("ollama_unavailable")
        ollama_menu.configure(values=[value])
        ollama_var.set(value)
        ollama_status.configure(
            text=tr("ollama_not_responding")
        )
        return

    if not models:
        value = tr("ollama_no_models")
        ollama_menu.configure(values=[value])
        ollama_var.set(value)
        ollama_status.configure(
            text=tr("ollama_active_install")
        )
        return

    ollama_menu.configure(values=models)

    saved = config.get("ollama_model", "")

    if saved in models:
        ollama_var.set(saved)
    elif ollama_var.get() not in models:
        ollama_var.set(models[0])

    ollama_status.configure(
        text=tr(
            "ollama_active_models",
            count=len(models),
        )
    )

# ============================================================
# DISPOSITIVOS DE AUDIO
# ============================================================

def expand_microphone_display_name(short_name):
    """
    sounddevice/PortAudio puede devolver algunos nombres de entrada truncados.
    Para la interfaz intentamos encontrar una versión más completa del mismo
    nombre mediante PyAudioWPatch. La captura sigue usando el índice de
    sounddevice, así que esto solo afecta a cómo se muestra el dispositivo.
    """
    if not short_name:
        return short_name

    try:
        candidates = []

        with pyaudio.PyAudio() as p:
            for index in range(p.get_device_count()):
                info = p.get_device_info_by_index(index)

                if int(info.get("maxInputChannels", 0)) <= 0:
                    continue

                name = str(info.get("name", "")).strip()

                if not name:
                    continue

                # PortAudio puede cortar uno de los dos nombres.
                if (
                    name.lower().startswith(short_name.lower())
                    or short_name.lower().startswith(name.lower())
                ):
                    candidates.append(name)

        if candidates:
            return max(candidates, key=len)

    except Exception:
        pass

    return short_name


def refresh_audio_devices():
    global mic_device_map
    global system_device_map

    mic_device_map = {}
    system_device_map = {}

    try:
        devices = sd.query_devices()

        for index, device in enumerate(devices):
            if int(device.get("max_input_channels", 0)) <= 0:
                continue

            name = device["name"]
            display_name = expand_microphone_display_name(name)
            label = f"{index} | {display_name}"

            mic_device_map[label] = {
                "index": index,
                "name": name,
                "display_name": display_name,
                "samplerate": int(device["default_samplerate"]),
            }
    except Exception:
        pass

    try:
        with pyaudio.PyAudio() as p:
            for device in p.get_loopback_device_info_generator():
                name = device["name"]
                index = int(device["index"])
                label = f"{index} | {name}"

                system_device_map[label] = {
                    "index": index,
                    "name": name,
                    "samplerate": int(device["defaultSampleRate"]),
                    "channels": int(device["maxInputChannels"]),
                }
    except Exception:
        pass

    not_found = (
        "No encontrado"
        if current_language() == "es"
        else "Not found"
    )

    mic_values = list(mic_device_map.keys()) or [not_found]
    system_values = list(system_device_map.keys()) or [not_found]

    settings_mic_menu.configure(values=mic_values)
    settings_system_menu.configure(values=system_values)

    saved_mic = config.get("microphone_name", "")
    saved_system = config.get("system_audio_name", "")

    try:
        default_mic_index = sd.default.device[0]
    except Exception:
        default_mic_index = None

    mic_candidates = ordered_microphone_candidates(
        list(mic_device_map.values()),
        selected_name=saved_mic,
        default_index=default_mic_index,
    )
    mic_choice = (
        mic_candidates[0]
        if mic_candidates
        else None
    )

    mic_match = next(
        (
            label
            for label, info in mic_device_map.items()
            if (
                mic_choice
                and info["index"] == mic_choice["index"]
            )
        ),
        None,
    )

    system_candidates = ordered_system_candidates(
        list(system_device_map.values()),
        selected_name=saved_system,
    )
    system_choice = (
        system_candidates[0]
        if system_candidates
        else None
    )

    system_match = next(
        (
            label
            for label, info in system_device_map.items()
            if (
                system_choice
                and info["index"] == system_choice["index"]
            )
        ),
        None,
    )

    settings_mic_var.set(mic_match or not_found)
    settings_system_var.set(system_match or not_found)

    update_audio_summary()


def update_audio_summary():
    mic_info = mic_device_map.get(
        settings_mic_var.get()
    )
    mic_name = (
        (mic_info or {}).get("display_name")
        or (mic_info or {}).get("name")
        or config.get("microphone_name", "")
        or "-"
    )

    system_name = selected_device_name(
        settings_system_var.get(),
        system_device_map,
    ) or config.get("system_audio_name", "") or "-"

    mic_audio_label.configure(
        text=(
            f"{tr('microphone')}: "
            f"{friendly_device_name(mic_name)}"
        )
    )

    system_audio_label.configure(
        text=(
            f"{tr('computer_audio')}: "
            f"{friendly_device_name(system_name)}"
        )
    )

def friendly_device_name(name):
    """
    Simplifica únicamente el nombre mostrado en la ventana principal.
    El nombre técnico completo sigue usándose internamente para seleccionar
    y abrir el dispositivo correcto.
    """
    if not name:
        return name

    clean = str(name).strip()

    # El contexto de la propia línea ya indica que es audio del PC, así que
    # no necesitamos enseñar el sufijo técnico de WASAPI.
    clean = re.sub(r"\s*\[Loopback\]\s*$", "", clean, flags=re.IGNORECASE)

    # Windows suele devolver:
    #   Micrófono (...) / Audífono (...) / Altavoces (...)
    # Para la UI nos interesa el nombre identificativo dentro de paréntesis.
    first = clean.find("(")
    last = clean.rfind(")")

    if first != -1 and last > first:
        prefix = clean[:first].strip().lower()

        generic_prefixes = (
            "micrófono",
            "microfono",
            "microphone",
            "audífono",
            "audifono",
            "headphone",
            "headset",
            "altavoz",
            "altavoces",
            "speaker",
            "speakers",
            "auricular",
            "auriculares",
        )

        if prefix.startswith(generic_prefixes):
            inner = clean[first + 1:last].strip()
            if inner:
                clean = inner

    return clean


def short_device_name(name, max_len=32):
    if len(name) <= max_len:
        return name
    return name[: max_len - 1] + "…"


def current_microphone_candidates():
    try:
        devices = sd.query_devices()
    except Exception:
        return []

    current = []

    for index, device in enumerate(devices):
        if int(
            device.get("max_input_channels", 0)
        ) <= 0:
            continue

        current.append(
            {
                "index": index,
                "name": device["name"],
                "samplerate": int(
                    device["default_samplerate"]
                ),
            }
        )

    selected_name = selected_device_name(
        settings_mic_var.get(),
        mic_device_map,
    ) or config.get(
        "microphone_name",
        "",
    )

    try:
        default_index = sd.default.device[0]
    except Exception:
        default_index = None

    return ordered_microphone_candidates(
        current,
        selected_name=selected_name,
        default_index=default_index,
    )

def resolve_system_audio():
    selected_name = selected_device_name(
        settings_system_var.get(),
        system_device_map,
    )

    if not selected_name:
        raise RuntimeError(
            tr("error_no_system_selected")
        )

    return selected_name

def live_microphone_devices():
    try:
        devices = sd.query_devices()
    except Exception:
        return [], None

    result = []

    for index, device in enumerate(devices):
        if int(
            device.get(
                "max_input_channels",
                0,
            )
        ) <= 0:
            continue

        result.append(
            {
                "index": index,
                "name": device["name"],
                "samplerate": int(
                    device["default_samplerate"]
                ),
            }
        )

    try:
        default_index = sd.default.device[0]
    except Exception:
        default_index = None

    return result, default_index


def live_system_devices():
    p = pyaudio.PyAudio()

    try:
        result = []

        for device in (
            p.get_loopback_device_info_generator()
        ):
            result.append(
                {
                    "index": int(
                        device["index"]
                    ),
                    "name": device["name"],
                    "samplerate": int(
                        device[
                            "defaultSampleRate"
                        ]
                    ),
                    "channels": int(
                        device[
                            "maxInputChannels"
                        ]
                    ),
                }
            )

        default_index = None

        try:
            default = (
                p.get_default_wasapi_loopback()
            )
            default_index = int(
                default["index"]
            )
        except Exception:
            pass

        return result, default_index

    finally:
        p.terminate()


def close_input_stream(stream):
    if stream is None:
        return

    try:
        stream.stop()
    except Exception:
        try:
            stream.stop_stream()
        except Exception:
            pass

    try:
        stream.close()
    except Exception:
        pass


def write_gap_silence(
    wav,
    gap_started_at,
):
    if gap_started_at is None:
        return

    duration = max(
        0.0,
        time.perf_counter()
        - gap_started_at,
    )

    silence = silent_samples(
        duration,
        TARGET_SAMPLE_RATE,
    )

    if silence.size:
        wav.write(silence)


def open_microphone_stream(info):
    stream = sd.InputStream(
        device=int(info["index"]),
        samplerate=int(
            info["samplerate"]
        ),
        channels=1,
        dtype="float32",
        blocksize=CHUNK,
    )
    stream.start()
    return stream


def open_system_stream(device_name):
    p = pyaudio.PyAudio()

    try:
        device = next(
            (
                item
                for item in (
                    p.get_loopback_device_info_generator()
                )
                if item["name"] == device_name
            ),
            None,
        )

        if device is None:
            raise RuntimeError(
                tr("error_system_gone")
            )

        rate = int(
            device["defaultSampleRate"]
        )
        channels = max(
            1,
            int(
                device["maxInputChannels"]
            ),
        )

        stream = p.open(
            format=pyaudio.paInt16,
            channels=channels,
            rate=rate,
            input=True,
            input_device_index=int(
                device["index"]
            ),
            frames_per_buffer=CHUNK,
        )

        return (
            p,
            stream,
            rate,
            channels,
            device["name"],
        )

    except Exception:
        p.terminate()
        raise


# ============================================================
# GRABACION DE AUDIO
# ============================================================

def record_microphone(output_file):
    configured_name = (
        selected_device_name(
            settings_mic_var.get(),
            mic_device_map,
        )
        or config.get(
            "microphone_name",
            "",
        )
    )

    start_times["mic"] = (
        time.perf_counter()
    )

    current_name = ""
    stream = None
    stream_rate = TARGET_SAMPLE_RATE
    gap_started_at = (
        start_times["mic"]
    )
    last_device_check = 0.0
    automatic_fallback = False

    with sf.SoundFile(
        output_file,
        mode="w",
        samplerate=TARGET_SAMPLE_RATE,
        channels=1,
        subtype="PCM_16",
    ) as wav:

        while not stop_event.is_set():
            if stream is None:
                devices, default_index = (
                    live_microphone_devices()
                )

                chosen, changed = (
                    choose_microphone_failover(
                        devices,
                        current_name=current_name,
                        configured_name=(
                            configured_name
                        ),
                        default_index=(
                            default_index
                        ),
                    )
                )

                if chosen is None:
                    stop_event.wait(
                        HOTPLUG_RETRY_SECONDS
                    )
                    continue

                try:
                    stream = (
                        open_microphone_stream(
                            chosen
                        )
                    )
                except Exception:
                    stream = None
                    stop_event.wait(
                        HOTPLUG_RETRY_SECONDS
                    )
                    continue

                stream_rate = int(
                    chosen["samplerate"]
                )

                is_fallback = (
                    changed
                    or (
                        configured_name
                        and chosen["name"]
                        != configured_name
                    )
                )

                automatic_fallback = (
                    automatic_fallback
                    or is_fallback
                )

                if gap_started_at is not None:
                    write_gap_silence(
                        wav,
                        gap_started_at,
                    )
                    gap_started_at = None

                current_name = chosen["name"]
                last_device_check = (
                    time.perf_counter()
                )

                display_name = (
                    expand_microphone_display_name(
                        current_name
                    )
                )

                messages.put(
                    (
                        (
                            "mic_fallback"
                            if automatic_fallback
                            else "device_mic"
                        ),
                        display_name,
                    )
                )

            try:
                data, overflowed = (
                    stream.read(CHUNK)
                )

                if overflowed:
                    messages.put(
                        (
                            "log",
                            "Microphone input overflow",
                        )
                    )

                if mic_muted:
                    data.fill(0)

                converted = resample_mono(
                    data,
                    stream_rate,
                    TARGET_SAMPLE_RATE,
                )

                wav.write(converted)

                now = time.perf_counter()

                if (
                    now - last_device_check
                    >= HOTPLUG_CHECK_SECONDS
                ):
                    live, _ = (
                        live_microphone_devices()
                    )

                    if current_name not in {
                        item["name"]
                        for item in live
                    }:
                        raise RuntimeError(
                            "microphone disconnected"
                        )

                    last_device_check = now

            except Exception:
                close_input_stream(stream)
                stream = None

                if gap_started_at is None:
                    gap_started_at = (
                        time.perf_counter()
                    )

                automatic_fallback = True

        close_input_stream(stream)

        if gap_started_at is not None:
            write_gap_silence(
                wav,
                gap_started_at,
            )

def record_system_audio(output_file):
    configured_name = (
        selected_device_name(
            settings_system_var.get(),
            system_device_map,
        )
        or config.get(
            "system_audio_name",
            "",
        )
    )

    start_times["system"] = (
        time.perf_counter()
    )

    current_name = ""
    p = None
    stream = None
    stream_rate = TARGET_SAMPLE_RATE
    stream_channels = 1
    gap_started_at = (
        start_times["system"]
    )
    last_device_check = 0.0
    automatic_fallback = False

    with sf.SoundFile(
        output_file,
        mode="w",
        samplerate=TARGET_SAMPLE_RATE,
        channels=1,
        subtype="PCM_16",
    ) as wav:

        while not stop_event.is_set():
            if stream is None:
                devices, default_index = (
                    live_system_devices()
                )

                chosen, changed = (
                    choose_system_failover(
                        devices,
                        current_name=current_name,
                        configured_name=(
                            configured_name
                        ),
                        default_index=(
                            default_index
                        ),
                    )
                )

                if chosen is None:
                    stop_event.wait(
                        HOTPLUG_RETRY_SECONDS
                    )
                    continue

                try:
                    (
                        p,
                        stream,
                        stream_rate,
                        stream_channels,
                        actual_name,
                    ) = open_system_stream(
                        chosen["name"]
                    )
                except Exception:
                    p = None
                    stream = None
                    stop_event.wait(
                        HOTPLUG_RETRY_SECONDS
                    )
                    continue

                is_fallback = (
                    changed
                    or (
                        configured_name
                        and actual_name
                        != configured_name
                    )
                )

                automatic_fallback = (
                    automatic_fallback
                    or is_fallback
                )

                if gap_started_at is not None:
                    write_gap_silence(
                        wav,
                        gap_started_at,
                    )
                    gap_started_at = None

                current_name = actual_name
                last_device_check = (
                    time.perf_counter()
                )

                messages.put(
                    (
                        (
                            "system_fallback"
                            if automatic_fallback
                            else "device_system"
                        ),
                        actual_name,
                    )
                )

            try:
                data = stream.read(
                    CHUNK,
                    exception_on_overflow=False,
                )

                mono = (
                    pcm16_bytes_to_mono_float(
                        data,
                        stream_channels,
                    )
                )

                converted = resample_mono(
                    mono,
                    stream_rate,
                    TARGET_SAMPLE_RATE,
                )

                wav.write(converted)

                now = time.perf_counter()

                if (
                    now - last_device_check
                    >= HOTPLUG_CHECK_SECONDS
                ):
                    live, _ = (
                        live_system_devices()
                    )

                    if current_name not in {
                        item["name"]
                        for item in live
                    }:
                        raise RuntimeError(
                            "system audio disconnected"
                        )

                    last_device_check = now

            except Exception:
                close_input_stream(stream)
                stream = None

                if p is not None:
                    try:
                        p.terminate()
                    except Exception:
                        pass

                p = None

                if gap_started_at is None:
                    gap_started_at = (
                        time.perf_counter()
                    )

                automatic_fallback = True

        close_input_stream(stream)

        if p is not None:
            try:
                p.terminate()
            except Exception:
                pass

        if gap_started_at is not None:
            write_gap_silence(
                wav,
                gap_started_at,
            )

# ============================================================
# WHISPER
# ============================================================

def transcribe_audio(
    model,
    audio_file,
    speaker,
    offset,
    track_duration,
    completed_duration,
    total_duration,
):
    last_percent = -1

    initial_percent = transcription_percent(
        0,
        track_duration,
        completed_duration,
        total_duration,
    )

    messages.put(
        (
            "status",
            tr(
                "transcribing_progress",
                speaker=speaker,
                percent=initial_percent,
            ),
        )
    )

    segments, _ = model.transcribe(
        str(audio_file),
        language=selected_transcription_language,
        vad_filter=True,
        beam_size=5,
    )

    result = []

    for segment in segments:
        percent = transcription_percent(
            segment.end,
            track_duration,
            completed_duration,
            total_duration,
        )

        if percent != last_percent:
            messages.put(
                (
                    "status",
                    tr(
                        "transcribing_progress",
                        speaker=speaker,
                        percent=percent,
                    ),
                )
            )
            last_percent = percent

        segment_text = segment.text.strip()

        if not segment_text:
            continue

        result.append(
            {
                "start": segment.start + offset,
                "speaker": speaker,
                "text": segment_text,
            }
        )

    final_percent = transcription_percent(
        track_duration,
        track_duration,
        completed_duration,
        total_duration,
    )

    if final_percent != last_percent:
        messages.put(
            (
                "status",
                tr(
                    "transcribing_progress",
                    speaker=speaker,
                    percent=final_percent,
                ),
            )
        )

    return result

# ============================================================
# RESUMEN / OLLAMA
# ============================================================

def split_transcript(text, max_chars=10000):
    lines = text.splitlines()
    chunks = []
    current = []
    current_size = 0

    for line in lines:
        size = len(line) + 1

        if current and current_size + size > max_chars:
            chunks.append("\n".join(current))
            current = []
            current_size = 0

        current.append(line)
        current_size += size

    if current:
        chunks.append("\n".join(current))

    return chunks


def summary_instructions(summary_type):
    if current_language() == "es":
        if summary_type == "executive":
            return """
Genera un resumen ejecutivo de la reunión.
Incluye:
# Resumen ejecutivo
# Decisiones clave
# Próximos pasos
Sé conciso y prioriza lo relevante.
"""

        if summary_type == "actions":
            return """
Céntrate en información accionable.
Usa:
# Decisiones tomadas
# Acciones
Para cada acción indica responsable y fecha
solo si aparecen explícitamente.
# Temas pendientes
# Bloqueos o riesgos
"""

        if summary_type == "brief":
            return """
Genera un resumen muy breve.
Usa:
# Resumen
# Decisiones
# Acciones
No añadas detalle innecesario.
"""

        if summary_type == "custom":
            return (
                config.get(
                    "custom_summary_prompt",
                    "",
                ).strip()
                or "Resume la reunión sin inventar información."
            )

        return """
Genera un acta de reunión.
Usa:
# Resumen ejecutivo
# Decisiones tomadas
# Acciones
Para cada acción indica, solo si se conoce:
- Acción
- Responsable
- Fecha límite
# Temas pendientes
# Riesgos o problemas
# Otros puntos relevantes
"""

    if summary_type == "executive":
        return """
Generate an executive summary of the meeting.
Use:
# Executive summary
# Key decisions
# Next steps
Be concise and prioritize what matters.
"""

    if summary_type == "actions":
        return """
Focus on actionable information.
Use:
# Decisions
# Actions
For each action include owner and due date
only when explicitly stated.
# Pending topics
# Blockers or risks
"""

    if summary_type == "brief":
        return """
Generate a very brief meeting summary.
Use:
# Summary
# Decisions
# Actions
Avoid unnecessary detail.
"""

    if summary_type == "custom":
        return (
            config.get(
                "custom_summary_prompt",
                "",
            ).strip()
            or DEFAULT_CONFIG["custom_summary_prompt"]
        )

    return """
Generate meeting minutes.
Use:
# Executive summary
# Decisions
# Actions
For each action include, only when known:
- Action
- Owner
- Due date
# Pending topics
# Risks or issues
# Other relevant points
"""

def summarize_with_ollama(transcript):
    chunks = split_transcript(transcript)

    if current_language() == "es":
        system_prompt = """
Eres un asistente especializado en analizar
reuniones de trabajo.

La información procede de una transcripción
automática y puede contener errores.

YO representa el micrófono del usuario.
REUNION representa el audio del ordenador.

No inventes información, responsables, fechas,
decisiones, acciones ni conclusiones.
Si algo no está claro, indícalo.
Responde siempre en español.
"""
        extraction_prompt = """
Extrae únicamente la información relevante:
temas, decisiones, acciones, responsables,
fechas, pendientes y riesgos.
No inventes datos.

TRANSCRIPCIÓN:
"""
        no_invent = "No inventes información."

    else:
        system_prompt = """
You are an assistant specialized in analyzing
work meetings.

The information comes from an automatic
transcript and may contain errors.

ME represents the user's microphone.
MEETING represents computer audio.

Do not invent information, owners, dates,
decisions, actions or conclusions.
If something is unclear, say so.
Always respond in English.
"""
        extraction_prompt = """
Extract only the relevant information:
topics, decisions, actions, owners, dates,
pending items and risks.
Do not invent information.

TRANSCRIPT:
"""
        no_invent = "Do not invent information."

    partial_summaries = []
    total = len(chunks)

    for index, chunk in enumerate(
        chunks,
        start=1,
    ):
        messages.put(
            (
                "status",
                tr(
                    "summary_block",
                    index=index,
                    total=total,
                    model=selected_ollama_model,
                ),
            )
        )

        response = ollama.chat(
            model=selected_ollama_model,
            messages=[
                {
                    "role": "system",
                    "content": system_prompt,
                },
                {
                    "role": "user",
                    "content": (
                        f"{extraction_prompt}\n\n{chunk}"
                    ),
                },
            ],
            options={"temperature": 0.2},
        )

        partial_summaries.append(
            response["message"]["content"]
        )

    combined = "\n\n---\n\n".join(
        partial_summaries
    )

    messages.put(
        (
            "status",
            tr("generating_final_summary"),
        )
    )

    final_instruction = summary_instructions(
        selected_summary_type
    )

    response = ollama.chat(
        model=selected_ollama_model,
        messages=[
            {
                "role": "system",
                "content": system_prompt,
            },
            {
                "role": "user",
                "content": (
                    f"{final_instruction}\n\n"
                    f"{no_invent}\n\n"
                    f"{combined}"
                ),
            },
        ],
        options={"temperature": 0.2},
    )

    return response["message"]["content"]

# ============================================================
# PROCESAMIENTO
# ============================================================

def process_meeting():
    global processing
    global current_folder
    global transcript_file
    global summary_file

    try:
        set_stage("audio", "working")
        messages.put(("status", tr("saving_audio")))

        mic_thread.join()
        system_thread.join()

        # Los WAV ya están cerrados. Si el usuario indicó un nombre,
        # renombramos ahora la carpeta para no interferir con los streams.
        if pending_meeting_title:
            base_name = current_folder.name
            target = current_folder.parent / (
                f"{base_name}_{pending_meeting_title}"
            )

            if target.exists():
                target = current_folder.parent / (
                    f"{base_name}_{pending_meeting_title}_{int(time.time())}"
                )

            current_folder.rename(target)
            current_folder = target
            transcript_file = current_folder / "transcript.txt"
            summary_file = current_folder / "summary.md"

        mic_file = current_folder / "microphone.wav"
        system_file = current_folder / "system_audio.wav"

        if "mic" not in start_times or "system" not in start_times:
            raise RuntimeError(
                tr("error_audio_sources")
            )

        set_stage("audio", "done")

        set_stage("whisper", "working")

        if selected_whisper_model not in installed_whisper_models():
            messages.put(
                (
                    "status",
                    tr("downloading_whisper", model=selected_whisper_model),
                )
            )
        else:
            messages.put(
                (
                    "status",
                    tr("loading_whisper", model=selected_whisper_model),
                )
            )

        try:
            whisper = WhisperModel(
                selected_whisper_model,
                device="cpu",
                compute_type="int8",
            )
        except Exception as e:
            raise RuntimeError(
                tr("error_whisper_load")
            ) from e

        common_start = min(
            start_times["mic"],
            start_times["system"],
        )

        mic_info = sf.info(
            str(mic_file)
        )
        system_info = sf.info(
            str(system_file)
        )

        mic_duration = (
            mic_info.frames
            / mic_info.samplerate
            if mic_info.samplerate
            else 0.0
        )
        system_duration = (
            system_info.frames
            / system_info.samplerate
            if system_info.samplerate
            else 0.0
        )

        total_transcription_duration = (
            mic_duration
            + system_duration
        )

        mic_segments = transcribe_audio(
            whisper,
            mic_file,
            tr("speaker_me"),
            start_times["mic"] - common_start,
            mic_duration,
            0.0,
            total_transcription_duration,
        )

        system_segments = transcribe_audio(
            whisper,
            system_file,
            tr("speaker_meeting"),
            start_times["system"] - common_start,
            system_duration,
            mic_duration,
            total_transcription_duration,
        )

        all_segments = mic_segments + system_segments
        all_segments.sort(key=lambda item: item["start"])

        transcript_lines = []

        for item in all_segments:
            transcript_lines.append(
                f"[{format_time(item['start'])}] "
                f"{item['speaker']}: {item['text']}"
            )

        transcript = "\n\n".join(transcript_lines)

        transcript_file.write_text(
            transcript,
            encoding="utf-8",
        )

        if not transcript.strip():
            raise RuntimeError(
                tr("error_no_voice")
            )

        set_stage("whisper", "done")

        set_stage("summary", "working")
        messages.put(
            (
                "status",
                tr("summarizing", model=selected_ollama_model),
            )
        )

        try:
            summary = summarize_with_ollama(transcript)
        except Exception as e:
            raise RuntimeError(
                tr("error_summary")
            ) from e

        summary_file.write_text(
            summary,
            encoding="utf-8",
        )

        set_stage("summary", "done")

        if not config.get("keep_audio", True):
            for audio_file in (mic_file, system_file):
                try:
                    if audio_file.exists():
                        audio_file.unlink()
                except Exception:
                    pass

        messages.put(("complete", None))

    except Exception as e:
        messages.put(("error", str(e)))

    finally:
        processing = False


# ============================================================
# ERRORES AMIGABLES
# ============================================================

def friendly_error(kind, error):
    if kind == "system_audio":
        return tr("error_system_capture")

    return str(error) or tr("error_occurred")

# ============================================================
# ESTADOS VISUALES
# ============================================================

STAGE_COLORS = {
    "pending": ("#ECE6DB", "#5D4B3D"),
    "working": ("#FFE6A7", "#6F1D1B"),
    "done": ("#E7DFC9", "#432818"),
    "error": ("#F4D8D7", "#6F1D1B"),
}


def set_stage(name, state):
    stage_state[name] = state
    messages.put(("stage", (name, state)))


def sync_result_button_with_stage(name, state):
    mapping = {
        "audio": globals().get("folder_button"),
        "whisper": globals().get("transcript_button"),
        "summary": globals().get("summary_button"),
    }

    button = mapping.get(name)

    if button is None:
        return

    try:
        button.set_available(
            state == "done"
        )
    except Exception:
        pass


def apply_stage(name, state):
    label = {
        "audio": stage_audio,
        "whisper": stage_whisper,
        "summary": stage_summary,
    }[name]

    fg, text_color = STAGE_COLORS[state]

    title = {
        "audio": tr("stage_audio"),
        "whisper": tr("stage_whisper"),
        "summary": tr("stage_summary"),
    }[name]

    if state == "done":
        label.configure(
            text=title,
            image=stage_done_icon,
            compound="left",
            fg_color=fg,
            text_color=text_color,
        )
        sync_result_button_with_stage(
            name,
            state,
        )
        return

    if state == "error":
        label.configure(
            text=title,
            image=stage_error_icon,
            compound="left",
            fg_color=fg,
            text_color=text_color,
        )
        sync_result_button_with_stage(
            name,
            state,
        )
        return

    prefix = {
        "pending": "○",
        "working": "●",
    }[state]

    label.configure(
        text=f"{prefix}  {title}",
        image=None,
        compound="left",
        fg_color=fg,
        text_color=text_color,
    )

    sync_result_button_with_stage(
        name,
        state,
    )

def reset_stages():
    for stage in stage_state:
        apply_stage(stage, "pending")



# ============================================================
# ICONOS EMBEBIDOS / TOOLTIPS
# ============================================================

DROPDOWN_ARROW_ICON_B64 = "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAABmJLR0QA/wD/AP+gvaeTAAABB0lEQVRYhe2Uu0oDQRRAz93dMdhrEuvYBNYqtYUGIhrwbxUUFxQ/wUjSWBsfveDO5tokKGEkO7Nl5pTzOgeGGYhEIpFtR1yDw373UZTdpNSL25e39yaCUa/TXhi5VuGrmM6P1+cT5y6lpTCwRopRr9NuIrdGCoUBSsu1xhmwYxkDTwK5NfJw1t878JWfH3b3KyN3ArnCrEqrS9c65xWsDvjOKIAjhZkRe3oz/XytK/+7d5FWJ/fPH3OvgNAIH/nGAN8IX3mtgLoRIfLaAZsiQuVeAf9FJGVmQ+XeAfD7tpfPa7I8JFeYZKUOfT8u74D1CIBQeXDAKqIycgWQljpu+mVHIpHI9vIDN5uxfioQs6QAAAAASUVORK5CYII="

STAGE_DONE_ICON_B64 = "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAABmJLR0QA/wD/AP+gvaeTAAABq0lEQVRYhe2UvU7bYBSGn+Mfkk5dIIaNKhnK31B1oKKqRBopFXAVlXoBHarOhRVugdsAgRRoVXWgK6hBVSKxNaEdkWph850OrSVkxcEOmKV+Rn/WOa8fn+9AQcH/juTdYHl6umyX/UOEsNXuvYifW3kHcEr+B+AZysNB57kGqM96iyq8A67U8OZeA6zUaiXLyDZgK7J58L339V4DXNoXG8CcwqnxS+tJ7+UyhPVZb9Ey8gVADUtJXw85GEirPrcAadVH3OkvyKI+4s4MZFUf4Qx62JiZ/CzKAyvQ1f1u/zxNocC+WAfmENrm983qIwYbUEoKT0NXWs2qV7mpyPWFY0Rffzw7828VYCxkDTgWmA9d+fRqZnwqqUBc/eG3/lHa5okBdju9n2MhjX8hHgfqHCSFyDr1cYbegpXa5MSlQwtYUDh1JXy51/71IzofZerjDL0Fw0yMOvVxUu2BQSZC47xFeP9XfflJlsHLHACgWfUqoSstgXmBjsIjAGPp86yDd53Ui2i/2z93Am0onCjUAFuUrds0hxFWcbPqVYwrOwrBlV+uj6q+oKAg4g8D2ss5nt5maAAAAABJRU5ErkJggg=="
STAGE_ERROR_ICON_B64 = "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAABmJLR0QA/wD/AP+gvaeTAAABdElEQVRYhe2UMU4CQRiFv2V3EA8gizGG1mKpqI0RtNHEmxgtOAaFxsR7WGhhxMRYU0lBCTFGVg9gdmdhLEClgNmdsdBiXzkzee9L5v8f5MqV64/l6C5PNjcewVkVsTxoh+GbiXHL98uyKG5AfZw/v2wve1dI8VkBVZdFr9Py/bJZuNcBVZ96LJcWYCLHh8ATEEjhPZxWq+tp4ceVyposendAgKIvXXGke6/9gi/DgnA7QA1F33G9xtlw+JrlrfTE7uVgMPoVQFYIm/DMAGkQtuFGAMsgxlGU2IYbA8D8hBMAvdlxAPREnDRN19UYYAEEtuGQ3gMLFTmOAtT3gcJLSiXXxssYoOX75YJw74Ea0y/o4bClJsmtSVlZASwomf2JHDcwLKt5Wa/h/LSblJUVQJY9t4UwrmLdnttAaAFsGs4UQjuEBeFeM5t2IZOdLA13MRq9izjZ42c7rrQZKX4ROF3TkmmH4ZuIkyY43alHrly5/rE+AcOY5o8yIIpRAAAAAElFTkSuQmCC"

SETTINGS_ICON_B64 = "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAABmJLR0QA/wD/AP+gvaeTAAAEO0lEQVRYhe2XT2gcZRjGf+9O0l1UCDbmz2xSSIyaoNFiqf8aQcRaST30UNxmd4W0YunFQykoeFByVLStRcX2ZKFkd7P+uQgNUhUV0qK0eghKDUqjbWaTVIPBFHdNdl4P83Xd7M5sEor00gf28L7f877PM7Mz7/cN3MB1hqy1QOeyDeSLX4FOY8f7AcilR0FaiViPyfrY/Fr61a1K1MkkASQ6MExhqR1kI8hGnOwWAESeAvDWmF/GXwEr3gGdSbVQlGlD3wdsAt0XQD8K8j3oMQAsbZWWxMy1GVAVcpkzwEMrcSvwDfbAIyKiazZgrvoj4BzoKMgwsN4sT4G8h8hplpYuAVAvG1DpQzUJ0m14f4AkQJ8G7sfSZ/zuhr+BqZGtiHuqMm34lynk75LOPX9W1elQiFx3EjgMNC5fDD0pbbs+q6wJ+RkgGvsc9EhZZgJX+4AFoIlw5FW/MpEhV6LxEwibEX4uUz/i9axGlQGdzvQyebwBpMOkfgd3m7QnzoC8ZnIv6FS2u7K2ZMSOTyLWNmDOZNqYPN6g05nemgY0l+nH1XHCkRywwxTvl2jyVwAKfx8EJoF6xD0UZABAWmMXED1gwp2EIzlcHddcpj/QAKqXgStAxGTOY+9KlZp27skjvGjI2yubVaF14gQwgffsRIArRsPfgETjZwkXo8DVp3W48jUSO/4h8KUxfEj1WH2QvsiQi5L2AhzCxahE42fLOcsmoc5mb6HgtoE2e0WhMd/OruwnpOeAHnINL+nMcDrIBC6TKKC0kl9n62zWlebYQsnkMvGl4i9Ac5lQt7QPTPj1VSdztMZErIVZ6qyuqyb8X8NVQWtOuNWiZECaYwvUWV0gd+MNHahz232lL6buA/aa8BUstyvwp/Kc4RVRq6f86qHiGZDm2ILOZR3yxVmgBZU+4IsqBxZvARboT9h/vS6ybzHoCnUqNQgCwgzhf3LS+OxC+XrlHHiAfNEBWryEJlWHlnOc1E6Qxz3HeqCmuGYtROJeQJSC5aiT3hxoAIpNwE1A3oul28x2r8eF9yMgb5hwVOzkySBxAJziIHAn3l+aB25GpCnQgNjJk4h1DxGrFfjYpA9rLt0BQDhyAOgEFqE05XyhM5kuhDe9gA8o5G1Ccq/YA6PlvKoTkdixH70zwMhv5llsBE7p1MhucF82tHckmjgfLD58O0X9FLjVayoOHbvnRaRqB/Xfji+lnyBE1dZpUHs7dnoGET1YEr8Kl63SHq/aEf3PhHWL47j1Y4h8h+t+gkgKuM2sLrEusl+nUmMIFz1lNgCPMi0JRO8oGXWJY8kOVDdRtzjuJ/V/Hsm+xR54eKUj2cqTcDbd/J+47kV5N5Ar8jbK8yZ60NTWxKq+C8qP2Tqd6cVV73a6ugURQfA2LdFesRM/qJNOePx4KqjnmgwsMzOXbaBQ/BrFwR7YDlzTh8kNXHf8C/5frX+T87iOAAAAAElFTkSuQmCC"
TRANSCRIPT_ICON_B64 = "iVBORw0KGgoAAAANSUhEUgAAACgAAAAoCAYAAACM/rhtAAAABmJLR0QA/wD/AP+gvaeTAAADBElEQVRYhe2YSWgUQRSGv9etjnFBssCMQ/AgwUzECC4IosIwUSFX8eKCevDiwWtu4oYe9OrNm4IRNwQvLmPUQ0DEJYIhPUgwHkxmwOXglsxSz0NaE0mqzUxmMkH9L91V9frV1/X69aMK/mt6kqDBjY2NNTWLCztBo1PypmQcUS//vabn4cDAcEUBN7cuqw3lso+A1hL8DiM8UNWL9W7D9au9vdlSAR3bwLxcrqNEOID5KO2CXPpY+NCfaAkfPBYwV5CsK5hojiRFaAN5WecNrbsKBZvtpuaGxTUyd5tBTwnEJrNRtNsgex966YFiAK1vJYLru/4UBAfQnXr/OekN3RjR/AaBZ7+Y4ATwFkCQTS48bVu5dEtZAEtRd+r9Z3Hdg35TQEcWLHRWCpzz++oxentrLBKvCiDAvd53PSCv/eb6W88GvyW99GER9gM5YIHCze0ropN+ChUHBFB0cPROan/2JfvSFxDdBxhgScExV9qbmkJVAZTR72+C7vdlLqvoab/Zmp37peNPvioCGCQTzhxHtQcApSO+ItoQZD+nQhwf/OuaRCxyFGEYwDEYk1YP4QhwC1jkuuYQcHJGAQU5r+gOYInAsZ8BVwFBEPikkAfmoBwIAqxIiJPe0B1F94zL5t+kUMvY4izf3hJeZfNVqRDT5WU6gc7xfbaKkzfOZuDVZH5mNEksFQdBrTV/xrMYJlQcRGi02VYFEH6vOAqLbHZVA4TxFcfOUVVAW8UZr6oCTkWzHrCk/2CiJbrWoVBXzDMG92NX3+DzYucqGjARC+8SNZc0eEM4QYIhEQvv9n/gU9bfF+IuL9OZaImmZm2IAUqZqFSVPUkcQ//dVObN9LDGVPYkKQgj8VgkVuz+16Z/L0kcQ//9VGZg2mS+Zn2SzPoQWwFVf53H1MXj5d8a+D7rR1uSt9lZJxaRJ6BtwGo3Hcm1Temgogilx25VzGObmXUFC/NDZ0BelpdqUr0I5XJnbYOBFb+9qSmUdb/uBG0WmFdOKoWsiHq1bsO16ZzA/tef9APqWhu+cLPXvQAAAABJRU5ErkJggg=="
STOP_ICON_B64 = "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAABmJLR0QA/wD/AP+gvaeTAAAAY0lEQVRYhe3VoRaAMAxD0QTH4V/RoPllKDIoJGy0kly/9LkBZmYNklZJoe92SXNrnx0BAWBM9gfJqRqg5PHbQPJxYyiOlznAAQ5wgAN6AqKwf779A70BC4AjcxzAlnhnZj9zAQcqToj5LBSFAAAAAElFTkSuQmCC"
FOLDER_ICON_B64 = "iVBORw0KGgoAAAANSUhEUgAAACgAAAAoCAYAAACM/rhtAAAABmJLR0QA/wD/AP+gvaeTAAACgElEQVRYhe2Yz0sUcRTAP29mrSwkITY3F6LC3JGgFK1bpK4GnapDp/oP6hxRRgtFYV67e+jkyWMm/gKJIMzoIK1hCqa0W2aElGbOvg67O+3qru22P4iYz+l933e+bz+82TcDAy4uLtsimxOtx7w+0za7gItAbQ41FCGsMXpGpiO9xRY0UxdtgdqAqfIcaAeqcqwhgFeE84e9ezxzS99GiynodPASmMuWbxI4nkgNgEyKqpn5aBw1MFAuk+i2it4beRO9XXTBYMP+TtQYTKTvD4c/3Mq1SEd9zRE1ZBQ4WGxJwzFVaU7G9q6dPfkUGXobnZWYtgHziVpd7daBB83NVBQq6HSww/KFFO4ADIcjW4YnFzZ3Mnd0UZB+46feHXwX/Zi6Y2Q78jekdHI6v5PiV7hmV8irzgbv0dQdTxH9gLjkubq6E+sVKxdUxTKUyj/7aZMiZ4HamJp9IWgJQawkggBPZmZ+AH35nAkGfN0I14Gm8YDvDNORUSjyLS4Ej8fT7SwMTjr5TBcHLd8jcn+TFAFdtO2N/uRKlN3JONstvlpypzTiQ5JpZ7v/4IAor0tk5KAithBrSQzJFrII5vcmKQYpQ5JGxiGR9bWHpVdKJ21IUsgoODT75WtpdbbydGphOVP+n3nMZMMVLBRXsFBcwUJxBQslRVDXklFr46HqcosELf8+x0T4nox/CwoTydCzunajbGYO9k1HRXnhxMkgBMZ4oOYlIo2JrSFBn8WE1VJqGUqlwmniXzMAJobDkVOApgkCBC1/PWyMgPhLKZUV5b3tsdvHpj7NJFNpnzXmllY+W9VVj21TdqD4EfaWSW1eVHttNa+MhaMLZfpNF5f/g1+3r8LGhYLkRQAAAABJRU5ErkJggg=="

REFRESH_ICON_B64 = "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAABmJLR0QA/wD/AP+gvaeTAAADcUlEQVRYhe2WTWhcVRTHf+e+yUSIhYJp5k3S1jqWmQmNbuK20L6MKd2IpI1mobauTKALN5W0GwMFk0IRpKALwdJKamoEkYKfM7NxazYtMa8iKRXaeYOtCGm0TfLucZEm5uPNpC8rwfx3795zz/93P87hwZb+75I4wYVsKqOGPlXxRNgHpB7lCFSZFNGyWMaKv1Sno9Z35dueSi6E5ptfg99jARSyqQyOjKhyBDAbhFsVvkhYHfz+RvXmagD3FvDEfj9ID4HlMZLh5d3jauS6Kr3AQ+CiivQ4CbN7vx84JT8wTsLsVpEe4BIwJ8orocg1rz39+pp0aaDlaifO0kDdEyjkU4OKDC9+6WhCwpPfTd2t1AXO7mwTs3AO6ANQYbA8FZx9dAJzQMOfTUFyYoL5ugBe3j0ucAEIBRko+pWP6xmvg8+5AyqcBxzQ0yW/OhwF4EQuzqYyiFwFkoL0xzUHmL53/6dndmy7KfASyIuZ5qY5kIOA8yB5/0ylUucNqJGzQBPo6GbMl1SeqnyqIm8CIch7QMPamMTaAW9vy7NAD/C30fCduKYHc605I3pMVBc3ZxUVfgaei4pfByAJeZXFkxn/4cbdO3EBHGPfVqVfaz/v+cwEdqIWAEgjoIoZjWsOgDHDsmCnralR4iHXxiGsuX4IzKH25vSmzLf0X9ah9ub0UETZr3qrXtY9jMPzUQmMxZIwV4qTd36La+7lW7sF+y1wpuQH766cW66CXnD+MHyFrm8WACog1maAgbgAoMcAUeGvtTPLANOdmO2zq8yvi/L1vwBiQysX41p3d7TuChfsERa74VhNgAjtw3CuOBVcimu6UuGCfR9oVORyeapya+18VLOYF/QUYFS5UGh339isuZd3TwBHgRmE01Exkd2q6FdHVkB8Usi5se/dy7snBD4AVNG3onZfE2AJQoVBwFHhw66cO+Zld7ZtZNzd0bqr0O5+LnCexSo7Wfarn9WKXy7Dzk4ats+6c8B8yQ+SS+NdufRriH4EPAk8RHRc1HxphQmbqtyemUG2zbptjvICoi+DHAUagRlB+4t+9XI94GWAITA/5t0K8KDkB0+vDDqQd/c4yghCLxv/R4aKXDGOnHqcnrGqER3e6+6YSzi25N++FxV8IO/uSaB9ivFAO4AWQEGrApNWpAwyVuu+t7SlKP0D2ipAaC7DXwAAAAAASUVORK5CYII="
MIC_ICON_B64 = "iVBORw0KGgoAAAANSUhEUgAAACgAAAAoCAYAAACM/rhtAAAFdUlEQVR4nO2YT2xcVxXGf+fe92amxGlqk3gmbtq0wcrY00SkmhRVqsRLPK5jNZXoZhaABJEQlVA3CIkNFDkWEAkJCYklhQX/moWFEAiQjaq4I8GmqitUhOM0plCpbZwWmbii9sy8d+9hMfPKAEnsmQkoi3zSbO57d853z5/vnvPgDu6gK0j7d3uhWsXOgOlYkigi4DYg+28emx4dunuyfGhP5wv/QbwnA/3sVYDHi/kzauST6vURRKzARYFfa7Px3Rde//vGDJhZ8P9PggJQGRsZEtxPMoGdBqEeJ5uC+CAwA6EVGrG75BL9zIXLV1/qlWRP7p9JQ6vufC4MppuJLjcSd8apOZjLbN3fjF1lK3YLoTVFY+WXE0eG87Mtb/cV7h2hChagMlb47BNHRrQyll+eKh0Yut67lWLh/JNH79WJscIPOvd2g65PVGp5QhTOCHiPfv23y2+uT4+Spe3ZcpkQEO/cs83Evy9QnSodGJoDR5dp1S1BmQU/PTq0W5RjW7Hf2q3uV4DMr9KkRV6XlohnQBZX3/1z7PSlTGAGmokbB6h2abOnnEi8ikIooBv1xLaJXfdARkgEBKNBL7Z6TlppkwqtuRE5ANV2SEVueIib4n9fVX3iDsF+0TvBdk7Fzt9MNvrubnoiGDsvqhgVNFPPxTd5VVESAOO1J1vdbmpV5QMb/wBey1q5Sz9sHqYlzh/ISBXsi2BOH90zaISPNp1vJC5YBZi7sST1RzCKCKKI4ESErdVIRPi5NSZQr2dpi3MVbBXsHLgaJPVm7ou5jC14r7+rrb719gyYape9Yi/5ITMgvz90aLdmNpcygflInPifesO5C8trywDTo4V9cajPGMzXAHXKxy6sXHklJd+VsR081+ggOZvb/yURr9d2Xf32k0u4WfCTxcJxMfwiG9qRraZThRpKIqLH7gqDvY3Eu0TdFxZX3nkuighqNZJKcf+ns6GMvh+7H9YuXf3rdm3YjkIc2sGsiH4zMObcnveGhmfBV8G+cGnt5bomxxux+77Ataw1J3KhmRTk7kbi5+PEP7a48s5zVbAnavhqFQv+K4E1ZwN4AGB5GydtG+L2CbVSLCzkQnNy0/lPLF5c+00VbAk0Pf3po/cPNhtbJay1NrCvz//xrTehVTDtwtDp0cLeOOAvKHFS33qw9sbGtTRKO3HUddHZ/50+cq9WivkF/tVSGUBu0OdJOo9US2QAThbzZ7vtD7cN8Rx4Bblihs7Xk2QllwmmTo4Xvrq0RAz4KGp5klaF2o4JT16MWmtzyzRPHB6uhMY8W0+cE8y34IPesn+knoiKheNT44W/nSrt18p44TuThwb/a4JLyXWuT4zln3l8fP/6qYdGdOJw/nOd/7kddiwzabV9/HDhkaxhIRfawc04uSiY72F1YfBPa691SsjUQyP3OedPAp/KBuaUV2g0k6cXL7eKZqdy05UORhDUIKmM7n3YhOE5a2Q6MEIj8Tj1K4JcAVB0QJBjudCGKDSde9V5vnHh0tpcKjc7tdm1UHfqVmV8eNKIedp7HjUi91kjeFUEiJ1fN0ZeFuX59V1rz6c3za0W6puRVNLBPZ/fZe6xBxLRH4WWciPxn8+6+Gfzq+vvpXt6Idc3ymXCcrkcpgk/cbgw/8SREY3Ghh8FiA6Sq5ZKGfpouXoaZFK0pGaJpXRB0vlDFKD2BnVY7sdEb80CoFFp30DgzI9FZMC31UxEj4nIkFd9BZVrANZCM3Zfrl1+9w+9fP7o2YObPszeY/xTmcCgbYLOKwqExhwXaTEJjaBO8rD9vXurCCrAh/JvbyRr+x4jIUg1Q7xaAC/iXXskcA7i+tar0LqVerB3e6OvgWanH4Panrs19+7thn8CZUYrLHBcUm0AAAAASUVORK5CYII="
MUTED_MIC_ICON_B64 = "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"
APP_ICON_B64 = "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"


def embedded_icon(encoded, size=(20, 20)):
    raw = base64.b64decode(encoded)
    image = Image.open(io.BytesIO(raw)).convert("RGBA")
    return ctk.CTkImage(
        light_image=image,
        dark_image=image,
        size=size,
    )


def embedded_icon_tinted(
    encoded,
    color,
    size=(20, 20),
):
    """Crea una variante monocroma conservando el alpha del icono."""
    raw = base64.b64decode(encoded)
    image = Image.open(
        io.BytesIO(raw)
    ).convert("RGBA")

    r = int(color[1:3], 16)
    g = int(color[3:5], 16)
    b = int(color[5:7], 16)

    alpha = image.getchannel("A")
    tinted = Image.new(
        "RGBA",
        image.size,
        (r, g, b, 0),
    )
    tinted.putalpha(alpha)

    return ctk.CTkImage(
        light_image=tinted,
        dark_image=tinted,
        size=size,
    )


def interactive_parts(widget):
    """
    Devuelve el widget y las piezas Tk internas que CustomTkinter usa
    realmente para recibir el ratón (canvas, texto e imagen).
    """
    parts = [widget]

    for attr in (
        "_canvas",
        "_text_label",
        "_image_label",
    ):
        part = getattr(
            widget,
            attr,
            None,
        )

        if (
            part is not None
            and part not in parts
        ):
            parts.append(part)

    return parts


def bind_pointer_cursor(widget):
    """Cursor de mano para controles interactivos."""
    for part in interactive_parts(widget):
        try:
            part.configure(
                cursor="hand2"
            )
        except Exception:
            pass


def bind_interaction_event(
    widget,
    sequence,
    callback,
):
    """
    Enlaza un evento al CTkButton y también a sus elementos internos.
    Esto evita que texto/iconos tengan un comportamiento distinto al fondo.
    """
    for part in interactive_parts(widget):
        try:
            part.bind(
                sequence,
                callback,
                add="+",
            )
        except Exception:
            pass


def bind_hover_content(
    widget,
    *,
    normal_text_color=None,
    hover_text_color=None,
    normal_image=None,
    hover_image=None,
):
    """
    El fondo mantiene el hover oscuro del CTkButton y el contenido
    (texto/icono) cambia a crema para conservar contraste.
    """

    def on_enter(_event=None):
        effective_hover_text_color = (
            hover_text_color
            if hover_text_color is not None
            else COLOR_CREAM
        )

        if normal_text_color is not None:
            try:
                widget.configure(
                    text_color=effective_hover_text_color
                )
            except Exception:
                pass

        if hover_image is not None:
            try:
                widget.configure(
                    image=hover_image
                )
            except Exception:
                pass

    def on_leave(_event=None):
        if normal_text_color is not None:
            try:
                widget.configure(
                    text_color=normal_text_color
                )
            except Exception:
                pass

        if normal_image is not None:
            try:
                widget.configure(
                    image=normal_image
                )
            except Exception:
                pass

    bind_pointer_cursor(widget)

    bind_interaction_event(
        widget,
        "<Enter>",
        on_enter,
    )
    bind_interaction_event(
        widget,
        "<Leave>",
        on_leave,
    )



class ToolTip:
    def __init__(self, widget, text):
        self.widget = widget
        self.text = text
        self.window = None
        self.after_id = None

        bind_interaction_event(
            widget,
            "<Enter>",
            self._schedule,
        )
        bind_interaction_event(
            widget,
            "<Leave>",
            self._hide,
        )
        bind_interaction_event(
            widget,
            "<ButtonPress>",
            self._hide,
        )

    def _schedule(self, _event=None):
        self._cancel()
        self.after_id = self.widget.after(450, self._show)

    def _cancel(self):
        if self.after_id is not None:
            try:
                self.widget.after_cancel(self.after_id)
            except Exception:
                pass
            self.after_id = None

    def _show(self):
        self._cancel()

        if self.window is not None:
            return

        x = self.widget.winfo_rootx() + self.widget.winfo_width() // 2
        y = self.widget.winfo_rooty() + self.widget.winfo_height() + 6

        self.window = tk.Toplevel(self.widget)
        self.window.wm_overrideredirect(True)
        self.window.wm_geometry(f"+{x}+{y}")

        label = tk.Label(
            self.window,
            text=self.text,
            background="#202020",
            foreground="white",
            relief="solid",
            borderwidth=0,
            padx=8,
            pady=4,
            font=("Segoe UI", 9),
        )
        label.pack()

    def set_text(self, text):
        self.text = text
        self._hide()

    def _hide(self, _event=None):
        self._cancel()
        if self.window is not None:
            self.window.destroy()
            self.window = None


settings_icon = None
transcript_icon = None
folder_icon = None
stop_icon = None


# ============================================================
# PALETA VISUAL
# ============================================================

COLOR_BURGUNDY = "#6F1D1B"
COLOR_TAN = "#BB9457"
COLOR_DARK_BROWN = "#432818"
COLOR_SIENNA = "#99582A"
COLOR_CREAM = "#FFE6A7"

COLOR_BUTTON = COLOR_SIENNA
COLOR_BUTTON_HOVER = COLOR_BURGUNDY
COLOR_BUTTON_TEXT = COLOR_CREAM

COLOR_SECONDARY_BUTTON = COLOR_TAN
COLOR_SECONDARY_HOVER = COLOR_SIENNA
COLOR_SECONDARY_TEXT = COLOR_DARK_BROWN

COLOR_FRAME_LIGHT = "#F4EFE6"
COLOR_FRAME_DARK = "#31251F"
COLOR_DISABLED = "#D8CDBD"

COLOR_APP_BG = "#F8F6F1"
COLOR_SETTINGS_BG = "#F8F6F1"
COLOR_PANEL = "#F1EDE6"
COLOR_STAGE = "#ECE6DB"

# Mismo aspecto visual que el botón Stop cuando está deshabilitado.
COLOR_DISABLED_CONTROL = "#C9BBA8"
# Los iconos deshabilitados siguen en crema como Stop.
# El texto necesita más contraste sobre el mismo fondo.
COLOR_DISABLED_ICON = COLOR_CREAM
COLOR_DISABLED_TEXT = COLOR_CREAM


class BorderedContainer(tk.Frame):
    """Marco uniforme de 1 px para dropdowns, inputs y textareas."""

    def __init__(
        self,
        parent,
        *,
        width,
        height,
        border_color=COLOR_TAN,
        inner_color=COLOR_PANEL,
        border_size=1,
    ):
        self.field_width = int(width)
        self.field_height = int(height)
        self.border_size = int(border_size)

        super().__init__(
            parent,
            width=self.field_width,
            height=self.field_height,
            bg=border_color,
            bd=0,
            highlightthickness=0,
        )
        self.pack_propagate(False)

        self.inner = tk.Frame(
            self,
            width=self.field_width - 2 * self.border_size,
            height=self.field_height - 2 * self.border_size,
            bg=inner_color,
            bd=0,
            highlightthickness=0,
        )
        self.inner.place(
            x=self.border_size,
            y=self.border_size,
        )
        self.inner.pack_propagate(False)


BUTTON_VARIANTS = {
    "secondary": {
        "fg": COLOR_TAN,
        "hover": COLOR_SIENNA,
        "text": COLOR_DARK_BROWN,
        "hover_text": COLOR_CREAM,
    },
    "dark": {
        "fg": COLOR_DARK_BROWN,
        "hover": COLOR_BURGUNDY,
        "text": COLOR_CREAM,
        "hover_text": COLOR_CREAM,
    },
    "microphone": {
        "fg": COLOR_CREAM,
        "hover": COLOR_TAN,
        "text": COLOR_DARK_BROWN,
        "hover_text": COLOR_CREAM,
    },
    "primary": {
        "fg": COLOR_SIENNA,
        "hover": COLOR_BURGUNDY,
        "text": COLOR_CREAM,
        "hover_text": COLOR_CREAM,
    },
    "result": {
        "fg": COLOR_TAN,
        "hover": COLOR_SIENNA,
        "text": COLOR_DARK_BROWN,
        "hover_text": COLOR_CREAM,
    },
}


class ThemedButton(ctk.CTkButton):
    """Botón reutilizable con comportamiento visual definido por variante."""

    def __init__(
        self,
        master,
        *,
        variant="secondary",
        normal_image=None,
        hover_image=None,
        **kwargs,
    ):
        style = BUTTON_VARIANTS[variant]

        self._normal_bg = style["fg"]
        self._hover_bg = style["hover"]
        self._normal_text = style["text"]
        self._hover_text = style["hover_text"]
        self._normal_image = normal_image
        self._hover_image = hover_image or normal_image

        kwargs.setdefault("fg_color", self._normal_bg)
        kwargs.setdefault("hover_color", self._hover_bg)
        kwargs.setdefault("text_color", self._normal_text)

        if normal_image is not None:
            kwargs["image"] = normal_image

        super().__init__(master, **kwargs)

        self.after_idle(self._install_interactions)

    def _parts(self):
        parts = [self]

        for attr in (
            "_canvas",
            "_text_label",
            "_image_label",
        ):
            part = getattr(self, attr, None)

            if part is not None and part not in parts:
                parts.append(part)

        return parts

    def _install_interactions(self):
        for part in self._parts():
            try:
                part.configure(cursor="hand2")
            except Exception:
                pass

            try:
                part.bind(
                    "<Enter>",
                    self._on_enter_theme,
                    add="+",
                )
                part.bind(
                    "<Leave>",
                    self._on_leave_theme,
                    add="+",
                )
            except Exception:
                pass

    def _disabled(self):
        try:
            return self.cget("state") == "disabled"
        except Exception:
            return False

    def _on_enter_theme(self, _event=None):
        if self._disabled():
            return

        try:
            self.configure(
                fg_color=self._hover_bg,
                text_color=self._hover_text,
            )
        except Exception:
            pass

        if self._hover_image is not None:
            try:
                self.configure(image=self._hover_image)
            except Exception:
                pass

    def _on_leave_theme(self, _event=None):
        if self._disabled():
            return

        try:
            self.configure(
                fg_color=self._normal_bg,
                text_color=self._normal_text,
            )
        except Exception:
            pass

        if self._normal_image is not None:
            try:
                self.configure(image=self._normal_image)
            except Exception:
                pass

    def _set_pointer(self, enabled):
        cursor = "hand2" if enabled else ""

        for part in self._parts():
            try:
                part.configure(cursor=cursor)
            except Exception:
                pass

    def set_images(self, normal_image, hover_image=None):
        self._normal_image = normal_image
        self._hover_image = hover_image or normal_image

        try:
            self.configure(image=self._normal_image)
        except Exception:
            pass


class ResultButton(ThemedButton):
    """
    Botón de resultado con dos estados visuales compartidos:

    unavailable:
      mismo fondo que Stop deshabilitado;
      iconos y texto en crema, exactamente el mismo color;
      sin hover ni cursor de acción.

    available:
      todos los resultados usan exactamente la variante "result".
    """

    def __init__(
        self,
        master,
        *,
        normal_image=None,
        hover_image=None,
        disabled_image=None,
        **kwargs,
    ):
        self._result_available = False
        self._result_disabled_image = (
            disabled_image
            if disabled_image is not None
            else hover_image
        )

        # Creamos el CTkButton en estado normal y aplicamos el estado visual
        # unavailable nosotros mismos para tener control total del estilo.
        kwargs.pop("state", None)

        super().__init__(
            master,
            variant="result",
            normal_image=normal_image,
            hover_image=hover_image,
            state="normal",
            **kwargs,
        )

        self.set_available(False)

    def _disabled(self):
        return not self._result_available

    def _apply_unavailable_style(self):
        try:
            super().configure(
                state="disabled",
                fg_color=COLOR_DISABLED_CONTROL,
                hover_color=COLOR_DISABLED_CONTROL,
                text_color=COLOR_DISABLED_TEXT,
                text_color_disabled=COLOR_DISABLED_TEXT,
            )
        except Exception:
            pass

        if self._result_disabled_image is not None:
            try:
                super().configure(
                    image=self._result_disabled_image
                )
            except Exception:
                pass

        self._set_pointer(False)

    def _apply_available_style(self):
        try:
            super().configure(
                state="normal",
                fg_color=self._normal_bg,
                hover_color=self._hover_bg,
                text_color=self._normal_text,
            )
        except Exception:
            pass

        if self._normal_image is not None:
            try:
                super().configure(
                    image=self._normal_image
                )
            except Exception:
                pass

        self._set_pointer(True)

    def set_available(self, available):
        self._result_available = bool(available)

        if self._result_available:
            self._apply_available_style()
        else:
            self._apply_unavailable_style()

    @property
    def available(self):
        return self._result_available


def style_option_menu(widget):
    # Kept for backward compatibility with older layout code.
    # New settings dropdowns use StyledDropdown below.
    try:
        widget.configure(
            fg_color=COLOR_PANEL,
            button_color=COLOR_PANEL,
            button_hover_color=COLOR_CREAM,
            text_color=COLOR_DARK_BROWN,
            dropdown_fg_color=COLOR_PANEL,
            dropdown_hover_color=COLOR_CREAM,
            dropdown_text_color=COLOR_DARK_BROWN,
            dynamic_resizing=False,
        )
    except Exception:
        pass


class StyledDropdown:
    """
    Dropdown visualmente integrado con los inputs de Settings.

    - Borde nativo nítido de 1 px COLOR_TAN.
    - Fondo COLOR_PANEL.
    - Flecha SVG personalizada.
    - Popup con exactamente el mismo ancho que el select.
    - Hover del texto claro; botón de flecha en COLOR_TAN.
    """

    def __init__(
        self,
        parent,
        variable,
        values,
        width=490,
        height=30,
        command=None,
        icon_resolver=None,
    ):
        self.variable = variable
        self.values = list(values)
        self.width = int(width)
        self.height = int(height)
        self.command = command
        self.icon_resolver = icon_resolver
        self.state = "normal"
        self.popup = None

        self.frame = BorderedContainer(
            parent,
            width=self.width,
            height=self.height,
        )

        self.text_button = ctk.CTkButton(
            self.frame.inner,
            text=self.variable.get(),
            anchor="w",
            width=self.width - 30,
            height=self.height - 2,
            corner_radius=0,
            border_width=0,
            fg_color=COLOR_PANEL,
            hover_color=COLOR_CREAM,
            text_color=COLOR_DARK_BROWN,
            command=self._toggle,
        )
        self.text_button.place(
            x=0,
            y=0,
        )

        self.arrow_button = ctk.CTkButton(
            self.frame.inner,
            text="",
            image=dropdown_arrow_icon,
            width=28,
            height=self.height - 2,
            corner_radius=0,
            border_width=0,
            fg_color=COLOR_PANEL,
            # En hover el fondo hace match exacto con el borde.
            hover_color=COLOR_TAN,
            command=self._toggle,
        )
        self.arrow_button.place(
            x=self.width - 30,
            y=0,
        )

        bind_pointer_cursor(
            self.text_button
        )
        bind_pointer_cursor(
            self.arrow_button
        )

        self._trace_id = self.variable.trace_add(
            "write",
            self._sync_text,
        )

        self._sync_text()

    def _icon_for(self, value):
        if self.icon_resolver is None:
            return None

        try:
            return self.icon_resolver(value)
        except Exception:
            return None

    def _sync_text(self, *_):
        try:
            value = self.variable.get()
            icon = self._icon_for(value)

            self.text_button.configure(
                text=value,
                image=icon,
                compound="left",
            )
        except Exception:
            pass

    def pack(self, *args, **kwargs):
        return self.frame.pack(
            *args,
            **kwargs,
        )

    def place(self, *args, **kwargs):
        return self.frame.place(
            *args,
            **kwargs,
        )

    def grid(self, *args, **kwargs):
        return self.frame.grid(
            *args,
            **kwargs,
        )

    def configure(self, **kwargs):
        if "values" in kwargs:
            self.values = list(
                kwargs.pop("values")
            )

        if "state" in kwargs:
            self.state = kwargs.pop(
                "state"
            )
            button_state = (
                "normal"
                if self.state == "normal"
                else "disabled"
            )

            self.text_button.configure(
                state=button_state
            )
            self.arrow_button.configure(
                state=button_state
            )

        if kwargs:
            self.frame.configure(
                **kwargs
            )

    config = configure

    def _close_popup(self):
        if self.popup is not None:
            try:
                self.popup.destroy()
            except Exception:
                pass

            self.popup = None

    def _select(self, value):
        self.variable.set(value)
        self._close_popup()

        if self.command is not None:
            self.command(value)

    def _toggle(self):
        if self.state != "normal":
            return

        if (
            self.popup is not None
            and self.popup.winfo_exists()
        ):
            self._close_popup()
            return

        self._show_popup()

    def _show_popup(self):
        if not self.values:
            return

        self.frame.update_idletasks()

        x = self.frame.winfo_rootx()
        y = (
            self.frame.winfo_rooty()
            + self.frame.winfo_height()
            + 2
        )

        visible_rows = min(
            len(self.values),
            8,
        )
        row_height = 30
        popup_height = (
            visible_rows * row_height
            + 2
        )

        self.popup = tk.Toplevel(
            self.frame
        )
        self.popup.overrideredirect(True)
        self.popup.attributes(
            "-topmost",
            True,
        )
        self.popup.geometry(
            f"{self.width}x{popup_height}"
            f"+{x}+{y}"
        )

        outer = tk.Frame(
            self.popup,
            bg=COLOR_PANEL,
            bd=0,
            relief="flat",
            highlightthickness=1,
            highlightbackground=COLOR_TAN,
            highlightcolor=COLOR_TAN,
        )
        outer.pack(
            fill="both",
            expand=True,
        )

        if len(self.values) > 8:
            body = ctk.CTkScrollableFrame(
                outer,
                fg_color=COLOR_PANEL,
                corner_radius=0,
                scrollbar_button_color=COLOR_TAN,
                scrollbar_button_hover_color=COLOR_CREAM,
            )
            body.pack(
                padx=1,
                pady=1,
                fill="both",
                expand=True,
            )
        else:
            body = ctk.CTkFrame(
                outer,
                fg_color=COLOR_PANEL,
                corner_radius=0,
            )
            body.pack(
                padx=1,
                pady=1,
                fill="both",
                expand=True,
            )

        current = self.variable.get()

        for value in self.values:
            option = ctk.CTkButton(
                body,
                text=value,
                image=self._icon_for(value),
                compound="left",
                anchor="w",
                height=row_height,
                corner_radius=0,
                fg_color=(
                    COLOR_STAGE
                    if value == current
                    else COLOR_PANEL
                ),
                hover_color=COLOR_CREAM,
                text_color=COLOR_DARK_BROWN,
                command=(
                    lambda v=value:
                    self._select(v)
                ),
            )
            option.pack(
                fill="x",
                padx=0,
                pady=0,
            )
            bind_pointer_cursor(
                option
            )

        try:
            self.popup.focus_force()
            self.popup.bind(
                "<Escape>",
                lambda _event:
                self._close_popup(),
            )
            self.popup.bind(
                "<FocusOut>",
                lambda _event:
                self.popup.after(
                    100,
                    self._close_popup,
                )
                if self.popup is not None
                else None,
            )
        except Exception:
            pass


def style_primary_button(widget):
    widget.configure(
        fg_color=COLOR_BUTTON,
        hover_color=COLOR_BUTTON_HOVER,
        text_color=COLOR_BUTTON_TEXT,
    )


def style_secondary_button(widget):
    widget.configure(
        fg_color=COLOR_SECONDARY_BUTTON,
        hover_color=COLOR_SECONDARY_HOVER,
        text_color=COLOR_SECONDARY_TEXT,
    )


def resolved_widget_bg(widget):
    current = widget

    while current is not None:
        try:
            fg = current.cget("fg_color")

            if fg != "transparent":
                return current._apply_appearance_mode(fg)
        except Exception:
            pass

        current = getattr(current, "master", None)

    return "#D9D9D9"


class CircularActionButton(tk.Label):
    """
    Botón circular con imágenes PNG antialias.
    Evita el aspecto pixelado de Canvas/Tk en diagonales y curvas.
    """

    def __init__(
        self,
        master,
        diameter,
        icon,
        command,
        fg_color=None,
        hover_color=None,
        icon_color=None,
        disabled_color=None,
    ):
        self.command = command
        self._state = "normal"

        try:
            self.configure(
                cursor="hand2"
            )
        except Exception:
            pass

        if icon == "play":
            normal_b64 = "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"
            hover_b64 = "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"
            disabled_b64 = "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"
        else:
            normal_b64 = "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"
            hover_b64 = "iVBORw0KGgoAAAANSUhEUgAAADgAAAA4CAYAAACohjseAAAJM0lEQVR4nNWaa4xdVRXHf2vtc+fVplAgM9NqJCVpZxiqhVQgkujtw1KMjQbN/eIjPJRKAP0kJnzQcWJiDCEEjZ9sFBQfjfPVxELp4xresQZKHaZ1rEII0xZpaxmZ9t6z1/LDOXd6OzPt3Dszff2Tk5uce84+63/W2muvs/ZfmH9IP8juIgrQWcYHIU53YQnCkSJSd50BPq/GzNdA/aC7i2i5TDrdf6+tunbR8ePAlXAlcPj9eOrld94Zn3xtqURgEOaL7JwJliDUG1MskrS/170iRW8Vs1UOHzd8qSJL3HEEBMTcT6jKCM5bIK+r2ost7brvT3tGP5wYu0QYHJwb0bkQ1PzBDnDHys5PxFS+bvB5kBVJ0CCAu2MOPslGQRABEUGAaI67vy3CDnH5wxXDoztroZ2/xGnD/LwQrH/g7b1LNpj4w8C6RDVEc8wdd6KAe2a/yNRnuYNLjbkQVESCSn6/7zX8p6Ny9W+HhoYq/aADdS/0fBGc8NqG3q6VDo+q6ucg8wBO6oLmZGbz8gzHXAiJiIgK0WyfwPe2v3noz9C8Nxs2on7g9T1dPxSRR1SlJTU3HBdBmxlvJnhG1oNKEAFz3+quD+0Yfvf9Zkg2ZFBtwE/3XrOkjeTJEHRjNVoWhkKYG5Vzw7MERktQjdH+UTW5Z/eB0ReKRZLpMvZk6EwXFIskgxDXr+i+uU2SPaqysZpaCvj5JgcgoAJaTS1FZXlL8N1re5fcVy6TFoskDdx/dtQ8t35F980aeAaRxRY9RWYe+HzAIQpooiIVY/Ou4dEtM4XrWT3YDzoIcd3yrls08AzI4hg9XixyAEIWMal5bFF+sba3875BiCXOHkln86ACvm5lZ6emuldUOqN5lHMMdIHhgCUqoWrx9p3DR7afzZPTerCUE5eqPh1UOi16egmRAxAHie6u6NPrVnZ25eSm8JlyolTK3sS63u4fFBLdkF7EOXcuCKg5FoJ0SVWfBrQ0TUSecSKvFmxtz9KeROwNy7w220X7wsBJC4km1Wh37xg+9OvJoTptiCrxZ6pS8CzWGyYnIvNyNAVBUzMT+Mn63qVX54X/xCATBEsQBsDW9XStDUFvrzaZVFSFNI2kaZr/zvKopjnRhimqG5YE7cbjtwEv1fGamFt9eRErIt+fTTyOj5/iqqsWIapZGT1biHDs6AlUlSTRxoYSQmrmAg8VVyz5+eCB0ffJvOg1LgrY2uu7bkhcXqubezNCVRgfP8WXvlzkGw/eCZUq2myY5TAztKON3dte5fHHtqI6Y6E1AXdiS6Ihjb75ueHRLUVIypAmAMXsS9zUuScETSy1hjKniFCtRhYvXsQDD95J6xULoFKlmfiayhI2fe2z7Nr5N156cR8LF3ZgZjPflxXk7u7fBH65BqxMHqvlMrGvr68F2GRmIDPXqPVQFSqVKl6pYmnE53DENMXGK00nG4EQ3RHhxrU9S5cPgPWDan9etXTF48sEWRazudgUQZi/DCoiqM4uAtywELRFiJ8C2F1Etdb9CsTbQtAWNxqIh0sTcjqprAGgDDo2liUTg0/mK/q8tu0uJFxQcweRVatXry6sAdPr9uQeE+lxd/IeymWJvFsHsKy1daxtAEz7wIvFa9vAl+Zly2VLEMDBVWgt/OfERyCvPeNRbwf5WM7+ciYo7ngQaUtC/Cjk2TKouED14to2f3BAXKswi+XgcoMCRHPBL71vvjnBLIGMoIQ4XnXhWD75LttlArIk6e5V1XAcsq9gLQ+9N4b7QRXBL2+CLoJE56QtaB8B0CO1/ovwNggyeZfk8kJWyTiHqgvjKQfRsdX5suD8dS4fAZcCHDyIIPgb5fJbJ9dA0E2bsv5F1eWlaBb9AnSrzxfEcUTA/XmAsdWIDgxkIdna5kPmfihkpVrTYWo2f5Ft2V5h0/e5EKKZVdHnATZtIirgpRJh+97D/wN2BlXw5jYbRYSOjlbcHJvrEQ1tbyEpJM2StCCImR/ssIV7ARkYwLKFfjC/wnjK3N0b/OB1d5IkcPToCZ7Z9ipyxQJCRyva0YoumMXR3kJyzSKGX93P0N//RXt7a+MkHQtZi+N320ZGThXzhpmc/h9ZUyyGcGh4bxK0N2aF6YxERbJumgjceNOK/GO1qW7jGS9MksCBoX9z7NgYbW0tjRLMG2ZUYjXesGvkvX/mttuEFRM7Sb3ddxWCPlVJreG9v2xew4cfnmya1HSmtrW3EEJo2Hu1hlMljb/Zuf/wXfXN34nybDDvYbySLtxa5YNHQtAV0dykAS/W7Fi4sGNWnKaO11SScRFIo1Wc8GNA+uqSZL3xPgSybWTklKHfUZrPpmY2L0dTycWJhaDB8cd27X93f6mUfQLW/p4yUXJtSlzf0/3HQqKlaoMtxIsBBwsq6uYHk7Ry060jR8cmKzGmhN/gYBaqIST3p9EOqkpwLslGlCsY7rGKfXXbyNETtfP1F003vxzg2aF3jqbiXyETJ0258aIj31WK0b5bHj7ychGSgWkcMW0CGQArQdj95uFXUrdvJSrKeRDKzRpO2pJooVK1LTsPHHmi1qaf7tKzZshBiEVIdg0f2VIxNicqAbCLHK6OUy0kmlRS27Jj/6HN/aDl2YgQAMpkUo1dw6MTJFVEvclSbj4woZcpaKGak8u3/M4p72qo3KiFwJqeJRsT9SeDypI09RRpfBdqDnCcqEES3FN3efi54dEnGtWuNS3luq3n6qXtUvhVIejG1By380bU3TERQiEo0ewAUe7dfmD0hWaEeU0ZdYZe7fquexX5UVBdmpphRqwpJJsdtx41jZoIIQlKjFYBeZTKyceeO3jsv41KuGqYjSG1e3zDdV2dtPIAyP1Btcvcc93naSllXak33bM8l5K6ZOvRhKQyNTslzu/BH98+fHgfnBZJzMbYplGreADWLevsStrDF8z9bpxbQtAks35CO8rk7FvTwgq5/lKkJskcQmxQom599sDocN2zZrVMzXXeSKmE1ogC3NHX3Wforeb2GVxucHy5w6KCitasEyA64H7c8UMi8iYuf1G1F68YOrynXunbB96s184wcE706sbJiU55y19cde2V4+OVbilYd5rPnASIyNiCttaRltff+mCyBKtUIvQNzo3YeUM/aLFIkksdG9ajFoskpSbED43i/+7BGVE0rtagAAAAAElFTkSuQmCC"
            disabled_b64 = "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"

        self._normal_img = tk.PhotoImage(data=normal_b64)
        self._hover_img = tk.PhotoImage(data=hover_b64)
        self._disabled_img = tk.PhotoImage(data=disabled_b64)

        super().__init__(
            master,
            image=self._normal_img,
            bd=0,
            highlightthickness=0,
            bg=resolved_widget_bg(master),
            cursor="hand2",
        )

        self.bind("<Enter>", self._on_enter, add="+")
        self.bind("<Leave>", self._on_leave, add="+")
        self.bind("<Button-1>", self._on_click, add="+")

    def _on_enter(self, _event=None):
        if self._state != "disabled":
            self.configure(image=self._hover_img)

    def _on_leave(self, _event=None):
        if self._state == "disabled":
            self.configure(image=self._disabled_img)
        else:
            self.configure(image=self._normal_img)

    def _on_click(self, _event=None):
        if self._state != "disabled" and self.command:
            self.command()

    def configure(self, cnf=None, **kwargs):
        if cnf is not None:
            return super().configure(cnf, **kwargs)

        state = kwargs.pop("state", None)

        if state is not None:
            self._state = state

            if state == "disabled":
                kwargs["image"] = self._disabled_img
                kwargs["cursor"] = ""
            else:
                kwargs["image"] = self._normal_img
                kwargs["cursor"] = "hand2"

        return super().configure(**kwargs)

    config = configure


def fixed_menu(
    parent,
    variable,
    values,
    command=None,
    icon_resolver=None,
):
    """Dropdown de 490 px con popup del mismo ancho y estilo."""
    menu = StyledDropdown(
        parent,
        variable=variable,
        values=values,
        width=490,
        height=30,
        command=command,
        icon_resolver=icon_resolver,
    )

    return menu.frame, menu


# ============================================================
# COMPROBACION DE ENTORNO AL ARRANCAR
# ============================================================

def check_output_folder():
    """Comprueba que la carpeta de salida se puede crear y escribir."""
    output_root = Path(
        config.get(
            "output_folder",
            str(APP_FOLDER / "meetings"),
        )
    ).expanduser()

    try:
        output_root.mkdir(parents=True, exist_ok=True)

        with tempfile.NamedTemporaryFile(
            dir=output_root,
            prefix=".meeting_assistant_",
            delete=True,
        ):
            pass

        return True, str(output_root)

    except Exception as e:
        return False, str(e)


def startup_check_worker():
    messages.put(
        (
            "startup_status",
            tr("checking_ollama"),
        )
    )

    ollama_result = ensure_ollama_running()

    messages.put(
        (
            "startup_status",
            tr("checking_folder"),
        )
    )

    output_ok, output_detail = check_output_folder()

    messages.put(
        (
            "startup_result",
            {
                "ollama": ollama_result,
                "output_ok": output_ok,
                "output_detail": output_detail,
            },
        )
    )

def begin_startup_checks():
    start_button.configure(state="disabled")
    stop_button.configure(state="disabled")
    settings_button.configure(state="disabled")
    status_label.configure(
        text=tr("checking_environment")
    )

    threading.Thread(
        target=startup_check_worker,
        daemon=True,
    ).start()

def finish_startup_checks(result):
    refresh_audio_devices()
    refresh_whisper_models()
    refresh_ollama_models()

    problems = []
    ollama_result = result["ollama"]

    if not ollama_result["ok"]:
        problems.append(
            ollama_result["message"]
        )
    elif not ollama_result["models"]:
        problems.append(
            tr("startup_no_models")
        )

    if not result["output_ok"]:
        problems.append(
            tr(
                "startup_bad_folder",
                detail=result["output_detail"],
            )
        )

    if not current_microphone_candidates():
        problems.append(
            tr("startup_no_mic")
        )

    if not system_device_map:
        problems.append(
            tr("startup_no_loopback")
        )

    settings_button.configure(state="normal")

    if problems:
        start_button.configure(state="disabled")
        status_label.configure(
            text=tr("review_required")
        )

        messagebox.showwarning(
            tr("startup_title"),
            "\n\n".join(
                f"• {item}"
                for item in problems
            ),
        )
        return

    start_button.configure(state="normal")

    status_label.configure(
        text=(
            tr("ready_ollama_started")
            if ollama_result["started"]
            else tr("ready")
        )
    )

def update_mute_button():
    if mic_muted:
        mute_button.set_images(
            muted_mic_icon,
            muted_mic_icon_hover,
        )
    else:
        mute_button.set_images(
            mic_icon,
            mic_icon_hover,
        )

def toggle_microphone_mute():
    global mic_muted

    if not recording:
        return

    mic_muted = not mic_muted
    update_mute_button()

    status_label.configure(
        text=(
            tr("recording_muted")
            if mic_muted
            else tr("recording")
        )
    )

# ============================================================
# INICIAR / FINALIZAR
# ============================================================

def start_recording():
    global stop_event
    global mic_muted
    global pending_meeting_title
    global current_mic_display
    global current_system_display
    global current_mic_is_fallback
    global mic_thread
    global system_thread
    global recording
    global recording_started_at
    global start_times
    global current_folder
    global transcript_file
    global summary_file
    global selected_whisper_model
    global selected_ollama_model
    global selected_summary_type
    global selected_transcription_language

    if recording or processing:
        return

    current_mic_display = ""
    current_system_display = ""
    current_mic_is_fallback = False
    mic_muted = False
    pending_meeting_title = ""

    running, models, _ = ollama_runtime_status()

    if not running:
        messagebox.showwarning(
            tr("ollama_missing_title"),
            tr("ollama_stopped"),
        )
        return

    if (
        not models
        or ollama_var.get() not in models
    ):
        messagebox.showwarning(
            tr("ollama_model_title"),
            tr("ollama_model_missing"),
        )
        return

    refresh_audio_devices()

    if not current_microphone_candidates():
        messagebox.showwarning(
            tr("microphone_title"),
            tr("microphone_missing"),
        )
        return

    if not system_device_map:
        messagebox.showwarning(
            tr("computer_audio_title"),
            tr("computer_audio_missing"),
        )
        return

    if (
        settings_system_var.get()
        not in system_device_map
    ):
        messagebox.showwarning(
            tr("meeting_audio_title"),
            tr("meeting_audio_select"),
        )
        return

    selected_whisper_model = parse_whisper_value(
        whisper_var.get()
    )
    selected_ollama_model = ollama_var.get()
    selected_summary_type = (
        summary_type_id_from_display(
            summary_type_var.get()
        )
    )

    transcription_setting = config.get(
        "transcription_language",
        "auto",
    )

    selected_transcription_language = (
        None
        if transcription_setting == "auto"
        else transcription_setting
    )

    write_config()
    set_model_controls(False)

    start_times = {}
    reset_stages()
    apply_stage("audio", "working")

    output_root = Path(
        config["output_folder"]
    ).expanduser()

    try:
        output_root.mkdir(
            parents=True,
            exist_ok=True,
        )
    except Exception:
        messagebox.showerror(
            tr("folder_title"),
            tr("folder_unavailable"),
        )
        set_model_controls(True)
        return

    timestamp = datetime.now().strftime(
        "%Y-%m-%d_%H-%M-%S"
    )
    current_folder = output_root / timestamp

    try:
        current_folder.mkdir()
    except FileExistsError:
        current_folder = (
            output_root
            / f"{timestamp}_{int(time.time())}"
        )
        current_folder.mkdir()

    transcript_file = (
        current_folder / "transcript.txt"
    )
    summary_file = (
        current_folder / "summary.md"
    )

    mic_file = (
        current_folder / "microphone.wav"
    )
    system_file = (
        current_folder / "system_audio.wav"
    )

    stop_event = threading.Event()

    mic_thread = threading.Thread(
        target=record_microphone,
        args=(mic_file,),
        daemon=True,
    )

    system_thread = threading.Thread(
        target=record_system_audio,
        args=(system_file,),
        daemon=True,
    )

    mic_thread.start()
    system_thread.start()

    recording = True
    recording_started_at = time.time()

    start_button.configure(state="disabled")
    stop_button.configure(state="normal")
    mute_button.configure(state="normal")
    update_mute_button()

    transcript_button.set_available(False)
    summary_button.set_available(False)
    folder_button.set_available(False)

    status_label.configure(
        text=tr("recording")
    )
    timer_label.configure(text="00:00:00")

    mic_audio_label.configure(
        text=tr("microphone_connecting")
    )
    system_audio_label.configure(
        text=tr("computer_audio_connecting")
    )

    update_timer()

def stop_recording():
    global recording
    global processing
    global pending_meeting_title
    global mic_muted

    if not recording:
        return

    recording = False
    processing = True
    stop_event.set()

    stop_button.configure(state="disabled")
    mute_button.configure(state="normal")
    mic_muted = False

    status_label.configure(
        text=tr("finalizing_recording")
    )

    entered_name = simpledialog.askstring(
        tr("save_meeting_title"),
        tr("meeting_name_optional"),
        parent=root,
    )

    pending_meeting_title = sanitize_name(
        entered_name or ""
    )

    progress_bar.pack(
        pady=(3, 6),
        padx=35,
        fill="x",
    )
    progress_bar.start()

    threading.Thread(
        target=process_meeting,
        daemon=True,
    ).start()

def update_timer():
    if not recording:
        return

    elapsed = time.time() - recording_started_at
    timer_label.configure(text=format_time(elapsed))
    root.after(500, update_timer)


# ============================================================
# RESULTADOS
# ============================================================

def open_transcript():
    open_path(transcript_file)


def open_summary():
    open_path(summary_file)


def open_folder():
    open_path(current_folder)


# ============================================================
# CONFIGURACION
# ============================================================

def browse_output_folder():
    folder = filedialog.askdirectory(
        initialdir=settings_output_var.get() or str(APP_FOLDER)
    )

    if folder:
        settings_output_var.set(folder)


def save_settings():
    summary_id = summary_type_id_from_display(
        summary_type_var.get()
    )

    custom_prompt = settings_custom_prompt.get(
        "1.0",
        "end",
    ).strip()

    if (
        summary_id == "custom"
        and not custom_prompt
    ):
        messagebox.showwarning(
            tr("custom_prompt_title"),
            tr("custom_prompt_required"),
            parent=settings_window,
        )
        return

    write_config()
    refresh_ui_language()
    update_audio_summary()
    settings_window.withdraw()

def show_settings():
    refresh_audio_devices()
    refresh_whisper_models()
    refresh_ollama_models()

    app_language_var.set(
        language_display(
            config.get("language", "en")
        )
    )

    transcription_language_var.set(
        transcription_language_display(
            config.get(
                "transcription_language",
                "auto",
            )
        )
    )

    summary_type_var.set(
        summary_type_display(
            config.get(
                "summary_type",
                "meeting_minutes",
            )
        )
    )

    settings_output_var.set(
        config.get(
            "output_folder",
            str(APP_FOLDER / "meetings"),
        )
    )

    settings_keep_audio_var.set(
        bool(config.get("keep_audio", True))
    )

    settings_custom_prompt.delete(
        "1.0",
        "end",
    )
    settings_custom_prompt.insert(
        "1.0",
        config.get(
            "custom_summary_prompt",
            DEFAULT_CONFIG["custom_summary_prompt"],
        ),
    )

    settings_window.deiconify()
    settings_window.lift()
    settings_window.focus_force()

def refresh_ui_language():
    language = current_language()

    settings_window.title(
        tr("settings")
    )

    language_section_label.configure(
        text=tr("language")
    )
    app_language_label.configure(
        text=tr("application_language")
    )
    transcription_language_label.configure(
        text=tr("transcription_language")
    )

    processing_section_label.configure(
        text=tr("processing")
    )
    whisper_model_label.configure(
        text=tr("whisper_model")
    )
    ai_model_label.configure(
        text=tr("ai_model")
    )
    summary_type_label.configure(
        text=tr("summary_type")
    )

    audio_section_label.configure(
        text=tr("audio")
    )
    microphone_settings_label.configure(
        text=tr("microphone")
    )
    computer_audio_settings_label.configure(
        text=tr("computer_audio")
    )
    refresh_audio_button.configure(
        text=tr("update_devices")
    )

    files_section_label.configure(
        text=tr("files_privacy")
    )
    save_location_label.configure(
        text=tr("save_meetings_in")
    )
    keep_audio_switch.configure(
        text=tr("keep_audio")
    )

    custom_prompt_label.configure(
        text=tr("custom_prompt")
    )
    save_settings_button.configure(
        text=tr("save_settings")
    )

    summary_button.configure(
        text=tr("stage_summary")
    )

    current_summary = (
        summary_type_id_from_display(
            summary_type_var.get()
        )
    )

    summary_type_menu.configure(
        values=summary_type_display_values(
            language
        )
    )
    summary_type_var.set(
        summary_type_display(
            current_summary,
            language,
        )
    )

    current_transcription = (
        transcription_language_id_from_display(
            transcription_language_var.get()
        )
    )

    transcription_language_menu.configure(
        values=(
            transcription_language_display_values(
                language
            )
        )
    )
    transcription_language_var.set(
        transcription_language_display(
            current_transcription,
            language,
        )
    )

    refresh_whisper_models()
    refresh_ollama_models()
    update_audio_summary()

    for stage_name, state in stage_state.items():
        apply_stage(
            stage_name,
            state,
        )

    if not recording and not processing:
        status_label.configure(
            text=tr("ready")
        )
    elif recording:
        status_label.configure(
            text=(
                tr("recording_muted")
                if mic_muted
                else tr("recording")
            )
        )

    tooltip_start.set_text(
        tr("tooltip_start")
    )
    tooltip_stop.set_text(
        tr("tooltip_stop")
    )
    tooltip_mute.set_text(
        tr("tooltip_mute")
    )
    tooltip_transcript.set_text(
        tr("tooltip_transcript")
    )
    tooltip_folder.set_text(
        tr("tooltip_folder")
    )
    tooltip_settings.set_text(
        tr("tooltip_settings")
    )
    tooltip_refresh_models.set_text(
        tr("tooltip_refresh_models")
    )
    tooltip_change_folder.set_text(
        tr("tooltip_change_folder")
    )


# ============================================================
# CONTROLES UI
# ============================================================

def set_model_controls(enabled):
    state = (
        "normal"
        if enabled
        else "disabled"
    )

    app_language_menu.configure(state=state)
    transcription_language_menu.configure(
        state=state
    )
    whisper_menu.configure(state=state)
    ollama_menu.configure(state=state)
    summary_type_menu.configure(state=state)
    refresh_ollama_button.configure(
        state=state
    )
    settings_button.configure(state=state)

def poll_messages():
    global current_mic_display
    global current_system_display
    global current_mic_is_fallback

    try:
        while True:
            kind, data = (
                messages.get_nowait()
            )

            if kind == "startup_status":
                status_label.configure(
                    text=data
                )

            elif kind == "startup_result":
                finish_startup_checks(data)

            elif kind == "status":
                status_label.configure(
                    text=data
                )

            elif kind == "device_mic":
                current_mic_display = (
                    friendly_device_name(data)
                )
                current_mic_is_fallback = False

                mic_audio_label.configure(
                    text=(
                        f"{tr('microphone')}: "
                        f"{current_mic_display}"
                    )
                )

            elif kind == "mic_fallback":
                current_mic_display = (
                    friendly_device_name(data)
                )
                current_mic_is_fallback = True

                mic_audio_label.configure(
                    text=(
                        f"{tr('microphone')}: "
                        f"{current_mic_display} "
                        f"({tr('automatic')})"
                    )
                )

            elif kind == "device_system":
                current_system_display = (
                    friendly_device_name(data)
                )

                system_audio_label.configure(
                    text=(
                        f"{tr('computer_audio')}: "
                        f"{current_system_display}"
                    )
                )

            elif kind == "system_fallback":
                current_system_display = (
                    friendly_device_name(data)
                )

                system_audio_label.configure(
                    text=(
                        f"{tr('computer_audio')}: "
                        f"{current_system_display} "
                        f"({tr('automatic')})"
                    )
                )

            elif kind == "stage":
                stage_name, state = data
                apply_stage(
                    stage_name,
                    state,
                )

            elif kind == "log":
                print(data)

            elif kind == "error":
                progress_bar.stop()
                progress_bar.pack_forget()

                for stage in (
                    "audio",
                    "whisper",
                    "summary",
                ):
                    if (
                        stage_state.get(stage)
                        == "working"
                    ):
                        apply_stage(
                            stage,
                            "error",
                        )
                        break

                status_label.configure(
                    text=tr("error_occurred")
                )

                messagebox.showerror(
                    "Meeting Assistant",
                    data,
                )

                start_button.configure(
                    state="normal"
                )
                stop_button.configure(
                    state="disabled"
                )
                mute_button.configure(
                    state="normal"
                )
                set_model_controls(True)

                if (
                    transcript_file
                    and transcript_file.exists()
                ):
                    transcript_button.configure(
                        state="normal"
                    )

                if (
                    current_folder
                    and current_folder.exists()
                ):
                    folder_button.configure(
                        state="normal"
                    )

            elif kind == "complete":
                progress_bar.stop()
                progress_bar.pack_forget()

                status_label.configure(
                    text=tr("processed_ok")
                )

                timer_label.configure(
                    text=tr("done")
                )

                start_button.configure(
                    state="normal"
                )
                mute_button.configure(
                    state="normal"
                )

                set_model_controls(True)
                refresh_whisper_models()

    except queue.Empty:
        pass

    root.after(
        100,
        poll_messages,
    )

def on_close():
    if recording:
        messagebox.showinfo(
            tr("meeting_in_progress_title"),
            tr("meeting_in_progress_close"),
        )
        return

    if processing:
        messagebox.showinfo(
            tr("processing_title"),
            tr("processing_close"),
        )
        return

    root.destroy()

# ============================================================
# INTERFAZ PRINCIPAL
# ============================================================

ctk.set_appearance_mode("system")
ctk.set_default_color_theme("blue")

root = ctk.CTk()
root.title("Meeting Assistant")
root.geometry("375x275")
root.resizable(False, False)
root.configure(fg_color=COLOR_APP_BG)

try:
    ctypes.windll.shell32.SetCurrentProcessExplicitAppUserModelID(
        "MeetingAssistant.Local"
    )
except Exception:
    pass

APP_ICON_FILE = APP_FOLDER / "MeetingAssistant.ico"

try:
    root.iconbitmap(str(APP_ICON_FILE))
except Exception:
    pass

settings_icon = embedded_icon(
    SETTINGS_ICON_B64,
    (21, 21),
)
transcript_icon = embedded_icon(
    TRANSCRIPT_ICON_B64,
    (19, 19),
)
folder_icon = embedded_icon(
    FOLDER_ICON_B64,
    (20, 20),
)
refresh_icon = embedded_icon(
    REFRESH_ICON_B64,
    (17, 17),
)
mic_icon = embedded_icon(
    MIC_ICON_B64,
    (18, 18),
)
muted_mic_icon = embedded_icon(
    MUTED_MIC_ICON_B64,
    (18, 18),
)
dropdown_arrow_icon = embedded_icon(
    DROPDOWN_ARROW_ICON_B64,
    (13, 13),
)

transcript_icon_hover = embedded_icon_tinted(
    TRANSCRIPT_ICON_B64,
    COLOR_CREAM,
    (19, 19),
)
folder_icon_hover = embedded_icon_tinted(
    FOLDER_ICON_B64,
    COLOR_CREAM,
    (20, 20),
)
refresh_icon_hover = embedded_icon_tinted(
    REFRESH_ICON_B64,
    COLOR_CREAM,
    (17, 17),
)
mic_icon_hover = embedded_icon_tinted(
    MIC_ICON_B64,
    COLOR_CREAM,
    (18, 18),
)
muted_mic_icon_hover = embedded_icon_tinted(
    MUTED_MIC_ICON_B64,
    COLOR_CREAM,
    (18, 18),
)
settings_icon_hover = embedded_icon_tinted(
    SETTINGS_ICON_B64,
    COLOR_CREAM,
    (21, 21),
)
stage_done_icon = embedded_icon(
    STAGE_DONE_ICON_B64,
    (15, 15),
)
stage_error_icon = embedded_icon(
    STAGE_ERROR_ICON_B64,
    (15, 15),
)


def whisper_model_status_icon(value):
    """
    Installed Whisper models use the exact same SVG check icon as
    completed pipeline states.
    """
    model = parse_whisper_value(value)

    if model in installed_whisper_models():
        return stage_done_icon

    return None


# ------------------------------------------------------------
# SESSION
# ------------------------------------------------------------

session_frame = ctk.CTkFrame(
    root,
    corner_radius=12,
    fg_color=COLOR_FRAME_LIGHT,
)
session_frame.pack(
    padx=14,
    pady=(11, 6),
    fill="x",
)

status_label = ctk.CTkLabel(
    session_frame,
    text=tr("checking_environment"),
    font=ctk.CTkFont(size=13),
)
status_label.pack(
    pady=(7, 1),
)

control_row = ctk.CTkFrame(
    session_frame,
    fg_color="transparent",
)
control_row.pack(
    pady=(0, 7),
)

timer_label = ctk.CTkLabel(
    control_row,
    text="00:00:00",
    font=ctk.CTkFont(
        family="Consolas",
        size=32,
        weight="bold",
    ),
)
timer_label.grid(
    row=0,
    column=0,
    padx=(0, 20),
)

start_button = CircularActionButton(
    control_row,
    diameter=54,
    icon="play",
    command=start_recording,
    fg_color=COLOR_SIENNA,
    hover_color=COLOR_BURGUNDY,
)
start_button.grid(
    row=0,
    column=1,
    padx=(0, 8),
)
tooltip_start = ToolTip(
    start_button,
    tr("tooltip_start"),
)

stop_button = CircularActionButton(
    control_row,
    diameter=54,
    icon="stop",
    command=stop_recording,
    fg_color=COLOR_BURGUNDY,
    hover_color=COLOR_DARK_BROWN,
)
stop_button.configure(
    state="disabled"
)
stop_button.grid(
    row=0,
    column=2,
)
tooltip_stop = ToolTip(
    stop_button,
    tr("tooltip_stop"),
)


# ------------------------------------------------------------
# PIPELINE
# ------------------------------------------------------------

stages_frame = ctk.CTkFrame(
    root,
    fg_color="transparent",
)
stages_frame.pack(
    pady=(0, 5),
)

stage_audio = ctk.CTkLabel(
    stages_frame,
    text="",
    width=92,
    height=25,
    corner_radius=8,
)
stage_audio.grid(
    row=0,
    column=0,
    padx=4,
)

stage_whisper = ctk.CTkLabel(
    stages_frame,
    text="",
    width=92,
    height=25,
    corner_radius=8,
)
stage_whisper.grid(
    row=0,
    column=1,
    padx=4,
)

stage_summary = ctk.CTkLabel(
    stages_frame,
    text="",
    width=92,
    height=25,
    corner_radius=8,
)
stage_summary.grid(
    row=0,
    column=2,
    padx=4,
)

reset_stages()


# ------------------------------------------------------------
# AUDIO
# ------------------------------------------------------------

audio_frame = ctk.CTkFrame(
    root,
    corner_radius=9,
    fg_color=COLOR_PANEL,
)
audio_frame.pack(
    padx=14,
    pady=(0, 5),
    fill="x",
)

mic_audio_label = ctk.CTkLabel(
    audio_frame,
    text=tr("microphone_loading"),
    font=ctk.CTkFont(size=10),
    anchor="w",
    justify="left",
    wraplength=330,
)
mic_audio_label.pack(
    padx=10,
    pady=(5, 0),
    fill="x",
)

system_audio_label = ctk.CTkLabel(
    audio_frame,
    text=tr("computer_audio_loading"),
    font=ctk.CTkFont(size=10),
    anchor="w",
    justify="left",
    wraplength=330,
)
system_audio_label.pack(
    padx=10,
    pady=(1, 5),
    fill="x",
)


# ------------------------------------------------------------
# PROGRESS
# ------------------------------------------------------------

progress_bar = ctk.CTkProgressBar(
    root,
    mode="indeterminate",
    progress_color=COLOR_SIENNA,
)


# ------------------------------------------------------------
# BOTTOM BAR
# ------------------------------------------------------------

results_frame = ctk.CTkFrame(
    root,
    fg_color="transparent",
)
results_frame.pack(
    padx=14,
    pady=(2, 7),
    fill="x",
)

# Equivalente visual a justify-content: space-between.
# Los dos extremos tienen el mismo peso, por lo que el grupo central
# permanece realmente centrado.
results_frame.grid_columnconfigure(
    0,
    weight=1,
    uniform="bottom_edges",
)
results_frame.grid_columnconfigure(
    1,
    weight=0,
)
results_frame.grid_columnconfigure(
    2,
    weight=1,
    uniform="bottom_edges",
)

mute_button = ThemedButton(
    results_frame,
    variant="microphone",
    normal_image=mic_icon,
    hover_image=mic_icon_hover,
    text="",
    width=32,
    height=31,
    corner_radius=8,
    state="normal",
    command=toggle_microphone_mute,
)
mute_button.grid(
    row=0,
    column=0,
    sticky="w",
)
tooltip_mute = ToolTip(
    mute_button,
    tr("tooltip_mute"),
)

center_results = ctk.CTkFrame(
    results_frame,
    fg_color="transparent",
)
center_results.grid(
    row=0,
    column=1,
)

transcript_button = ResultButton(
    center_results,
    normal_image=transcript_icon,
    hover_image=transcript_icon_hover,
    disabled_image=transcript_icon_hover,
    text="",
    width=38,
    height=31,
    command=open_transcript,
)
transcript_button.grid(
    row=0,
    column=0,
    padx=4,
)
tooltip_transcript = ToolTip(
    transcript_button,
    tr("tooltip_transcript"),
)

summary_button = ResultButton(
    center_results,
    text=tr("stage_summary"),
    width=104,
    height=31,
    command=open_summary,
)
summary_button.grid(
    row=0,
    column=1,
    padx=4,
)

folder_button = ResultButton(
    center_results,
    normal_image=folder_icon,
    hover_image=folder_icon_hover,
    disabled_image=folder_icon_hover,
    text="",
    width=38,
    height=31,
    command=open_folder,
)
folder_button.grid(
    row=0,
    column=2,
    padx=4,
)
tooltip_folder = ToolTip(
    folder_button,
    tr("tooltip_folder"),
)

settings_button = ThemedButton(
    results_frame,
    variant="dark",
    normal_image=settings_icon,
    hover_image=settings_icon_hover,
    text="",
    width=34,
    height=31,
    corner_radius=8,
    command=show_settings,
)
settings_button.grid(
    row=0,
    column=2,
    sticky="e",
)
tooltip_settings = ToolTip(
    settings_button,
    tr("tooltip_settings"),
)


# ============================================================
# SETTINGS WINDOW
# ============================================================

settings_window = ctk.CTkToplevel(root)
settings_window.title(
    tr("settings")
)
settings_window.geometry("590x610")
settings_window.resizable(
    False,
    False,
)
settings_window.configure(
    fg_color=COLOR_SETTINGS_BG
)
settings_window.withdraw()

try:
    settings_window.iconbitmap(
        str(APP_ICON_FILE)
    )
except Exception:
    pass

settings_window.protocol(
    "WM_DELETE_WINDOW",
    settings_window.withdraw,
)

settings_body = ctk.CTkScrollableFrame(
    settings_window,
    width=535,
    height=535,
    fg_color=COLOR_PANEL,
)
settings_body.pack(
    padx=20,
    pady=18,
    fill="both",
    expand=True,
)


# ------------------------------------------------------------
# LANGUAGE
# ------------------------------------------------------------

language_section_label = ctk.CTkLabel(
    settings_body,
    text=tr("language"),
    font=ctk.CTkFont(
        size=14,
        weight="bold",
    ),
)
language_section_label.pack(
    anchor="w",
    padx=16,
    pady=(10, 7),
)

app_language_label = ctk.CTkLabel(
    settings_body,
    text=tr("application_language"),
    font=ctk.CTkFont(size=11),
)
app_language_label.pack(
    anchor="w",
    padx=16,
    pady=(0, 2),
)

app_language_var = ctk.StringVar(
    value=language_display(
        config.get("language", "en")
    )
)

app_language_holder, app_language_menu = (
    fixed_menu(
        settings_body,
        app_language_var,
        list(APP_LANGUAGE_LABELS.values()),
    )
)
app_language_holder.pack(
    anchor="w",
    padx=16,
    pady=(0, 7),
)

transcription_language_label = ctk.CTkLabel(
    settings_body,
    text=tr("transcription_language"),
    font=ctk.CTkFont(size=11),
)
transcription_language_label.pack(
    anchor="w",
    padx=16,
    pady=(0, 2),
)

transcription_language_var = ctk.StringVar(
    value=transcription_language_display(
        config.get(
            "transcription_language",
            "auto",
        )
    )
)

transcription_holder, transcription_language_menu = (
    fixed_menu(
        settings_body,
        transcription_language_var,
        transcription_language_display_values(),
    )
)
transcription_holder.pack(
    anchor="w",
    padx=16,
    pady=(0, 13),
)


# ------------------------------------------------------------
# PROCESSING
# ------------------------------------------------------------

processing_section_label = ctk.CTkLabel(
    settings_body,
    text=tr("processing"),
    font=ctk.CTkFont(
        size=14,
        weight="bold",
    ),
)
processing_section_label.pack(
    anchor="w",
    padx=16,
    pady=(6, 7),
)

whisper_model_label = ctk.CTkLabel(
    settings_body,
    text=tr("whisper_model"),
    font=ctk.CTkFont(size=11),
)
whisper_model_label.pack(
    anchor="w",
    padx=16,
    pady=(0, 2),
)

whisper_var = ctk.StringVar(
    value=config.get(
        "whisper_model",
        DEFAULT_WHISPER_MODEL,
    )
)

whisper_holder, whisper_menu = fixed_menu(
    settings_body,
    whisper_var,
    [DEFAULT_WHISPER_MODEL],
    command=update_whisper_note,
    icon_resolver=whisper_model_status_icon,
)
whisper_holder.pack(
    anchor="w",
    padx=16,
    pady=(0, 2),
)

whisper_status = ctk.CTkLabel(
    settings_body,
    text="",
    font=ctk.CTkFont(size=10),
)
whisper_status.pack(
    anchor="w",
    padx=16,
    pady=(0, 9),
)

ai_model_label = ctk.CTkLabel(
    settings_body,
    text=tr("ai_model"),
    font=ctk.CTkFont(size=11),
)
ai_model_label.pack(
    anchor="w",
    padx=16,
    pady=(0, 2),
)

ai_row = ctk.CTkFrame(
    settings_body,
    fg_color="transparent",
    width=490,
    height=30,
)
ai_row.pack(
    anchor="w",
    padx=16,
    pady=(0, 2),
)
ai_row.pack_propagate(False)

ollama_var = ctk.StringVar(
    value=config.get("ollama_model", "")
)

ollama_menu = StyledDropdown(
    ai_row,
    variable=ollama_var,
    values=[tr("searching_models")],
    width=448,
    height=30,
)
ollama_menu.place(
    x=0,
    y=0,
)

refresh_ollama_button = ThemedButton(
    ai_row,
    variant="secondary",
    normal_image=refresh_icon,
    hover_image=refresh_icon_hover,
    text="",
    width=36,
    height=30,
    command=refresh_ollama_models,
)
refresh_ollama_button.place(
    x=454,
    y=0,
)
tooltip_refresh_models = ToolTip(
    refresh_ollama_button,
    tr("tooltip_refresh_models"),
)

ollama_status = ctk.CTkLabel(
    settings_body,
    text=tr("searching_ollama"),
    font=ctk.CTkFont(size=10),
)
ollama_status.pack(
    anchor="w",
    padx=16,
    pady=(0, 9),
)

summary_type_label = ctk.CTkLabel(
    settings_body,
    text=tr("summary_type"),
    font=ctk.CTkFont(size=11),
)
summary_type_label.pack(
    anchor="w",
    padx=16,
    pady=(0, 2),
)

summary_type_var = ctk.StringVar(
    value=summary_type_display(
        config.get(
            "summary_type",
            "meeting_minutes",
        )
    )
)

summary_holder, summary_type_menu = fixed_menu(
    settings_body,
    summary_type_var,
    summary_type_display_values(),
)
summary_holder.pack(
    anchor="w",
    padx=16,
    pady=(0, 13),
)


# ------------------------------------------------------------
# AUDIO
# ------------------------------------------------------------

audio_section_label = ctk.CTkLabel(
    settings_body,
    text=tr("audio"),
    font=ctk.CTkFont(
        size=14,
        weight="bold",
    ),
)
audio_section_label.pack(
    anchor="w",
    padx=16,
    pady=(6, 7),
)

microphone_settings_label = ctk.CTkLabel(
    settings_body,
    text=tr("microphone"),
    font=ctk.CTkFont(size=11),
)
microphone_settings_label.pack(
    anchor="w",
    padx=16,
    pady=(0, 2),
)

settings_mic_var = ctk.StringVar()

mic_holder, settings_mic_menu = fixed_menu(
    settings_body,
    settings_mic_var,
    ["..."],
)
mic_holder.pack(
    anchor="w",
    padx=16,
    pady=(0, 7),
)

computer_audio_settings_label = ctk.CTkLabel(
    settings_body,
    text=tr("computer_audio"),
    font=ctk.CTkFont(size=11),
)
computer_audio_settings_label.pack(
    anchor="w",
    padx=16,
    pady=(0, 2),
)

settings_system_var = ctk.StringVar()

system_holder, settings_system_menu = fixed_menu(
    settings_body,
    settings_system_var,
    ["..."],
)
system_holder.pack(
    anchor="w",
    padx=16,
    pady=(0, 7),
)

refresh_audio_button = ThemedButton(
    settings_body,
    variant="secondary",
    normal_image=refresh_icon,
    hover_image=refresh_icon_hover,
    text=tr("update_devices"),
    compound="left",
    width=175,
    height=28,
    command=refresh_audio_devices,
)
refresh_audio_button.pack(
    anchor="w",
    padx=16,
    pady=(0, 13),
)


# ------------------------------------------------------------
# FILES AND PRIVACY
# ------------------------------------------------------------

files_section_label = ctk.CTkLabel(
    settings_body,
    text=tr("files_privacy"),
    font=ctk.CTkFont(
        size=14,
        weight="bold",
    ),
)
files_section_label.pack(
    anchor="w",
    padx=16,
    pady=(6, 7),
)

save_location_label = ctk.CTkLabel(
    settings_body,
    text=tr("save_meetings_in"),
    font=ctk.CTkFont(size=11),
)
save_location_label.pack(
    anchor="w",
    padx=16,
    pady=(0, 2),
)

settings_output_var = ctk.StringVar(
    value=config.get(
        "output_folder",
        str(APP_FOLDER / "meetings"),
    )
)

folder_row = ctk.CTkFrame(
    settings_body,
    fg_color="transparent",
    width=490,
    height=30,
)
folder_row.pack(
    anchor="w",
    padx=16,
    pady=(0, 5),
)
folder_row.pack_propagate(False)

settings_output_border = BorderedContainer(
    folder_row,
    width=448,
    height=30,
)
settings_output_border.place(
    x=0,
    y=0,
)

settings_output_entry = ctk.CTkEntry(
    settings_output_border.inner,
    textvariable=settings_output_var,
    width=446,
    height=28,
    fg_color=COLOR_PANEL,
    border_width=0,
    corner_radius=0,
)
settings_output_entry.place(
    x=0,
    y=0,
)

browse_button = ThemedButton(
    folder_row,
    variant="secondary",
    normal_image=folder_icon,
    hover_image=folder_icon_hover,
    text="",
    width=36,
    height=30,
    command=browse_output_folder,
)
browse_button.place(
    x=454,
    y=0,
)
tooltip_change_folder = ToolTip(
    browse_button,
    tr("tooltip_change_folder"),
)

settings_keep_audio_var = ctk.BooleanVar(
    value=bool(
        config.get("keep_audio", True)
    )
)

keep_audio_switch = ctk.CTkSwitch(
    settings_body,
    text=tr("keep_audio"),
    variable=settings_keep_audio_var,
    button_color=COLOR_SIENNA,
    button_hover_color=COLOR_BURGUNDY,
    progress_color=COLOR_TAN,
)
keep_audio_switch.pack(
    anchor="w",
    padx=16,
    pady=(2, 13),
)


# ------------------------------------------------------------
# CUSTOM PROMPT
# ------------------------------------------------------------

custom_prompt_label = ctk.CTkLabel(
    settings_body,
    text=tr("custom_prompt"),
    font=ctk.CTkFont(
        size=14,
        weight="bold",
    ),
)
custom_prompt_label.pack(
    anchor="w",
    padx=16,
    pady=(6, 5),
)

settings_custom_prompt_border = BorderedContainer(
    settings_body,
    width=490,
    height=105,
)
settings_custom_prompt_border.pack(
    padx=16,
    pady=(0, 12),
)

settings_custom_prompt = ctk.CTkTextbox(
    settings_custom_prompt_border.inner,
    width=488,
    height=103,
    fg_color=COLOR_PANEL,
    border_width=0,
    corner_radius=0,
)
settings_custom_prompt.place(
    x=0,
    y=0,
)

settings_custom_prompt.insert(
    "1.0",
    config.get(
        "custom_summary_prompt",
        DEFAULT_CONFIG[
            "custom_summary_prompt"
        ],
    ),
)

save_settings_button = ThemedButton(
    settings_body,
    variant="primary",
    text=tr("save_settings"),
    width=190,
    height=34,
    command=save_settings,
)
save_settings_button.pack(
    pady=(0, 14),
)

# El switch no hereda ThemedButton, pero también es interactivo.
bind_pointer_cursor(
    keep_audio_switch
)

# ============================================================
# ARRANQUE
# ============================================================

root.protocol("WM_DELETE_WINDOW", on_close)

root.after(100, poll_messages)
root.after(120, refresh_ui_language)
root.after(150, begin_startup_checks)

root.mainloop()
