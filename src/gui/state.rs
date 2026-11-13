//! The app's state machines that need no egui: the open document, the simulation job and the
//! modal dialogs. Each has one place that changes it, so the UI draws one thing per state.
//! (The update check lives in `update::Updater`, the canvas tools in `editor::Tool`.)

use super::session::Snapshot;
use std::collections::VecDeque;
use std::path::PathBuf;

#[derive(PartialEq, Clone, Copy, Debug)]
pub enum Mode {
    Schematic,
    Netlist,
}

/// How the open document came to be.
#[derive(PartialEq, Clone, Copy, Debug)]
pub enum Origin {
    New,
    Example,
    Opened,
    /// From an LTspice `.asc`; saves as `.sws` next to it.
    Imported,
    /// From a crash-recovery copy: unsaved until saved.
    Restored,
    /// A schematic's netlist opened as text: unsaved until saved.
    Converted,
}

/// The open document. Dirty is computed from the content's edit version, never remembered.
#[derive(Debug)]
pub struct Document {
    pub mode: Mode,
    pub path: Option<PathBuf>,
    pub origin: Origin,
    /// Edit version at the last load or save; `None` when the content was never saved as it is.
    saved: Option<u64>,
    /// Bumped by every document switch; a simulation belongs to the generation that started it.
    pub generation: u64,
}

impl Document {
    pub fn new() -> Document {
        Document {
            mode: Mode::Schematic,
            path: None,
            origin: Origin::New,
            saved: Some(0),
            generation: 0,
        }
    }

    /// Another document is open now; `version` is its content's edit version.
    pub fn replace(&mut self, mode: Mode, path: Option<PathBuf>, origin: Origin, version: u64) {
        let saved = (!matches!(origin, Origin::Restored | Origin::Converted)).then_some(version);
        *self = Document {
            mode,
            path,
            origin,
            saved,
            generation: self.generation + 1,
        };
    }

    pub fn dirty(&self, version: u64) -> bool {
        self.saved != Some(version)
    }

    /// Written to `path` at edit `version`.
    pub fn saved_as(&mut self, path: PathBuf, version: u64) {
        self.path = Some(path);
        self.saved = Some(version);
    }

    pub fn origin_name(&self) -> &'static str {
        match self.origin {
            Origin::New => "new",
            Origin::Example => "example",
            Origin::Opened => "opened",
            Origin::Imported => "imported",
            Origin::Restored => "restored",
            Origin::Converted => "converted",
        }
    }
}

/// A running simulation's handle: stopping it must not wait for it.
pub trait Cancel {
    fn cancel(&self);
}

/// The simulation job. Results reach the document whose generation started the run, or nobody.
#[derive(Debug, PartialEq)]
pub enum Sim<J> {
    Idle,
    Running {
        doc: u64,
        job: J,
    },
    /// Stop was pressed; the engine stops at its next check.
    Cancelling {
        doc: u64,
        job: J,
    },
    Finished {
        doc: u64,
        secs: f64,
    },
    Failed {
        doc: u64,
        msg: String,
    },
    Cancelled {
        doc: u64,
    },
}

pub enum SimEvent<J> {
    Start {
        doc: u64,
        job: J,
    },
    Cancel,
    /// The run of document generation `doc` ended (a thread that died reports `Err` too).
    Done {
        doc: u64,
        result: Result<f64, String>,
    },
    /// The document was replaced: a running job is stopped and its result dropped.
    DocChanged,
}

impl<J: Cancel> Sim<J> {
    /// The one transition function. Returns true when a `Done` result belongs to the open
    /// document and should be shown; a `Start` while busy stops the new job and is ignored.
    pub fn on(&mut self, ev: SimEvent<J>) -> bool {
        let state = std::mem::replace(self, Sim::Idle);
        let (next, deliver) = match (state, ev) {
            (
                s @ (Sim::Running { .. } | Sim::Cancelling { .. }),
                SimEvent::Start { job: new, .. },
            ) => {
                new.cancel();
                (s, false)
            }
            (_, SimEvent::Start { doc, job }) => (Sim::Running { doc, job }, false),
            (Sim::Running { doc, job }, SimEvent::Cancel) => {
                job.cancel();
                (Sim::Cancelling { doc, job }, false)
            }
            (Sim::Running { doc, .. }, SimEvent::Done { doc: d, result }) if d == doc => {
                match result {
                    Ok(secs) => (Sim::Finished { doc, secs }, true),
                    Err(msg) => (Sim::Failed { doc, msg }, true),
                }
            }
            // a run that ends after Stop: a finished one still counts, an error is the cancel
            (Sim::Cancelling { doc, .. }, SimEvent::Done { doc: d, result }) if d == doc => {
                match result {
                    Ok(secs) => (Sim::Finished { doc, secs }, true),
                    Err(_) => (Sim::Cancelled { doc }, false),
                }
            }
            (Sim::Running { job, .. } | Sim::Cancelling { job, .. }, SimEvent::DocChanged) => {
                job.cancel();
                (Sim::Idle, false)
            }
            (_, SimEvent::DocChanged) => (Sim::Idle, false),
            (s, _) => (s, false),
        };
        *self = next;
        deliver
    }

