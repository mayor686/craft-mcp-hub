//! Whether a newer Craft MCP Hub has been released, so the panel and `crafthub list` can point to it.
//!
//! The hub asks GitHub at most once a day, with one HEAD request to the repository's
//! `releases/latest` page: GitHub answers with a redirect to the latest release's tag, and nothing
//! about this computer is sent beyond the request itself. The answer is kept in
//! `~/.crafthub/update-check.json`. `CRAFTHUB_NO_UPDATE_CHECK=1` turns the check off.

use crate::{files, paths, proc, timefmt, VERSION};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::fs;

pub const RELEASES_URL: &str = "https://github.com/mayor686/craft-mcp-hub/releases";
const CHECK_EVERY_SECS: u64 = 24 * 60 * 60;

/// A release newer than this program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Newer {
    pub version: String,
    /// The release's page on GitHub.
    pub url: String,
}

#[derive(Serialize, Deserialize)]
struct Checked {
    /// When GitHub was last asked, in seconds since 1970.
    at: u64,
    /// The latest release then, without the `v`.
    latest: String,
}

fn cache_file() -> std::path::PathBuf {
    paths::hub_home().join("update-check.json")
}

/// Whether the check is turned off (`CRAFTHUB_NO_UPDATE_CHECK` set to anything but empty or `0`).
pub fn disabled() -> bool {
    std::env::var("CRAFTHUB_NO_UPDATE_CHECK").is_ok_and(|v| !v.is_empty() && v != "0")
}

/// Orders two versions like `0.6.10` and `0.6.9` by their numbers. A pre-release (`1.0.0-beta`)
/// comes before the release.
pub fn compare(a: &str, b: &str) -> Ordering {
    let split = |v: &str| {
        let v = v.trim().trim_start_matches('v');
        let (numbers, pre) = v.split_once('-').map_or((v, false), |(n, _)| (n, true));
        let numbers: Vec<u64> = numbers.split('.').map(|n| n.parse().unwrap_or(0)).collect();
        (numbers, pre)
    };
    let ((mut x, x_pre), (mut y, y_pre)) = (split(a), split(b));
    let len = x.len().max(y.len());
    x.resize(len, 0);
    y.resize(len, 0);
    x.cmp(&y).then(y_pre.cmp(&x_pre))
}

/// A version as it may appear in a URL: digits, letters, `.`, `-`.
fn plain(version: &str) -> bool {
    !version.is_empty() && version.len() < 40 && version.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
}

/// The version the `releases/latest` redirect points to, from the response's headers.
fn latest_from_headers(headers: &str) -> Option<String> {
    let location = headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim().eq_ignore_ascii_case("location").then(|| value.trim().to_string())
    })?;
    let tag = location.rsplit_once("/tag/")?.1;
    let version = tag.trim_end_matches('/').trim_start_matches('v').to_string();
    plain(&version).then_some(version)
}

fn ask_github() -> Result<String, String> {
    let curl = paths::find_in_path("curl").ok_or("curl was not found on this computer")?;
    let url = format!("{RELEASES_URL}/latest");
    let out = proc::run(&curl, &["-sSI", "--proto", "=https", "--max-time", "10", &url]);
    if !out.ok {
        return Err(format!("could not reach GitHub: {}", out.text));
    }
    latest_from_headers(&out.stdout).ok_or_else(|| "GitHub's answer did not name a release".to_string())
}

fn newer_than_this(latest: &str) -> Option<Newer> {
    (compare(latest, VERSION) == Ordering::Greater).then(|| Newer { version: latest.to_string(), url: format!("{RELEASES_URL}/tag/v{latest}") })
}

fn read_cache() -> Option<Checked> {
    serde_json::from_str(&fs::read_to_string(cache_file()).ok()?).ok()
}

/// A newer release, asking GitHub when the last answer is more than a day old (or always, with
/// `now`). `Ok(None)` when this is the latest, or when the check is turned off.
pub fn newer(now: bool) -> Result<Option<Newer>, String> {
    if disabled() {
        return Ok(None);
    }
    let fresh = read_cache().filter(|c| !now && timefmt::now_secs().saturating_sub(c.at) < CHECK_EVERY_SECS && plain(&c.latest));
    let latest = match fresh {
        Some(checked) => checked.latest,
        None => {
            let latest = ask_github()?;
            // Kept only where the hub already has its folder: a check alone doesn't create it.
            if paths::hub_home().is_dir() {
                let checked = serde_json::to_string(&Checked { at: timefmt::now_secs(), latest: latest.clone() }).unwrap_or_default();
                let _ = files::write_atomic(&cache_file(), checked.as_bytes());
            }
            latest
        }
    };
    Ok(newer_than_this(&latest))
}

/// A newer release as the last check found it, without asking GitHub.
pub fn known_newer() -> Option<Newer> {
    if disabled() {
        return None;
    }
    read_cache().filter(|c| plain(&c.latest)).and_then(|c| newer_than_this(&c.latest))
}

/// How to update on this system: the install command again.
pub fn how_to_update() -> &'static str {
    if cfg!(windows) {
        "irm https://raw.githubusercontent.com/mayor686/craft-mcp-hub/main/install.ps1 | iex"
    } else {
        "curl -fsSL https://raw.githubusercontent.com/mayor686/craft-mcp-hub/main/install.sh | sh"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_ordered_by_their_numbers() {
        assert_eq!(compare("0.6.1", "0.6.0"), Ordering::Greater);
        assert_eq!(compare("0.6.10", "0.6.9"), Ordering::Greater);
        assert_eq!(compare("v0.7.0", "0.6.9"), Ordering::Greater);
        assert_eq!(compare("0.6", "0.6.0"), Ordering::Equal);
        assert_eq!(compare("0.6.0", "0.6.0"), Ordering::Equal);
        assert_eq!(compare("1.0.0-beta.1", "1.0.0"), Ordering::Less);
        assert_eq!(compare("0.5.1", "0.6.0"), Ordering::Less);
    }

    #[test]
    fn the_latest_release_is_read_from_the_redirect() {
        let headers =
            "HTTP/2 302 \r\ndate: Wed, 18 Nov 2026 14:30:00 GMT\r\nLocation: https://github.com/AndrianLeah/craft-mcp-hub/releases/tag/v0.6.1\r\n\r\n";
        assert_eq!(latest_from_headers(headers).as_deref(), Some("0.6.1"));
        assert_eq!(latest_from_headers("HTTP/2 200 \r\ncontent-type: text/html\r\n"), None);
        assert_eq!(latest_from_headers("location: https://github.com/x/y/releases/tag/v1.0?evil=<b>\r\n"), None);
    }

    #[test]
    fn only_a_newer_release_is_offered() {
        assert_eq!(newer_than_this(VERSION), None);
        assert_eq!(newer_than_this("0.0.1"), None);
        let newer = newer_than_this("99.0.0").expect("a newer release");
        assert_eq!(newer.url, "https://github.com/mayor686/craft-mcp-hub/releases/tag/v99.0.0");
    }
}
