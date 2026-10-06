# Installs (or with -Uninstall removes) in_spotitube.dll and ml_spotitube.dll (YouTube search
# in the Media Library) in Winamp's Plugins folder.
# Usage: .\install.ps1 [-Dll path\to\in_spotitube.dll] [-Winamp <folder>] [-Uninstall]
# Double-click install.bat to run it without changing the execution policy.
param(
    [string]$Dll,
    [string]$Winamp,
    [switch]$Uninstall
)
$ErrorActionPreference = 'Stop'

function Find-Winamp {
    # The installer records its folder in both places; fall back to the default location.
    $candidates = @(
        (Get-ItemProperty 'HKCU:\Software\Winamp' -ErrorAction SilentlyContinue).'(default)',
        (Get-ItemProperty 'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\Winamp' -ErrorAction SilentlyContinue).InstallLocation,
        "${env:ProgramFiles(x86)}\Winamp"
    )
    $candidates | Where-Object { $_ -and (Test-Path (Join-Path $_ 'winamp.exe')) } | Select-Object -First 1
}

function Exit-WithPause([int]$code) {
    Read-Host 'Press Enter to close'
    exit $code
}

try {
    if (-not $Winamp) { $Winamp = Find-Winamp }
    if (-not $Winamp) { throw 'Winamp not found. Pass -Winamp <folder>.' }
    $plugins = Join-Path $Winamp 'Plugins'
    if (-not (Test-Path $plugins)) { throw "Winamp Plugins folder not found: $plugins. Pass -Winamp <folder>." }

    if (-not $Uninstall) {
        if (-not $Dll) {
            # Release download sits next to the script; a source checkout has it under target.
            $Dll = @("$PSScriptRoot\in_spotitube.dll", "$PSScriptRoot\target\i686-pc-windows-msvc\release\in_spotitube.dll") |
                Where-Object { Test-Path $_ } | Select-Object -First 1
            if (-not $Dll) { throw 'in_spotitube.dll not found. Download it from the Releases page or build it first.' }
        }
        $Dll = (Resolve-Path $Dll).Path
        # The Media Library plugin is optional; it ships next to the input plugin.
        $mlDll = Join-Path (Split-Path $Dll) 'ml_spotitube.dll'
        if (-not (Test-Path $mlDll)) { $mlDll = $null }
    }
} catch {
    Write-Host $_.Exception.Message -ForegroundColor Red
    Exit-WithPause 1
}

$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole(
    [Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    Write-Host 'Requesting administrator rights to write to the Plugins folder...'
    $argList = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSCommandPath`"", '-Winamp', "`"$Winamp`"")
    if ($Uninstall) { $argList += '-Uninstall' } else { $argList += @('-Dll', "`"$Dll`"") }
    try {
        $proc = Start-Process powershell -Verb RunAs -Wait -PassThru -ArgumentList $argList
        exit $proc.ExitCode
    } catch {
        Write-Host 'Administrator rights were declined; nothing was changed.' -ForegroundColor Red
        Exit-WithPause 1
    }
}

try {
    $wasRunning = $false
    while (Get-Process winamp -ErrorAction SilentlyContinue) {
        $wasRunning = $true
        Read-Host 'Winamp is running. Close it, then press Enter'
    }

    # The pre-rename plugin claims the same links; leaving both would make them fight over playback.
    foreach ($name in @('in_spotify.dll') + @(if ($Uninstall) { 'in_spotitube.dll', 'ml_spotitube.dll' })) {
        $path = Join-Path $plugins $name
        if (Test-Path $path) {
            Remove-Item $path
            Write-Host "Removed $name."
        }
    }

    if ($Uninstall) {
        Write-Host "Settings and the cached Spotify login stay in $env:APPDATA\in_spotitube; delete that folder to remove them."
    } else {
        Copy-Item $Dll $plugins -Force
        if ($mlDll) { Copy-Item $mlDll $plugins -Force }
        else { Write-Warning 'ml_spotitube.dll not found next to in_spotitube.dll; YouTube search in the Media Library is not installed.' }
        Write-Host "Installed to $plugins." -ForegroundColor Green

        $missing = @(
            @{ Exe = 'yt-dlp'; Id = 'yt-dlp.yt-dlp' },  # also pulls in yt-dlp.FFmpeg
            @{ Exe = 'ffmpeg'; Id = 'yt-dlp.FFmpeg' }
        ) | Where-Object { -not (Get-Command $_.Exe -ErrorAction SilentlyContinue) }
        if ($missing) {
            $names = ($missing | ForEach-Object Exe) -join ' and '
            $answer = Read-Host "$names (needed for YouTube) not found on PATH. Install with winget now? [y/N]"
            if ($answer -match '^[yY]') {
                # yt-dlp's package depends on FFmpeg, so installing it covers both.
                winget install --id $missing[0].Id -e
                if ($LASTEXITCODE -ne 0) { Write-Warning "winget exited with code $LASTEXITCODE; install $names manually." }
            }
        }

        if ($wasRunning -and (Read-Host 'Start Winamp again? [Y/n]') -notmatch '^[nN]') {
            # Elevated here; explorer.exe starts it with normal user rights.
            Start-Process explorer.exe (Join-Path $Winamp 'winamp.exe')
        }
    }
} catch {
    Write-Host $_.Exception.Message -ForegroundColor Red
    Exit-WithPause 1
}
Exit-WithPause 0
