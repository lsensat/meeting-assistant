@echo off
cd /d "%~dp0"

if not exist ".venv\Scripts\python.exe" (
    powershell -NoProfile -Command "Add-Type -AssemblyName PresentationFramework; [System.Windows.MessageBox]::Show('No se encuentra el entorno .venv de Meeting Assistant.','Meeting Assistant','OK','Error')"
    exit /b 1
)

".venv\Scripts\python.exe" bootstrap.py
