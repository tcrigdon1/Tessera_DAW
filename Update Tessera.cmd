@echo off
rem Double-click to get the newest Tessera: downloads the latest code,
rem builds it (with ASIO), refreshes the shortcuts and starts Tessera.
title Update Tessera

rem git pull may rewrite this file while it runs, which confuses cmd.exe,
rem so run from a copy in the temp folder.
if not "%~1"=="--from-temp" (
    copy /y "%~f0" "%TEMP%\tessera-update.cmd" >nul
    "%TEMP%\tessera-update.cmd" --from-temp "%~dp0."
    exit /b
)
set "ROOT=%~f2"
cd /d "%ROOT%"

:check_running
tasklist /FI "IMAGENAME eq tessera.exe" 2>nul | find /I "tessera.exe" >nul
if not errorlevel 1 (
    echo Tessera is still open. Save your work, close Tessera, then press a key.
    pause >nul
    goto check_running
)

echo.
echo Downloading the latest version...
git pull
if errorlevel 1 goto failed

echo.
echo Building Tessera. The first build after an update can take a few minutes...
cargo build --release --features asio
if errorlevel 1 goto failed

call "%ROOT%\Create Desktop Shortcut.cmd" /quiet

echo.
echo Starting Tessera...
start "" /d "%ROOT%\target\release" "%ROOT%\target\release\tessera.exe"
exit /b 0

:failed
echo.
echo Something went wrong (see the messages above). Nothing was changed in
echo your projects. Copy the messages above if you need help.
pause
exit /b 1
