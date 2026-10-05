# Installs in_spotitube.dll into Winamp's Plugins folder.
# Usage: .\install.ps1 [-Dll path\to\in_spotitube.dll] [-Winamp 'C:\Program Files (x86)\Winamp']
param(
    [string]$Dll,
    [string]$Winamp = "${env:ProgramFiles(x86)}\Winamp"
)
$ErrorActionPreference = 'Stop'

if (-not $Dll) {
    # Release download sits next to the script; a source checkout has it under target.
    $Dll = @("$PSScriptRoot\in_spotitube.dll", "$PSScriptRoot\target\i686-pc-windows-msvc\release\in_spotitube.dll") |
        Where-Object { Test-Path $_ } | Select-Object -First 1
    if (-not $Dll) { throw 'in_spotitube.dll not found. Download it from the Releases page or build it first.' }
}
$Dll = (Resolve-Path $Dll).Path
$plugins = Join-Path $Winamp 'Plugins'
if (-not (Test-Path $plugins)) { throw "Winamp Plugins folder not found: $plugins. Pass -Winamp <folder>." }

$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole(
    [Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    Write-Host 'Requesting administrator rights to write to the Plugins folder...'
    $proc = Start-Process powershell -Verb RunAs -Wait -PassThru -ArgumentList @(
        '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSCommandPath`"", '-Dll', "`"$Dll`"", '-Winamp', "`"$Winamp`"")
    exit $proc.ExitCode
}

while (Get-Process winamp -ErrorAction SilentlyContinue) {
    Read-Host 'Winamp is running. Close it, then press Enter'
}

# The pre-rename plugin claims the same links; leaving both would make them fight over playback.
$legacy = Join-Path $plugins 'in_spotify.dll'
if (Test-Path $legacy) {
    Remove-Item $legacy
    Write-Host 'Removed old in_spotify.dll.'
}
Copy-Item $Dll $plugins -Force
Write-Host "Installed to $plugins."

if (-not (Get-Command yt-dlp -ErrorAction SilentlyContinue)) {
    $answer = Read-Host 'yt-dlp (needed for YouTube) is not on PATH. Install it with winget now? [y/N]'
    if ($answer -match '^[yY]') {
        winget install --id yt-dlp.yt-dlp -e
        if ($LASTEXITCODE -ne 0) { Write-Warning "winget exited with code $LASTEXITCODE; install yt-dlp manually." }
    }
}
Read-Host 'Done. Press Enter to close'
