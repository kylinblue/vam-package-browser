@echo off
REM Verify Content-Disposition matching strategy on the VaM Hub.
REM Run from project root:  scripts\probe-hub-download.cmd
REM
REM Three inline test blocks — no batch-label calls, since those misbehaved
REM on the first probe of this script in some shells.

setlocal

set "COOKIE=vamhubconsent=yes"
set "UA=vam-package-browser/0.1 (probe)"

echo.
echo ================================================================
echo TEST 1 - Free hub-hosted: Damarmau / Forest assetbundle
echo URL:  https://hub.virtamate.com/resources/forest-assetbundle.37103/download
echo ================================================================
echo.
echo --- HEAD with redirect-follow ---
curl.exe -sS -I -L --max-redirs 5 -A "%UA%" --cookie "%COOKIE%" "https://hub.virtamate.com/resources/forest-assetbundle.37103/download"
echo.
echo --- GET range 0-0 ---
curl.exe -sS -D - -o NUL -r 0-0 -L --max-redirs 5 -A "%UA%" --cookie "%COOKIE%" "https://hub.virtamate.com/resources/forest-assetbundle.37103/download"
echo.
echo.

echo ================================================================
echo TEST 2 - Paid offsite: wunderwise / Touchy
echo URL:  https://hub.virtamate.com/resources/touchy.58217/download
echo ================================================================
echo.
echo --- HEAD with redirect-follow ---
curl.exe -sS -I -L --max-redirs 5 -A "%UA%" --cookie "%COOKIE%" "https://hub.virtamate.com/resources/touchy.58217/download"
echo.
echo --- GET range 0-0 ---
curl.exe -sS -D - -o NUL -r 0-0 -L --max-redirs 5 -A "%UA%" --cookie "%COOKIE%" "https://hub.virtamate.com/resources/touchy.58217/download"
echo.
echo.

echo ================================================================
echo TEST 3 - Bogus id (expect 404)
echo URL:  https://hub.virtamate.com/resources/this-does-not-exist.99999999/download
echo ================================================================
echo.
echo --- HEAD with redirect-follow ---
curl.exe -sS -I -L --max-redirs 5 -A "%UA%" --cookie "%COOKIE%" "https://hub.virtamate.com/resources/this-does-not-exist.99999999/download"
echo.

endlocal
