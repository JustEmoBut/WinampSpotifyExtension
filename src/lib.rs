//! Winamp input plugin that plays Spotify tracks through librespot.
//! Playlist entries: `spotify:{track,album,playlist}:<id>` or the matching open.spotify.com links.
//! Also plays YouTube videos/playlists (audio only) through yt-dlp + ffmpeg, see `youtube`.
//! Album/playlist/artist entries are expanded into their tracks when played.
//! Struct layouts and IPC ids follow the Winamp SDK headers in2.h / out.h / wa_ipc.h / ipc_pe.h.

mod youtube;

use std::collections::HashMap;
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use librespot::core::{
    SpotifyUri, authentication::Credentials, cache::Cache, config::SessionConfig,
    session::Session, spotify_id::SpotifyId,
};
use librespot::metadata::{Album, Artist, Metadata, Playlist, Track};
use librespot::oauth::OAuthClientBuilder;
use librespot::playback::{
    audio_backend::{Sink, SinkResult},
    config::{Bitrate, PlayerConfig},
    convert::Converter,
    decoder::AudioPacket,
    mixer::NoOpVolume,
    player::{Player, PlayerEvent},
};
use tokio::runtime::Runtime;

type Hwnd = *mut c_void;

const IN_VER_UNICODE: i32 = 0x0F00_0100;
const IN_UNICODE_MASK: i32 = 0x0F00_0000;
const IN_MODULE_FLAG_USES_OUTPUT_PLUGIN: i32 = 1;
const GETFILEINFO_TITLE_LENGTH: usize = 2048;
const INFOBOX_UNCHANGED: i32 = 1;
const WM_WA_MPEG_EOF: u32 = 0x0400 + 2; // WM_USER + 2
const MB_ICONERROR: u32 = 0x10;
const MB_YESNO: u32 = 0x04;
const MB_ICONINFORMATION: u32 = 0x40;
const IDYES: i32 = 6;
const SW_SHOWNORMAL: i32 = 1;
const WM_COPYDATA: u32 = 0x004A;
const WM_WA_IPC: u32 = 0x0400; // WM_USER
const IPC_STARTPLAY: isize = 102;
const IPC_SETPLAYLISTPOS: isize = 121;
const IPC_GETLISTPOS: isize = 125;
const IPC_UPDTITLE: isize = 243;
const IPC_REFRESHPLCACHE: isize = 247;
const IPC_GETWND: isize = 260;
const IPC_GETWND_PE: usize = 1;
const IPC_PE_DELETEINDEX: usize = 104;
const IPC_PE_INSERTFILENAMEW: usize = 114;
const MAX_PATH: usize = 260;

/// Stock plugins Winamp loads before ours (In.cpp preload order); in_mp3 claims every http(s) URL.
const STOCK_INPUT_PLUGINS: [&str; 16] = [
    "in_mp3.dll", "in_avi.dll", "in_cdda.dll", "in_linein.dll", "in_mkv.dll", "in_nsv.dll",
    "in_swf.dll", "in_vorbis.dll", "in_mp4.dll", "in_dshow.dll", "in_flac.dll", "in_flv.dll",
    "in_wm.dll", "in_wave.dll", "in_midi.dll", "in_mod.dll",
];

const SAMPLE_RATE: i32 = 44100; // librespot::playback::SAMPLE_RATE
const CHANNELS: i32 = 2; // librespot::playback::NUM_CHANNELS
const BITS: i32 = 16;
const BYTES_PER_FRAME: usize = (CHANNELS * BITS / 8) as usize;
/// SA/VSAAddPCMData read at least this many frames (in2.h: "needs at least 576 samples").
const VIS_MIN_FRAMES: usize = 576;
/// Shown for YouTube, whose real bitrate isn't known when the output opens.
// ponytail: display only; print yt-dlp's `abr` and call SetInfo again if accuracy matters.
const YOUTUBE_DISPLAY_KBPS: i32 = 160;
const DEFAULT_SPOTIFY_KBPS: i32 = 320;
const DEFAULT_YOUTUBE_FORMAT: &str = "bestaudio";
const CONFIG_FILE: &str = "config.ini";
const DEFAULT_CONFIG: &str = "; SpotiTube settings. Restart Winamp after changing spotify_bitrate.
; Spotify quality in kbps: 96, 160 or 320.
spotify_bitrate=320
; yt-dlp format selector, e.g. bestaudio, bestaudio[abr<=128], worstaudio.
youtube_format=bestaudio
";
const POLL: Duration = Duration::from_millis(10);
const VOLUME_KEEP: i32 = -666; // SDK convention: re-apply current volume

const OAUTH_REDIRECT: &str = "http://127.0.0.1:8989/login";
const OAUTH_SCOPES: &[&str] = &["streaming"];

#[repr(C)]
struct OutModule {
    version: i32,
    description: *mut u8,
    id: isize,
    h_main_window: Hwnd,
    h_dll_instance: *mut c_void,
    config: Option<unsafe extern "C" fn(Hwnd)>,
    about: Option<unsafe extern "C" fn(Hwnd)>,
    init: Option<unsafe extern "C" fn()>,
    quit: Option<unsafe extern "C" fn()>,
    open: unsafe extern "C" fn(i32, i32, i32, i32, i32) -> i32,
    close: unsafe extern "C" fn(),
    write: unsafe extern "C" fn(*mut u8, i32) -> i32,
    can_write: unsafe extern "C" fn() -> i32,
    is_playing: unsafe extern "C" fn() -> i32,
    pause: unsafe extern "C" fn(i32) -> i32,
    set_volume: unsafe extern "C" fn(i32),
    set_pan: unsafe extern "C" fn(i32),
    flush: unsafe extern "C" fn(i32),
    get_output_time: unsafe extern "C" fn() -> i32,
    get_written_time: unsafe extern "C" fn() -> i32,
}

