//! Checks GitHub for a newer release. The app downloads nothing by itself: a newer release
//! opens in the browser, and the user drags the app to Applications.
//!
//! The request is anonymous. A private repository answers 404, and no token is built into the app.

use std::cmp::Ordering;
use std::process::Command;

use serde::Deserialize;

const LATEST: &str = "https://api.github.com/repos/07rjain/mac-storage-cleaner/releases/latest";
const RELEASE_PREFIX: &str = "https://github.com/07rjain/mac-storage-cleaner/";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub page: String,
    pub dmg: Option<String>,
}

/// `true` when `latest` is a higher `major.minor.patch` than `current`.
pub fn is_newer(latest: &str, current: &str) -> bool {
    parts(latest).cmp(&parts(current)) == Ordering::Greater
}

fn parts(version: &str) -> [u64; 3] {
    let version = version.trim().trim_start_matches('v');
    let mut numbers = [0; 3];
    for (slot, part) in version.split('.').take(3).enumerate() {
        let digits: String = part.chars().take_while(|c| c.is_ascii_digit()).collect();
        numbers[slot] = digits.parse().unwrap_or(0);
    }
    numbers
}

/// Only this project's own GitHub release URLs. An unexpected address is not opened.
pub fn is_release_url(url: &str) -> bool {
    url.starts_with(RELEASE_PREFIX)
        && url
            .chars()
            .all(|c| c.is_ascii() && !c.is_ascii_control() && c != ' ')
}

pub fn fetch_latest() -> Result<Release, String> {
    let output = Command::new("/usr/bin/curl")
        .args([
            "-sS",
            "-m",
            "20",
            "-w",
            "\n%{http_code}",
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            "User-Agent: mac-storage-cleaner",
            LATEST,
        ])
        .output()
        .map_err(|error| format!("Couldn't start the update check: {error}"))?;
    let text = String::from_utf8_lossy(&output.stdout);
    let (body, code) = text.rsplit_once('\n').unwrap_or((text.as_ref(), ""));
    match code.trim() {
        "200" => parse_release(body).ok_or_else(|| "The release list was unreadable.".into()),
        "404" => Err(
            "Releases aren't visible. The GitHub repository has to be public for the app to see them."
                .into(),
        ),
        "000" => {
            let detail = String::from_utf8_lossy(&output.stderr);
            let detail = detail.trim();
            if detail.is_empty() {
                Err("Couldn't reach GitHub.".into())
            } else {
                Err(format!("Couldn't reach GitHub: {detail}"))
            }
        }
        other => Err(format!("GitHub returned status {other}.")),
    }
}

pub fn parse_release(body: &str) -> Option<Release> {
    let payload: Payload = serde_json::from_str(body).ok()?;
    let version = payload.tag_name.trim().trim_start_matches('v').to_string();
    if version.is_empty() || !is_release_url(&payload.html_url) {
        return None;
    }
    let dmg = payload.assets.into_iter().find_map(|asset| {
        (asset.name.ends_with(".dmg") && is_release_url(&asset.browser_download_url))
            .then_some(asset.browser_download_url)
    });
    Some(Release {
        version,
        page: payload.html_url,
        dmg,
    })
}

#[derive(Deserialize)]
struct Payload {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_compares_major_minor_and_patch() {
        assert!(is_newer("0.2.2", "0.2.1"));
        assert!(is_newer("v0.3.0", "0.2.9"));
        assert!(!is_newer("0.2.1", "0.2.1"));
        assert!(!is_newer("0.2.0", "0.2.1"));
        assert!(is_newer("1.0", "0.9.9"));
    }

    #[test]
    fn parses_a_release_and_keeps_only_its_dmg() {
        let body = r#"{
            "tag_name": "v0.2.2",
            "html_url": "https://github.com/07rjain/mac-storage-cleaner/releases/tag/v0.2.2",
            "assets": [
                {
                    "name": "notes.txt",
                    "browser_download_url": "https://github.com/07rjain/mac-storage-cleaner/releases/download/v0.2.2/notes.txt"
                },
                {
                    "name": "Mac-Storage-Cleaner-0.2.2.dmg",
                    "browser_download_url": "https://github.com/07rjain/mac-storage-cleaner/releases/download/v0.2.2/Mac-Storage-Cleaner-0.2.2.dmg"
                }
            ]
        }"#;
        let release = parse_release(body).unwrap();
        assert_eq!(release.version, "0.2.2");
        assert_eq!(
            release.dmg.as_deref(),
            Some(
                "https://github.com/07rjain/mac-storage-cleaner/releases/download/v0.2.2/Mac-Storage-Cleaner-0.2.2.dmg"
            )
        );
    }

    #[test]
    fn rejects_a_release_that_points_somewhere_else() {
        let body = r#"{
            "tag_name": "v9.9.9",
            "html_url": "https://example.com/evil",
            "assets": [{
                "name": "Mac-Storage-Cleaner-9.9.9.dmg",
                "browser_download_url": "https://example.com/Mac-Storage-Cleaner-9.9.9.dmg"
            }]
        }"#;
        assert!(parse_release(body).is_none());
        assert!(!is_release_url(
            "https://github.com/07rjain/mac-storage-cleaner/releases/download/v0.2.1/a.dmg\nhttps://evil"
        ));
    }
}
