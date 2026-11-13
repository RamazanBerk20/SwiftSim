//! Everyday safety: crash-recovery copies of the open document and the recent-files list,
//! both kept in the app's config folder (`config_dir` in mod.rs).

use super::schematic::Schematic;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, TryLockError};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// A recovery copy: the unsaved document and the file it belongs to.
#[derive(Serialize, Deserialize)]
pub struct Snapshot {
    pub path: Option<PathBuf>,
    pub doc: Doc,
}

#[derive(Serialize, Deserialize)]
pub enum Doc {
    Schematic(Schematic),
    Netlist(String),
}

/// This instance's recovery copy, `recovery-<pid>.json`. Each instance holds a lock on its
/// `recovery-<pid>.lock` while it runs, so a copy whose lock is free was left behind by a crash.
/// The copy never goes anywhere near the user's own file.
pub struct Recovery {
    file: PathBuf,
    lock: Option<File>,
    written: bool,
}

fn lock_file(json: &Path) -> io::Result<File> {
    File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(json.with_extension("lock"))
}

/// A running instance rewrites its copy every `AUTOSAVE` (30 s) while it has unsaved changes,
/// so where locks don't work, a copy this old was left by an instance that is gone.
const STALE: Duration = Duration::from_secs(120);

/// Whether the instance owning `json` (a `recovery-`/`window-<pid>.json`) is gone. Where the file
/// system can't lock (some network mounts) a copy counts as gone once it is `STALE`.
fn gone(json: &Path, lock_json: &Path) -> bool {
    let Ok(l) = lock_file(lock_json) else {
        return false;
    };
    match l.try_lock() {
        Ok(()) => true,
        Err(TryLockError::WouldBlock) => false,
        Err(TryLockError::Error(_)) => json
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|t| t.elapsed().is_ok_and(|age| age > STALE)),
    }
}

impl Recovery {
    /// Lists copies left by instances that are gone (newest first), then claims this instance's slot.
    pub fn start(dir: &Path) -> (Recovery, Vec<PathBuf>) {
        let mut orphans: Vec<(SystemTime, PathBuf)> = fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("recovery-") && n.ends_with(".json"))
            })
            .filter(|p| gone(p, p))
            .filter_map(|p| Some((p.metadata().ok()?.modified().ok()?, p)))
            .collect();
        orphans.sort_by_key(|o| std::cmp::Reverse(o.0));
        let file = dir.join(format!("recovery-{}.json", std::process::id()));
        let lock = fs::create_dir_all(dir)
            .and_then(|_| lock_file(&file))
            .ok()
            .filter(|l| l.try_lock().is_ok());
        (
            Recovery {
                file,
                lock,
                written: false,
            },
            orphans.into_iter().map(|(_, p)| p).collect(),
        )
    }

    /// False where the config folder can't hold a lock: crash recovery then goes by file age.
    pub fn locked(&self) -> bool {
        self.lock.is_some()
    }

    /// Writes via a temp file + rename, so a crash mid-write keeps the previous copy intact.
    pub fn write(&mut self, s: &Snapshot) -> io::Result<()> {
        atomic_write(&self.file, &serde_json::to_vec(s)?)?;
        self.written = true;
        Ok(())
    }

    /// Drops this instance's copy (the document is clean again).
    pub fn remove(&mut self) {
        if std::mem::take(&mut self.written) {
            let _ = fs::remove_file(&self.file);
        }
    }

    /// Takes over a copy left by a crash: moves it into this instance's slot and returns its contents.
    pub fn adopt(&mut self, orphan: &Path) -> io::Result<Snapshot> {
        let s = read(orphan)?;
        fs::rename(orphan, &self.file)?;
        self.written = true;
        self.forget_lock(orphan);
        Ok(s)
    }

    /// Deletes a copy left by a crash.
    pub fn discard(&self, orphan: &Path) {
        let _ = fs::remove_file(orphan);
        self.forget_lock(orphan);
    }

    fn forget_lock(&self, orphan: &Path) {
        // a reused pid can make the orphan's lock file our own live one
        if orphan != self.file {
            let _ = fs::remove_file(orphan.with_extension("lock"));
        }
    }

    /// Clean exit. Never runs on a panic or a kill, so the copy survives those.
    pub fn close(&mut self) {
        self.remove();
        self.lock = None; // Windows can't delete a file that is still open
        let _ = fs::remove_file(self.file.with_extension("lock"));
    }
}

