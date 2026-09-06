@echo off
setlocal
cd /d "%~dp0"

set "PYTHON=%~dp0.venv\Scripts\python.exe"

if not exist "%PYTHON%" (
    echo ERROR: No se encuentra .venv\Scripts\python.exe
    echo Abre primero Meeting Assistant.bat para preparar el entorno.
    pause
    exit /b 1
)

"%PYTHON%" -c "import pytest" >nul 2>&1
if errorlevel 1 (
    echo Instalando pytest...
    "%PYTHON%" -m pip install -r requirements-dev.txt
    if errorlevel 1 (
        echo ERROR instalando pytest.
        pause
        exit /b 1
    )
)

echo.
echo Ejecutando tests de Meeting Assistant...
echo.
"%PYTHON%" -m pytest -q tests
echo.
pause
