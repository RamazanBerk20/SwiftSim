//! Updates: reads SwiftSim's release list on GitHub and installs a newer release in place.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

/// This build's version: the release tag for release builds (set by the release workflow),
/// otherwise Cargo's version (local and CI builds).
pub const VERSION: &str = match option_env!("SWIFTSIM_VERSION") {
    Some(v) if !v.is_empty() => v,
    _ => env!("CARGO_PKG_VERSION"),
};

const RELEASES: &str = "https://api.github.com/repos/RamazanBerk20/SwiftSim/releases?per_page=30";
const DOWNLOADS: &str = "https://github.com/RamazanBerk20/SwiftSim/releases/download/";
const STAGE: &str = ".swiftsim-update";

#[derive(Clone, Debug, PartialEq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub tag: String,
    pub url: String,
    pub assets: Vec<Asset>,
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
            let assets = r["assets"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|a| {
                    let url = a["browser_download_url"].as_str().filter(|u| u.starts_with(DOWNLOADS))?;
                    Some(Asset { name: a["name"].as_str()?.to_string(), url: url.to_string(), size: a["size"].as_u64()? })
                })
                .collect();
            Some((key(tag)?, Release { tag: tag.to_string(), url: r["html_url"].as_str()?.to_string(), assets }))
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

/// The file this copy runs from: the AppImage itself, not the program inside its temporary mount.
pub fn self_path() -> PathBuf {
    std::env::var_os("APPIMAGE").map(PathBuf::from).or_else(|| std::env::current_exe().ok()).unwrap_or_else(|| "swiftsim".into())
}

fn writable(target: &Path) -> bool {
    let probe = target.with_file_name(STAGE);
    fs::create_dir_all(&probe).is_ok() && fs::remove_dir(&probe).is_ok()
}

/// The release file to install for this copy and the path it replaces. `None` when SwiftSim shouldn't
/// replace itself: builds from source, and copies a package manager owns or that the user can't write
/// (Windows can still ask for administrator rights).
pub fn plan(rel: &Release) -> Option<(Asset, PathBuf)> {
    option_env!("SWIFTSIM_VERSION").filter(|v| !v.is_empty())?;
    let target = self_path();
    let name = if cfg!(windows) {
        "swiftsim-windows-x86_64.zip"
    } else if std::env::var_os("APPIMAGE").is_some() {
        "SwiftSim-x86_64.AppImage"
    } else {
        "swiftsim-linux-x86_64.tar.gz"
    };
    if !cfg!(windows) && (target.starts_with("/usr") || !writable(&target)) {
        return None;
    }
    Some((rel.assets.iter().find(|a| a.name == name)?.clone(), target))
}

/// Removes what an earlier update left behind.
pub fn cleanup() {
    let t = self_path();
    let _ = fs::remove_file(t.with_extension("exe.old"));
    let _ = fs::remove_dir_all(t.with_file_name(STAGE));
}

/// Downloads `asset`, checks that it runs and reports version `tag`, then replaces `target` with it.
/// The running program keeps working; the new version starts with the next launch.
/// Blocking: run it off the UI thread. `progress` is in 1/1000.
pub fn install(tag: &str, asset: &Asset, target: &Path, progress: &AtomicU32) -> Result<(), String> {
    let beside = target.with_file_name(STAGE);
    let stage = if fs::create_dir_all(&beside).is_ok() { beside } else { std::env::temp_dir().join("swiftsim-update") };
    fs::create_dir_all(&stage).map_err(|e| format!("couldn't create {}: {e}", stage.display()))?;
    let result = download(asset, &stage.join(&asset.name), progress)
        .and_then(|()| unpack(&stage, &asset.name))
        .and_then(|new| verify(&new, tag).map(|()| new))
        .and_then(|new| swap(&new, target));
    let _ = fs::remove_dir_all(&stage);
    result
}