pub fn read(file: &Path) -> io::Result<Snapshot> {
    Ok(serde_json::from_slice(&fs::read(file)?)?)
}

/// Replaces `path` with `data` so that a failure at any point leaves the previous file intact:
/// writes a temp file in the same folder, flushes it to disk, then renames it over `path`.
/// A symlink is followed (its target is replaced); a read-only file stays an error, as with `fs::write`.
/// Where the folder is read-only but the file is writable, the file is written in place (as Save did
/// before), since no temp file can be made next to it.
pub fn atomic_write(path: &Path, data: &[u8]) -> io::Result<()> {
    let path = if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        fs::canonicalize(path)?
    } else {
        path.to_path_buf()
    };
    let old = fs::metadata(&path).ok();
    if old.as_ref().is_some_and(|m| m.permissions().readonly()) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the file is read-only",
        ));
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{}.tmp", std::process::id()));
    let tmp = PathBuf::from(tmp);
    let f = match File::create(&tmp) {
        Err(e)
            if old.is_some()
                && matches!(
                    e.kind(),
                    io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem
                ) =>
        {
            return File::options()
                .write(true)
                .truncate(true)
                .open(&path)
                .and_then(|mut f| f.write_all(data).and_then(|()| f.sync_all()))
                .map_err(|_| e);
        }
        f => f,
    };
    let r = (|| {
        let mut f = f?;
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        if let Some(m) = &old {
            fs::set_permissions(&tmp, m.permissions())?;
        }
        fs::rename(&tmp, &path)
    })();
    if r.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    r
}

/// This window's state, `window-<pid>.json` in the config folder, for `swiftsim --mcp`'s
/// `gui_status` tool. The window's recovery lock tells whether it still runs.
pub struct WindowStatus {
    file: PathBuf,
    last: String,
    written: Option<std::time::Instant>,
}

/// Status writes are at least this far apart, so a run's progress doesn't keep the disk busy.
const STATUS_EVERY: Duration = Duration::from_millis(500);
/// The status file is rewritten this often even unchanged, so where locks don't work its age
/// (`STALE`) still tells a running window from a crashed one.
const STATUS_HEARTBEAT: Duration = Duration::from_secs(30);

impl WindowStatus {
    pub fn new(dir: &Path) -> WindowStatus {
        WindowStatus {
            file: dir.join(format!("window-{}.json", std::process::id())),
            last: String::new(),
            written: None,
        }
    }

    /// Writes `v` when it changed (at most every `STATUS_EVERY`) or on the heartbeat; returns when
    /// to call again. A status hint, not user data: replaced whole but not flushed to disk.
    pub fn update(&mut self, v: &serde_json::Value) -> Duration {
        let text = v.to_string();
        let since = self.written.map_or(Duration::MAX, |t| t.elapsed());
        let due = if text != self.last {
            STATUS_EVERY
        } else {
            STATUS_HEARTBEAT
        };
        if since < due {
            return due - since;
        }
        if write_private(&self.file, text.as_bytes()).is_ok() {
            self.last = text;
        }
        self.written = Some(std::time::Instant::now());
        STATUS_HEARTBEAT
    }

    pub fn close(&self) {
        let _ = fs::remove_file(&self.file);
    }
}

