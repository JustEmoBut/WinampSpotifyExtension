# Winamp SpotiTube

Winamp 5.9 input plugin (`in_spotitube.dll`) in Rust: Spotify via librespot, YouTube via external yt-dlp + ffmpeg.

## Build & install
- Target is 32-bit `i686-pc-windows-msvc` (default via `.cargo/config.toml`); Winamp is a 32-bit process.
- `cargo build --release`, `cargo test --release`.
- Install: `.\install.ps1` (self-elevates, removes the pre-rename `in_spotify.dll`). Release: push a `v*` tag; `.github/workflows/release.yml` builds and attaches the DLL + script.
- Renamed from `in_spotify`: `cache_dir()` moves `%APPDATA%\in_spotify` to `in_spotitube` once so cached credentials survive.
- `Cargo.lock` pins `vergen` 9.0.6: librespot-core 0.8.0's build script breaks with vergen 9.1.0. Don't `cargo update` blindly.

## Layout
- `src/lib.rs`: Winamp ABI (`In_Module`/`Out_Module` per SDK `in2.h`/`out.h`), Spotify playback, shared output path (`write_pcm`), playlist IPC.
- `src/youtube.rs`: link parsing, yt-dlp/ffmpeg process handling.

## Winamp constraints (verified against Winamp 5.9 source)
- `in_mp3` is always loaded first and claims every `http(s)` URL. `Init()` wraps the stock plugins' `IsOurFile` so our links fall through to us.
- Winamp rewrites bare `spotify:` entries into file paths when saving the playlist (`<dir>\spotify:track:...`). Insert `https://open.spotify.com/track/<id>` entries; `parse_link` also accepts the mangled form.
- `SAAddPCMData`/`VSAAddPCMData` always read 576 frames: the buffer passed to them must be at least that long (`VIS_MIN_FRAMES`).
- All DSP/vis/output calls go through `write_pcm` under the `OUT_OPEN` lock: Winamp's EQ (`benskiQ`) uses a global, unsynchronized buffer and expects a single caller.
- Album/playlist entries are replaced in place via `IPC_PE_INSERTFILENAMEW` + `IPC_PE_DELETEINDEX`, then `IPC_STARTPLAY`.

## YouTube
- yt-dlp needs `--encoding utf-8`; `PYTHONIOENCODING`/`PYTHONUTF8` silently drop non-ASCII characters.
- `watch?v=..&list=..` expands the playlist, except auto mixes (`list=RD...`), which yt-dlp can't flat-list.
- Seeking restarts ffmpeg with `-ss`; a 403 on open is retried once with a freshly resolved URL.

## Debugging crashes
- Known open issue: intermittent heap corruption (`0xc0000374`) seen after the YouTube changes; root cause not found yet.
- Crash dumps land in `%LOCALAPPDATA%\CrashDumps`; analyze with `cdbX86.exe -z <dmp>` (WinDbg, x86 build) and symbol path including `target\i686-pc-windows-msvc\release`.
- To catch corruption at the write site, enable full page heap for `winamp.exe` (admin):
  `reg add "HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\winamp.exe" /v GlobalFlag /t REG_SZ /d 0x02000000 /f`
  and remove it afterwards with `reg delete ... /v GlobalFlag /f`.