fn download(asset: &Asset, to: &Path, progress: &AtomicU32) -> Result<(), String> {
    let agent = ureq::Agent::new_with_config(ureq::Agent::config_builder().timeout_connect(Some(Duration::from_secs(30))).timeout_global(Some(Duration::from_secs(30 * 60))).build());
    let mut r = agent.get(&asset.url).header("User-Agent", format!("SwiftSim/{VERSION}")).call().map_err(|e| format!("couldn't download {}: {e}", asset.name))?;
    let mut body = r.body_mut().as_reader();
    let mut file = fs::File::create(to).map_err(|e| format!("couldn't create {}: {e}", to.display()))?;
    let (mut buf, mut got) = (vec![0; 1 << 16], 0u64);
    loop {
        let n = body.read(&mut buf).map_err(|e| format!("download of {} failed: {e}", asset.name))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| format!("couldn't write {}: {e}", to.display()))?;
        got += n as u64;
        progress.store((got * 1000 / asset.size.max(1)).min(1000) as u32, Ordering::Relaxed);
    }
    if got != asset.size {
        return Err(format!("download of {} is incomplete ({got} of {} bytes)", asset.name, asset.size));
    }
    Ok(())
}

/// The new program inside the downloaded file (`tar` ships with Linux and Windows 10+, and reads .zip there).
fn unpack(stage: &Path, name: &str) -> Result<PathBuf, String> {
    let archive = stage.join(name);
    let tar = |flag: &str, members: &[&str]| -> Result<(), String> {
        let tool = std::env::var_os("SystemRoot").map_or("tar".into(), |r| PathBuf::from(r).join("System32").join("tar.exe"));
        let st = Command::new(tool).arg(flag).arg(&archive).arg("-C").arg(stage).args(members).status().map_err(|e| format!("couldn't run tar: {e}"))?;
        if st.success() { Ok(()) } else { Err(format!("couldn't unpack {name}")) }
    };
    let new = if name.ends_with(".tar.gz") {
        tar("-xzf", &["--strip-components=1", "swiftsim-linux-x86_64/swiftsim"])?;
        stage.join("swiftsim")
    } else if name.ends_with(".zip") {
        tar("-xf", &["swiftsim.exe"])?;
        stage.join("swiftsim.exe")
    } else {
        archive.clone()
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&new, fs::Permissions::from_mode(0o755)).map_err(|e| format!("couldn't make {} executable: {e}", new.display()))?;
    }
    Ok(new)
}

fn verify(new: &Path, tag: &str) -> Result<(), String> {
    let out = Command::new(new).arg("--help").output().map_err(|e| format!("the downloaded program doesn't start: {e}"))?;
    if String::from_utf8_lossy(&out.stdout).contains(tag) { Ok(()) } else { Err(format!("the downloaded program isn't SwiftSim {tag}")) }
}

#[cfg(not(windows))]
fn swap(new: &Path, target: &Path) -> Result<(), String> {
    fs::rename(new, target).map_err(|e| format!("couldn't replace {}: {e}", target.display()))
}

/// Windows can't overwrite a running program but can rename it, so the old file moves aside.
#[cfg(windows)]
fn swap(new: &Path, target: &Path) -> Result<(), String> {
    let old = target.with_extension("exe.old");
    let _ = fs::remove_file(&old);
    if fs::rename(target, &old).is_err() {
        return swap_elevated(new, target, &old);
    }
    fs::rename(new, target).or_else(|_| fs::copy(new, target).map(|_| ())).map_err(|e| {
        let _ = fs::rename(&old, target);
        format!("couldn't replace {}: {e}", target.display())
    })
}

