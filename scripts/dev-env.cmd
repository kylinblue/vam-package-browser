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

rem Locate vcvars64.bat: prefer vswhere (finds any VS edition/version with
rem the C++ toolset), fall back to known install paths.
set "VCVARS="
if exist "%VSWHERE_DIR%\vswhere.exe" (
    for /f "usebackq tokens=*" %%i in (`"%VSWHERE_DIR%\vswhere.exe" -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -find VC\Auxiliary\Build\vcvars64.bat`) do set "VCVARS=%%i"
)
if not defined VCVARS (
    for %%p in (
        "C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
        "C:\Program Files\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
        "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat"
        "C:\Program Files\Microsoft Visual Studio\2022\Professional\VC\Auxiliary\Build\vcvars64.bat"
        "C:\Program Files\Microsoft Visual Studio\2022\Enterprise\VC\Auxiliary\Build\vcvars64.bat"
    ) do if not defined VCVARS if exist %%p set "VCVARS=%%~p"
)
if not defined VCVARS (
    echo dev-env.cmd: could not find vcvars64.bat. Install Visual Studio Build
    echo Tools with the "Desktop development with C++" workload, then retry.
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
