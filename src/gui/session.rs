//! Everyday safety: crash-recovery copies of the open document and the recent-files list,
//! both kept in the app's config folder (`config_dir` in mod.rs).

use super::schematic::Schematic;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

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
    File::options().read(true).write(true).create(true).truncate(false).open(json.with_extension("lock"))
}

impl Recovery {
    /// Lists copies left by instances that are gone (newest first), then claims this instance's slot.
    pub fn start(dir: &Path) -> (Recovery, Vec<PathBuf>) {
        let mut orphans: Vec<(std::time::SystemTime, PathBuf)> = fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("recovery-") && n.ends_with(".json")))
            .filter(|p| lock_file(p).is_ok_and(|l| l.try_lock().is_ok()))
            .filter_map(|p| Some((p.metadata().ok()?.modified().ok()?, p)))
            .collect();
        orphans.sort_by_key(|o| std::cmp::Reverse(o.0));
        let file = dir.join(format!("recovery-{}.json", std::process::id()));
        let lock = fs::create_dir_all(dir).and_then(|_| lock_file(&file)).ok().filter(|l| l.try_lock().is_ok());
        (Recovery { file, lock, written: false }, orphans.into_iter().map(|(_, p)| p).collect())
    }

    /// Writes via a temp file + rename, so a crash mid-write keeps the previous copy intact.
    pub fn write(&mut self, s: &Snapshot) -> io::Result<()> {
        let tmp = self.file.with_extension("tmp");
        fs::write(&tmp, serde_json::to_vec(s)?)?;
        fs::rename(&tmp, &self.file)?;
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

const RECENT: usize = 10;

/// Recently used files, newest first, skipping ones that no longer exist.
pub fn recent(dir: &Path) -> Vec<PathBuf> {
    fs::read_to_string(dir.join("recent.txt")).unwrap_or_default().lines().map(PathBuf::from).filter(|p| p.exists()).take(RECENT).collect()
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
    let _ = fs::create_dir_all(dir).and_then(|_| fs::write(dir.join("recent.txt"), text));
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
        Snapshot { path: Some("/work/amp.cir".into()), doc: Doc::Netlist(net.into()) }
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
        assert_eq!((net(&s), s.path.as_deref()), ("lost work", Some(Path::new("/work/amp.cir"))));
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
}