/// The same move for a copy in a protected folder such as Program Files, through the administrator prompt.
#[cfg(windows)]
fn swap_elevated(new: &Path, target: &Path, old: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let (t, o, n) = (target.display(), old.display(), new.display());
    let cmd = format!("/c move /y \"{t}\" \"{o}\" && (copy /y \"{n}\" \"{t}\" || move /y \"{o}\" \"{t}\")");
    let ps = format!("Start-Process cmd -Verb RunAs -Wait -WindowStyle Hidden -ArgumentList '{}'", cmd.replace('\'', "''"));
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let _ = Command::new("powershell").args(["-NoProfile", "-NonInteractive", "-Command", &ps]).creation_flags(CREATE_NO_WINDOW).status();
    // the prompt's result doesn't reach us, so look at what's on disk
    match (fs::read(new), fs::read(target)) {
        (Ok(a), Ok(b)) if a == b => Ok(()),
        _ => Err(format!("{} wasn't replaced (administrator permission is needed)", target.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = r#"[
        {"tag_name":"v0.2.0-rc1","html_url":"https://x/rc-next","draft":true,"prerelease":true},
        {"tag_name":"v0.1.0-rc10","html_url":"https://x/rc10","draft":false,"prerelease":true},
        {"tag_name":"v0.1.0-rc3","html_url":"https://x/rc3","draft":false,"prerelease":true},
        {"tag_name":"v0.0.9","html_url":"https://x/old","draft":false,"prerelease":false,"assets":[
            {"name":"a.zip","size":5,"browser_download_url":"https://github.com/RamazanBerk20/SwiftSim/releases/download/v0.0.9/a.zip"},
            {"name":"evil.zip","size":5,"browser_download_url":"https://example.com/evil.zip"}
        ]},
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

    #[test]
    fn only_files_from_our_release_page_can_be_installed() {
        let names: Vec<String> = newest(LIST, "0.0.8").unwrap().unwrap().assets.into_iter().map(|a| a.name).collect();
        assert_eq!(names, ["a.zip"]);
    }

    /// Serves `body` to one HTTP request on localhost and returns its URL.
    #[cfg(unix)]
    fn serve_once(body: Vec<u8>) -> String {
        use std::io::BufRead;
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/f", l.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut r = std::io::BufReader::new(s.try_clone().unwrap());
            let mut line = String::new();
            while r.read_line(&mut line).unwrap() > 2 {
                line.clear();
            }
            write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
            s.write_all(&body).unwrap();
        });
        url
    }

    #[cfg(unix)]
    #[test]
    fn install_replaces_only_with_a_working_copy_of_the_right_version() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("swiftsim-update-test-{}", std::process::id()));
        let inner = dir.join("src/swiftsim-linux-x86_64");
        fs::create_dir_all(&inner).unwrap();
        let script = "#!/bin/sh\necho SwiftSim v9.9.9\n";
        fs::write(inner.join("swiftsim"), script).unwrap();
        fs::set_permissions(inner.join("swiftsim"), fs::Permissions::from_mode(0o755)).unwrap();
        let tgz = dir.join("a.tar.gz");
        assert!(Command::new("tar").arg("-czf").arg(&tgz).arg("-C").arg(dir.join("src")).arg("swiftsim-linux-x86_64").status().unwrap().success());
        let bytes = fs::read(&tgz).unwrap();
        let target = dir.join("app");
        fs::write(&target, "old").unwrap();
        let asset = |url, size| Asset { name: "swiftsim-linux-x86_64.tar.gz".into(), url, size };
        let p = AtomicU32::new(0);

        // the release's program reports another version: nothing changes
        assert!(install("v1.2.3", &asset(serve_once(bytes.clone()), bytes.len() as u64), &target, &p).is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "old");
        // a download that stops early: nothing changes
        assert!(install("v9.9.9", &asset(serve_once(bytes[..10].to_vec()), bytes.len() as u64), &target, &p).is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "old");

        install("v9.9.9", &asset(serve_once(bytes.clone()), bytes.len() as u64), &target, &p).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), script);
        assert_eq!(p.load(Ordering::Relaxed), 1000);
        assert!(!dir.join(STAGE).exists(), "stage folder is removed");
        fs::remove_dir_all(&dir).unwrap();
    }
}
