//! YouTube audio via external tools: yt-dlp resolves metadata and the audio stream URL,
//! ffmpeg decodes it to raw 44.1 kHz 16-bit stereo PCM that goes through `write_pcm`.

use std::io::{ErrorKind, Read};
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};

use crate::{
    LENGTH_MS, SAMPLE_RATE, forget_title, is_current, mark_title_pending, output_draining, post_eof,
    replace_current_entry, show_error, store_title, write_pcm,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// 1152 stereo frames per read, enough for Winamp's vis (needs >= 576 samples).
const CHUNK_BYTES: usize = 1152 * 4;
const MS_PER_SEC: f64 = 1000.0;
/// Auto-generated YouTube mixes/radios; yt-dlp can't flat-list them.
const MIX_PREFIX: &str = "RD";

pub enum Link {
    Video(String),
    Playlist(String),
}

/// The YouTube track currently playing; used to restart ffmpeg on seek.
pub static STREAM: Mutex<Option<Source>> = Mutex::new(None);

#[derive(Clone)]
pub struct Source {
    /// watch?v= URL, to re-resolve `audio` when YouTube rejects it.
    page: String,
    /// Direct googlevideo audio URL from yt-dlp.
    audio: String,
}

/// yt-dlp self-update (`-U`) is attempted at most once per Winamp session.
static UPDATE_TRIED: AtomicBool = AtomicBool::new(false);

/// Serializes background title lookups so a big playlist doesn't spawn dozens of yt-dlp processes.
static TITLE_LOOKUP: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

/// Accepts youtube.com/watch?v=, youtu.be/, /shorts/, music.youtube.com and /playlist?list= links.
/// A watch link that also carries `list=` expands the playlist, unless it's an auto mix.
pub fn parse(s: &str) -> Option<Link> {
    let rest = s.strip_prefix("https://").or_else(|| s.strip_prefix("http://"))?;
    let (host, path_query) = rest.split_once('/').unwrap_or((rest, ""));
    let (path, query) = path_query.split_once('?').unwrap_or((path_query, ""));
    let query = query.split('#').next().unwrap_or("");
    let param = |name: &str| {
        query.split('&').find_map(|kv| kv.strip_prefix(name)?.strip_prefix('=')).map(str::to_owned)
    };

    let video = match host {
        "youtu.be" | "www.youtu.be" => Some(path.split('/').next()?.to_owned()),
        "youtube.com" | "www.youtube.com" | "m.youtube.com" | "music.youtube.com" => {
            if path == "watch" {
                // A real playlist on a watch link wins; auto-generated mixes ("RD...") can't be
                // listed by yt-dlp, so those play just the video.
                if let Some(list) = param("list").filter(|id| is_id(id) && !id.starts_with(MIX_PREFIX)) {
                    return Some(Link::Playlist(list));
                }
                param("v")
            } else if let Some(id) = path.strip_prefix("shorts/") {
                Some(id.split('/').next()?.to_owned())
            } else if path == "playlist" {
                return param("list").filter(|id| is_id(id)).map(Link::Playlist);
            } else {
                None
            }
        }
        _ => None,
    }?;
    (video.len() == 11 && is_id(&video)).then_some(Link::Video(video))
}

fn is_id(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Canonical playlist entry for a video id; also the title-cache key.
fn video_url(id: &str) -> String {
    format!("https://www.youtube.com/watch?v={id}")
}

fn tool(exe: &str) -> Command {
    let mut cmd = Command::new(exe);
    cmd.creation_flags(CREATE_NO_WINDOW).stdin(Stdio::null());
    cmd
}

/// yt-dlp with UTF-8 output. PYTHONIOENCODING/PYTHONUTF8 don't work here: non-ASCII
/// characters get silently dropped; only `--encoding utf-8` keeps them.
fn yt_dlp_command() -> Command {
    let mut cmd = tool("yt-dlp");
    cmd.args(["--encoding", "utf-8", "--no-warnings"]);
    cmd
}

/// Runs yt-dlp on a single video and returns its stdout lines, or a user-facing error.
fn yt_dlp(args: &[&str]) -> Result<Vec<String>, String> {
    yt_dlp_raw(&[&["--no-playlist"], args].concat())
}

/// Runs yt-dlp; on failure, self-updates once per session and retries, since YouTube
/// changes often break older yt-dlp versions.
fn yt_dlp_raw(args: &[&str]) -> Result<Vec<String>, String> {
    let first = run_yt_dlp(args);
    match &first {
        Err((_, true)) if self_update() => run_yt_dlp(args).map_err(|(msg, _)| msg),
        _ => first.map_err(|(msg, _)| msg),
    }
}

/// Error carries whether yt-dlp ran and failed (worth retrying after an update).
fn run_yt_dlp(args: &[&str]) -> Result<Vec<String>, (String, bool)> {
    let output = match yt_dlp_command().args(args).output() {
        Ok(o) => o,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            return Err(("yt-dlp not found on PATH.
Install: winget install yt-dlp.yt-dlp
Then restart Winamp.".into(), false));
        }
        Err(e) => return Err((format!("yt-dlp failed to start: {e}"), false)),
    };
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err((format!("yt-dlp: {}", err.trim()), true));
    }
    Ok(String::from_utf8_lossy(&output.stdout).lines().map(str::to_owned).collect())
}

/// Runs `yt-dlp -U` the first time it's called; true if a newer version was installed.
fn self_update() -> bool {
    if UPDATE_TRIED.swap(true, Ordering::SeqCst) {
        return false;
    }
    match tool("yt-dlp").arg("-U").output() {
        // "yt-dlp is up to date" means retrying won't help.
        Ok(o) if o.status.success() => !String::from_utf8_lossy(&o.stdout).contains("up to date"),
        _ => false,
    }
}

/// yt-dlp prints duration in seconds (may be fractional) or "NA".
fn duration_ms(s: &str) -> i32 {
    s.trim().parse::<f64>().map_or(-1, |secs| (secs * MS_PER_SEC) as i32)
}

pub fn play(id: String, key: String, generation: u64) {
    let lines = match yt_dlp(&["-f", "bestaudio", "--print", "title", "--print", "duration", "--print", "urls", &video_url(&id)]) {
        Ok(l) => l,
        Err(msg) => {
            if is_current(generation) {
                show_error(&msg);
            }
            return;
        }
    };
    let [title, duration, url, ..] = lines.as_slice() else {
        show_error(&format!("yt-dlp: unexpected output for {id}"));
        return;
    };
    if !is_current(generation) {
        return;
    }
    let length = duration_ms(duration);
    LENGTH_MS.store(length, Ordering::SeqCst);
    store_title(&key, title.clone(), length);
    let source = Source { page: video_url(&id), audio: url.clone() };
    *STREAM.lock().unwrap() = Some(source.clone());
    stream(source, 0, generation);
}

enum Outcome {
    /// Stop or seek took over.
    Stopped,
    Finished,
    Failed { started: bool, stderr: String },
}

/// Decodes `source` from `start_ms` with ffmpeg and feeds Winamp until EOF, stop or seek.
pub fn stream(mut source: Source, start_ms: u32, generation: u64) {
    let mut outcome = run_ffmpeg(&source.audio, start_ms, generation);

    // googlevideo intermittently answers 403 when ffmpeg opens a fresh yt-dlp URL;
    // a newly resolved URL usually works, so retry once before any audio went out.
    if let Outcome::Failed { started: false, stderr } = &outcome {
        if stderr.contains("403") && is_current(generation) {
            if let Ok(lines) = yt_dlp(&["-f", "bestaudio", "--print", "urls", &source.page]) {
                if let Some(url) = lines.into_iter().next() {
                    source.audio = url;
                    if is_current(generation) {
                        *STREAM.lock().unwrap() = Some(source.clone());
                    }
                    outcome = run_ffmpeg(&source.audio, start_ms, generation);
                }
            }
        }
    }

    if !is_current(generation) {
        return;
    }
    match outcome {
        Outcome::Stopped => return,
        Outcome::Failed { stderr, .. } => show_error(&format!("ffmpeg: {stderr}")),
        Outcome::Finished => {}
    }
    while output_draining(generation) {
        std::thread::sleep(crate::POLL);
    }
    post_eof(generation);
}

fn run_ffmpeg(url: &str, start_ms: u32, generation: u64) -> Outcome {
    let start = format!("{:.3}", start_ms as f64 / MS_PER_SEC);
    let rate = SAMPLE_RATE.to_string();
    let child = tool("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-nostdin"])
        // Reconnect if the HTTP stream drops, e.g. after a long pause.
        .args(["-reconnect", "1", "-reconnect_streamed", "1", "-reconnect_delay_max", "5"])
        .args(["-ss", &start, "-i", url, "-vn", "-f", "s16le", "-ar", &rate, "-ac", "2", "pipe:1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            let msg = if e.kind() == ErrorKind::NotFound {
                "ffmpeg not found on PATH.\nInstall: winget install yt-dlp.FFmpeg\nThen restart Winamp.".to_owned()
            } else {
                format!("ffmpeg failed to start: {e}")
            };
            show_error(&msg);
            return Outcome::Stopped;
        }
    };

    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut bytes = vec![0u8; CHUNK_BYTES];
    let mut started = false;
    loop {
        let n = match read_full(&mut stdout, &mut bytes) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        started = true;
        let pcm: Vec<i16> = bytes[..n].chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
        if !write_pcm(&pcm, Some(generation)) {
            // Stop or seek: ffmpeg may still be streaming; don't leave it running.
            let _ = child.kill();
            let _ = child.wait();
            return Outcome::Stopped;
        }
    }

    drop(stdout);
    match child.wait_with_output() {
        Ok(o) if o.status.success() => Outcome::Finished,
        Ok(o) => Outcome::Failed { started, stderr: String::from_utf8_lossy(&o.stderr).trim().to_owned() },
        Err(e) => Outcome::Failed { started, stderr: e.to_string() },
    }
}

