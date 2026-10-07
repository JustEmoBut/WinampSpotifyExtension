# Winamp SpotiTube

Two Winamp 5.9 plugins in Rust: the input plugin `in_spotitube.dll` (Spotify via librespot, YouTube via external yt-dlp + ffmpeg) and the Media Library plugin `ml_spotitube.dll` (YouTube search).

## Build & install
- Target is 32-bit `i686-pc-windows-msvc` (default via `.cargo/config.toml`); Winamp is a 32-bit process.
- Cargo workspace: `in_spotitube` (root) and `ml_spotitube/`. `cargo build --release --workspace`, `cargo test --release --workspace`; `-- --ignored` runs live tests (network, cached Spotify login).
- Install: `install.bat` (plain batch, so PowerShell execution policy can't block it; finds Winamp via the registry (only a folder with `winamp.exe` counts: Winamp's uninstaller leaves `Plugins` behind when it holds our DLLs), self-elevates with a one-line `Start-Process -Verb RunAs`, removes the pre-rename `in_spotify.dll`; `/uninstall` removes both plugins). Batch gotchas: save `%~dp0` before `shift` (it shifts `%0` too) call `%SystemRoot%\System32\find.exe` by full path (Git's Unix `find` can shadow it), and elevate through `cmd.exe /c ""script" args"`: an elevated `.bat` is started as `cmd /C "script" args`, which strips the outer quotes when an argument is quoted, so the script silently never runs (verified with an elevated probe; non-elevated launches don't show it). Winamp 5.9.2's `elevator.exe` is signed with Winamp SA's revoked certificate (Windows blocks its elevation, the first-run wizard loops): install.bat strips that signature itself, no Windows SDK (zeroes the PE security directory entry and truncates the trailing certificate block, keeps `elevator.exe.bak`), only when `Get-AuthenticodeSignature` reports `UnknownError` with the pinned thumbprint; result matches `signtool remove /s` except the PE checksum. It clears `PSModulePath` first: started from PowerShell 7, Windows PowerShell inherits it and can't load `Get-AuthenticodeSignature`. If Winamp isn't installed it runs a `winamp*.exe` setup (NSIS) from its own folder or Downloads with `/S` via `start /wait`. Missing yt-dlp/ffmpeg are installed with `winget install yt-dlp.yt-dlp` without asking (agreements accepted via flags). Release: push a `v*` tag; `.github/workflows/release.yml` builds a `SpotiTube-<tag>.zip` (both DLLs + `install.bat`, flat) and attaches it. `.github/workflows/ci.yml` tests every push to main and saves the build cache that the release job restores (tags can only read main's caches).
- Renamed from `in_spotify`: `cache_dir()` moves `%APPDATA%\in_spotify` to `in_spotitube` once so cached credentials survive.
- `Cargo.lock` pins `vergen` 9.0.6: librespot-core 0.8.0's build script breaks with vergen 9.1.0. Don't `cargo update` blindly.

## Layout
- `src/lib.rs`: Winamp ABI (`In_Module`/`Out_Module` per SDK `in2.h`/`out.h`), Spotify playback, shared output path (`write_pcm`), playlist IPC.
- `src/youtube.rs`: link parsing, yt-dlp/ffmpeg process handling.
- `src/albumart.rs`: album art provider registered through Winamp's Wasabi service API.
- `ml_spotitube/`: Media Library plugin (`ml_spotitube.dll`, no librespot) with a YouTube search view ("More" fetches the next page with yt-dlp `-I`; pages can overlap since YouTube's ranking shifts, so duplicates are dropped; a failed search runs `yt-dlp -U` once per session and retries) built from raw Win32 controls, skinned via `ML_IPC_SKINWINDOW`. It only enqueues links; `in_spotitube.dll` plays them.
- `src/update.rs`: startup update check against GitHub's `releases/latest` API (needs the repo to be public: unauthenticated requests to a private repo get 404). Only release builds check: CI bakes the tag in as `SPOTITUBE_VERSION`. Each version is offered once (`update_notified` file). Accepting downloads the zip (only from this repo's release URLs), checks it against the asset's `digest` (`sha256:`, via `BCryptHash`), extracts it with `%SystemRoot%\System32\tar.exe` into `%TEMP%\SpotiTube-update-<tag>` (tag must be path-safe) and starts its `install.bat /update` (install.bat also switches to update mode when it runs from a `SpotiTube-update-*` folder, because v0.3.3 and older start it without arguments), which closes Winamp gracefully (`taskkill` without `/f`, up to 30 s, never forced), installs without prompts and restarts it. The update prompt uses `MB_SETFOREGROUND | MB_TOPMOST`: it's shown from a worker thread, which otherwise ends up behind other windows. Never relax the download checks: this path runs downloaded code.
- Settings: `%APPDATA%\in_spotitube\config.ini` (`spotify_bitrate`, `spotify_normalisation`, `youtube_format`, `check_updates`), opened by the plugin's Configure button.

## Winamp constraints (verified against Winamp 5.9 source)
- `in_mp3` is always loaded first and claims every `http(s)` URL. `Init()` wraps the stock plugins' `IsOurFile` so our links fall through to us.
- Winamp rewrites bare `spotify:` entries into file paths when saving the playlist (`<dir>\spotify:track:...`). Insert `https://open.spotify.com/track/<id>` entries; `parse_link` also accepts the mangled form.
- `SAAddPCMData`/`VSAAddPCMData` always read 576 frames: the buffer passed to them must be at least that long (`VIS_MIN_FRAMES`).
- All DSP/vis/output calls go through `write_pcm` under the `OUT_OPEN` lock: Winamp's EQ (`benskiQ`) uses a global, unsynchronized buffer and expects a single caller.
- Album/playlist entries are replaced in place via `IPC_PE_INSERTFILENAMEW` + `IPC_PE_DELETEINDEX`, then `IPC_SETPLAYLISTPOS` + the Stop/Play button commands (40047/40045). Never `IPC_STARTPLAY` to start a given entry: `BeginPlayback()` resets the position to 0 (shuffle: random).
- Winamp source for verifying ABI: `github.com/manfromafar/winamp` (the official WinampDesktop repo is gone). Sparse-clone it; key paths: `Src/Winamp/IN2.H`, `Src/Winamp/In.cpp`, `Src/albumart/AlbumArt.cpp`, `Src/Plugins/Input/in_wv/wasabi/`.
- `In_Module.service` is only written by Winamp when `version` has `IN_INIT_RET`; we don't set it, so our struct ends at `out_mod`.
- Tags come from the exported `winampGetExtendedFileInfoW` (keys like `artist`, `album`, `year`); returning 0 lets Winamp fall back to `GetFileInfo`.

## Album art
- Wasabi objects are C++ `Dispatchable`s: a vtable with one `thiscall` `_dispatch(msg, retval, params, nparam)`, each param passed as a pointer to its value (`bfc/dispatch.h`). Rust mirrors this with `extern "thiscall"`.
- `IPC_GET_API_SERVICE` gives `api_service`; the provider factory is registered in `Init` and deregistered in `Quit`.
- For URLs Winamp only asks `ALBUMARTPROVIDER_TYPE_EMBEDDED` providers, first `IsMine` wins. Buffers returned to Winamp must come from `api_memmgr` (`sysMalloc`), since Winamp frees them.
- Images are downloaded with WinINet (3 s timeout) because Winamp may call on the UI thread. Classic skins have no album art panel.

## Media Library plugin (`ml_spotitube/`)
- ABI per `gen_ml/ml.h` and `ml_ipc_0313.h`: `MLHDR_VER` (0x17, wide description), tree item via `ML_IPC_TREEITEM_ADDW`, view created on `ML_MSG_TREE_ONCREATEVIEW`.
- The library doesn't clear the previous view's area: the view paints its own background (`WADLG_WNDBG` from `ML_IPC_SKIN_WADLG_GETFUNC`).
- The library's dialog navigation swallows Enter; the query edit is subclassed to return `DLGC_WANTALLKEYS`.
- Tree icon: drawn in code as a 24-bit `HBITMAP` (no resource compiler), added with `ML_IPC_IMAGELIST_ADD` + `MLIF_FILTER1` (white maps to the skin's item color). The tree item's `imageIndex` is a tag, not an index; keep the bitmap alive, the library copies it on each reload.
- Results are enqueued with `IPC_PLAYFILEW` + `enqueueFileWithMetaStructW` (5.9 layout has an `ext` field).
- The results' right-click menu goes through `ML_IPC_TRACKSKINNEDPOPUPEX` (`MLSKINNEDPOPUP`) so it's drawn in the skin; with `TPM_RETURNCMD` it returns the chosen command id.

## YouTube
- yt-dlp needs `--encoding utf-8`; `PYTHONIOENCODING`/`PYTHONUTF8` silently drop non-ASCII characters.
- `watch?v=..&list=..` expands the playlist, except auto mixes (`list=RD...`), which yt-dlp can't flat-list.
- Seeking restarts ffmpeg with `-ss`; a 403 on open is retried once with a freshly resolved URL.

## Debugging crashes
- Known open issue: intermittent heap corruption (`0xc0000374`); root cause not found yet. Dumps show it detected in unrelated `free`s (clearing a playlist, NDE strings, a worker thread) with no thread in our code, once in a session where Spotify never played. Reproduced by clearing a large Spotify playlist; a full page heap run didn't crash yet.
- Crash dumps land in `%LOCALAPPDATA%\CrashDumps`; analyze with `cdbX86.exe -z <dmp>` (WinDbg, x86 build) and symbol path including `target\i686-pc-windows-msvc\release`.
- To catch corruption at the write site, enable full page heap for `winamp.exe` (admin):
  `reg add "HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\winamp.exe" /v GlobalFlag /t REG_SZ /d 0x02000000 /f`
  and remove it afterwards with `reg delete ... /v GlobalFlag /f`.
