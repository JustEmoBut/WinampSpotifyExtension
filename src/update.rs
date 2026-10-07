//! Update check and one-click update: on startup, asks GitHub for the latest release and, once
//! per new version, offers to install it. Installing downloads the release zip, checks its
//! SHA-256 against the digest GitHub reports, extracts it with Windows' tar and runs the bundled
//! `install.bat /update` (which elevates, closes Winamp, installs and restarts it).
//! Only release builds know their version (`SPOTITUBE_VERSION`, set by CI from the tag), so local
//! builds never check. `check_updates=0` in config.ini turns it off.

use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;

use crate::{
    IDYES, MB_ICONERROR, MB_ICONINFORMATION, MB_YESNO, MessageBoxW, SW_SHOWNORMAL, ShellExecuteW, cache_dir,
    config_value, module, to_wide,
};

/// Shown from a worker thread, which Windows doesn't let take the foreground on its own.
const MB_SETFOREGROUND: u32 = 0x0001_0000;
const MB_TOPMOST: u32 = 0x0004_0000;

const LATEST_RELEASE_API: &str = "https://api.github.com/repos/JustEmoBut/WinampSpotifyExtension/releases/latest";
/// Only zips from this repository's releases are installed.
const DOWNLOAD_PREFIX: &str = "https://github.com/JustEmoBut/WinampSpotifyExtension/releases/download/";
/// Remembers the last version offered, so each release is offered only once.
const NOTIFIED_FILE: &str = "update_notified";
const MAX_API_BYTES: usize = 1024 * 1024;
const MAX_ZIP_BYTES: usize = 64 * 1024 * 1024;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// ShellExecuteW returns a value greater than this on success.
const SHELL_EXECUTE_OK: usize = 32;
/// CNG pseudo-handle for SHA-256 (Windows 10+).
const BCRYPT_SHA256_ALG_HANDLE: usize = 0x41;
const SHA256_BYTES: usize = 32;

#[link(name = "bcrypt")]
unsafe extern "system" {
    fn BCryptHash(
        algorithm: usize, secret: *const u8, secret_len: u32, input: *const u8, input_len: u32, output: *mut u8,
        output_len: u32,
    ) -> i32;
}

struct Release {
    tag: String,
    page: String,
    zip_url: String,
    /// Lowercase hex SHA-256 of the zip.
    sha256: String,
}

pub fn check_in_background() {
    let Some(current) = option_env!("SPOTITUBE_VERSION") else { return };
    if config_value("check_updates").is_some_and(|v| v == "0") {
        return;
    }
    std::thread::spawn(move || {
        // Network or parse failures just mean no prompt this time.
        let Some(json) = crate::albumart::download(LATEST_RELEASE_API, MAX_API_BYTES) else { return };
        let Some(release) = parse_release(&String::from_utf8_lossy(&json)) else { return };
        let notified = cache_dir().join(NOTIFIED_FILE);
        if !is_newer(&release.tag, current) || std::fs::read_to_string(&notified).is_ok_and(|v| v.trim() == release.tag) {
            return;
        }
        // Written before asking so a "No" isn't asked again; failure only means asking again next start.
        let _ = std::fs::create_dir_all(cache_dir()).and_then(|()| std::fs::write(&notified, &release.tag));
        let question = format!(
            "SpotiTube {} is available (you have {current}).\n\nUpdate now? Winamp will close, update and restart by itself (Windows may ask for administrator rights).\n\nTurn these checks off with check_updates=0 in config.ini.",
            release.tag
        );
        if !ask(&question, MB_YESNO | MB_ICONINFORMATION) {
            return;
        }
        if let Err(e) = install(&release) {
            let text = format!("Update failed: {e}\n\nOpen the download page instead?");
            if ask(&text, MB_YESNO | MB_ICONERROR) {
                open(&release.page);
            }
        }
    });
}

fn ask(text: &str, kind: u32) -> bool {
    let (text, caption) = (to_wide(text), to_wide("SpotiTube update"));
    let kind = kind | MB_SETFOREGROUND | MB_TOPMOST;
    unsafe { MessageBoxW(module().h_main_window, text.as_ptr(), caption.as_ptr(), kind) == IDYES }
}

fn open(target: &str) -> bool {
    run(target, None)
}