#[repr(C)]
struct InModule {
    version: i32,
    description: *const u8,
    h_main_window: Hwnd,
    h_dll_instance: *mut c_void,
    file_extensions: *const u8,
    is_seekable: i32,
    uses_output_plug: i32,
    config: unsafe extern "C" fn(Hwnd),
    about: unsafe extern "C" fn(Hwnd),
    init: unsafe extern "C" fn(),
    quit: unsafe extern "C" fn(),
    get_file_info: unsafe extern "C" fn(*const u16, *mut u16, *mut i32),
    info_box: unsafe extern "C" fn(*const u16, Hwnd) -> i32,
    is_our_file: unsafe extern "C" fn(*const u16) -> i32,
    play: unsafe extern "C" fn(*const u16) -> i32,
    pause: unsafe extern "C" fn(),
    un_pause: unsafe extern "C" fn(),
    is_paused: unsafe extern "C" fn() -> i32,
    stop: unsafe extern "C" fn(),
    get_length: unsafe extern "C" fn() -> i32,
    get_output_time: unsafe extern "C" fn() -> i32,
    set_output_time: unsafe extern "C" fn(i32),
    set_volume: unsafe extern "C" fn(i32),
    set_pan: unsafe extern "C" fn(i32),
    // Filled in by Winamp.
    sa_vsa_init: Option<unsafe extern "C" fn(i32, i32)>,
    sa_vsa_deinit: Option<unsafe extern "C" fn()>,
    sa_add_pcm_data: Option<unsafe extern "C" fn(*mut c_void, i32, i32, i32)>,
    sa_get_mode: Option<unsafe extern "C" fn() -> i32>,
    sa_add: Option<unsafe extern "C" fn(*mut c_void, i32, i32) -> i32>,
    vsa_add_pcm_data: Option<unsafe extern "C" fn(*mut c_void, i32, i32, i32)>,
    vsa_get_mode: Option<unsafe extern "C" fn(*mut i32, *mut i32) -> i32>,
    vsa_add: Option<unsafe extern "C" fn(*mut c_void, i32) -> i32>,
    vsa_set_info: Option<unsafe extern "C" fn(i32, i32)>,
    dsp_is_active: Option<unsafe extern "C" fn() -> i32>,
    dsp_do_samples: Option<unsafe extern "C" fn(*mut i16, i32, i32, i32, i32) -> i32>,
    eq_set: Option<unsafe extern "C" fn(i32, *mut u8, i32)>,
    set_info: Option<unsafe extern "C" fn(i32, i32, i32, i32)>,
    out_mod: *mut OutModule,
}

static mut MODULE: InModule = InModule {
    version: IN_VER_UNICODE,
    description: c"SpotiTube: Spotify + YouTube".as_ptr().cast(),
    h_main_window: std::ptr::null_mut(),
    h_dll_instance: std::ptr::null_mut(),
    file_extensions: b"\0\0".as_ptr(), // double-NUL: no extensions, URIs matched by IsOurFile
    is_seekable: 1,
    uses_output_plug: IN_MODULE_FLAG_USES_OUTPUT_PLUGIN,
    config,
    about,
    init,
    quit,
    get_file_info,
    info_box,
    is_our_file,
    play,
    pause,
    un_pause,
    is_paused,
    stop,
    get_length,
    get_output_time,
    set_output_time,
    set_volume,
    set_pan,
    sa_vsa_init: None,
    sa_vsa_deinit: None,
    sa_add_pcm_data: None,
    sa_get_mode: None,
    sa_add: None,
    vsa_add_pcm_data: None,
    vsa_get_mode: None,
    vsa_add: None,
    vsa_set_info: None,
    dsp_is_active: None,
    dsp_do_samples: None,
    eq_set: None,
    set_info: None,
    out_mod: std::ptr::null_mut(),
};

#[repr(C)]
struct CopyDataStruct {
    dw_data: usize,
    cb_data: u32,
    lp_data: *mut c_void,
}

#[repr(C)]
struct FileInfoW {
    file: [u16; MAX_PATH],
    index: i32,
}

#[link(name = "user32")]
unsafe extern "system" {
    fn PostMessageW(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> i32;
    fn SendMessageW(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> isize;
    fn MessageBoxW(hwnd: Hwnd, text: *const u16, caption: *const u16, kind: u32) -> i32;
}

#[link(name = "shell32")]
unsafe extern "system" {
    fn ShellExecuteW(
        hwnd: Hwnd, op: *const u16, file: *const u16, params: *const u16, dir: *const u16, show: i32,
    ) -> *mut c_void;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(name: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
}

#[unsafe(no_mangle)]
pub extern "C" fn winampGetInModule2() -> *mut c_void {
    (&raw mut MODULE).cast()
}

fn module() -> &'static InModule {
    // SAFETY: Winamp fills the struct once before calling any entry point.
    unsafe { &*(&raw const MODULE) }
}

fn out() -> &'static OutModule {
    // SAFETY: Winamp sets out_mod before Play().
    unsafe { &*module().out_mod }
}

struct Engine {
    session: Session,
    player: Arc<Player>,
}

static RUNTIME: OnceLock<Runtime> = OnceLock::new();
static ENGINE: tokio::sync::Mutex<Option<Engine>> = tokio::sync::Mutex::const_new(None);
static PLAYER: Mutex<Option<Arc<Player>>> = Mutex::new(None);
/// Guards every outMod call made from the librespot thread against Close().
static OUT_OPEN: Mutex<bool> = Mutex::new(false);
static GENERATION: AtomicU64 = AtomicU64::new(0);
static PAUSED: AtomicBool = AtomicBool::new(false);
static LENGTH_MS: AtomicI32 = AtomicI32::new(-1);
static CURRENT: Mutex<String> = Mutex::new(String::new());
static TITLES: Mutex<Option<HashMap<String, (String, i32)>>> = Mutex::new(None);
/// Tag fields for Winamp's extended file info, keyed like `TITLES`.
static META: Mutex<Option<HashMap<String, Meta>>> = Mutex::new(None);

#[derive(Clone, Default)]
struct Meta {
    title: String,
    artist: String,
    album: String,
    year: String,
    track: String,
}


fn runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| Runtime::new().expect("tokio runtime"))
}