    pub fn busy(&self) -> bool {
        matches!(self, Sim::Running { .. } | Sim::Cancelling { .. })
    }

    pub fn job(&self) -> Option<&J> {
        match self {
            Sim::Running { job, .. } | Sim::Cancelling { job, .. } => Some(job),
            _ => None,
        }
    }

    /// The document generation the state belongs to.
    pub fn doc(&self) -> Option<u64> {
        match self {
            Sim::Idle => None,
            Sim::Running { doc, .. }
            | Sim::Cancelling { doc, .. }
            | Sim::Finished { doc, .. }
            | Sim::Failed { doc, .. }
            | Sim::Cancelled { doc } => Some(*doc),
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Sim::Idle => "idle",
            Sim::Running { .. } => "running",
            Sim::Cancelling { .. } => "cancelling",
            Sim::Finished { .. } => "finished",
            Sim::Failed { .. } => "failed",
            Sim::Cancelled { .. } => "cancelled",
        }
    }
}

/// One `.param` in the Parameters panel: the schematic's value and the slider's.
#[derive(Clone, Debug, PartialEq)]
pub struct LiveParam {
    pub name: String,
    /// The value in the schematic.
    pub base: f64,
    pub value: f64,
    pub min: f64,
    pub max: f64,
    pub log: bool,
}

/// Slider range for a value: a decade either side, on a log scale; around zero, ±1 linear.
pub fn param_range(v: f64) -> (f64, f64, bool) {
    if v == 0.0 {
        (-1.0, 1.0, false)
    } else {
        (
            v.min(v * 10.0).min(v / 10.0),
            v.max(v * 10.0).max(v / 10.0),
            true,
        )
    }
}

/// Live `.param` values: they change what simulations run with, never the schematic, until
/// applied. `rerun` is a run waiting for the superseded one to stop (at most one ever waits).
#[derive(Default, Debug)]
pub struct Live {
    pub params: Vec<LiveParam>,
    /// Schematic edit version the list was read at.
    seen: Option<u64>,
    pub rerun: bool,
}

impl Live {
    /// Re-reads the list when the schematic changed; a slider keeps its value and range while
    /// its parameter's schematic value stays the same.
    pub fn sync(&mut self, version: u64, found: impl FnOnce() -> Vec<(String, f64)>) {
        if self.seen == Some(version) {
            return;
        }
        self.seen = Some(version);
        let old = std::mem::take(&mut self.params);
        self.params = found()
            .into_iter()
            .map(|(name, base)| {
                old.iter()
                    .find(|p| p.name == name && p.base == base)
                    .cloned()
                    .unwrap_or_else(|| {
                        let (min, max, log) = param_range(base);
                        LiveParam {
                            name,
                            base,
                            value: base,
                            min,
                            max,
                            log,
                        }
                    })
            })
            .collect();
    }

    /// The values that differ from the schematic's.
    pub fn overrides(&self) -> Vec<(String, f64)> {
        self.params
            .iter()
            .filter(|p| p.value != p.base)
            .map(|p| (p.name.clone(), p.value))
            .collect()
    }

    /// Sets a value by name (`expr::fold`); the range widens to hold it. Err for an unknown name.
    pub fn set(&mut self, name: &str, v: f64) -> Result<bool, String> {
        if !v.is_finite() {
            return Err(format!("{name}: not a finite number"));
        }
        let key = swiftsim::expr::fold(name);
        let p = self
            .params
            .iter_mut()
            .find(|p| swiftsim::expr::fold(&p.name) == key)
            .ok_or(format!("no numeric .param '{name}' in the schematic"))?;
        if p.log && (v <= 0.0) != (p.base <= 0.0) || v == 0.0 {
            p.log = false;
        }
        p.min = p.min.min(v);
        p.max = p.max.max(v);
        let changed = p.value != v;
        p.value = v;
        Ok(changed)
    }

    /// Back to the schematic's values; true when something changed.
    pub fn reset(&mut self) -> bool {
        let changed = self.params.iter().any(|p| p.value != p.base);
        self.params.iter_mut().for_each(|p| p.value = p.base);
        changed
    }