/// States of the SwiftSim windows that are running (stale files of crashed ones are removed).
pub fn windows(dir: &Path) -> Vec<serde_json::Value> {
    let mut out = vec![];
    for p in fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
    {
        let Some(pid) = p
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_prefix("window-")?.strip_suffix(".json"))
        else {
            continue;
        };
        if gone(&p, &dir.join(format!("recovery-{pid}.json"))) {
            let _ = fs::remove_file(&p);
            continue;
        }
        if let Some(v) = fs::read(&p)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
        {
            out.push(v);
        }
    }
    out
}

/// Requests from `swiftsim --mcp` to a window, and the window's answers, are files in a folder
/// only this user can use: `$XDG_RUNTIME_DIR/swiftsim` (else `ipc` in the config folder), mode
/// 0700, files 0600. On Windows it is `ipc` in `%APPDATA%\SwiftSim`, which only the user's own
/// account can open (the profile's default ACL). Nothing listens on a network port.
pub fn ipc_dir(config: &Path) -> io::Result<PathBuf> {
    private_dir(
        std::env::var_os("XDG_RUNTIME_DIR")
            .filter(|d| !d.is_empty() && cfg!(unix))
            .map_or_else(|| config.join("ipc"), |d| PathBuf::from(d).join("swiftsim")),
    )
}

