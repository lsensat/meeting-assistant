import importlib.util
import subprocess
import sys
from pathlib import Path
import tkinter as tk
from tkinter import messagebox

APP_FOLDER = Path(__file__).resolve().parent
ERROR_LOG = APP_FOLDER / "meeting_error.log"

REQUIRED = {
    "numpy": "numpy",
    "customtkinter": "customtkinter",
    "sounddevice": "sounddevice",
    "soundfile": "soundfile",
    "pyaudiowpatch": "PyAudioWPatch",
    "ollama": "ollama",
    "faster_whisper": "faster-whisper",
    "huggingface_hub": "huggingface-hub",
    "PIL": "Pillow",
}

app_py = APP_FOLDER / "app.py"


def show_message(kind, title, text):
    root = tk.Tk()
    root.withdraw()

    if kind == "error":
        messagebox.showerror(title, text)
    else:
        messagebox.showinfo(title, text)

    root.destroy()


def missing_packages():
    return [
        package_name
        for module_name, package_name in REQUIRED.items()
        if importlib.util.find_spec(module_name) is None
    ]


missing = missing_packages()

if missing:
    show_message(
        "info",
        "Meeting Assistant",
        "Faltan algunos componentes de Python y se instalarán automáticamente:\n\n"
        + "\n".join(f"• {item}" for item in missing),
    )

    result = subprocess.run(
        [
            sys.executable,
            "-m",
            "pip",
            "install",
            *missing,
        ],
        cwd=str(APP_FOLDER),
        capture_output=True,
        text=True,
    )

    if result.returncode != 0:
        details = (result.stdout + "\n" + result.stderr).strip()

        if len(details) > 3500:
            details = details[-3500:]

        show_message(
            "error",
            "Meeting Assistant",
            "No se han podido instalar automáticamente las dependencias:\n\n"
            + "\n".join(f"• {item}" for item in missing)
            + "\n\n"
            + details,
        )
        sys.exit(1)

if not app_py.exists():
    show_message(
        "error",
        "Meeting Assistant",
        "No se encuentra app.py en la carpeta de la aplicación.",
    )
    sys.exit(1)

with ERROR_LOG.open("w", encoding="utf-8") as log:
    process = subprocess.run(
        [sys.executable, str(app_py)],
        cwd=str(APP_FOLDER),
        stdout=log,
        stderr=subprocess.STDOUT,
        text=True,
    )

if process.returncode != 0:
    try:
        details = ERROR_LOG.read_text(encoding="utf-8").strip()
    except Exception:
        details = ""

    if len(details) > 3500:
        details = details[-3500:]

    show_message(
        "error",
        "Meeting Assistant - Error",
        "La aplicación se ha cerrado por un error.\n\n"
        + (details or "No se ha podido recuperar el detalle del error.")
        + "\n\nEl error completo está guardado en:\n"
        + str(ERROR_LOG),
    )
