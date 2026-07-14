@echo off
setlocal
:: Purge everything VaM Package Browser has derived: the SQLite index,
:: thumbnail cache, and all settings. Your .var library is NOT touched --
:: the app never stores anything inside it. After purging, the app starts
:: from a blank slate (re-scan + re-generate thumbnails).
::
:: IMPORTANT: close VaM Package Browser before running this. An open app
:: holds the SQLite file and the delete will partially fail.

set "DATA_DIR=%APPDATA%\com.github.kylinblue.vam-package-browser"
:: Pre-2026-07 installs used the old bundle identifier; purge that too.
set "LEGACY_DIR=%APPDATA%\com.github.kylinblue.vam-package-browser"

if not exist "%DATA_DIR%" if not exist "%LEGACY_DIR%" (
    echo Nothing to purge: no app data directory exists.
    exit /b 0
)

echo This will permanently delete the app's data:
echo.
if exist "%DATA_DIR%"   echo     %DATA_DIR%
if exist "%LEGACY_DIR%" echo     %LEGACY_DIR%   (legacy location)
echo.
echo   - index.sqlite  (package index, settings, hub-sync data, tags)
echo   - thumbs\       (generated thumbnail cache)
echo.
echo Your .var library folders are NOT affected.
echo Make sure VaM Package Browser is closed first.
echo.
set /p CONFIRM="Type YES to confirm: "
if /i not "%CONFIRM%"=="YES" (
    echo Aborted. Nothing was deleted.
    exit /b 1
)

if exist "%DATA_DIR%"   rmdir /s /q "%DATA_DIR%"
if exist "%LEGACY_DIR%" rmdir /s /q "%LEGACY_DIR%"
if exist "%DATA_DIR%" goto :failed
if exist "%LEGACY_DIR%" goto :failed

echo.
echo Done. App data removed.
exit /b 0

:failed
echo.
echo WARNING: some files could not be deleted. Is the app still running?
exit /b 1