/// `dir`, created if needed, after making sure it is a real folder only this user can open.
fn private_dir(dir: PathBuf) -> io::Result<PathBuf> {
    fs::create_dir_all(&dir)?;
    let m = fs::symlink_metadata(&dir)?;
    if !m.is_dir() {
        return Err(io::Error::other(format!(
            "{} is not a folder",
            dir.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // chmod only succeeds for the folder's owner, so this also proves it is ours
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        if fs::metadata(&dir)?.permissions().mode() & 0o077 != 0 {
            return Err(io::Error::other(format!(
                "{} is open to other users",
                dir.display()
            )));
        }
    }
    Ok(dir)
}

/// Writes `data` to `path` readable by this user only, whole or not at all (temp file + rename).
pub fn write_private(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let mut o = File::options();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    let r = o
        .open(&tmp)
        .and_then(|mut f| f.write_all(data))
        .and_then(|()| fs::rename(&tmp, path));
    if r.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    r
}

/// A request to the window of `pid`: `req-<pid>-<id>.json`; its answer is `resp-<pid>-<id>.json`.
fn request_file(dir: &Path, pid: u32, id: &str, answer: bool) -> PathBuf {
    dir.join(format!(
        "{}-{pid}-{id}.json",
        if answer { "resp" } else { "req" }
    ))
}

/// Milliseconds since 1970: request deadlines, which the window and the MCP server compare.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Whether request `req` may still be carried out: its `deadline_ms` has not passed.
pub fn live_request(req: &serde_json::Value) -> bool {
    req["deadline_ms"].as_u64().is_some_and(|d| now_ms() <= d)
}

/// How long `ask` keeps waiting after a request's deadline, for an answer the window wrote in time.
const ANSWER_GRACE: Duration = Duration::from_secs(2);

/// The MCP side: sends `req` ({"op", "args"}) to the window of `pid` and waits for its answer.
/// The window carries out the request only before its deadline (now + `timeout`), so a caller
/// that gave up is never acted for later.
pub fn ask(
    dir: &Path,
    pid: u32,
    req: &serde_json::Value,
    timeout: Duration,
    cancelled: impl Fn() -> bool,
) -> Result<serde_json::Value, String> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = format!(
        "{}x{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let (q, a) = (
        request_file(dir, pid, &id, false),
        request_file(dir, pid, &id, true),
    );
    let mut req = req.clone();
    req["deadline_ms"] = (now_ms() + timeout.as_millis() as u64).into();
    write_private(&q, req.to_string().as_bytes())
        .map_err(|e| format!("couldn't reach the window: {e}"))?;
    let end = std::time::Instant::now() + timeout + ANSWER_GRACE;
    while std::time::Instant::now() < end && !cancelled() {
        if let Ok(b) = fs::read(&a) {
            let _ = fs::remove_file(&a);
            let v: serde_json::Value = serde_json::from_slice(&b)
                .map_err(|e| format!("bad answer from the window: {e}"))?;
            return match v.get("error").and_then(|e| e.as_str()) {
                Some(e) => Err(e.to_string()),
                None => Ok(v["ok"].clone()),
            };
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    let _ = fs::remove_file(&q);
    let _ = fs::remove_file(&a);
    Err("the SwiftSim window did not answer in time; the request was not carried out".into())
}

/// The window side: a thread that hands this window's requests (id, request) to `tx` and calls
/// `wake` so the UI looks at them. It stops when the receiver is gone.
pub fn listen(
    dir: PathBuf,
    tx: std::sync::mpsc::Sender<(String, serde_json::Value)>,
    wake: impl Fn() + Send + 'static,
) {
    forget_requests(&dir); // left by an earlier window that had this pid
    let prefix = format!("req-{}-", std::process::id());
    let _ = std::thread::Builder::new()
        .name("mcp-inbox".into())
        .spawn(move || {
            loop {
                for p in fs::read_dir(&dir)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|e| e.path())
                {
                    let Some(id) = p
                        .file_name()
                        .and_then(|n| n.to_str())
                        .and_then(|n| n.strip_prefix(&prefix)?.strip_suffix(".json"))
                        .map(str::to_string)
                    else {
                        continue;
                    };
                    let req = fs::read(&p)
                        .ok()
                        .and_then(|b| serde_json::from_slice(&b).ok());
                    let _ = fs::remove_file(&p);
                    if let Some(req) = req {
                        if tx.send((id, req)).is_err() {
                            return;
                        }
                        wake();
                    }
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        });
}

/// Removes this window's request and answer files (at start and at exit).
pub fn forget_requests(dir: &Path) {
    let pid = std::process::id();
    for p in fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
    {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name
            .strip_prefix("req-")
            .or_else(|| name.strip_prefix("resp-"))
            .is_some_and(|r| r.starts_with(&format!("{pid}-")))
        {
            let _ = fs::remove_file(&p);
        }
    }
}

/// The window's answer to request `id`.
pub fn answer(dir: &Path, id: &str, r: Result<serde_json::Value, String>) {
    let v = match r {
        Ok(v) => serde_json::json!({"ok": v}),
        Err(e) => serde_json::json!({"error": e}),
    };
    let _ = write_private(
        &request_file(dir, std::process::id(), id, true),
        v.to_string().as_bytes(),
    );
}

const RECENT: usize = 10;

/// Recently used files, newest first, skipping ones that no longer exist.
pub fn recent(dir: &Path) -> Vec<PathBuf> {
    fs::read_to_string(dir.join("recent.txt"))
        .unwrap_or_default()
        .lines()
        .map(PathBuf::from)
        .filter(|p| p.exists())
        .take(RECENT)
        .collect()
}

/// Moves `p` to the front of the list. Re-read on every call, so several open windows share one list.
pub fn add_recent(dir: &Path, p: &Path) {
    // not canonicalize: on Windows that gives \\?\ paths, which break relative .include lookups
    let p = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    let mut v = recent(dir);
    v.retain(|q| *q != p);
    v.insert(0, p);
    v.truncate(RECENT);
    let text: String = v.iter().map(|q| format!("{}\n", q.display())).collect();
    let _ = fs::create_dir_all(dir)
        .and_then(|_| atomic_write(&dir.join("recent.txt"), text.as_bytes()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("swiftsim-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn snap(net: &str) -> Snapshot {
        Snapshot {
            path: Some("/work/amp.cir".into()),
            doc: Doc::Netlist(net.into()),
        }
    }

    fn net(s: &Snapshot) -> &str {
        match &s.doc {
            Doc::Netlist(t) => t,
            Doc::Schematic(_) => panic!("expected a netlist"),
        }
    }

    #[test]
    fn recovery_lifecycle() {
        let d = tmp("rec");
        let (mut r, orphans) = Recovery::start(&d);
        assert!(orphans.is_empty());
        r.write(&snap("v1")).unwrap();
        r.write(&snap("v2")).unwrap();
        assert_eq!(net(&read(&r.file).unwrap()), "v2");
        // our own live copy is not offered to another instance
        assert!(Recovery::start(&d).1.is_empty());
        // a clean save removes it
        r.remove();
        assert!(!r.file.exists());

        // a crashed instance leaves a copy whose lock nobody holds
        let dead = d.join("recovery-4000000000.json");
        fs::write(&dead, serde_json::to_vec(&snap("lost work")).unwrap()).unwrap();
        let (mut r2, orphans) = Recovery::start(&d);
        assert_eq!(orphans, vec![dead.clone()]);
        let s = r2.adopt(&dead).unwrap();
        assert_eq!(
            (net(&s), s.path.as_deref()),
            ("lost work", Some(Path::new("/work/amp.cir")))
        );
        assert!(!dead.exists() && read(&r2.file).is_ok());

        // discard deletes it for good; a clean exit leaves nothing behind
        fs::write(&dead, b"{}").unwrap();
        r2.discard(&dead);
        assert!(!dead.exists());
        r2.close();
        r.close();
        assert_eq!(fs::read_dir(&d).unwrap().count(), 0);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn recent_files() {
        let d = tmp("recent");
        let f: Vec<PathBuf> = (0..12).map(|i| d.join(format!("c{i}.sws"))).collect();
        for p in &f {
            fs::write(p, "{}").unwrap();
            add_recent(&d, p);
        }
        let r = recent(&d);
        assert_eq!(r.len(), 10);
        assert_eq!(r[0], f[11]);
        add_recent(&d, &f[5]); // re-opening moves it to the top without duplicating it
        let r = recent(&d);
        assert_eq!((r.len(), &r[0], &r[1]), (10, &f[5], &f[11]));
        assert_eq!(r.iter().filter(|p| **p == f[5]).count(), 1);
        fs::remove_file(&f[11]).unwrap(); // missing files are skipped
        assert!(!recent(&d).contains(&f[11]));
        let _ = fs::remove_dir_all(&d);
    }

    /// Save never writes into the user's file: the old contents stay whole until the new file is
    /// complete (a second hard link to the old file still sees them), and a failed write leaves
    /// the file and the folder as they were.
    #[cfg(unix)]
    #[test]
    fn atomic_write_replaces_instead_of_truncating() {
        let d = tmp("atomic");
        let f = d.join("c.sws");
        fs::write(&f, "old").unwrap();
        fs::hard_link(&f, d.join("link")).unwrap();
        atomic_write(&f, b"new").unwrap();
        assert_eq!(fs::read_to_string(&f).unwrap(), "new");
        assert_eq!(
            fs::read_to_string(d.join("link")).unwrap(),
            "old",
            "the old file was overwritten in place"
        );
        // a read-only folder: no temp file can be made, so a writable file is written in place,
        // and a new file can't be made at all
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&d, fs::Permissions::from_mode(0o555)).unwrap();
        let r = atomic_write(&f, b"newer");
        let fresh = atomic_write(&d.join("new.sws"), b"x");
        fs::set_permissions(&d, fs::Permissions::from_mode(0o755)).unwrap();
        r.unwrap();
        assert_eq!(fs::read_to_string(&f).unwrap(), "newer");
        assert_eq!(
            fs::read_dir(&d).unwrap().count(),
            2 + fresh.is_ok() as usize,
            "temp file left behind"
        ); // (root can write anyway)
        // symlinks are followed, read-only files refused
        std::os::unix::fs::symlink(&f, d.join("sym")).unwrap();
        atomic_write(&d.join("sym"), b"via link").unwrap();
        assert_eq!(fs::read_to_string(&f).unwrap(), "via link");
        assert!(
            fs::symlink_metadata(d.join("sym"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        fs::set_permissions(&f, fs::Permissions::from_mode(0o444)).unwrap();
        assert!(atomic_write(&f, b"x").is_err());
        let _ = fs::remove_dir_all(&d);
    }

    /// A request reaches this process's window and its answer comes back; the folder and files are
    /// the user's only.
    #[test]
    fn window_requests_round_trip() {
        let d = tmp("ipc");
        let dir = private_dir(d.join("ipc")).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        listen(dir.clone(), tx, || {});
        let dir2 = dir.clone();
        let window = std::thread::spawn(move || {
            let (id, req) = rx.recv().unwrap();
            assert_eq!(req["op"], "ping");
            assert!(live_request(&req), "{req}");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    fs::metadata(&dir2).unwrap().permissions().mode() & 0o777,
                    0o700
                );
            }
            answer(&dir2, &id, Ok(serde_json::json!({"pong": 1})));
            let (id, _) = rx.recv().unwrap();
            answer(&dir2, &id, Err("refused".into()));
        });
        let ask = |op: &str| {
            ask(
                &dir,
                std::process::id(),
                &serde_json::json!({"op": op}),
                Duration::from_secs(10),
                || false,
            )
        };
        assert!(live_request(
            &serde_json::json!({"deadline_ms": now_ms() + 1000})
        ));
        assert!(
            !live_request(&serde_json::json!({"deadline_ms": now_ms() - 1}))
                && !live_request(&serde_json::json!({}))
        );
        assert_eq!(ask("ping"), Ok(serde_json::json!({"pong": 1})));
        assert_eq!(ask("x"), Err("refused".into()));
        window.join().unwrap();
        // nobody answers: a timeout, and the request is taken back
        let r = super::ask(
            &dir,
            4_000_000_000,
            &serde_json::json!({}),
            Duration::from_millis(100),
            || false,
        );
        assert!(r.is_err() && fs::read_dir(&dir).unwrap().count() == 0);
        // leftovers of this pid go; other windows' files stay
        for f in [
            "req-4000000000-1.json",
            &format!("req-{}-1.json", std::process::id()),
            &format!("resp-{}-2.json", std::process::id()),
        ] {
            fs::write(dir.join(f), "{}").unwrap();
        }
        forget_requests(&dir);
        assert_eq!(
            fs::read_dir(&dir)
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect::<Vec<_>>(),
            ["req-4000000000-1.json"]
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn window_status_is_listed_while_the_window_runs() {
        let d = tmp("window");
        let (mut rec, _) = Recovery::start(&d);
        let mut w = WindowStatus::new(&d);
        assert_eq!(
            w.update(&serde_json::json!({"simulation": {"state": "running"}})),
            STATUS_HEARTBEAT
        );
        assert_eq!(
            windows(&d),
            [serde_json::json!({"simulation": {"state": "running"}})]
        );
        // a change right after a write waits its turn; the next call after that writes it
        let wait = w.update(&serde_json::json!({"simulation": {"state": "finished"}}));
        assert!(wait > Duration::ZERO && wait <= STATUS_EVERY);
        assert_eq!(windows(&d)[0]["simulation"]["state"], "running");
        std::thread::sleep(wait);
        w.update(&serde_json::json!({"simulation": {"state": "finished"}}));
        assert_eq!(windows(&d)[0]["simulation"]["state"], "finished");
        // a window that crashed (its lock is free) is not listed, and its file goes
        let dead = d.join("window-4000000000.json");
        fs::write(&dead, "{}").unwrap();
        assert_eq!(windows(&d).len(), 1);
        assert!(!dead.exists());
        w.close();
        rec.close();
        assert!(windows(&d).is_empty());
        let _ = fs::remove_dir_all(&d);
    }
}