fn player() -> Option<Arc<Player>> {
    PLAYER.lock().unwrap().clone()
}

fn wide_to_string(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: Winamp passes NUL-terminated wide strings.
    unsafe {
        let len = (0..).take_while(|&i| *p.add(i) != 0).count();
        String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
    }
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn show_error(msg: &str) {
    let text = to_wide(msg);
    let caption = to_wide("Spotify/YouTube plugin");
    unsafe { MessageBoxW(module().h_main_window, text.as_ptr(), caption.as_ptr(), MB_ICONERROR) };
}

/// Accepts `spotify:<kind>:<id>` and `https://open.spotify.com/[intl-xx/]<kind>/<id>[?...]`
/// for kind = track, album, playlist or artist.
fn parse_link(s: &str) -> Option<SpotifyUri> {
    // Winamp saves a bare `spotify:` entry as a relative file path, so after a restart it comes
    // back as `<playlist dir>\spotify:...`; strip that prefix.
    let s = s.rfind("\\spotify:").map_or(s, |i| &s[i + 1..]);
    let (kind, id) = if let Some(rest) = s.strip_prefix("spotify:") {
        rest.split_once(':')?
    } else {
        let path = s
            .strip_prefix("https://open.spotify.com/")
            .or_else(|| s.strip_prefix("http://open.spotify.com/"))?;
        let path = path.split(['?', '#']).next()?;
        let mut parts = path.split('/').filter(|p| !p.is_empty() && !p.starts_with("intl-"));
        (parts.next()?, parts.next()?)
    };
    let id = SpotifyId::from_base62(id).ok()?;
    match kind {
        "track" => Some(SpotifyUri::Track { id }),
        "album" => Some(SpotifyUri::Album { id }),
        "playlist" => Some(SpotifyUri::Playlist { user: None, id }),
        "artist" => Some(SpotifyUri::Artist { id }),
        _ => None,
    }
}

static HOOKED_ORIGINALS: [AtomicUsize; STOCK_INPUT_PLUGINS.len()] =
    [const { AtomicUsize::new(0) }; STOCK_INPUT_PLUGINS.len()];
static HOOKED_UNICODE: [AtomicBool; STOCK_INPUT_PLUGINS.len()] =
    [const { AtomicBool::new(false) }; STOCK_INPUT_PLUGINS.len()];

fn is_supported(s: &str) -> bool {
    parse_link(s).is_some() || youtube::parse(s).is_some()
}

/// IsOurFile wrapper for stock plugin `I`: hides our links so Winamp falls through to us.
unsafe extern "C" fn hooked_is_our_file<const I: usize>(file: *const c_void) -> i32 {
    let name = if HOOKED_UNICODE[I].load(Ordering::Relaxed) {
        wide_to_string(file.cast())
    } else if file.is_null() {
        String::new()
    } else {
        unsafe { std::ffi::CStr::from_ptr(file.cast()) }.to_string_lossy().into_owned()
    };
    if is_supported(&name) {
        return 0;
    }
    let original: unsafe extern "C" fn(*const c_void) -> i32 =
        unsafe { std::mem::transmute(HOOKED_ORIGINALS[I].load(Ordering::Relaxed)) };
    unsafe { original(file) }
}

fn hook_stock_plugin<const I: usize>() {
    let name = to_wide(STOCK_INPUT_PLUGINS[I]);
    unsafe {
        let dll = GetModuleHandleW(name.as_ptr());
        if dll.is_null() {
            return; // not installed / not loaded
        }
        let get = GetProcAddress(dll, c"winampGetInModule2".as_ptr().cast());
        if get.is_null() {
            return;
        }
        let get: unsafe extern "C" fn() -> *mut InModule = std::mem::transmute(get);
        let m = get();
        if m.is_null() {
            return;
        }
        HOOKED_UNICODE[I].store((*m).version & IN_UNICODE_MASK == IN_UNICODE_MASK, Ordering::Relaxed);
        HOOKED_ORIGINALS[I].store((*m).is_our_file as usize, Ordering::Relaxed);
        (*m).is_our_file = std::mem::transmute(
            hooked_is_our_file::<I> as unsafe extern "C" fn(*const c_void) -> i32,
        );
    }
}

fn hook_stock_plugins() {
    hook_stock_plugin::<0>();
    hook_stock_plugin::<1>();
    hook_stock_plugin::<2>();
    hook_stock_plugin::<3>();
    hook_stock_plugin::<4>();
    hook_stock_plugin::<5>();
    hook_stock_plugin::<6>();
    hook_stock_plugin::<7>();
    hook_stock_plugin::<8>();
    hook_stock_plugin::<9>();
    hook_stock_plugin::<10>();
    hook_stock_plugin::<11>();
    hook_stock_plugin::<12>();
    hook_stock_plugin::<13>();
    hook_stock_plugin::<14>();
    hook_stock_plugin::<15>();
}

/// Replaces the album/playlist entry at the current playlist position with its tracks, then plays.
async fn expand_task(uri: SpotifyUri, generation: u64) {
    let tracks = match expand(&uri).await {
        Ok(t) if t.is_empty() => {
            show_error("No playable tracks in this album/playlist/artist.");
            return;
        }
        Ok(t) => t,
        Err(msg) => {
            show_error(&msg);
            return;
        }
    };
    if is_current(generation) {
        replace_current_entry(&tracks);
    }
}

/// Replaces the playlist entry at the current position with `entries`, then starts the first one.
fn replace_current_entry(entries: &[String]) {
    let main = module().h_main_window;
    unsafe {
        let pe = SendMessageW(main, WM_WA_IPC, IPC_GETWND_PE, IPC_GETWND) as Hwnd;
        let pos = SendMessageW(main, WM_WA_IPC, 0, IPC_GETLISTPOS);
        for (i, track) in entries.iter().enumerate() {
            let mut info = FileInfoW { file: [0; MAX_PATH], index: pos as i32 + 1 + i as i32 };
            for (dst, src) in info.file.iter_mut().zip(track.encode_utf16().take(MAX_PATH - 1)) {
                *dst = src;
            }
            let mut cds = CopyDataStruct {
                dw_data: IPC_PE_INSERTFILENAMEW,
                cb_data: size_of::<FileInfoW>() as u32,
                lp_data: (&raw mut info).cast(),
            };
            SendMessageW(pe, WM_COPYDATA, 0, (&raw mut cds) as isize);
        }
        SendMessageW(pe, WM_WA_IPC, IPC_PE_DELETEINDEX, pos);
        SendMessageW(main, WM_WA_IPC, pos as usize, IPC_SETPLAYLISTPOS);
        SendMessageW(main, WM_WA_IPC, 0, IPC_STARTPLAY);
    }
}

async fn expand(uri: &SpotifyUri) -> Result<Vec<String>, String> {
    let (session, _) = engine().await?;
    let items: Vec<SpotifyUri> = match uri {
        SpotifyUri::Album { .. } => Album::get(&session, uri)
            .await
            .map_err(|e| format!("Album lookup failed: {e}"))?
            .tracks()
            .cloned()
            .collect(),
        // An artist expands to its top tracks for the account's country.
        SpotifyUri::Artist { .. } => Artist::get(&session, uri)
            .await
            .map_err(|e| format!("Artist lookup failed: {e}"))?
            .top_tracks
            .for_country(&session.country())
            .0,
        _ => Playlist::get(&session, uri)
            .await
            .map_err(|e| format!("Playlist lookup failed: {e}"))?
            .tracks()
            .cloned()
            .collect(),
    };
    // Episodes and local files are skipped; only tracks are playable here. Entries are https
    // links because Winamp rewrites bare `spotify:` entries into file paths when saving.
    Ok(items
        .iter()
        .filter_map(|u| match u {
            SpotifyUri::Track { id } => id.to_base62().ok(),
            _ => None,
        })
        .map(|id| format!("https://open.spotify.com/track/{id}"))
        .collect())
}

/// Reads `key` from config.ini (`key=value` lines, `;` comments).
fn config_value(key: &str) -> Option<String> {
    let text = std::fs::read_to_string(cache_dir().join(CONFIG_FILE)).ok()?;
    parse_config(&text, key)
}

fn parse_config(text: &str, key: &str) -> Option<String> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with(';'))
        .find_map(|l| Some(l.split_once('=').filter(|(k, _)| k.trim() == key)?.1.trim().to_owned()))
        .filter(|v| !v.is_empty())
}

