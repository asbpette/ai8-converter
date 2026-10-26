@echo off
rem Installs Ghostscript, which AI8 Converter needs to read EPS and PS files.
rem Ghostscript is separate software by Artifex Software, Inc., licensed under
rem the GNU AGPL v3. It is obtained from Artifex, not from AI8 Converter.
rem
rem Pass /noprompt to skip the "press any key" pauses (the NSIS installer does).
rem
rem Two cmd traps this avoids:
rem  * %errorlevel% expands at parse time inside a parenthesised block, so
rem    there are no blocks here, only "goto".
rem  * "if errorlevel 1" is a signed ">= 1" test, so it MISSES the negative
rem    exit codes package managers return (winget's "no package found" is
rem    0x8A150014). Hence "neq 0", and a check that the file really appeared.
setlocal
set "NOPAUSE="
if /i "%~1"=="/noprompt" set "NOPAUSE=1"

echo AI8 Converter needs Ghostscript to read EPS and PS files.
echo Ghostscript is separate software by Artifex Software, Inc. - GNU AGPL v3.
echo.

call :find
if defined GSEXE goto done

rem Chocolatey first when it is there: it is quick and unattended. The Windows
rem Package Manager is no use, as it has no Ghostscript manifest at all.
where choco >nul 2>&1
if %errorlevel% neq 0 goto download

echo Installing with Chocolatey...
choco install ghostscript -y
echo.
rem Believe the filesystem, not the exit code.
call :find
if defined GSEXE goto done
echo Chocolatey did not install it. Trying Artifex directly.
echo.

:download
rem Fetch Artifex's own installer and run it. This downloads from Artifex, so
rem you receive Ghostscript from them under their licence; nothing is
rem redistributed with AI8 Converter. The helper verifies the download carries
rem a valid Artifex signature before running it.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0Install Ghostscript.ps1"
echo.
call :find
if not defined GSEXE goto page

:done
echo Ghostscript is ready:
echo   %GSEXE%
echo.
echo Start AI8 Converter, or press "Re-detect" on its Ghostscript line.
if not defined NOPAUSE pause
exit /b 0

:page
echo Ghostscript was not installed.
echo.
echo Install it yourself from:
echo   https://ghostscript.com/releases/gsdnld.html
echo Pick the 64-bit installer for Windows. AI8 Converter picks it up on its
echo own; there is also a "Re-detect" button on its Ghostscript line.
if not defined NOPAUSE pause
exit /b 1

rem Locate Ghostscript the same way the app does: the versioned install
rem directories first, then PATH.
:find
set "GSEXE="
rem "for /d" does not reliably expand a wildcard inside a quoted path, which is
rem how this missed real installs; "dir /b /ad" does. The "if exist" guard means
rem the command needs no redirection, which cannot be escaped reliably here.
if not exist "%ProgramFiles%\gs\" goto findpath
for /f "delims=" %%D in ('dir /b /ad "%ProgramFiles%\gs"') do if exist "%ProgramFiles%\gs\%%D\bin\gswin64c.exe" set "GSEXE=%ProgramFiles%\gs\%%D\bin\gswin64c.exe"
if defined GSEXE exit /b 0

:findpath
rem Pre-test so the "for /f" command needs no redirection: escaping ">" inside
rem the single quotes is unreliable.
where gswin64c.exe >nul 2>&1
if %errorlevel% neq 0 exit /b 0
for /f "delims=" %%P in ('where gswin64c.exe') do set "GSEXE=%%P"
exit /b 0