    /// Another document: nothing carries over, and a waiting rerun is dropped.
    pub fn clear(&mut self) {
        *self = Live::default();
    }
}

/// What to do once the user has answered "Unsaved changes" (or right away when nothing is unsaved).
#[derive(Clone, Debug, PartialEq)]
pub enum Pending {
    New,
    Open,
    OpenPath(PathBuf),
    Example(usize),
    Restart(PathBuf),
    Quit,
}

pub enum Modal {
    /// A recovery copy left by a crash: restore or discard.
    Recover(PathBuf, Snapshot),
    /// Unsaved changes before `Pending`.
    Confirm(Pending),
    /// Update installed: restart into the new version now?
    Restart(String, PathBuf),
}

/// At most one dialog is open; others wait their turn. A new question about unsaved changes
/// replaces the one already asked (the latest request wins), wherever it is.
#[derive(Default)]
pub struct Modals {
    open: Option<Modal>,
    queue: VecDeque<Modal>,
}

impl Modals {
    pub fn push(&mut self, m: Modal) {
        if let Modal::Confirm(p) = &m {
            for slot in self.open.iter_mut().chain(self.queue.iter_mut()) {
                if let Modal::Confirm(q) = slot {
                    *q = p.clone();
                    return;
                }
            }
        }
        if self.open.is_none() {
            self.open = Some(m);
        } else {
            self.queue.push_back(m);
        }
    }

    pub fn open(&self) -> Option<&Modal> {
        self.open.as_ref()
    }

