# Winamp SpotiTube

Winamp 5.9 input plugin (`in_spotitube.dll`) in Rust: Spotify via librespot, YouTube via external yt-dlp + ffmpeg.

## Build & install
- Target is 32-bit `i686-pc-windows-msvc` (default via `.cargo/config.toml`); Winamp is a 32-bit process.
- `cargo build --release`, `cargo test --release`.
- Install: `install.bat` (wraps `install.ps1`, which finds Winamp via the registry, self-elevates and removes the pre-rename `in_spotify.dll`; `-Uninstall` removes the plugin). Release: push a `v*` tag; `.github/workflows/release.yml` builds and attaches the DLL + both scripts.
- Renamed from `in_spotify`: `cache_dir()` moves `%APPDATA%\in_spotify` to `in_spotitube` once so cached credentials survive.
- `Cargo.lock` pins `vergen` 9.0.6: librespot-core 0.8.0's build script breaks with vergen 9.1.0. Don't `cargo update` blindly.

## Layout
- `src/lib.rs`: Winamp ABI (`In_Module`/`Out_Module` per SDK `in2.h`/`out.h`), Spotify playback, shared output path (`write_pcm`), playlist IPC.
- `src/youtube.rs`: link parsing, yt-dlp/ffmpeg process handling.
- `src/albumart.rs`: album art provider registered through Winamp's Wasabi service API.
- Settings: `%APPDATA%\in_spotitube\config.ini` (`spotify_bitrate`, `youtube_format`), opened by the plugin's Configure button.

## Winamp constraints (verified against Winamp 5.9 source)
- `in_mp3` is always loaded first and claims every `http(s)` URL. `Init()` wraps the stock plugins' `IsOurFile` so our links fall through to us.
- Winamp rewrites bare `spotify:` entries into file paths when saving the playlist (`<dir>\spotify:track:...`). Insert `https://open.spotify.com/track/<id>` entries; `parse_link` also accepts the mangled form.
- `SAAddPCMData`/`VSAAddPCMData` always read 576 frames: the buffer passed to them must be at least that long (`VIS_MIN_FRAMES`).
- All DSP/vis/output calls go through `write_pcm` under the `OUT_OPEN` lock: Winamp's EQ (`benskiQ`) uses a global, unsynchronized buffer and expects a single caller.
- Album/playlist entries are replaced in place via `IPC_PE_INSERTFILENAMEW` + `IPC_PE_DELETEINDEX`, then `IPC_STARTPLAY`.

- Winamp source for verifying ABI: `github.com/manfromafar/winamp` (the official WinampDesktop repo is gone). Sparse-clone it; key paths: `Src/Winamp/IN2.H`, `Src/Winamp/In.cpp`, `Src/albumart/AlbumArt.cpp`, `Src/Plugins/Input/in_wv/wasabi/`.
- `In_Module.service` is only written by Winamp when `version` has `IN_INIT_RET`; we don't set it, so our struct ends at `out_mod`.
- Tags come from the exported `winampGetExtendedFileInfoW` (keys like `artist`, `album`, `year`); returning 0 lets Winamp fall back to `GetFileInfo`.

## Album art
- Wasabi objects are C++ `Dispatchable`s: a vtable with one `thiscall` `_dispatch(msg, retval, params, nparam)`, each param passed as a pointer to its value (`bfc/dispatch.h`). Rust mirrors this with `extern "thiscall"`.
- `IPC_GET_API_SERVICE` gives `api_service`; the provider factory is registered in `Init` and deregistered in `Quit`.
- For URLs Winamp only asks `ALBUMARTPROVIDER_TYPE_EMBEDDED` providers, first `IsMine` wins. Buffers returned to Winamp must come from `api_memmgr` (`sysMalloc`), since Winamp frees them.
- Images are downloaded with WinINet (3 s timeout) because Winamp may call on the UI thread. Classic skins have no album art panel.

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
