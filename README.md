# WinampSpotifyExtension

A Winamp input plugin (`in_spotify.dll`) that plays Spotify tracks, albums and playlists straight from the Winamp playlist. Audio is decoded by [librespot](https://github.com/librespot-org/librespot) and fed through Winamp's own output, EQ, DSP and visualization plugins.

> **Disclaimer:** librespot is an unofficial Spotify client. Using it may violate Spotify's Terms of Service and could put your account at risk. Use at your own risk. This project only streams; it never saves audio to disk.

## Features

- Paste `open.spotify.com` links or `spotify:` URIs into the playlist (track, album, playlist)
- Album and playlist entries expand into their individual tracks when played
- Track titles (`Artist - Title`) and durations appear in the playlist
- Seek, pause, volume, EQ, DSP and visualizers work like any local file
- One-time browser login; credentials are cached

## Requirements

| | |
|---|---|
| OS | Windows 10/11 (x64 or x86) |
| Winamp | 5.9.2 (build 10042) — other 5.x versions with Unicode input plugin support should work |
| Spotify | **Premium** account (librespot cannot play on free accounts) |
| Build tools | [Rust](https://rustup.rs) (stable) and Visual Studio / Build Tools with the **Desktop development with C++** workload |

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

### 2. Build the plugin

Winamp is a 32-bit application, so the plugin must be built for `i686` (already the default via `.cargo/config.toml`).

```powershell
git clone https://github.com/JustEmoBut/WinampSpotifyExtension.git
cd WinampSpotifyExtension
rustup target add i686-pc-windows-msvc
cargo build --release
```

Output: `target\i686-pc-windows-msvc\release\in_spotify.dll`

### 3. Install

Close Winamp, then in an **administrator** PowerShell from the repository folder:

```powershell
Copy-Item 'target\i686-pc-windows-msvc\release\in_spotify.dll' 'C:\Program Files (x86)\Winamp\Plugins\' -Force
```

To uninstall, delete `in_spotify.dll` from the `Plugins` folder (and optionally `%APPDATA%\in_spotify`).

## Usage

1. Copy a link in Spotify (*Share → Copy link*), e.g.
   `https://open.spotify.com/track/...`, `https://open.spotify.com/album/...`, `https://open.spotify.com/playlist/...`
   — or a URI such as `spotify:track:<id>`.
2. In the Winamp playlist: **Add → Add URL**, paste and play.
3. **First play only:** your browser opens the Spotify login page. After you approve, the browser is redirected to `http://127.0.0.1:8989/login` and playback starts. Credentials are cached in `%APPDATA%\in_spotify`, so you won't be asked again.

Titles show as raw links until you are logged in; after the first track starts they fill in automatically.

## Troubleshooting

| Problem | Fix |
|---|---|
| UAC: *"This app has been blocked for your protection"* | See [Fix Winamp's revoked certificate](#1-fix-winamps-revoked-certificate-if-needed). |
| Playlist entry stuck at **[Connecting to host]** | The plugin isn't loaded, so Winamp's stock `in_mp3` grabbed the link. Check that `in_spotify.dll` is in `Winamp\Plugins` and restart Winamp. |
| Login fails / browser shows a connection error | Port `8989` must be free; close whatever is using it and try again. |
| *Spotify connect failed* after it used to work | Delete `%APPDATA%\in_spotify` to force a fresh login. |
| `signtool.exe`: *not a valid application for this OS platform* | You used the `arm64` build; use the `x64` (or `x86`) folder. |
| Build error in `librespot-core` build script about `vergen` | Keep the committed `Cargo.lock` (it pins `vergen` to 9.0.6); don't run `cargo update` blindly. |

## Limitations

- Artist links, podcasts/episodes and local files are not supported (skipped when expanding playlists).
- A very short glitch may be audible right after seeking.
- Output is always 44.1 kHz / 16-bit stereo.

## How it works

- Winamp always lets its stock `in_mp3` plugin claim every `http(s)` URL first. On startup this plugin wraps the stock plugins' `IsOurFile` so that Spotify links fall through to it; other URLs (e.g. internet radio) are unaffected.
- A custom librespot `Sink` converts decoded audio to 16-bit PCM and writes it through Winamp's DSP, visualization and output plugin.
- Album/playlist entries are replaced in place with their tracks via Winamp's playlist IPC.

## License

[MIT](LICENSE). librespot is licensed separately under MIT.