/// Fills `buf` unless the stream ends first; returns the byte count read.
fn read_full(r: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// Replaces the playlist entry with its videos (titles cached from the flat listing).
pub fn expand(list_id: String, generation: u64) {
    let url = format!("https://www.youtube.com/playlist?list={list_id}");
    // Title last: it may itself contain '|'.
    let lines = match yt_dlp_raw(&["--flat-playlist", "--print", "%(id)s|%(duration)s|%(title)s", &url]) {
        Ok(l) => l,
        Err(msg) => {
            show_error(&msg);
            return;
        }
    };

    let mut entries = Vec::new();
    for line in &lines {
        let mut parts = line.splitn(3, '|');
        let (Some(id), Some(duration), Some(title)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        if id.len() != 11 || !is_id(id) {
            continue;
        }
        let key = video_url(id);
        // Cache before inserting so the entries show titles immediately.
        crate::TITLES.lock().unwrap().get_or_insert_default().insert(key.clone(), (title.to_owned(), duration_ms(duration)));
        entries.push(key);
    }
    if entries.is_empty() {
        show_error("No playable videos in this YouTube playlist.");
        return;
    }
    if is_current(generation) {
        replace_current_entry(&entries);
    }
}

/// Looks up a video's title in the background (one yt-dlp at a time).
pub fn request_title(key: &str, id: String) {
    mark_title_pending(key);
    let key = key.to_owned();
    std::thread::spawn(move || {
        let _one_at_a_time = TITLE_LOOKUP.lock().unwrap();
        match yt_dlp(&["--skip-download", "--print", "title", "--print", "duration", &video_url(&id)]) {
            Ok(lines) if lines.len() >= 2 => store_title(&key, lines[0].clone(), duration_ms(&lines[1])),
            // Leave the link as the title; Play() reports the actual error.
            _ => forget_title(&key),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video(s: &str) -> Option<String> {
        match parse(s)? {
            Link::Video(id) => Some(id),
            Link::Playlist(_) => None,
        }
    }

    #[test]
    fn parses_links() {
        let id = "jNQXAC9IVRw";
        for s in [
            format!("https://www.youtube.com/watch?v={id}"),
            format!("https://youtube.com/watch?feature=share&v={id}&t=5"),
            format!("https://youtu.be/{id}?si=abc"),
            format!("https://www.youtube.com/shorts/{id}"),
            format!("https://music.youtube.com/watch?v={id}&list=RDAMVM{id}"),
            format!("https://m.youtube.com/watch?v={id}#t=1"),
        ] {
            assert_eq!(video(&s).as_deref(), Some(id), "{s}");
        }
        let pl = "https://www.youtube.com/playlist?list=PLbpi6ZahtOH6Blw3RGYpWkSByi_T7Rygb";
        assert!(matches!(parse(pl), Some(Link::Playlist(l)) if l == "PLbpi6ZahtOH6Blw3RGYpWkSByi_T7Rygb"));
        let watch_pl = "https://www.youtube.com/watch?v=e4McVxHYg24&list=PLSLunwKqc-itQqwh25asHNbSWFPYSWp8y";
        assert!(matches!(parse(watch_pl), Some(Link::Playlist(l)) if l == "PLSLunwKqc-itQqwh25asHNbSWFPYSWp8y"));
        let mix = "https://www.youtube.com/watch?v=S020dzrrYW0&list=RDS020dzrrYW0&start_radio=1";
        assert_eq!(video(mix).as_deref(), Some("S020dzrrYW0"));
        for s in [
            "https://www.youtube.com/watch?v=short",
            "https://www.youtube.com/channel/UC123",
            "https://notyoutube.com/watch?v=jNQXAC9IVRw",
            "https://open.spotify.com/track/4uLU6hMCjMI75M1A2tKUQC",
            "C:\\music\\a.mp3",
        ] {
            assert!(parse(s).is_none(), "{s}");
        }
    }

    #[test]
    fn parses_durations() {
        assert_eq!(duration_ms("19"), 19_000);
        assert_eq!(duration_ms("19.5\n"), 19_500);
        assert_eq!(duration_ms("NA"), -1);
    }
}
