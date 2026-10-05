//! Winamp input plugin that plays Spotify tracks through librespot.
//! Playlist entries: `spotify:{track,album,playlist}:<id>` or the matching open.spotify.com links.
//! Album/playlist entries are expanded into their tracks when played.
//! Struct layouts and IPC ids follow the Winamp SDK headers in2.h / out.h / wa_ipc.h / ipc_pe.h.

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
use librespot::metadata::{Album, Metadata, Playlist, Track};
use librespot::oauth::OAuthClientBuilder;
use librespot::playback::{
    audio_backend::{Sink, SinkResult},
    config::PlayerConfig,
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
const BITRATE_KBPS: i32 = 320;
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
    description: c"Spotify (librespot)".as_ptr().cast(),
    h_main_window: std::ptr::null_mut(),
    h_dll_instance: std::ptr::null_mut(),
    file_extensions: b"\0\0".as_ptr(), // double-NUL: no extensions, URIs matched by IsOurFile
    is_seekable: 1,
    uses_output_plug: IN_MODULE_FLAG_USES_OUTPUT_PLUGIN,
    config: about,
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
    let caption = to_wide("Spotify plugin");
    unsafe { MessageBoxW(module().h_main_window, text.as_ptr(), caption.as_ptr(), MB_ICONERROR) };
}

/// Accepts `spotify:<kind>:<id>` and `https://open.spotify.com/[intl-xx/]<kind>/<id>[?...]`
/// for kind = track, album or playlist.
fn parse_link(s: &str) -> Option<SpotifyUri> {
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
        _ => None,
    }
}

static HOOKED_ORIGINALS: [AtomicUsize; STOCK_INPUT_PLUGINS.len()] =
    [const { AtomicUsize::new(0) }; STOCK_INPUT_PLUGINS.len()];
static HOOKED_UNICODE: [AtomicBool; STOCK_INPUT_PLUGINS.len()] =
    [const { AtomicBool::new(false) }; STOCK_INPUT_PLUGINS.len()];

/// IsOurFile wrapper for stock plugin `I`: hides Spotify links so Winamp falls through to us.
unsafe extern "C" fn hooked_is_our_file<const I: usize>(file: *const c_void) -> i32 {
    let name = if HOOKED_UNICODE[I].load(Ordering::Relaxed) {
        wide_to_string(file.cast())
    } else if file.is_null() {
        String::new()
    } else {
        unsafe { std::ffi::CStr::from_ptr(file.cast()) }.to_string_lossy().into_owned()
    };
    if parse_link(&name).is_some() {
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
            show_error("No playable tracks in this album/playlist.");
            return;
        }
        Ok(t) => t,
        Err(msg) => {
            show_error(&msg);
            return;
        }
    };
    if !is_current(generation) {
        return;
    }
    let main = module().h_main_window;
    unsafe {
        let pe = SendMessageW(main, WM_WA_IPC, IPC_GETWND_PE, IPC_GETWND) as Hwnd;
        let pos = SendMessageW(main, WM_WA_IPC, 0, IPC_GETLISTPOS);
        for (i, track) in tracks.iter().enumerate() {
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
        _ => Playlist::get(&session, uri)
            .await
            .map_err(|e| format!("Playlist lookup failed: {e}"))?
            .tracks()
            .cloned()
            .collect(),
    };
    // Episodes and local files are skipped; only tracks are playable here.
    Ok(items
        .iter()
        .filter(|u| matches!(u, SpotifyUri::Track { .. }))
        .filter_map(|u| u.to_uri().ok())
        .collect())
}

