//! Updates: reads SwiftSim's release list on GitHub and installs a newer release in place.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

/// This build's version: the release tag for release builds (set by the release workflow),
/// otherwise Cargo's version (local and CI builds).
pub const VERSION: &str = match option_env!("SWIFTSIM_VERSION") {
    Some(v) if !v.is_empty() => v,
    _ => env!("CARGO_PKG_VERSION"),
};

const RELEASES: &str = "https://api.github.com/repos/RamazanBerk20/SwiftSim/releases?per_page=30";
const DOWNLOADS: &str = "https://github.com/RamazanBerk20/SwiftSim/releases/download/";
/// Prefix of the folder an install downloads into, `<prefix>-<pid>` next to the program.
const STAGE: &str = ".swiftsim-update";
/// The release's list of SHA-256 hashes (`sha256sum` format), made by `release.yml`.
const SUMS: &str = "SHA256SUMS";

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
    let (a, b, c) = (
        n.next()??,
        n.next().unwrap_or(Some(0))?,
        n.next().unwrap_or(Some(0))?,
    );
    let pn = pre.map_or(0, |p| {
        p.chars()
            .filter(char::is_ascii_digit)
            .collect::<String>()
            .parse()
            .unwrap_or(0)
    });
    Some((a, b, c, pre.is_none(), pn))
}

/// The newest release above `current` in GitHub's release list (JSON). Pre-releases count only
/// when `current` is one: testers hear about release candidates, stable users only about stable releases.
pub fn newest(json: &str, current: &str) -> Result<Option<Release>, String> {
    let list: Vec<serde_json::Value> =
        serde_json::from_str(json).map_err(|e| format!("unexpected reply from GitHub: {e}"))?;
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
                    let url = a["browser_download_url"]
                        .as_str()
                        .filter(|u| u.starts_with(DOWNLOADS))?;
                    Some(Asset {
                        name: a["name"].as_str()?.to_string(),
                        url: url.to_string(),
                        size: a["size"].as_u64()?,
                    })
                })
                .collect();
            Some((
                key(tag)?,
                Release {
                    tag: tag.to_string(),
                    url: r["html_url"].as_str()?.to_string(),
                    assets,
                },
            ))
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
    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .build(),
    );
    let mut r = agent
        .get(url)
        .header("User-Agent", format!("SwiftSim/{VERSION}"))
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| match e {
            ureq::Error::StatusCode(404) => {
                "the release list isn't public (the GitHub repository is private)".to_string()
            }
            ureq::Error::StatusCode(403 | 429) => {
                "GitHub's rate limit was reached; try again later".to_string()
            }
            e => format!("couldn't reach GitHub: {e}"),
        })?;
    r.body_mut()
        .read_to_string()
        .map_err(|e| format!("couldn't read GitHub's reply: {e}"))
}

/// The file this copy runs from: the AppImage itself, not the program inside its temporary mount.
pub fn self_path() -> PathBuf {
    std::env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_else(|| "swiftsim".into())
}

fn writable(target: &Path) -> bool {
    let probe = stage_dir(target);
    fs::create_dir_all(&probe).is_ok() && fs::remove_dir(&probe).is_ok()
}

/// This process's download folder next to `target`.
fn stage_dir(target: &Path) -> PathBuf {
    target.with_file_name(format!("{STAGE}-{}", std::process::id()))
}

/// Where the replaced program is kept until the new one has started once.
pub fn old_path(target: &Path) -> PathBuf {
    let mut p = target.as_os_str().to_owned();
    p.push(".old");
    p.into()
}

/// What "Install update" downloads and the file it replaces.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub asset: Asset,
    /// The release's `SHA256SUMS`, which must list `asset`.
    pub sums: Asset,
    pub target: PathBuf,
}

/// The release file to install for this copy and the path it replaces. `None` when SwiftSim shouldn't
/// replace itself: builds from source, copies a package manager owns or that the user can't write
/// (Windows can still ask for administrator rights), and releases without a `SHA256SUMS`.
pub fn plan(rel: &Release) -> Option<Plan> {
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
    Some(Plan {
        asset: rel.assets.iter().find(|a| a.name == name)?.clone(),
        sums: rel.assets.iter().find(|a| a.name == SUMS)?.clone(),
        target,
    })
}

