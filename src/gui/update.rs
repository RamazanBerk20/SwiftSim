//! Update awareness: reads SwiftSim's release list on GitHub and reports a newer release.
//! Only public data is read; nothing is downloaded or installed.

use std::time::Duration;

/// This build's version: the release tag for release builds (set by the release workflow),
/// otherwise Cargo's version (local and CI builds).
pub const VERSION: &str = match option_env!("SWIFTSIM_VERSION") {
    Some(v) if !v.is_empty() => v,
    _ => env!("CARGO_PKG_VERSION"),
};

const RELEASES: &str = "https://api.github.com/repos/RamazanBerk20/SwiftSim/releases?per_page=30";

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub tag: String,
    pub url: String,
}

/// Sort key (major, minor, patch, is final, pre-release number):
/// `v0.1.0-rc3` < `0.1.0-rc.10` < `0.1.0` < `0.1.1-rc1`.
/// shortcut: a pre-release is ranked by its number only (rcN); add alpha/beta ordering if those tags are used.
fn key(v: &str) -> Option<(u64, u64, u64, bool, u64)> {
    let v = v.trim().trim_start_matches(['v', 'V']);
    let (core, pre) = match v.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (v, None),
    };
    let mut n = core.split('.').map(|x| x.parse::<u64>().ok());
    let (a, b, c) = (n.next()??, n.next().unwrap_or(Some(0))?, n.next().unwrap_or(Some(0))?);
    let pn = pre.map_or(0, |p| p.chars().filter(char::is_ascii_digit).collect::<String>().parse().unwrap_or(0));
    Some((a, b, c, pre.is_none(), pn))
}

/// The newest release above `current` in GitHub's release list (JSON). Pre-releases count only
/// when `current` is one: testers hear about release candidates, stable users only about stable releases.
pub fn newest(json: &str, current: &str) -> Result<Option<Release>, String> {
    let list: Vec<serde_json::Value> = serde_json::from_str(json).map_err(|e| format!("unexpected reply from GitHub: {e}"))?;
    let cur = key(current).ok_or(format!("unknown version '{current}'"))?;
    Ok(list
        .iter()
        .filter(|r| !r["draft"].as_bool().unwrap_or(false))
        .filter(|r| !cur.3 || !r["prerelease"].as_bool().unwrap_or(false))
        .filter_map(|r| {
            let tag = r["tag_name"].as_str()?;
            Some((key(tag)?, Release { tag: tag.to_string(), url: r["html_url"].as_str()?.to_string() }))
        })
        .filter(|(k, _)| *k > cur)
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, r)| r))
}

/// Asks GitHub for the release list (blocking, up to 10 s; run it off the UI thread).
pub fn check() -> Result<Option<Release>, String> {
    newest(&fetch(RELEASES)?, VERSION)
}

fn fetch(url: &str) -> Result<String, String> {
    let agent = ureq::Agent::new_with_config(ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(10))).build());
    let mut r = agent
        .get(url)
        .header("User-Agent", format!("SwiftSim/{VERSION}"))
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| match e {
            ureq::Error::StatusCode(404) => "the release list isn't public (the GitHub repository is private)".to_string(),
            ureq::Error::StatusCode(403 | 429) => "GitHub's rate limit was reached; try again later".to_string(),
            e => format!("couldn't reach GitHub: {e}"),
        })?;
    r.body_mut().read_to_string().map_err(|e| format!("couldn't read GitHub's reply: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = r#"[
        {"tag_name":"v0.2.0-rc1","html_url":"https://x/rc-next","draft":true,"prerelease":true},
        {"tag_name":"v0.1.0-rc10","html_url":"https://x/rc10","draft":false,"prerelease":true},
        {"tag_name":"v0.1.0-rc3","html_url":"https://x/rc3","draft":false,"prerelease":true},
        {"tag_name":"v0.0.9","html_url":"https://x/old","draft":false,"prerelease":false},
        {"tag_name":"nightly","html_url":"https://x/n","draft":false,"prerelease":true}
    ]"#;

    // network: the same request against a public repo's releases (run with --ignored)
    #[test]
    #[ignore]
    fn live_github_release_list() {
        let json = fetch("https://api.github.com/repos/emilk/egui/releases?per_page=30").unwrap();
        let r = newest(&json, "0.0.1").unwrap().expect("egui has stable releases");
        assert!(r.tag.starts_with('0') && r.url.starts_with("https://github.com/emilk/egui/releases/"), "{r:?}");
    }

    #[test]
    fn version_order() {
        assert!(key("v0.1.0-rc3") < key("0.1.0-rc.10"));
        assert!(key("0.1.0-rc10") < key("v0.1.0"));
        assert!(key("v0.1.0") < key("0.1.1-rc1"));
        assert!(key("1.2") == key("1.2.0"));
        assert!(key("nightly").is_none());
    }

    #[test]
    fn picks_newest_release_for_the_channel() {
        // a release candidate sees newer candidates (rc10 over rc3; drafts never)
        assert_eq!(newest(LIST, "v0.1.0-rc3").unwrap().map(|r| r.tag), Some("v0.1.0-rc10".into()));
        assert_eq!(newest(LIST, "v0.1.0-rc10").unwrap(), None);
        // a stable build only sees stable releases
        assert_eq!(newest(LIST, "0.0.8").unwrap().map(|r| r.url), Some("https://x/old".into()));
        assert_eq!(newest(LIST, "0.1.0").unwrap(), None);
        assert!(newest("{\"message\":\"Not Found\"}", "0.1.0").is_err());
    }
}
