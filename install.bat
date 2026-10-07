@echo off
rem Installs in_spotitube.dll and ml_spotitube.dll into Winamp's Plugins folder.
rem Usage: install.bat [/uninstall] [/winamp "C:\path\to\Winamp"] [/update]
rem /update (used by the plugin's one-click update): closes Winamp itself, asks nothing,
rem restarts Winamp and closes its window when it succeeded.
rem Plain batch on purpose: PowerShell execution policy (even when set by Group Policy) only
rem applies to script files, so this runs where install scripts are blocked.
setlocal EnableExtensions
rem Saved before argument parsing: shift also shifts %0.
set "HERE=%~dp0"
set "SELF=%~f0"
rem Full path: Git for Windows can put its Unix find ahead of System32 on PATH.
set "FIND=%SystemRoot%\System32\find.exe"

set "MODE=install"
set "WINAMP="
set "UPDATE="
rem Seconds to wait for Winamp to exit after asking it to close.
set "CLOSE_TIMEOUT=30"
:parse_args
if "%~1"=="" goto args_done
if /i "%~1"=="/uninstall" set "MODE=uninstall"
if /i "%~1"=="-uninstall" set "MODE=uninstall"
if /i "%~1"=="/update" set "UPDATE=1"
if /i "%~1"=="/winamp" (set "WINAMP=%~2" & shift)
if /i "%~1"=="-winamp" (set "WINAMP=%~2" & shift)
shift
goto parse_args
:args_done
rem Started from the one-click updater's temp folder: update mode even without /update, since
rem v0.3.3 and older start install.bat with no arguments.
echo "%HERE%" | "%FIND%" /i "\SpotiTube-update-" >nul && set "UPDATE=1"

rem --- Find Winamp: argument, installer's registry keys, default folder.
if defined WINAMP goto check_winamp
for /f "tokens=2,*" %%A in ('reg query "HKCU\Software\Winamp" /ve 2^>nul ^| "%FIND%" "REG_SZ"') do set "WINAMP=%%B"
if defined WINAMP if exist "%WINAMP%\winamp.exe" goto check_winamp
set "WINAMP="
for /f "tokens=2,*" %%A in ('reg query "HKLM\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\Winamp" /v InstallLocation 2^>nul ^| "%FIND%" "REG_SZ"') do set "WINAMP=%%B"
if defined WINAMP if exist "%WINAMP%\winamp.exe" goto check_winamp
set "WINAMP=%ProgramFiles(x86)%\Winamp"
:check_winamp
if not exist "%WINAMP%\Plugins\" (
    echo Winamp not found. Run: install.bat /winamp "C:\path\to\Winamp"
    goto fail
)
set "PLUGINS=%WINAMP%\Plugins"

rem --- Find the DLLs: next to this script (release zip) or in the build output (source checkout).
if "%MODE%"=="uninstall" goto elevate
set "SRC=%HERE%"
if exist "%SRC%in_spotitube.dll" goto found_dll
set "SRC=%HERE%target\i686-pc-windows-msvc\release\"
if exist "%SRC%in_spotitube.dll" goto found_dll
echo in_spotitube.dll not found. Download the release zip or build it first.
goto fail
:found_dll

rem --- Writing to Program Files needs administrator rights.
:elevate
net session >nul 2>&1
if not errorlevel 1 goto is_admin
echo Requesting administrator rights to write to "%PLUGINS%"...
rem A one-line command, not a script file, so execution policy doesn't apply.
rem Through cmd.exe /c with the whole command in one more pair of quotes: an elevated .bat is
rem started as cmd /C "script" args, and with a quoted argument cmd strips the first and last
rem quote, so the script never runs.
set "UPDATE_ARG="
if defined UPDATE set "UPDATE_ARG= /update"
powershell -NoProfile -Command "Start-Process -FilePath cmd.exe -ArgumentList '/c \"\"%SELF%\" /%MODE%%UPDATE_ARG% /winamp \"%WINAMP%\"\"' -Verb RunAs" >nul 2>&1
if errorlevel 1 (
    echo Could not get administrator rights. Right-click install.bat and choose "Run as administrator".
    goto fail
)
exit /b 0
:is_admin
rem "/install" from the elevation step above is just the default mode.

set "WAS_RUNNING="
set "WAITED=0"
:wait_winamp
tasklist /fi "imagename eq winamp.exe" 2>nul | "%FIND%" /i "winamp.exe" >nul
if errorlevel 1 goto winamp_closed
if defined UPDATE goto close_winamp
set "WAS_RUNNING=1"
echo Winamp is running. Close it, then press any key.
pause >nul
goto wait_winamp

:close_winamp
rem Without /f taskkill sends Winamp a close message, so it saves its playlist and settings.
rem It is never killed: if it doesn't exit in time, the update stops and says so.
if not defined WAS_RUNNING (
    set "WAS_RUNNING=1"
    echo Closing Winamp...
    taskkill /im winamp.exe >nul 2>&1
)
if %WAITED% geq %CLOSE_TIMEOUT% (
    echo Winamp didn't close within %CLOSE_TIMEOUT% seconds. Close it and run install.bat again.
    goto fail
)
set /a WAITED+=1
rem One-second pause that, unlike timeout.exe, works without a console input.
ping -n 2 127.0.0.1 >nul
goto wait_winamp
:winamp_closed

rem The pre-rename plugin claims the same links; leaving both would make them fight over playback.
if exist "%PLUGINS%\in_spotify.dll" del "%PLUGINS%\in_spotify.dll" && echo Removed old in_spotify.dll.

if "%MODE%"=="install" goto do_install
for %%F in (in_spotitube.dll ml_spotitube.dll) do if exist "%PLUGINS%\%%F" del "%PLUGINS%\%%F" && echo Removed %%F.
echo Settings and the cached Spotify login stay in "%APPDATA%\in_spotitube"; delete that folder to remove them.
goto done

:do_install
copy /y "%SRC%in_spotitube.dll" "%PLUGINS%\" >nul || goto copy_failed
if exist "%SRC%ml_spotitube.dll" (
    copy /y "%SRC%ml_spotitube.dll" "%PLUGINS%\" >nul || goto copy_failed
) else (
    echo Warning: ml_spotitube.dll not found; YouTube search in the Media Library is not installed.
)
echo Installed to "%PLUGINS%".

rem yt-dlp's winget package also pulls in yt-dlp.FFmpeg.
set "MISSING="
where yt-dlp >nul 2>&1 || set "MISSING=yt-dlp"
where ffmpeg >nul 2>&1 || set "MISSING=%MISSING% ffmpeg"
if not defined MISSING goto restart
if defined UPDATE goto restart
echo.
echo Needed for YouTube but not found on PATH:%MISSING%
set /p "ANSWER=Install with winget now? [y/N] "
if /i not "%ANSWER%"=="y" goto restart
winget install --id yt-dlp.yt-dlp -e
if errorlevel 1 echo winget failed; install yt-dlp and ffmpeg manually.

:restart
if not defined WAS_RUNNING goto done
set "ANSWER=y"
if not defined UPDATE set /p "ANSWER=Start Winamp again? [Y/n] "
rem Elevated here; explorer.exe starts it with normal user rights.
if /i not "%ANSWER%"=="n" start "" explorer.exe "%WINAMP%\winamp.exe"
goto done

:copy_failed
echo Copy failed. Is Winamp still running?
goto fail

:done
if defined UPDATE exit /b 0
echo.
pause
exit /b 0

:fail
echo.
pause
exit /b 1