/// Removes what earlier updates left behind: the replaced program and download folders no
/// running install holds. The app calls it only once it has run for twice `RESTART_GRACE`, so
/// "Restart now" can still put the old program back if this one fails before then.
pub fn cleanup() {
    let t = self_path();
    let _ = fs::remove_file(old_path(&t));
    let dirs = t
        .parent()
        .and_then(|d| fs::read_dir(d).ok())
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path());
    for d in dirs.filter(|d| {
        d.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with(STAGE))
    }) {
        if fs::File::open(d.join("lock")).is_ok_and(|l| l.try_lock().is_ok())
            || !d.join("lock").exists()
        {
            let _ = fs::remove_dir_all(&d);
        }
    }
}

/// Where the update check and install stand. One value at a time, so a check can't start under a
/// running install and a failure is a state the banner shows, not only a status line.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum State {
    #[default]
    Idle,
    /// Asking GitHub; `manual` checks report "up to date" and errors, automatic ones stay silent.
    Checking {
        manual: bool,
    },
    /// A newer release; `plan` is `None` when this copy can't replace itself.
    Available {
        rel: Release,
        plan: Option<Plan>,
    },
    Installing {
        rel: Release,
        plan: Plan,
        cancelling: bool,
    },
    /// "Restart now": the new version at `exe` was started and is watched until it runs.
    Restarting {
        exe: PathBuf,
    },
    Failed {
        msg: String,
    },
}

#[derive(Debug)]
pub enum Event {
    Check {
        manual: bool,
    },
    /// The check's answer, with the install plan for a newer release.
    Checked(Result<Option<(Release, Option<Plan>)>, String>),
    Install,
    Cancel,
    /// The install thread's answer.
    Installed(Result<(), String>),
    Dismiss,
    /// Start the installed version at `exe` and quit once it runs.
    Restart {
        exe: PathBuf,
    },
    /// The restarted version is running (`Ok`), or failed to start and the old one was put back.
    Started(Result<(), String>),
}

/// What the app does after a transition.
#[derive(Debug, PartialEq)]
pub enum Out {
    StartCheck,
    StartInstall {
        tag: String,
        plan: Plan,
    },
    /// Set the running install's cancel flag.
    CancelInstall,
    /// Installed: ask whether to restart into `exe`.
    AskRestart {
        tag: String,
        exe: PathBuf,
    },
    /// Status-line news.
    UpToDate,
    AlreadyInstalled(String),
    Found(String),
    Cancelled,
    /// Start the program at `exe`.
    Spawn {
        exe: PathBuf,
    },
    /// The new version runs: close this window.
    Quit,
}

#[derive(Default)]
pub struct Updater {
    pub state: State,
    /// Tag installed by this session (restart pending): later checks don't offer it again.
    pub installed: Option<String>,
}

impl Updater {
    /// The one transition function. Events that don't apply in the current state are ignored.
    pub fn on(&mut self, ev: Event) -> Option<Out> {
        let state = std::mem::take(&mut self.state);
        let (next, out) = match (state, ev) {
            // a check never starts under a running check or install
            (
                State::Idle | State::Failed { .. } | State::Available { .. },
                Event::Check { manual },
            ) => (State::Checking { manual }, Some(Out::StartCheck)),
            (State::Checking { manual }, Event::Checked(r)) => match r {
                Ok(Some((rel, _))) if self.installed.as_deref() == Some(rel.tag.as_str()) => (
                    State::Idle,
                    manual.then_some(Out::AlreadyInstalled(rel.tag)),
                ),
                Ok(Some((rel, plan))) => {
                    let tag = rel.tag.clone();
                    (State::Available { rel, plan }, Some(Out::Found(tag)))
                }
                Ok(None) => (State::Idle, manual.then_some(Out::UpToDate)),
                Err(msg) if manual => (State::Failed { msg }, None),
                Err(_) => (State::Idle, None),
            },
            (
                State::Available {
                    rel,
                    plan: Some(plan),
                },
                Event::Install,
            ) => {
                let out = Out::StartInstall {
                    tag: rel.tag.clone(),
                    plan: plan.clone(),
                };
                (
                    State::Installing {
                        rel,
                        plan,
                        cancelling: false,
                    },
                    Some(out),
                )
            }
            (
                State::Installing {
                    rel,
                    plan,
                    cancelling: false,
                },
                Event::Cancel,
            ) => (
                State::Installing {
                    rel,
                    plan,
                    cancelling: true,
                },
                Some(Out::CancelInstall),
            ),
            (
                State::Installing {
                    rel,
                    plan,
                    cancelling,
                },
                Event::Installed(r),
            ) => match r {
                Ok(()) => {
                    self.installed = Some(rel.tag.clone());
                    (
                        State::Idle,
                        Some(Out::AskRestart {
                            tag: rel.tag,
                            exe: plan.target,
                        }),
                    )
                }
                // cancelled: the release is still on offer
                Err(_) if cancelling => (
                    State::Available {
                        rel,
                        plan: Some(plan),
                    },
                    Some(Out::Cancelled),
                ),
                Err(msg) => (State::Failed { msg }, None),
            },
            (State::Available { .. } | State::Failed { .. }, Event::Dismiss) => (State::Idle, None),
            (
                State::Idle | State::Available { .. } | State::Failed { .. },
                Event::Restart { exe },
            ) => (
                State::Restarting { exe: exe.clone() },
                Some(Out::Spawn { exe }),
            ),
            (State::Restarting { exe }, Event::Started(Ok(()))) => {
                (State::Restarting { exe }, Some(Out::Quit))
            }
            (State::Restarting { .. }, Event::Started(Err(msg))) => (State::Failed { msg }, None),
            (s, _) => (s, None),
        };
        self.state = next;
        out
    }
}