fn cache_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("in_spotify")
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
    let player = Player::new(PlayerConfig::default(), session.clone(), Box::new(NoOpVolume), || {
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
    TITLES.lock().unwrap().get_or_insert_default().insert(key.to_owned(), (title, track.duration));

    let main = module().h_main_window;
    let wide = to_wide(key);
    unsafe {
        // Synchronous so `wide` outlives Winamp's use of it.
        SendMessageW(main, WM_WA_IPC, wide.as_ptr() as usize, IPC_REFRESHPLCACHE);
        SendMessageW(main, WM_WA_IPC, 0, IPC_UPDTITLE);
    }
    Ok(track.duration)
}

/// Fetches a playlist entry's title in the background if we're already logged in.
fn request_title(key: &str) {
    let Some(uri @ SpotifyUri::Track { .. }) = parse_link(key) else { return };
    let session = match ENGINE.try_lock() {
        Ok(guard) => match guard.as_ref() {
            Some(e) if !e.session.is_invalid() => e.session.clone(),
            _ => return, // not logged in yet: don't trigger OAuth from a title lookup
        },
        Err(_) => return,
    };
    // Placeholder marks the lookup as pending so repeated GetFileInfo calls don't refetch.
    TITLES.lock().unwrap().get_or_insert_default().insert(key.to_owned(), (key.to_owned(), -1));
    let key = key.to_owned();
    runtime().spawn(async move {
        if fetch_title(&session, &uri, &key).await.is_err() {
            // Leave the link as the title; a later Play() retries and reports the error.
            TITLES.lock().unwrap().get_or_insert_default().remove(&key);
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
    loop {
        if !is_current(generation) {
            return;
        }
        let playing = {
            let open = OUT_OPEN.lock().unwrap();
            *open && unsafe { (out().is_playing)() } != 0
        };
        if !playing {
            break;
        }
        tokio::time::sleep(POLL).await;
    }
    unsafe { PostMessageW(module().h_main_window, WM_WA_MPEG_EOF, 0, 0) };
}

/// Feeds librespot's decoded PCM into Winamp's DSP, vis and output plugin.
struct WinampSink;

impl Sink for WinampSink {
    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        // Raw packets only occur in passthrough mode, which this plugin never enables.
        let Ok(samples) = packet.samples() else { return Ok(()) };
        let pcm = converter.f64_to_s16(samples);
        let frames = pcm.len() / CHANNELS as usize;

        // DSP plugins may return up to twice as many samples.
        let mut buf = pcm.clone();
        buf.resize(pcm.len() * 2, 0);

        loop {
            {
                let open = OUT_OPEN.lock().unwrap();
                if !*open {
                    return Ok(()); // stopped: drop the audio
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
                        return Ok(());
                    }
                }
                // DSP may have altered buf in place; restore before retrying.
                buf[..pcm.len()].copy_from_slice(&pcm);
            }
            std::thread::sleep(POLL);
        }
    }
}

unsafe extern "C" fn about(parent: Hwnd) {
    let text = to_wide(
        "Spotify input plugin (librespot)\n\nAdd Spotify track, album or playlist links (open.spotify.com/... or spotify:...) to the playlist.\nRequires Spotify Premium.",
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

unsafe extern "C" fn info_box(_file: *const u16, _parent: Hwnd) -> i32 {
    INFOBOX_UNCHANGED
}

unsafe extern "C" fn is_our_file(file: *const u16) -> i32 {
    parse_link(&wide_to_string(file)).is_some() as i32
}

unsafe extern "C" fn play(file: *const u16) -> i32 {
    let key = wide_to_string(file);
    let Some(uri) = parse_link(&key) else { return -1 };

    if !matches!(uri, SpotifyUri::Track { .. }) {
        // Output stays closed; IPC_STARTPLAY from expand_task stops us and plays the first track.
        let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
        runtime().spawn(expand_task(uri, generation));
        return 0;
    }

    let o = out();
    let max_latency = unsafe { (o.open)(SAMPLE_RATE, CHANNELS, BITS, -1, -1) };
    if max_latency < 0 {
        return 1;
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
            f(BITRATE_KBPS, SAMPLE_RATE / 1000, (CHANNELS == 2) as i32, 1);
        }
        (o.set_volume)(VOLUME_KEEP);
    }

    *OUT_OPEN.lock().unwrap() = true;
    PAUSED.store(false, Ordering::SeqCst);
    let known_len = TITLES.lock().unwrap().get_or_insert_default().get(&key).map(|t| t.1);
    LENGTH_MS.store(known_len.unwrap_or(-1), Ordering::SeqCst);
    *CURRENT.lock().unwrap() = key.clone();
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;

    // Login and loading run off the UI thread; first use opens the browser for OAuth.
    runtime().spawn(play_task(uri, key, generation));
    0
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
    GENERATION.fetch_add(1, Ordering::SeqCst);
    if let Some(p) = player() {
        p.stop();
    }
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
        ] {
            assert!(matches!(parse_link(&s), Some(SpotifyUri::Track { .. })), "{s}");
        }
        let pl = "https://open.spotify.com/playlist/0sKBl3lDo12p1HerjjAZap?si=460c80628d734f82";
        assert!(matches!(parse_link(pl), Some(SpotifyUri::Playlist { .. })));
        assert!(matches!(parse_link(&format!("spotify:album:{id}")), Some(SpotifyUri::Album { .. })));
        for s in [
            "C:\\music\\a.mp3",
            "spotify:track:bad!",
            "https://open.spotify.com/artist/4uLU6hMCjMI75M1A2tKUQC",
            "http://radio.example.com/stream",
        ] {
            assert!(parse_link(s).is_none(), "{s}");
        }
    }
}