fn spotify_kbps() -> i32 {
    config_value("spotify_bitrate").and_then(|v| v.parse().ok()).filter(|k| [96, 160, 320].contains(k)).unwrap_or(DEFAULT_SPOTIFY_KBPS)
}

fn spotify_bitrate() -> Bitrate {
    match spotify_kbps() {
        96 => Bitrate::Bitrate96,
        160 => Bitrate::Bitrate160,
        _ => Bitrate::Bitrate320,
    }
}

pub(crate) fn youtube_format() -> String {
    config_value("youtube_format").unwrap_or_else(|| DEFAULT_YOUTUBE_FORMAT.to_owned())
}

/// Plugin "Config" button: opens config.ini in the default editor, creating it first if needed.
unsafe extern "C" fn config(parent: Hwnd) {
    let dir = cache_dir();
    let path = dir.join(CONFIG_FILE);
    if !path.exists()
        && let Err(e) = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, DEFAULT_CONFIG))
    {
        show_error(&format!("Could not create {}: {e}", path.display()));
        return;
    }
    let file = to_wide(&path.to_string_lossy());
    let open = to_wide("open");
    unsafe { ShellExecuteW(parent, open.as_ptr(), file.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL) };
}

fn cache_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = base.join("in_spotitube");
    // Pre-rename installs kept credentials in `in_spotify`; move them so users stay logged in.
    let legacy = base.join("in_spotify");
    if !dir.exists() && legacy.exists() {
        if let Err(e) = std::fs::rename(&legacy, &dir) {
            show_error(&format!("Could not move {} to {}: {e}\nYou may need to log in to Spotify again.", legacy.display(), dir.display()));
        }
    }
    dir
}