/// Downloads `plan.asset` and the release's `SHA256SUMS`, checks the size and SHA-256 of the
/// download, and only then unpacks it, checks that it reports exactly version `tag` and replaces
/// `plan.target`, keeping the replaced program as `old_path` until the new one has started.
/// The running program keeps working; the new version starts with the next launch.
/// Blocking: run it off the UI thread. `progress` is in 1/1000; `cancel` stops the download.
pub fn install(
    tag: &str,
    plan: &Plan,
    progress: &AtomicU32,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let beside = stage_dir(&plan.target);
    let stage = if fs::create_dir_all(&beside).is_ok() {
        beside
    } else {
        std::env::temp_dir().join(format!("swiftsim-update-{}", std::process::id()))
    };
    fs::create_dir_all(&stage).map_err(|e| format!("couldn't create {}: {e}", stage.display()))?;
    // held while installing, so another window's cleanup() leaves this folder alone
    let lock = fs::File::create(stage.join("lock"))
        .map_err(|e| format!("couldn't create {}: {e}", stage.display()))?;
    let _ = lock.try_lock();
    let (asset, sums) = (&plan.asset, &plan.sums);
    let result = (|| {
        if sums.size > 1 << 16 {
            return Err(format!("{SUMS} of this release is too large"));
        }
        let list = download(sums, &stage.join(SUMS), &AtomicU32::new(0), cancel)?;
        let list = fs::read_to_string(&list.0)
            .map_err(|_| format!("{SUMS} of this release is not text"))?;
        let want = expected_sha256(&list, &asset.name).ok_or(format!(
            "{SUMS} of this release doesn't list {}",
            asset.name
        ))?;
        let (file, got) = download(asset, &stage.join(&asset.name), progress, cancel)?;
        if got != want {
            return Err(format!(
                "{} doesn't match the release's {SUMS} (SHA-256 {got}, expected {want})",
                asset.name
            ));
        }
        let new = unpack(&stage, &file, &asset.name)?;
        verify(&new, tag)?;
        swap(&new, &plan.target)
    })();
    drop(lock);
    let _ = fs::remove_dir_all(&stage);
    result
}

/// The SHA-256 (lowercase hex) that a `sha256sum` listing gives for file `name`.
fn expected_sha256(list: &str, name: &str) -> Option<String> {
    list.lines().find_map(|l| {
        let (hash, file) = l.trim_end().split_once(' ')?;
        let file = file.strip_prefix([' ', '*'])?;
        (file == name && hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| hash.to_ascii_lowercase())
    })
}

