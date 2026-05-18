@echo off
rem Sources MSVC env + adds cargo to PATH, then runs whatever command/args follow.
rem Usage:  scripts\dev-env.cmd cargo check --manifest-path src-tauri\Cargo.toml
rem         scripts\dev-env.cmd cargo build --release --manifest-path src-tauri\Cargo.toml
rem         scripts\dev-env.cmd npm run tauri dev

setlocal EnableExtensions

set "CARGO_BIN=%USERPROFILE%\.cargo\bin"
if exist "%CARGO_BIN%" set "PATH=%CARGO_BIN%;%PATH%"

rem vswhere is used by tauri's build script to locate VS; not on PATH by default.
set "VSWHERE_DIR=C:\Program Files (x86)\Microsoft Visual Studio\Installer"
if exist "%VSWHERE_DIR%\vswhere.exe" set "PATH=%VSWHERE_DIR%;%PATH%"

set "VCVARS=C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
if not exist "%VCVARS%" (
    echo dev-env.cmd: vcvars64.bat not found at "%VCVARS%"
    exit /b 2
)

call "%VCVARS%" >NUL
if errorlevel 1 (
    echo dev-env.cmd: vcvars64 failed
    exit /b 3
)

if "%~1"=="" (
    echo dev-env.cmd: ready - no command given
    exit /b 0
)

%*
exit /b %ERRORLEVEL%