async fn connect() -> Result<Session, String> {
    let cache = Cache::new(Some(cache_dir()), None, None, None).map_err(|e| e.to_string())?;
    let cached = cache.credentials();
    let config = SessionConfig::default();

    if let Some(creds) = cached {
        let session = Session::new(config.clone(), Some(cache.clone()));
        if session.connect(creds, true).await.is_ok() {
            return Ok(session);
        }
        // Stale cached credentials: fall through to a fresh browser login.
    }

    let client = OAuthClientBuilder::new(&config.client_id, OAUTH_REDIRECT, OAUTH_SCOPES.to_vec())
        .open_in_browser()
        .build()
        .map_err(|e| format!("OAuth setup failed: {e}"))?;
    let token = client
        .get_access_token_async()
        .await
        .map_err(|e| format!("Spotify login failed: {e}"))?;
    let session = Session::new(config, Some(cache));
    session
        .connect(Credentials::with_access_token(token.access_token), true)
        .await
        .map_err(|e| format!("Spotify connect failed: {e}"))?;
    Ok(session)
}

async fn engine() -> Result<(Session, Arc<Player>), String> {
    let mut guard = ENGINE.lock().await;
    if let Some(e) = guard.as_ref().filter(|e| !e.session.is_invalid() && !e.player.is_invalid()) {
        return Ok((e.session.clone(), e.player.clone()));
    }
    let session = connect().await?;
    let player_config = PlayerConfig { bitrate: spotify_bitrate(), ..PlayerConfig::default() };
    let player = Player::new(player_config, session.clone(), Box::new(NoOpVolume), || {
        Box::new(WinampSink)
    });
    *PLAYER.lock().unwrap() = Some(player.clone());
    *guard = Some(Engine { session: session.clone(), player: player.clone() });
    Ok((session, player))
}

/// Looks up "Artist - Title" and duration, caches them, and makes Winamp re-read the entry.
async fn fetch_title(session: &Session, uri: &SpotifyUri, key: &str) -> Result<i32, String> {
    let track = Track::get(session, uri).await.map_err(|e| e.to_string())?;
    let artists: Vec<_> = track.artists.0.iter().map(|a| a.name.as_str()).collect();
    let title = format!("{} - {}", artists.join(", "), track.name);
    store_meta(key, Meta {
        title: track.name.clone(),
        artist: artists.join(", "),
        album: track.album.name.clone(),
        year: track.album.date.year().to_string(),
        track: track.number.to_string(),
    });
    store_title(key, title, track.duration);
    Ok(track.duration)
}

/// Caches an entry's title/length and makes Winamp re-read it.
fn store_title(key: &str, title: String, length_ms: i32) {
    TITLES.lock().unwrap().get_or_insert_default().insert(key.to_owned(), (title, length_ms));
    let main = module().h_main_window;
    let wide = to_wide(key);
    unsafe {
        // Synchronous so `wide` outlives Winamp's use of it.
        SendMessageW(main, WM_WA_IPC, wide.as_ptr() as usize, IPC_REFRESHPLCACHE);
        SendMessageW(main, WM_WA_IPC, 0, IPC_UPDTITLE);
    }
}

fn store_meta(key: &str, meta: Meta) {
    META.lock().unwrap().get_or_insert_default().insert(key.to_owned(), meta);
}

/// Placeholder marking a title lookup as pending so repeated GetFileInfo calls don't refetch.
fn mark_title_pending(key: &str) {
    TITLES.lock().unwrap().get_or_insert_default().insert(key.to_owned(), (key.to_owned(), -1));
}

fn forget_title(key: &str) {
    TITLES.lock().unwrap().get_or_insert_default().remove(key);
}

/// Fetches a playlist entry's title in the background if we're already logged in.
fn request_title(key: &str) {
    if let Some(youtube::Link::Video(id)) = youtube::parse(key) {
        youtube::request_title(key, id);
        return;
    }
    let Some(uri @ SpotifyUri::Track { .. }) = parse_link(key) else { return };
    let session = match ENGINE.try_lock() {
        Ok(guard) => match guard.as_ref() {
            Some(e) if !e.session.is_invalid() => e.session.clone(),
            _ => return, // not logged in yet: don't trigger OAuth from a title lookup
        },
        Err(_) => return,
    };
    mark_title_pending(key);
    let key = key.to_owned();
    runtime().spawn(async move {
        if fetch_title(&session, &uri, &key).await.is_err() {
            // Leave the link as the title; a later Play() retries and reports the error.
            forget_title(&key);
        }
    });
}

fn is_current(generation: u64) -> bool {
    GENERATION.load(Ordering::SeqCst) == generation
}

async fn play_task(uri: SpotifyUri, key: String, generation: u64) {
    let (session, player) = match engine().await {
        Ok(e) => e,
        Err(msg) => {
            show_error(&msg);
            return;
        }
    };
    if !is_current(generation) {
        return;
    }

    match fetch_title(&session, &uri, &key).await {
        Ok(duration) => LENGTH_MS.store(duration, Ordering::SeqCst),
        Err(e) => {
            show_error(&format!("Track lookup failed: {e}"));
            return;
        }
    }

    let mut events = player.get_player_event_channel();
    player.load(uri, true, 0);

    while let Some(event) = events.recv().await {
        if !is_current(generation) {
            return;
        }
        if matches!(event, PlayerEvent::EndOfTrack { .. } | PlayerEvent::Unavailable { .. }) {
            break;
        }
    }

    // Let the output plugin drain before telling Winamp to advance.
    while output_draining(generation) {
        tokio::time::sleep(POLL).await;
    }
    post_eof(generation);
}

