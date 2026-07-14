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

if not exist "%DATA_DIR%" (
    echo Nothing to purge: "%DATA_DIR%" does not exist.
    exit /b 0
)

echo This will permanently delete the app's data directory:
echo.
echo     %DATA_DIR%
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

rmdir /s /q "%DATA_DIR%"
if exist "%DATA_DIR%" (
    echo.
    echo WARNING: some files could not be deleted. Is the app still running?
    exit /b 1
)

echo.
echo Done. "%DATA_DIR%" removed.
endlocal
