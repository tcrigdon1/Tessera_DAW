@echo off
rem Puts "Tessera" and "Update Tessera" shortcuts on the desktop and in the
rem Start menu. Builds Tessera first if it has not been built yet.
cd /d "%~dp0"

if not exist "%~dp0target\release\tessera.exe" (
    echo Tessera has not been built yet. Building it now, this takes a few minutes...
    cargo build --release --features asio
    if errorlevel 1 (
        echo.
        echo The build failed, see the messages above.
        pause
        exit /b 1
    )
)

powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\create-shortcuts.ps1" -Root "%~dp0."
if errorlevel 1 (
    pause
    exit /b 1
)
if /I not "%~1"=="/quiet" (
    echo.
    echo Done. Double-click "Tessera" on your desktop to start it.
    pause
)