/// True while track `generation` is current and the output plugin still has audio buffered.
fn output_draining(generation: u64) -> bool {
    let open = OUT_OPEN.lock().unwrap();
    is_current(generation) && *open && unsafe { (out().is_playing)() } != 0
}

/// Tells Winamp the track ended so it advances the playlist.
fn post_eof(generation: u64) {
    if is_current(generation) {
        unsafe { PostMessageW(module().h_main_window, WM_WA_MPEG_EOF, 0, 0) };
    }
}

/// Feeds librespot's decoded PCM into Winamp's DSP, vis and output plugin.
struct WinampSink;

impl Sink for WinampSink {
    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        // Raw packets only occur in passthrough mode, which this plugin never enables.
        let Ok(samples) = packet.samples() else { return Ok(()) };
        write_pcm(&converter.f64_to_s16(samples), None);
        Ok(())
    }
}

/// Blocks until `pcm` (interleaved 16-bit stereo) is handed to Winamp's DSP, vis and output.
/// Returns false if the output was closed, or `generation` stopped being current, meanwhile.
fn write_pcm(pcm: &[i16], generation: Option<u64>) -> bool {
    let frames = pcm.len() / CHANNELS as usize;

    // DSP plugins may return up to twice as many samples, and SA/VSAAddPCMData always read
    // VIS_MIN_FRAMES frames: librespot packets can be shorter, so zero-pad (Winamp otherwise
    // reads past the buffer and corrupts the heap).
    let mut buf = pcm.to_vec();
    buf.resize((pcm.len() * 2).max(VIS_MIN_FRAMES * CHANNELS as usize), 0);

    loop {
        {
            let open = OUT_OPEN.lock().unwrap();
            if !*open || generation.is_some_and(|g| !is_current(g)) {
                return false; // stopped or seeked: drop the audio
            }
            let m = module();
            let o = out();
            let mut n = frames;
            unsafe {
                if m.dsp_is_active.is_some_and(|f| f() != 0) {
                    if let Some(dsp) = m.dsp_do_samples {
                        n = dsp(buf.as_mut_ptr(), frames as i32, BITS, CHANNELS, SAMPLE_RATE) as usize;
                    }
                }
                let bytes = n * BYTES_PER_FRAME;
                if (o.can_write)() >= bytes as i32 {
                    let t = (o.get_written_time)();
                    if let Some(sa) = m.sa_add_pcm_data {
                        sa(buf.as_mut_ptr().cast(), CHANNELS, BITS, t);
                    }
                    if let Some(vsa) = m.vsa_add_pcm_data {
                        vsa(buf.as_mut_ptr().cast(), CHANNELS, BITS, t);
                    }
                    (o.write)(buf.as_mut_ptr().cast(), bytes as i32);
                    return true;
                }
            }
            // DSP may have altered buf in place; restore before retrying.
            buf[..pcm.len()].copy_from_slice(pcm);
        }
        std::thread::sleep(POLL);
    }
}

unsafe extern "C" fn about(parent: Hwnd) {
    let text = to_wide(
        "Spotify/YouTube input plugin\n\nAdd Spotify track, album, playlist or artist links (open.spotify.com/... or spotify:...) or YouTube video/playlist links to the playlist.\nSpotify requires Premium (librespot). YouTube requires yt-dlp and ffmpeg on PATH.",
    );
    let caption = to_wide("About");
    unsafe { MessageBoxW(parent, text.as_ptr(), caption.as_ptr(), 0) };
}

unsafe extern "C" fn init() {
    hook_stock_plugins();
}

unsafe extern "C" fn quit() {
    if let Some(p) = player() {
        p.stop();
    }
}

unsafe extern "C" fn get_file_info(file: *const u16, title: *mut u16, length_ms: *mut i32) {
    let mut key = wide_to_string(file);
    if key.is_empty() {
        key = CURRENT.lock().unwrap().clone();
    }
    let known = TITLES.lock().unwrap().get_or_insert_default().get(&key).cloned();
    if known.is_none() {
        request_title(&key);
    }
    let (name, len) = known.unwrap_or((key, -1));
    unsafe {
        if !title.is_null() {
            let wide: Vec<u16> = name.encode_utf16().take(GETFILEINFO_TITLE_LENGTH - 1).collect();
            std::ptr::copy_nonoverlapping(wide.as_ptr(), title, wide.len());
            *title.add(wide.len()) = 0;
        }
        if !length_ms.is_null() {
            *length_ms = len;
        }
    }
}

/// Web link for an entry; Spotify entries (possibly path-mangled) map to open.spotify.com.
fn web_url(key: &str) -> Option<String> {
    if youtube::parse(key).is_some() {
        return Some(key.to_owned());
    }
    let uri = parse_link(key)?;
    Some(format!("https://open.spotify.com/{}/{}", uri.item_type(), uri.to_id().ok()?))
}

fn format_length(ms: i32) -> String {
    if ms < 0 {
        return "unknown".into();
    }
    let secs = ms / 1000;
    format!("{}:{:02}", secs / 60, secs % 60)
}

