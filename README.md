# Winamp SpotiTube

A Winamp input plugin (`in_spotitube.dll`) that plays Spotify tracks, albums and playlists — and YouTube videos and playlists (audio only) — straight from the Winamp playlist. Spotify audio is decoded by [librespot](https://github.com/librespot-org/librespot), YouTube audio by [yt-dlp](https://github.com/yt-dlp/yt-dlp) + [ffmpeg](https://ffmpeg.org); both are fed through Winamp's own output, EQ, DSP and visualization plugins.

> **Disclaimer:** librespot is an unofficial Spotify client, and streaming YouTube through yt-dlp is not permitted by YouTube's Terms of Service. Either may put your account at risk. Use at your own risk. This project only streams; it never saves audio to disk.

## Features

- Paste `open.spotify.com` links or `spotify:` URIs into the playlist (track, album, playlist)
- Paste YouTube links: `youtube.com/watch?v=`, `youtu.be/`, `/shorts/`, `music.youtube.com`, `youtube.com/playlist?list=`
- Album and playlist entries expand into their individual tracks when played
- Track titles and durations appear in the playlist
- Seek, pause, volume, EQ, DSP and visualizers work like any local file
- One-time browser login; credentials are cached
- File info (Alt+3) shows title, length and link, and can open the link in the browser
- If yt-dlp fails, it updates itself once per session (`yt-dlp -U`) and retries

## Requirements

| | |
|---|---|
| OS | Windows 10/11 (x64 or x86) |
| Winamp | 5.9.2 (build 10042) — other 5.x versions with Unicode input plugin support should work |
| Spotify | **Premium** account (librespot cannot play on free accounts) |
| YouTube | `yt-dlp` and `ffmpeg` on `PATH`: `winget install yt-dlp.yt-dlp` (pulls in `yt-dlp.FFmpeg`) |
| Build tools (source only) | [Rust](https://rustup.rs) (stable) and Visual Studio / Build Tools with the **Desktop development with C++** workload |

## Installation

### 1. Fix Winamp's revoked certificate (if needed)

Winamp 5.9.2 binaries are signed with a code-signing certificate that has been **revoked** by its issuer (not merely expired). Reinstalling from the official site does not help, because the current installer ships the same signed files.

**Symptom:** on first launch Winamp shows its setup wizard, and pressing *Next* loops or shows:

> *User Account Control — This app has been blocked for your protection. An administrator has blocked you from running this app.*
> Publisher: Unknown · CLSID: `{3B29AB5C-52CB-4A36-9314-E3FEE0BA7468}`

**Cause:** that CLSID is the *Winamp Elevator* COM server (`elevator.exe`). Winamp launches it with administrator rights to register file associations, and Windows refuses to elevate a binary whose signature is revoked. `winamp.exe` itself runs fine without elevation.

You can confirm it in PowerShell:

```powershell
Get-AuthenticodeSignature 'C:\Program Files (x86)\Winamp\elevator.exe' | Select-Object Status, StatusMessage
# StatusMessage: A certificate was explicitly revoked by its issuer.
```

**Fix:** remove the revoked signature from `elevator.exe`. An unsigned binary gets a normal *Unknown publisher* UAC prompt instead of being blocked.

1. Close Winamp.
2. Find `signtool.exe` (ships with the Windows SDK, installed alongside Visual Studio's C++ workload). Use the build matching your CPU (`x64` on most PCs, **not** `arm64`):
   ```powershell
   Get-ChildItem 'C:\Program Files (x86)\Windows Kits\10\bin' -Recurse -Filter signtool.exe | Select-Object FullName
   ```
3. In an **administrator** PowerShell:
   ```powershell
   cd 'C:\Program Files (x86)\Winamp'
   Copy-Item elevator.exe elevator.exe.bak
   & 'C:\Program Files (x86)\Windows Kits\10\bin\<version>\x64\signtool.exe' remove /s elevator.exe
   Get-AuthenticodeSignature elevator.exe | Select-Object Status   # should print NotSigned
   ```
   PowerShell needs the `&` in front of a quoted path to run it.
4. Start Winamp, finish the setup wizard and answer **Yes** on the UAC prompt.

Notes:

- Removing the signature means Windows can no longer verify that file. The risk is low for a file from the official installer, but it is a deliberate trade-off.
- Reinstalling or updating Winamp restores the signed file, so repeat the steps afterwards.
- To undo: copy `elevator.exe.bak` back over `elevator.exe`.
- Do **not** disable UAC or certificate revocation checking system-wide to work around this.
- Alternative without touching any file: skip file-type association in the wizard and in *Preferences → File Types*, and set default apps in *Windows Settings → Apps → Default apps* instead.

### 2. Install the plugin

Download `in_spotitube.dll` and `install.ps1` from the [latest release](https://github.com/JustEmoBut/WinampSpotifyExtension/releases/latest) into the same folder, then in PowerShell:

```powershell
powershell -ExecutionPolicy Bypass -File .\install.ps1
```

The script asks for administrator rights, waits for Winamp to close, removes the pre-rename `in_spotify.dll` if present, copies the plugin to `Winamp\Plugins` and offers to install yt-dlp via winget. Pass `-Winamp <folder>` if Winamp isn't in `C:\Program Files (x86)\Winamp`.

To uninstall, delete `in_spotitube.dll` from the `Plugins` folder (and optionally `%APPDATA%\in_spotitube`).

### Building from source

Winamp is a 32-bit application, so the plugin must be built for `i686` (already the default via `.cargo/config.toml`).

```powershell
git clone https://github.com/JustEmoBut/WinampSpotifyExtension.git
cd WinampSpotifyExtension
rustup target add i686-pc-windows-msvc
cargo build --release
.\install.ps1
```

Output: `target\i686-pc-windows-msvc\release\in_spotitube.dll` (`install.ps1` picks it up automatically).

Releases are built by GitHub Actions when a `v*` tag is pushed.

## Usage

1. Copy a link in Spotify (*Share → Copy link*), e.g.
   `https://open.spotify.com/track/...`, `https://open.spotify.com/album/...`, `https://open.spotify.com/playlist/...`
   — or a URI such as `spotify:track:<id>`.
2. In the Winamp playlist: **Add → Add URL**, paste and play.
3. **First play only:** your browser opens the Spotify login page. After you approve, the browser is redirected to `http://127.0.0.1:8989/login` and playback starts. Credentials are cached in `%APPDATA%\in_spotitube`, so you won't be asked again.

Titles show as raw links until you are logged in; after the first track starts they fill in automatically.

**YouTube:** add a video or playlist link the same way. No login is needed. Each video takes ~3 seconds to start while yt-dlp resolves the stream. Restart Winamp after installing yt-dlp/ffmpeg so it sees the updated `PATH`. Keep yt-dlp current (`winget upgrade yt-dlp.yt-dlp`) — YouTube changes often and old versions stop working.

## Troubleshooting

| Problem | Fix |
|---|---|
| UAC: *"This app has been blocked for your protection"* | See [Fix Winamp's revoked certificate](#1-fix-winamps-revoked-certificate-if-needed). |
| Playlist entry stuck at **[Connecting to host]** | The plugin isn't loaded, so Winamp's stock `in_mp3` grabbed the link. Check that `in_spotitube.dll` is in `Winamp\Plugins` and restart Winamp. |
| Login fails / browser shows a connection error | Port `8989` must be free; close whatever is using it and try again. |
| *Spotify connect failed* after it used to work | Delete `%APPDATA%\in_spotitube` to force a fresh login. |
| `signtool.exe`: *not a valid application for this OS platform* | You used the `arm64` build; use the `x64` (or `x86`) folder. |
| *yt-dlp / ffmpeg not found on PATH* | Install them with winget (see Requirements), then restart Winamp. |
| YouTube error from yt-dlp (e.g. *Sign in to confirm*, *unavailable*) | Update yt-dlp: `winget upgrade yt-dlp.yt-dlp`. Private, age-restricted or region-locked videos can't play. |
| Build error in `librespot-core` build script about `vergen` | Keep the committed `Cargo.lock` (it pins `vergen` to 9.0.6); don't run `cargo update` blindly. |

## Limitations

- Artist links, podcasts/episodes and local files are not supported (skipped when expanding playlists).
- A very short glitch may be audible right after seeking.
- YouTube is audio only. A watch link with `&list=` expands the whole playlist, except auto-generated mixes (`list=RD...`), which play just that video.
- Pausing a YouTube video for a long time may end the stream early.
- **Known issue:** Winamp can occasionally crash with a heap corruption error (`0xc0000374`); the cause is still being investigated.
- Output is always 44.1 kHz / 16-bit stereo.

## How it works

- Winamp always lets its stock `in_mp3` plugin claim every `http(s)` URL first. On startup this plugin wraps the stock plugins' `IsOurFile` so that Spotify and YouTube links fall through to it; other URLs (e.g. internet radio) are unaffected.
- A custom librespot `Sink` converts decoded audio to 16-bit PCM and writes it through Winamp's DSP, visualization and output plugin.
- YouTube: `yt-dlp` resolves the title, duration and best audio stream URL; `ffmpeg` decodes it to the same 16-bit PCM path. Seeking restarts ffmpeg at the new position.
- Album/playlist entries are replaced in place with their tracks via Winamp's playlist IPC.

## License

[MIT](LICENSE). librespot is licensed separately under MIT.