fn run(target: &str, params: Option<&str>) -> bool {
    let (verb, target) = (to_wide("open"), to_wide(target));
    let params = params.map(to_wide);
    let params = params.as_ref().map_or(std::ptr::null(), |p| p.as_ptr());
    let r = unsafe {
        ShellExecuteW(module().h_main_window, verb.as_ptr(), target.as_ptr(), params, std::ptr::null(), SW_SHOWNORMAL)
    };
    r as usize > SHELL_EXECUTE_OK
}

/// Downloads, verifies and extracts the release, then starts its install.bat.
fn install(release: &Release) -> Result<(), String> {
    let zip = crate::albumart::download(&release.zip_url, MAX_ZIP_BYTES).ok_or("download failed")?;
    let actual = sha256_hex(&zip).ok_or("could not compute the checksum")?;
    if actual != release.sha256 {
        return Err("the download's checksum doesn't match the release; nothing was installed".into());
    }
    // The tag was checked to be path-safe in parse_release.
    let dir = std::env::temp_dir().join(format!("SpotiTube-update-{}", release.tag));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let zip_path = dir.join("update.zip");
    std::fs::write(&zip_path, &zip).map_err(|e| format!("could not write {}: {e}", zip_path.display()))?;
    extract(&zip_path, &dir)?;
    let installer = dir.join("install.bat");
    if !installer.exists() {
        return Err("the release zip has no install.bat".into());
    }
    // /update: the installer closes Winamp, installs without asking and restarts it.
    if !run(&installer.to_string_lossy(), Some("/update")) {
        return Err("could not start install.bat".into());
    }
    Ok(())
}

/// Extracts with Windows' bundled bsdtar (Windows 10 1803+), which reads zip files.
fn extract(zip: &Path, dir: &Path) -> Result<(), String> {
    let windir = std::env::var_os("SystemRoot").ok_or("SystemRoot is not set")?;
    let tar = Path::new(&windir).join("System32").join("tar.exe");
    let status = Command::new(tar)
        .creation_flags(CREATE_NO_WINDOW)
        .arg("-xf")
        .arg(zip)
        .arg("-C")
        .arg(dir)
        .status()
        .map_err(|e| format!("could not run tar: {e}"))?;
    if status.success() { Ok(()) } else { Err(format!("tar failed ({status})")) }
}

fn sha256_hex(data: &[u8]) -> Option<String> {
    let mut hash = [0u8; SHA256_BYTES];
    let len = u32::try_from(data.len()).ok()?;
    let status = unsafe {
        BCryptHash(BCRYPT_SHA256_ALG_HANDLE, std::ptr::null(), 0, data.as_ptr(), len, hash.as_mut_ptr(), SHA256_BYTES as u32)
    };
    (status == 0).then(|| hash.iter().map(|b| format!("{b:02x}")).collect())
}

/// Picks the release zip out of GitHub's `releases/latest` JSON. Rejects anything that isn't a
/// path-safe tag, a zip from this repository, or a SHA-256 digest.
fn parse_release(json: &str) -> Option<Release> {
    let tag = json_string(json, "tag_name")?;
    if tag.is_empty() || !tag.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')) {
        return None;
    }
    let page = json_string(json, "html_url")?;
    let key = "\"browser_download_url\"";
    let mut asset_start = 0;
    while let Some(i) = json[asset_start..].find(key) {
        let at = asset_start + i;
        let url = json_string(&json[at..], "browser_download_url")?;
        if url.starts_with(DOWNLOAD_PREFIX) && url.ends_with(".zip") {
            // An asset's digest comes before its download URL, after the previous asset's URL.
            let digest_at = asset_start + json[asset_start..at].rfind("\"digest\"")?;
            let digest = json_string(&json[digest_at..], "digest")?;
            let sha256 = digest.strip_prefix("sha256:")?.to_ascii_lowercase();
            if sha256.len() != 2 * SHA256_BYTES || !sha256.chars().all(|c| c.is_ascii_hexdigit()) {
                return None;
            }
            return Some(Release { tag, page, zip_url: url, sha256 });
        }
        asset_start = at + key.len();
    }
    None
}

/// Value of the first `"key":"value"` string in GitHub's JSON (the values used here have no escapes).
fn json_string(json: &str, key: &str) -> Option<String> {
    let after_key = &json[json.find(&format!("\"{key}\""))? + key.len() + 2..];
    let value = after_key.trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
    Some(value[..value.find('"')?].to_owned())
}