unsafe extern "C" fn info_box(file: *const u16, parent: Hwnd) -> i32 {
    let key = wide_to_string(file);
    let Some(url) = web_url(&key) else { return INFOBOX_UNCHANGED };
    let (title, len) = TITLES.lock().unwrap().get_or_insert_default().get(&key).cloned().unwrap_or((key, -1));
    let text = to_wide(&format!("{title}

Length: {}
Link: {url}

Open in browser?", format_length(len)));
    let caption = to_wide("Track info");
    let url = to_wide(&url);
    let open = to_wide("open");
    unsafe {
        if MessageBoxW(parent, text.as_ptr(), caption.as_ptr(), MB_YESNO | MB_ICONINFORMATION) == IDYES {
            ShellExecuteW(parent, open.as_ptr(), url.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL);
        }
    }
    INFOBOX_UNCHANGED
}

/// Winamp's tag lookup (`artist`, `album`, ...). Returns 0 for unknown keys or values so
/// Winamp falls back to GetFileInfo's "Artist - Title".
#[unsafe(no_mangle)]
pub unsafe extern "C" fn winampGetExtendedFileInfoW(file: *const u16, data: *const u8, dest: *mut u16, dest_len: i32) -> i32 {
    if data.is_null() || dest.is_null() || dest_len <= 0 {
        return 0;
    }
    // SAFETY: Winamp passes a NUL-terminated ANSI key.
    let field = unsafe { std::ffi::CStr::from_ptr(data.cast()) }.to_string_lossy().to_ascii_lowercase();
    let key = wide_to_string(file);
    let value = match field.as_str() {
        "type" => Some("0".to_owned()), // audio
        "length" => TITLES.lock().unwrap().get_or_insert_default().get(&key).map(|t| t.1).filter(|&l| l >= 0).map(|l| l.to_string()),
        _ => META.lock().unwrap().get_or_insert_default().get(&key).and_then(|m| meta_field(m, &field)),
    };
    let Some(value) = value.filter(|v| !v.is_empty()) else { return 0 };
    let wide: Vec<u16> = value.encode_utf16().take(dest_len as usize - 1).collect();
    unsafe {
        std::ptr::copy_nonoverlapping(wide.as_ptr(), dest, wide.len());
        *dest.add(wide.len()) = 0;
    }
    1
}

fn meta_field(m: &Meta, field: &str) -> Option<String> {
    let v = match field {
        "title" => &m.title,
        "artist" | "albumartist" => &m.artist,
        "album" => &m.album,
        "year" => &m.year,
        "track" => &m.track,
        _ => return None,
    };
    Some(v.clone())
}

unsafe extern "C" fn is_our_file(file: *const u16) -> i32 {
    is_supported(&wide_to_string(file)) as i32
}

unsafe extern "C" fn play(file: *const u16) -> i32 {
    let key = wide_to_string(file);

    if let Some(link) = youtube::parse(&key) {
        *youtube::STREAM.lock().unwrap() = None;
        return match link {
            youtube::Link::Playlist(id) => {
                // Output stays closed; the expansion's IPC_STARTPLAY plays the first video.
                let generation = next_generation();
                std::thread::spawn(move || youtube::expand(id, generation));
                0
            }
            youtube::Link::Video(id) => {
                let Some(generation) = open_output(&key, YOUTUBE_DISPLAY_KBPS) else { return 1 };
                std::thread::spawn(move || youtube::play(id, key, generation));
                0
            }
        };
    }

    let Some(uri) = parse_link(&key) else { return -1 };
    *youtube::STREAM.lock().unwrap() = None;

    if !matches!(uri, SpotifyUri::Track { .. }) {
        // Output stays closed; IPC_STARTPLAY from expand_task stops us and plays the first track.
        let generation = next_generation();
        runtime().spawn(expand_task(uri, generation));
        return 0;
    }

    let Some(generation) = open_output(&key, spotify_kbps()) else { return 1 };
    // Login and loading run off the UI thread; first use opens the browser for OAuth.
    runtime().spawn(play_task(uri, key, generation));
    0
}

fn next_generation() -> u64 {
    GENERATION.fetch_add(1, Ordering::SeqCst) + 1
}

/// Opens Winamp's output for 44.1 kHz 16-bit stereo and starts a new track generation.
fn open_output(key: &str, kbps: i32) -> Option<u64> {
    let o = out();
    let max_latency = unsafe { (o.open)(SAMPLE_RATE, CHANNELS, BITS, -1, -1) };
    if max_latency < 0 {
        return None;
    }
    let m = module();
    unsafe {
        if let Some(f) = m.sa_vsa_init {
            f(max_latency, SAMPLE_RATE);
        }
        if let Some(f) = m.vsa_set_info {
            f(SAMPLE_RATE, CHANNELS);
        }
        if let Some(f) = m.set_info {
            f(kbps, SAMPLE_RATE / 1000, (CHANNELS == 2) as i32, 1);
        }
        (o.set_volume)(VOLUME_KEEP);
    }

    *OUT_OPEN.lock().unwrap() = true;
    PAUSED.store(false, Ordering::SeqCst);
    let known_len = TITLES.lock().unwrap().get_or_insert_default().get(key).map(|t| t.1);
    LENGTH_MS.store(known_len.unwrap_or(-1), Ordering::SeqCst);
    *CURRENT.lock().unwrap() = key.to_owned();
    Some(next_generation())
}

unsafe extern "C" fn pause() {
    PAUSED.store(true, Ordering::SeqCst);
    if let Some(p) = player() {
        p.pause();
    }
    unsafe { (out().pause)(1) };
}

unsafe extern "C" fn un_pause() {
    PAUSED.store(false, Ordering::SeqCst);
    unsafe { (out().pause)(0) };
    if let Some(p) = player() {
        p.play();
    }
}

unsafe extern "C" fn is_paused() -> i32 {
    PAUSED.load(Ordering::SeqCst) as i32
}

unsafe extern "C" fn stop() {
    next_generation();
    if let Some(p) = player() {
        p.stop();
    }
    *youtube::STREAM.lock().unwrap() = None;
    let mut open = OUT_OPEN.lock().unwrap();
    if *open {
        *open = false;
        unsafe { (out().close)() };
    }
    drop(open);
    if let Some(f) = module().sa_vsa_deinit {
        unsafe { f() };
    }
}

unsafe extern "C" fn get_length() -> i32 {
    LENGTH_MS.load(Ordering::SeqCst)
}

unsafe extern "C" fn get_output_time() -> i32 {
    if !*OUT_OPEN.lock().unwrap() {
        return 0; // album/playlist expansion in progress
    }
    unsafe { (out().get_output_time)() }
}

unsafe extern "C" fn set_output_time(ms: i32) {
    let stream = youtube::STREAM.lock().unwrap().clone();
    if let Some(source) = stream {
        // Restart ffmpeg at the new position; the old reader sees a stale generation and exits.
        let generation = next_generation();
        let open = OUT_OPEN.lock().unwrap();
        if *open {
            unsafe { (out().flush)(ms) };
        }
        drop(open);
        std::thread::spawn(move || youtube::stream(source, ms.max(0) as u32, generation));
        return;
    }
    if let Some(p) = player() {
        p.seek(ms.max(0) as u32);
    }
    // ponytail: a packet decoded just before the seek may still slip in after Flush (tiny glitch);
    // track seek request ids from PlayerEvent if that becomes audible.
    let open = OUT_OPEN.lock().unwrap();
    if *open {
        unsafe { (out().flush)(ms) };
    }
}

unsafe extern "C" fn set_volume(volume: i32) {
    unsafe { (out().set_volume)(volume) };
}

unsafe extern "C" fn set_pan(pan: i32) {
    unsafe { (out().set_pan)(pan) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_links() {
        let id = "4uLU6hMCjMI75M1A2tKUQC";
        for s in [
            format!("spotify:track:{id}"),
            format!("https://open.spotify.com/track/{id}"),
            format!("https://open.spotify.com/intl-tr/track/{id}?si=abc"),
            format!("C:\\Users\\Just\\AppData\\Roaming\\Winamp\\spotify:track:{id}"),
        ] {
            assert!(matches!(parse_link(&s), Some(SpotifyUri::Track { .. })), "{s}");
        }
        let pl = "https://open.spotify.com/playlist/0sKBl3lDo12p1HerjjAZap?si=460c80628d734f82";
        assert!(matches!(parse_link(pl), Some(SpotifyUri::Playlist { .. })));
        assert!(matches!(parse_link(&format!("spotify:album:{id}")), Some(SpotifyUri::Album { .. })));
        let artist = "https://open.spotify.com/artist/0OdUWJ0sBjDrqHygGUXeCF";
        assert!(matches!(parse_link(artist), Some(SpotifyUri::Artist { .. })));
        assert_eq!(web_url(artist).as_deref(), Some(artist));
        for s in [
            "C:\\music\\a.mp3",
            "spotify:track:bad!",
            "http://radio.example.com/stream",
        ] {
            assert!(parse_link(s).is_none(), "{s}");
        }
    }

    #[test]
    fn builds_web_urls() {
        let id = "4uLU6hMCjMI75M1A2tKUQC";
        let want = format!("https://open.spotify.com/track/{id}");
        assert_eq!(web_url(&format!(r"C:\x\spotify:track:{id}")), Some(want.clone()));
        assert_eq!(web_url(&want), Some(want));
        let yt = "https://www.youtube.com/watch?v=jNQXAC9IVRw";
        assert_eq!(web_url(yt).as_deref(), Some(yt));
        assert_eq!(web_url(r"C:\music\a.mp3"), None);
        assert_eq!(format_length(61_500), "1:01");
        assert_eq!(format_length(-1), "unknown");
    }

    #[test]
    fn parses_config() {
        let text = "; spotify_bitrate=96
spotify_bitrate = 160
youtube_format=bestaudio[abr<=128]
empty=
";
        assert_eq!(parse_config(text, "spotify_bitrate").as_deref(), Some("160"));
        assert_eq!(parse_config(text, "youtube_format").as_deref(), Some("bestaudio[abr<=128]"));
        assert_eq!(parse_config(text, "empty"), None);
        assert_eq!(parse_config(text, "missing"), None);
        assert_eq!(parse_config(DEFAULT_CONFIG, "spotify_bitrate").as_deref(), Some("320"));
    }
}

#[cfg(test)]
mod live {
    use super::*;

    /// Needs cached Spotify credentials: `cargo test --release -- --ignored`.
    #[test]
    #[ignore]
    fn expands_artist_live() {
        let uri = parse_link("https://open.spotify.com/artist/0OdUWJ0sBjDrqHygGUXeCF").unwrap();
        let tracks = runtime().block_on(expand(&uri)).unwrap();
        println!("{} tracks: {tracks:?}", tracks.len());
        assert!(!tracks.is_empty());
    }

    #[test]
    #[ignore]
    fn fetches_track_meta_live() {
        let key = "https://open.spotify.com/track/4uLU6hMCjMI75M1A2tKUQC";
        let uri = parse_link(key).unwrap();
        runtime().block_on(async {
            let (session, _) = engine().await.unwrap();
            let track = Track::get(&session, &uri).await.unwrap();
            let covers: Vec<_> = track.album.covers.iter().chain(track.album.cover_group.iter()).map(|c| (c.width, c.id.to_base16().unwrap())).collect();
            println!("{} | {} | {} | #{} | covers {covers:?}", track.name, track.album.name, track.album.date.year(), track.number);
            assert!(!track.album.name.is_empty());
        });
    }
}
