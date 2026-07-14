@echo off
rem One-click dev launcher: sets up MSVC/cargo env and runs `tauri dev`.
rem Note: only one tauri dev instance can run at a time (Vite is pinned to port 1420).

cd /d "%~dp0"
call scripts\dev-env.cmd npm.cmd run tauri dev
set "EXITCODE=%ERRORLEVEL%"

if not "%EXITCODE%"=="0" (
    echo.
    echo tauri dev exited with code %EXITCODE%
    pause
)
exit /b %EXITCODE%
