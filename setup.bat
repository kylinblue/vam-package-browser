@echo off
setlocal EnableExtensions
rem One-time setup: checks the build prerequisites (Node.js, Visual Studio
rem Build Tools with the C++ workload, Rust) and installs any that are
rem missing via winget, then installs the frontend dependencies.
rem
rem Run this once, then use run-dev.bat to build and launch the app.
rem
rem Notes:
rem  - winget ships with modern Windows 10/11 ("App Installer"); it is only
rem    needed if a tool is actually missing.
rem  - The VS Build Tools download is large (several GB) and can take a
rem    while. Installers may pop UAC prompts - that's expected.

cd /d "%~dp0"

rem ---- detect what's already installed -----------------------------------
set "NEED_NODE=1"
where node >NUL 2>&1 && set "NEED_NODE="
if not defined NEED_NODE for /f "tokens=*" %%v in ('node --version') do echo setup: Node.js %%v found.

set "NEED_VC=1"
set "VSWHERE=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
if exist "%VSWHERE%" (
    for /f "usebackq tokens=*" %%i in (`"%VSWHERE%" -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -find VC\Auxiliary\Build\vcvars64.bat`) do set "NEED_VC="
)
if not defined NEED_VC echo setup: MSVC C++ Build Tools found.

set "NEED_RUST=1"
if exist "%USERPROFILE%\.cargo\bin\cargo.exe" set "NEED_RUST="
where cargo >NUL 2>&1 && set "NEED_RUST="
if not defined NEED_RUST echo setup: Rust toolchain found.

if not defined NEED_NODE if not defined NEED_VC if not defined NEED_RUST goto :deps

rem ---- something is missing: we need winget ------------------------------
set "WINGET=winget"
where winget >NUL 2>&1
if errorlevel 1 (
    if exist "%LOCALAPPDATA%\Microsoft\WindowsApps\winget.exe" (
        set "WINGET=%LOCALAPPDATA%\Microsoft\WindowsApps\winget.exe"
    ) else (
        echo setup: some tools are missing and winget was not found.
        echo Install "App Installer" from the Microsoft Store and re-run this
        echo script, or install the missing tools manually:
        goto :manual_urls
    )
)

if defined NEED_NODE (
    echo setup: installing Node.js LTS...
    "%WINGET%" install --id OpenJS.NodeJS.LTS -e --accept-source-agreements --accept-package-agreements
    if errorlevel 1 goto :winget_failed
    rem Current shell won't have the updated PATH; add the default location.
    set "PATH=%ProgramFiles%\nodejs;%PATH%"
)

rem Rust's MSVC toolchain needs link.exe; install Build Tools before Rust.
if defined NEED_VC (
    echo setup: installing Visual Studio Build Tools + C++ workload...
    echo        ^(this is the big one - several GB, grab a coffee^)
    "%WINGET%" install --id Microsoft.VisualStudio.2022.BuildTools -e --accept-source-agreements --accept-package-agreements --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
    if errorlevel 1 goto :winget_failed
)

if defined NEED_RUST (
    echo setup: installing Rust via rustup...
    "%WINGET%" install --id Rustlang.Rustup -e --accept-source-agreements --accept-package-agreements
    if errorlevel 1 goto :winget_failed
)

rem ---- frontend dependencies ---------------------------------------------
:deps
echo setup: installing frontend dependencies ^(npm install^)...
call npm install
if errorlevel 1 (
    echo.
    echo setup: npm install failed. If Node.js was just installed, open a NEW
    echo terminal ^(so PATH refreshes^) and run:  npm install
    echo Then start the app with run-dev.bat.
    exit /b 1
)

echo.
echo setup: done. Start the app with run-dev.bat.
echo        ^(If any tool was installed just now, run run-dev.bat from a NEW
echo        terminal so the updated PATH is picked up.^)
exit /b 0

:winget_failed
echo.
echo setup: a winget install failed. You can install the missing tool
echo manually instead:
:manual_urls
echo   Node.js LTS:        https://nodejs.org
echo   VS Build Tools:     https://visualstudio.microsoft.com/downloads/
echo                       ^(select the "Desktop development with C++" workload^)
echo   Rust:               https://rustup.rs
echo Then re-run this script.
exit /b 1