/// Downloads `asset` to `to`; returns the path and the SHA-256 (lowercase hex) of what arrived,
/// after checking that exactly `asset.size` bytes did.
fn download(
    asset: &Asset,
    to: &Path,
    progress: &AtomicU32,
    cancel: &AtomicBool,
) -> Result<(PathBuf, String), String> {
    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_global(Some(Duration::from_secs(30 * 60)))
            .build(),
    );
    let mut r = agent
        .get(&asset.url)
        .header("User-Agent", format!("SwiftSim/{VERSION}"))
        .call()
        .map_err(|e| format!("couldn't download {}: {e}", asset.name))?;
    let mut body = r.body_mut().as_reader();
    let mut file =
        fs::File::create(to).map_err(|e| format!("couldn't create {}: {e}", to.display()))?;
    let (mut buf, mut got) = (vec![0; 1 << 16], 0u64);
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(CANCELLED.into());
        }
        let n = body
            .read(&mut buf)
            .map_err(|e| format!("download of {} failed: {e}", asset.name))?;
        if n == 0 {
            break;
        }
        got += n as u64;
        if got > asset.size {
            break;
        }
        hash.update(&buf[..n]);
        file.write_all(&buf[..n])
            .map_err(|e| format!("couldn't write {}: {e}", to.display()))?;
        progress.store(
            (got * 1000 / asset.size.max(1)).min(1000) as u32,
            Ordering::Relaxed,
        );
    }
    if got != asset.size {
        return Err(format!(
            "download of {} has the wrong size ({got} instead of {} bytes)",
            asset.name, asset.size
        ));
    }
    file.sync_all()
        .map_err(|e| format!("couldn't write {}: {e}", to.display()))?;
    Ok((
        to.to_path_buf(),
        hash.finish()
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
    ))
}

/// The error text of an install stopped by its `cancel` flag.
pub const CANCELLED: &str = "update cancelled";

/// The new program inside the downloaded file (`tar` ships with Linux and Windows 10+, and reads .zip there).
fn unpack(stage: &Path, archive: &Path, name: &str) -> Result<PathBuf, String> {
    let tar = |flag: &str, members: &[&str]| -> Result<(), String> {
        let tool = std::env::var_os("SystemRoot").map_or("tar".into(), |r| {
            PathBuf::from(r).join("System32").join("tar.exe")
        });
        let st = Command::new(tool)
            .arg(flag)
            .arg(archive)
            .arg("-C")
            .arg(stage)
            .args(members)
            .status()
            .map_err(|e| format!("couldn't run tar: {e}"))?;
        if st.success() {
            Ok(())
        } else {
            Err(format!("couldn't unpack {name}"))
        }
    };
    let new = if name.ends_with(".tar.gz") {
        tar(
            "-xzf",
            &["--strip-components=1", "swiftsim-linux-x86_64/swiftsim"],
        )?;
        stage.join("swiftsim")
    } else if name.ends_with(".zip") {
        tar("-xf", &["swiftsim.exe"])?;
        stage.join("swiftsim.exe")
    } else {
        archive.to_path_buf()
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&new, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("couldn't make {} executable: {e}", new.display()))?;
    }
    Ok(new)
}

/// Runs only after the SHA-256 check: the new program's `--help` must start with `SwiftSim <tag>`.
fn verify(new: &Path, tag: &str) -> Result<(), String> {
    let out = Command::new(new)
        .arg("--help")
        .output()
        .map_err(|e| format!("the downloaded program doesn't start: {e}"))?;
    let out = String::from_utf8_lossy(&out.stdout);
    let mut first = out.lines().next().unwrap_or("").split_whitespace();
    if first.next() == Some("SwiftSim") && first.next() == Some(tag) {
        Ok(())
    } else {
        Err(format!("the downloaded program isn't SwiftSim {tag}"))
    }
}

/// The old program moves aside to `old_path` (kept until the new one has started once).
#[cfg(not(windows))]
fn swap(new: &Path, target: &Path) -> Result<(), String> {
    let old = old_path(target);
    let _ = fs::remove_file(&old);
    fs::rename(target, &old).map_err(|e| format!("couldn't replace {}: {e}", target.display()))?;
    fs::rename(new, target).map_err(|e| {
        let _ = fs::rename(&old, target);
        format!("couldn't replace {}: {e}", target.display())
    })
}

/// Puts the program kept by the last install back, after the new one failed to start.
pub fn rollback(target: &Path) -> Result<(), String> {
    let old = old_path(target);
    if !old.exists() {
        return Err(format!("the previous version ({}) is gone", old.display()));
    }
    let _ = fs::remove_file(target);
    fs::rename(&old, target).map_err(|e| format!("couldn't restore {}: {e}", target.display()))
}

