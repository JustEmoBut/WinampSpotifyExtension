//! Update check: on startup, asks GitHub for the latest release and offers its page once per
//! new version. Only release builds know their version (`SPOTITUBE_VERSION`, set by CI from the
//! tag), so local builds never check. `check_updates=0` in config.ini turns it off.

use crate::{
    IDYES, MB_ICONINFORMATION, MB_YESNO, MessageBoxW, SW_SHOWNORMAL, ShellExecuteW, cache_dir, config_value, module,
    to_wide,
};

const LATEST_RELEASE_API: &str = "https://api.github.com/repos/JustEmoBut/WinampSpotifyExtension/releases/latest";
/// Remembers the last version offered, so each release is offered only once.
const NOTIFIED_FILE: &str = "update_notified";

pub fn check_in_background() {
    let Some(current) = option_env!("SPOTITUBE_VERSION") else { return };
    if config_value("check_updates").is_some_and(|v| v == "0") {
        return;
    }
    std::thread::spawn(move || {
        // Network or parse failures just mean no prompt this time.
        let Some(json) = crate::albumart::download(LATEST_RELEASE_API) else { return };
        let json = String::from_utf8_lossy(&json);
        let (Some(latest), Some(page)) = (json_string(&json, "tag_name"), json_string(&json, "html_url")) else { return };
        let notified = cache_dir().join(NOTIFIED_FILE);
        if !is_newer(&latest, current) || std::fs::read_to_string(&notified).is_ok_and(|v| v.trim() == latest) {
            return;
        }
        // Written before asking so a "No" isn't asked again; failure only means asking again next start.
        let _ = std::fs::create_dir_all(cache_dir()).and_then(|()| std::fs::write(&notified, &latest));
        let text = to_wide(&format!(
            "SpotiTube {latest} is available (you have {current}).\n\nOpen the download page?\n\nTurn this off with check_updates=0 in config.ini."
        ));
        let caption = to_wide("SpotiTube update");
        let parent = module().h_main_window;
        unsafe {
            if MessageBoxW(parent, text.as_ptr(), caption.as_ptr(), MB_YESNO | MB_ICONINFORMATION) == IDYES {
                let (open, url) = (to_wide("open"), to_wide(&page));
                ShellExecuteW(parent, open.as_ptr(), url.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL);
            }
        }
    });
}

/// Value of a top-level `"key":"value"` string in GitHub's JSON (values here have no escapes).
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

    #[test]
    fn parses_release_json() {
        let json = r#"{"url":"x","html_url":"https://github.com/o/r/releases/tag/v0.3.1","tag_name" : "v0.3.1","draft":false}"#;
        assert_eq!(json_string(json, "tag_name").as_deref(), Some("v0.3.1"));
        assert_eq!(json_string(json, "html_url").as_deref(), Some("https://github.com/o/r/releases/tag/v0.3.1"));
        assert_eq!(json_string(r#"{"message":"Not Found"}"#, "tag_name"), None);
    }

    #[test]
    fn compares_versions() {
        assert!(is_newer("v0.3.10", "v0.3.9"));
        assert!(is_newer("v1.0.0", "v0.9.9"));
        assert!(!is_newer("v0.3.1", "v0.3.1"));
        assert!(!is_newer("v0.3.0", "v0.3.1"));
        assert!(!is_newer("nightly", "v0.3.1"));
    }
}