/// `v1.2.3` style versions; anything unparsable is never newer.
fn is_newer(latest: &str, current: &str) -> bool {
    let parse = |v: &str| -> Option<Vec<u32>> { v.trim_start_matches('v').split('.').map(|p| p.parse().ok()).collect() };
    matches!((parse(latest), parse(current)), (Some(l), Some(c)) if l > c)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "e2f090855274ce17132cd2342ac519e6d1cd0115170203fd52a15f45ae282d98";

    fn release_json(tag: &str, assets: &str) -> String {
        format!(r#"{{"html_url":"https://github.com/JustEmoBut/WinampSpotifyExtension/releases/tag/{tag}","tag_name":"{tag}","assets":[{assets}]}}"#)
    }

    fn asset(name: &str, digest: &str) -> String {
        format!(
            r#"{{"name":"{name}","uploader":{{"login":"x"}},"digest":"{digest}","browser_download_url":"{DOWNLOAD_PREFIX}v0.3.1/{name}"}}"#
        )
    }

    #[test]
    fn parses_release() {
        let assets = [asset("notes.txt", "sha256:00"), asset("SpotiTube-v0.3.1.zip", &format!("sha256:{}", HASH.to_uppercase()))].join(",");
        let r = parse_release(&release_json("v0.3.1", &assets)).unwrap();
        assert_eq!(r.tag, "v0.3.1");
        assert_eq!(r.zip_url, format!("{DOWNLOAD_PREFIX}v0.3.1/SpotiTube-v0.3.1.zip"));
        assert_eq!(r.sha256, HASH);
        assert!(r.page.ends_with("/tag/v0.3.1"));
    }

    #[test]
    fn rejects_unsafe_releases() {
        let zip = asset("SpotiTube.zip", &format!("sha256:{HASH}"));
        assert!(parse_release(&release_json("..\\..\\x", &zip)).is_none(), "path traversal in tag");
        assert!(parse_release(&release_json("v1", &asset("SpotiTube.zip", "sha1:abc"))).is_none(), "not sha256");
        // A digest belonging to the previous asset must not be used for a zip without one.
        let no_digest = format!(r#"{{"name":"a.zip","browser_download_url":"{DOWNLOAD_PREFIX}v1/a.zip"}}"#);
        let assets = [asset("notes.txt", &format!("sha256:{HASH}")), no_digest].join(",");
        assert!(parse_release(&release_json("v1", &assets)).is_none(), "digest from another asset");
        let foreign = r#"{"digest":"sha256:00","browser_download_url":"https://evil.example/SpotiTube.zip"}"#;
        assert!(parse_release(&release_json("v1", foreign)).is_none(), "zip from another host");
        assert!(parse_release(r#"{"message":"Not Found"}"#).is_none());
    }

    #[test]
    fn hashes_sha256() {
        assert_eq!(sha256_hex(b"abc").unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn compares_versions() {
        assert!(is_newer("v0.3.10", "v0.3.9"));
        assert!(is_newer("v1.0.0", "v0.9.9"));
        assert!(!is_newer("v0.3.1", "v0.3.1"));
        assert!(!is_newer("v0.3.0", "v0.3.1"));
        assert!(!is_newer("nightly", "v0.3.1"));
    }

    /// Real API and download, without running the installer: `cargo test --release -- --ignored`.
    #[test]
    #[ignore]
    fn downloads_and_verifies_latest_release_live() {
        let json = crate::albumart::download(LATEST_RELEASE_API, MAX_API_BYTES).unwrap();
        let release = parse_release(&String::from_utf8_lossy(&json)).unwrap();
        let zip = crate::albumart::download(&release.zip_url, MAX_ZIP_BYTES).unwrap();
        assert_eq!(sha256_hex(&zip).unwrap(), release.sha256);
        let dir = std::env::temp_dir().join("SpotiTube-update-livetest");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let zip_path = dir.join("update.zip");
        std::fs::write(&zip_path, &zip).unwrap();
        extract(&zip_path, &dir).unwrap();
        for f in ["install.bat", "in_spotitube.dll", "ml_spotitube.dll"] {
            assert!(dir.join(f).exists(), "{f} missing from {}", release.tag);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