/// Windows can't overwrite a running program but can rename it, so the old file moves aside.
#[cfg(windows)]
fn swap(new: &Path, target: &Path) -> Result<(), String> {
    let old = old_path(target);
    let _ = fs::remove_file(&old);
    if fs::rename(target, &old).is_err() {
        return swap_elevated(new, target, &old);
    }
    fs::rename(new, target)
        .or_else(|_| fs::copy(new, target).map(|_| ()))
        .map_err(|e| {
            let _ = fs::rename(&old, target);
            format!("couldn't replace {}: {e}", target.display())
        })
}

/// The same move for a copy in a protected folder such as Program Files, through the administrator prompt.
#[cfg(windows)]
fn swap_elevated(new: &Path, target: &Path, old: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let (t, o, n) = (target.display(), old.display(), new.display());
    let cmd = format!(
        "/c move /y \"{t}\" \"{o}\" && (copy /y \"{n}\" \"{t}\" || move /y \"{o}\" \"{t}\")"
    );
    let ps = format!(
        "Start-Process cmd -Verb RunAs -Wait -WindowStyle Hidden -ArgumentList '{}'",
        cmd.replace('\'', "''")
    );
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let _ = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &ps])
        .creation_flags(CREATE_NO_WINDOW)
        .status();
    // the prompt's result doesn't reach us, so look at what's on disk
    match (fs::read(new), fs::read(target)) {
        (Ok(a), Ok(b)) if a == b => Ok(()),
        _ => Err(format!(
            "{} wasn't replaced (administrator permission is needed)",
            target.display()
        )),
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
        let r = newest(&json, "0.0.1")
            .unwrap()
            .expect("egui has stable releases");
        assert!(
            r.tag.starts_with('0') && r.url.starts_with("https://github.com/emilk/egui/releases/"),
            "{r:?}"
        );
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
        assert_eq!(
            newest(LIST, "v0.1.0-rc3").unwrap().map(|r| r.tag),
            Some("v0.1.0-rc10".into())
        );
        assert_eq!(newest(LIST, "v0.1.0-rc10").unwrap(), None);
        // a stable build only sees stable releases
        assert_eq!(
            newest(LIST, "0.0.8").unwrap().map(|r| r.url),
            Some("https://x/old".into())
        );
        assert_eq!(newest(LIST, "0.1.0").unwrap(), None);
        assert!(newest("{\"message\":\"Not Found\"}", "0.1.0").is_err());
    }

    #[test]
    fn only_files_from_our_release_page_can_be_installed() {
        let names: Vec<String> = newest(LIST, "0.0.8")
            .unwrap()
            .unwrap()
            .assets
            .into_iter()
            .map(|a| a.name)
            .collect();
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
            write!(
                s,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            s.write_all(&body).unwrap();
        });
        url
    }

    #[cfg(unix)]
    #[test]
    fn install_replaces_only_with_a_verified_copy_of_the_right_version() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("swiftsim-update-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let ran = dir.join("ran");
        // a release tarball whose program reports `version` (and leaves a mark when it runs)
        let tarball = |version: &str| {
            let inner = dir.join("src/swiftsim-linux-x86_64");
            fs::create_dir_all(&inner).unwrap();
            let script = format!(
                "#!/bin/sh\ntouch '{}'\necho SwiftSim {version}\n",
                ran.display()
            );
            fs::write(inner.join("swiftsim"), &script).unwrap();
            fs::set_permissions(inner.join("swiftsim"), fs::Permissions::from_mode(0o755)).unwrap();
            let tgz = dir.join("a.tar.gz");
            assert!(
                Command::new("tar")
                    .arg("-czf")
                    .arg(&tgz)
                    .arg("-C")
                    .arg(dir.join("src"))
                    .arg("swiftsim-linux-x86_64")
                    .status()
                    .unwrap()
                    .success()
            );
            (fs::read(&tgz).unwrap(), script)
        };
        // the hash as sha256sum (not this program) computes it
        let sha = |b: &[u8]| {
            fs::write(dir.join("h"), b).unwrap();
            String::from_utf8(
                Command::new("sha256sum")
                    .arg(dir.join("h"))
                    .output()
                    .unwrap()
                    .stdout,
            )
            .unwrap()[..64]
                .to_string()
        };
        let name = "swiftsim-linux-x86_64.tar.gz";
        let target = dir.join("app");
        let p = AtomicU32::new(0);
        let no = AtomicBool::new(false);
        let plan = |body: Vec<u8>, size: usize, sums: String| Plan {
            asset: Asset {
                name: name.into(),
                url: serve_once(body),
                size: size as u64,
            },
            sums: Asset {
                name: SUMS.into(),
                url: serve_once(sums.clone().into_bytes()),
                size: sums.len() as u64,
            },
            target: target.clone(),
        };
        let (good, script) = tarball("v9.9.9");
        let sums = format!(
            "0000000000000000000000000000000000000000000000000000000000000000  other.zip\n{}  {name}\n",
            sha(&good)
        );
        fs::create_dir_all(&dir).unwrap();
        fs::write(&target, "old").unwrap();
        let unchanged = || {
            assert_eq!(fs::read_to_string(&target).unwrap(), "old");
            assert!(!old_path(&target).exists());
        };

        // bytes that don't match SHA256SUMS: never run, nothing changes
        let (evil, _) = tarball("v9.9.9");
        let evil = [evil, vec![0]].concat();
        let e = install(
            "v9.9.9",
            &plan(
                evil.clone(),
                evil.len(),
                format!("{}  {name}\n", sha(&good)),
            ),
            &p,
            &no,
        )
        .unwrap_err();
        assert!(e.contains("doesn't match"), "{e}");
        assert!(!ran.exists(), "an unverified download was executed");
        unchanged();
        // SHA256SUMS without this file
        let e = install(
            "v9.9.9",
            &plan(good.clone(), good.len(), "abc  other.zip\n".into()),
            &p,
            &no,
        )
        .unwrap_err();
        assert!(e.contains("doesn't list"), "{e}");
        unchanged();
        // a download that stops early
        assert!(
            install(
                "v9.9.9",
                &plan(good[..10].to_vec(), good.len(), sums.clone()),
                &p,
                &no
            )
            .is_err()
        );
        unchanged();
        // the release's program reports another version, or one the tag is only a prefix of
        let (long, _) = tarball("v9.9.90");
        let long_sums = format!("{}  {name}\n", sha(&long));
        assert!(
            install(
                "v9.9.9",
                &plan(long.clone(), long.len(), long_sums),
                &p,
                &no
            )
            .unwrap_err()
            .contains("isn't SwiftSim")
        );
        unchanged();
        let (good, script) = (tarball("v9.9.9").0, script);
        let sums = format!("{}  {name}\n", sha(&good));
        // a cancelled install stops without touching anything
        let e = install(
            "v9.9.9",
            &plan(good.clone(), good.len(), sums.clone()),
            &p,
            &AtomicBool::new(true),
        )
        .unwrap_err();
        assert_eq!(e, CANCELLED);
        unchanged();

        install("v9.9.9", &plan(good.clone(), good.len(), sums), &p, &no).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), script);
        assert_eq!(p.load(Ordering::Relaxed), 1000);
        // the replaced program stays until the new one has started; rollback puts it back
        assert_eq!(fs::read_to_string(old_path(&target)).unwrap(), "old");
        assert!(
            fs::read_dir(&dir)
                .unwrap()
                .flatten()
                .all(|e| !e.file_name().to_string_lossy().starts_with(STAGE)),
            "stage folder is removed"
        );
        rollback(&target).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "old");
        assert!(!old_path(&target).exists() && rollback(&target).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sha256sums_lines() {
        let h = "a".repeat(64);
        let list = format!(
            "{h}  SwiftSim-x86_64.AppImage\n{} *swiftsim.zip\r\nbad  x.zip\n",
            "B".repeat(64)
        );
        assert_eq!(expected_sha256(&list, "SwiftSim-x86_64.AppImage"), Some(h));
        assert_eq!(expected_sha256(&list, "swiftsim.zip"), Some("b".repeat(64)));
        assert_eq!(expected_sha256(&list, "x.zip"), None);
        assert_eq!(expected_sha256(&list, "SwiftSim"), None);
    }

    fn rel(tag: &str) -> Release {
        Release {
            tag: tag.into(),
            url: "u".into(),
            assets: vec![],
        }
    }

    fn plan_of(t: &str) -> Plan {
        let a = |n: &str| Asset {
            name: n.into(),
            url: "u".into(),
            size: 1,
        };
        Plan {
            asset: a("a"),
            sums: a(SUMS),
            target: t.into(),
        }
    }

    /// Every state against every event: what changes, and that the rest is ignored.
    #[test]
    fn update_state_transitions() {
        let states = || {
            vec![
                State::Idle,
                State::Checking { manual: true },
                State::Checking { manual: false },
                State::Available {
                    rel: rel("v2"),
                    plan: Some(plan_of("/x")),
                },
                State::Available {
                    rel: rel("v2"),
                    plan: None,
                },
                State::Installing {
                    rel: rel("v2"),
                    plan: plan_of("/x"),
                    cancelling: false,
                },
                State::Installing {
                    rel: rel("v2"),
                    plan: plan_of("/x"),
                    cancelling: true,
                },
                State::Failed { msg: "e".into() },
                State::Restarting { exe: "/x".into() },
            ]
        };
        let events = || {
            vec![
                Event::Check { manual: true },
                Event::Checked(Ok(Some((rel("v2"), Some(plan_of("/x")))))),
                Event::Checked(Ok(None)),
                Event::Checked(Err("net".into())),
                Event::Install,
                Event::Cancel,
                Event::Installed(Ok(())),
                Event::Installed(Err("disk".into())),
                Event::Dismiss,
                Event::Restart { exe: "/y".into() },
                Event::Started(Ok(())),
                Event::Started(Err("crash".into())),
            ]
        };
        use State as S;
        for (si, s) in states().into_iter().enumerate() {
            for (ei, ev) in events().into_iter().enumerate() {
                let mut u = Updater {
                    state: s.clone(),
                    installed: None,
                };
                let out = u.on(ev);
                let avail = || S::Available {
                    rel: rel("v2"),
                    plan: Some(plan_of("/x")),
                };
                let manual = matches!(s, S::Checking { manual: true });
                let (want, want_out) = match (si, ei) {
                    (0 | 3 | 4 | 7, 0) => (S::Checking { manual: true }, Some(Out::StartCheck)),
                    (1 | 2, 1) => (avail(), Some(Out::Found("v2".into()))),
                    (1 | 2, 2) => (S::Idle, manual.then_some(Out::UpToDate)),
                    (1, 3) => (S::Failed { msg: "net".into() }, None),
                    (2, 3) => (S::Idle, None), // automatic checks stay silent
                    (3, 4) => (
                        S::Installing {
                            rel: rel("v2"),
                            plan: plan_of("/x"),
                            cancelling: false,
                        },
                        Some(Out::StartInstall {
                            tag: "v2".into(),
                            plan: plan_of("/x"),
                        }),
                    ),
                    (5, 5) => (
                        S::Installing {
                            rel: rel("v2"),
                            plan: plan_of("/x"),
                            cancelling: true,
                        },
                        Some(Out::CancelInstall),
                    ),
                    (5 | 6, 6) => (
                        S::Idle,
                        Some(Out::AskRestart {
                            tag: "v2".into(),
                            exe: "/x".into(),
                        }),
                    ),
                    (5, 7) => (S::Failed { msg: "disk".into() }, None),
                    (6, 7) => (avail(), Some(Out::Cancelled)),
                    (3 | 4 | 7, 8) => (S::Idle, None),
                    (0 | 3 | 4 | 7, 9) => (
                        S::Restarting { exe: "/y".into() },
                        Some(Out::Spawn { exe: "/y".into() }),
                    ),
                    (8, 10) => (s.clone(), Some(Out::Quit)),
                    (8, 11) => (
                        S::Failed {
                            msg: "crash".into(),
                        },
                        None,
                    ),
                    _ => (s.clone(), None),
                };
                assert_eq!(
                    (&u.state, &out),
                    (&want, &want_out),
                    "state {si} event {ei}"
                );
                assert_eq!(
                    u.installed.is_some(),
                    matches!((si, ei), (5 | 6, 6)),
                    "state {si} event {ei}"
                );
            }
        }
        // after an install whose restart was put off, a check doesn't offer the same release again
        let mut u = Updater {
            state: State::Checking { manual: true },
            installed: Some("v2".into()),
        };
        assert_eq!(
            u.on(Event::Checked(Ok(Some((rel("v2"), None))))),
            Some(Out::AlreadyInstalled("v2".into()))
        );
        assert_eq!(u.state, State::Idle);
        u.state = State::Checking { manual: false };
        assert_eq!(
            u.on(Event::Checked(Ok(Some((rel("v3"), None))))),
            Some(Out::Found("v3".into()))
        );
    }
}