    /// Closes the open dialog and opens the next one waiting.
    pub fn close(&mut self) -> Option<Modal> {
        let m = self.open.take();
        self.open = self.queue.pop_front();
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn document_dirty_follows_the_edit_version() {
        let mut d = Document::new();
        assert!(!d.dirty(0) && d.dirty(1));
        for (origin, dirty_when_loaded) in [
            (Origin::New, false),
            (Origin::Example, false),
            (Origin::Opened, false),
            (Origin::Imported, false),
            (Origin::Restored, true),
            (Origin::Converted, true),
        ] {
            let g = d.generation;
            d.replace(Mode::Netlist, None, origin, 5);
            assert_eq!(
                (d.generation, d.origin, d.mode),
                (g + 1, origin, Mode::Netlist)
            );
            assert_eq!(d.dirty(5), dirty_when_loaded, "{origin:?}");
            assert!(d.dirty(6), "{origin:?}: an edit makes it dirty");
            d.saved_as("/a.cir".into(), 6);
            assert!(!d.dirty(6) && d.dirty(7));
            assert_eq!(d.path.as_deref(), Some(std::path::Path::new("/a.cir")));
        }
    }

    #[derive(Debug, PartialEq)]
    struct J(u32, Rc<Cell<bool>>);
    impl Cancel for J {
        fn cancel(&self) {
            self.1.set(true);
        }
    }

    fn job(n: u32) -> (J, Rc<Cell<bool>>) {
        let c = Rc::new(Cell::new(false));
        (J(n, c.clone()), c)
    }

    /// Every state against every event.
    #[test]
    fn simulation_job_transitions() {
        // each case is built fresh, with the cancel flag of the job it holds
        type Case<T> = fn() -> (T, Rc<Cell<bool>>);
        let states: Vec<Case<Sim<J>>> = vec![
            || (Sim::Idle, Rc::default()),
            || {
                let (j, c) = job(1);
                (Sim::Running { doc: 1, job: j }, c)
            },
            || {
                let (j, c) = job(1);
                (Sim::Cancelling { doc: 1, job: j }, c)
            },
            || (Sim::Finished { doc: 1, secs: 1.0 }, Rc::default()),
            || {
                (
                    Sim::Failed {
                        doc: 1,
                        msg: "x".into(),
                    },
                    Rc::default(),
                )
            },
            || (Sim::Cancelled { doc: 1 }, Rc::default()),
        ];
        // (event, its new job's cancel flag)
        let events: Vec<Case<SimEvent<J>>> = vec![
            || {
                let (j, c) = job(2);
                (SimEvent::Start { doc: 2, job: j }, c)
            },
            || (SimEvent::Cancel, Rc::default()),
            || {
                (
                    SimEvent::Done {
                        doc: 1,
                        result: Ok(2.0),
                    },
                    Rc::default(),
                )
            },
            || {
                (
                    SimEvent::Done {
                        doc: 1,
                        result: Err("e".into()),
                    },
                    Rc::default(),
                )
            },
            || {
                (
                    SimEvent::Done {
                        doc: 0,
                        result: Ok(2.0),
                    },
                    Rc::default(),
                )
            }, // another document's run
            || (SimEvent::DocChanged, Rc::default()),
        ];
        for (si, s) in states.iter().enumerate() {
            for (ei, e) in events.iter().enumerate() {
                let ((mut sim, old_cancelled), (ev, new_cancelled)) = (s(), e());
                let deliver = sim.on(ev);
                let busy = matches!(si, 1 | 2);
                let names = [
                    "idle",
                    "running",
                    "cancelling",
                    "finished",
                    "failed",
                    "cancelled",
                ];
                // (state after, delivered, old job stopped, new job stopped)
                let want = match (si, ei) {
                    (_, 0) if busy => (names[si], false, false, true), // Start while busy: the newcomer is stopped
                    (_, 0) => ("running", false, false, false),
                    (1, 1) => ("cancelling", false, true, false),
                    (1 | 2, 2) => ("finished", true, false, false),
                    (1, 3) => ("failed", true, false, false),
                    (2, 3) => ("cancelled", false, false, false),
                    (1 | 2, 5) => ("idle", false, true, false),
                    (_, 5) => ("idle", false, false, false),
                    _ => (names[si], false, false, false), // ignored, e.g. another document's result
                };
                assert_eq!(
                    (
                        sim.name(),
                        deliver,
                        old_cancelled.get(),
                        new_cancelled.get()
                    ),
                    want,
                    "state {si} event {ei}"
                );
                if ei == 0 {
                    assert_eq!(
                        sim.job().map(|j| j.0),
                        Some(if busy { 1 } else { 2 }),
                        "state {si}"
                    );
                }
            }
        }
    }

    #[test]
    fn live_params_follow_the_schematic() {
        assert_eq!(param_range(1e3), (100.0, 1e4, true));
        assert_eq!(param_range(-2.0), (-20.0, -0.2, true));
        assert_eq!(param_range(0.0), (-1.0, 1.0, false));
        let mut l = Live::default();
        l.sync(1, || vec![("R".into(), 1e3), ("g".into(), 2.0)]);
        assert!(l.overrides().is_empty());
        assert_eq!(l.set("r", 4.7e3), Ok(true));
        assert_eq!(l.set("r", 4.7e3), Ok(false));
        assert!(l.set("nope", 1.0).is_err() && l.set("g", f64::NAN).is_err());
        assert_eq!(l.set("g", 50.0), Ok(true)); // outside the range: it widens
        assert_eq!(l.params[1].max, 50.0);
        // an edit elsewhere keeps the sliders; a changed schematic value resets that one only
        l.sync(1, || panic!("same version: not re-read"));
        l.sync(2, || {
            vec![("R".into(), 1e3), ("g".into(), 3.0), ("c".into(), 1e-6)]
        });
        assert_eq!(l.overrides(), [("R".to_string(), 4.7e3)]);
        assert_eq!(l.params[1].value, 3.0);
        assert!(l.reset() && !l.reset() && l.overrides().is_empty());
        l.rerun = true;
        l.clear();
        assert!(!l.rerun && l.params.is_empty());
    }

    fn kinds(m: &Modals) -> Vec<String> {
        m.open
            .iter()
            .chain(&m.queue)
            .map(|m| match m {
                Modal::Recover(..) => "recover".to_string(),
                Modal::Confirm(p) => format!("confirm {p:?}"),
                Modal::Restart(t, _) => format!("restart {t}"),
            })
            .collect()
    }

    #[test]
    fn one_dialog_at_a_time() {
        let mut m = Modals::default();
        assert!(m.open().is_none() && m.close().is_none());
        let snap = || Snapshot {
            path: None,
            doc: super::super::session::Doc::Netlist(String::new()),
        };
        m.push(Modal::Recover("/r.json".into(), snap()));
        m.push(Modal::Confirm(Pending::New));
        m.push(Modal::Restart("v2".into(), "/x".into()));
        // a second unsaved-changes question replaces the waiting one in place
        m.push(Modal::Confirm(Pending::Quit));
        assert_eq!(kinds(&m), ["recover", "confirm Quit", "restart v2"]);
        assert!(matches!(m.close(), Some(Modal::Recover(..))));
        assert_eq!(kinds(&m), ["confirm Quit", "restart v2"]);
        // ...and the open one too
        m.push(Modal::Confirm(Pending::Open));
        assert_eq!(kinds(&m), ["confirm Open", "restart v2"]);
        assert!(matches!(m.close(), Some(Modal::Confirm(Pending::Open))));
        assert!(matches!(m.close(), Some(Modal::Restart(..))));
        assert!(m.open().is_none());
        m.push(Modal::Confirm(Pending::New));
        assert_eq!(kinds(&m), ["confirm New"]);
    }
}
