//! Desktop application shell.

pub mod asc;
mod bidi;
mod editor;
mod fonts;
mod i18n;
mod schematic;
mod session;
mod state;
pub mod update;
mod waveform;

/// Opened in the browser only when the user clicks "Support development" in the Help menu.
const SPONSOR_URL: &str = "https://github.com/sponsors/RamazanBerk20";

use bidi::Base;
use editor::{Editor, Tool};
use eframe::egui;
use egui::{Color32, CornerRadius, Key, Modifiers, RichText, Sense, Shape, Stroke, Vec2};
use i18n::{para, tr, trf};
use schematic::{Component, Kind, Schematic, Text, Wire};
use serde::{Deserialize, Serialize};
use session::{Doc, Recovery, Snapshot};
use state::{Document, Live, Modal, Modals, Mode, Origin, Pending, Sim, SimEvent};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant};
use swiftsim::netlist::tokenize;
use swiftsim::{Control, PlotKind, SimResult};
use waveform::WaveView;

pub fn run(file: Option<String>) -> eframe::Result {
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../../assets/icons/256.png"))
        .expect("bundled icon is a valid PNG");
    let opts = eframe::NativeOptions {
        // app_id must match swiftsim.desktop so Wayland compositors show the icon.
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([900.0, 600.0])
            .with_title("SwiftSim")
            .with_app_id("swiftsim")
            .with_icon(icon),
        // window geometry, panel sizes and `Settings`
        persistence_path: Some(config_dir().join("settings.ron")),
        // replaced by `fonts::Fallback`, which also has fonts of its own
        system_font_fallback: false,
        ..Default::default()
    };
    eframe::run_native(
        "SwiftSim",
        opts,
        Box::new(move |cc| Ok(Box::new(App::new(cc, file)))),
    )
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Waves,
    Netlist,
    Log,
}

/// Remembered across restarts (window size and panel sizes are kept by eframe itself).
#[derive(Serialize, Deserialize)]
struct Settings {
    dark: bool,
    show_op: bool,
    last_dir: Option<PathBuf>,
    #[serde(default = "yes")]
    check_updates: bool,
    /// UI language code; `None` follows the system.
    #[serde(default)]
    lang: Option<String>,
}

fn yes() -> bool {
    true
}

const AUTOSAVE: Duration = Duration::from_secs(30);

/// A running simulation: its thread answers with the document generation it ran for.
struct Job {
    ctl: Arc<Control>,
    rx: Receiver<(u64, Result<SimResult, String>)>,
    started: Instant,
}

impl state::Cancel for Job {
    fn cancel(&self) {
        self.ctl.cancel.store(true, Ordering::Relaxed);
    }
}

/// A running install: progress in 1/1000, its cancel flag, and the result.
type Install = (
    Arc<AtomicU32>,
    Arc<AtomicBool>,
    Receiver<Result<(), String>>,
);

/// The new version started by "Restart now", watched for a few seconds before this one quits.
const RESTART_GRACE: Duration = Duration::from_secs(4);

struct App {
    ed: Editor,
    /// The open document (schematic in `ed`, or `net_text`): path, origin, saved version.
    doc: Document,
    net_text: String,
    /// Edit version of `net_text`.
    net_version: u64,
    tab: Tab,
    waves: WaveView,
    results: Option<SimResult>,
    log: Vec<String>,
    /// Import warnings, kept at the top of the Log until another file is loaded.
    notes: Vec<String>,
    sim: Sim<Job>,
    /// Simulations started in this window, for `gui_status`.
    runs: u64,
    filter: String,
    modals: Modals,
    allow_close: bool,
    status: String,
    title: String,
    dark: bool,
    /// Folder the file dialogs start in.
    last_dir: Option<PathBuf>,
    rec: Recovery,
    autosaved: Instant,
    /// This window's state for `swiftsim --mcp`.
    window: session::WindowStatus,
    check_updates: bool,
    updater: update::Updater,
    /// Running update check.
    update_rx: Option<Receiver<Result<Option<update::Release>, String>>>,
    /// Running install: progress in 1/1000, its cancel flag, and the result.
    install: Option<Install>,
    /// The new version started by "Restart now" and when.
    child: Option<(std::process::Child, Instant)>,
    lang: Option<String>,
    /// Live `.param` values (the Parameters panel).
    live: Live,
    /// F6 moves the keyboard focus to the Parameters panel.
    focus_params: bool,
    /// Requests from `swiftsim --mcp` (`gui_*` tools): this window's request folder and inbox.
    inbox: Option<(PathBuf, Receiver<(String, serde_json::Value)>)>,
    /// When an agent last made a request: the window shows that it is being driven.
    agent_seen: Option<Instant>,
    /// When this copy has run long enough to count as started, and removes the program an
    /// update replaced (`update::cleanup`). `None` once done, or after an install in this window
    /// (whose replaced program must wait for the new version to run).
    cleanup_at: Option<Instant>,
}

/// How long the window shows "Driven by an agent" after an agent's last request.
const AGENT_SHOWN: Duration = Duration::from_secs(15);

impl App {
    fn new(cc: &eframe::CreationContext, file: Option<String>) -> App {
        let set = cc
            .storage
            .and_then(|s| eframe::get_value::<Settings>(s, eframe::APP_KEY))
            .unwrap_or(Settings {
                dark: true,
                show_op: true,
                last_dir: None,
                check_updates: true,
                lang: None,
            });
        i18n::set(set.lang.as_deref());
        cc.egui_ctx
            .add_font_provider(Arc::new(fonts::Fallback::new()));
        cc.egui_ctx.set_theme(if set.dark {
            egui::Theme::Dark
        } else {
            egui::Theme::Light
        });
        let (rec, orphans) = Recovery::start(&config_dir());
        let lib = lib_setting()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .map(|s| s.trim().to_string());
        swiftsim::netlist::set_ltspice_lib(lib.filter(|s| !s.is_empty()).map(PathBuf::from));
        let mut app = App {
            ed: Editor::default(),
            doc: Document::new(),
            net_text: String::new(),
            net_version: 0,
            tab: Tab::Waves,
            waves: WaveView::default(),
            results: None,
            log: vec![],
            notes: vec![],
            sim: Sim::Idle,
            runs: 0,
            filter: String::new(),
            modals: Modals::default(),
            allow_close: false,
            status: tr("Ready. Pick a part on the left (or press R, C, L, V, G, W…), press F5 to simulate.").into(),
            title: String::new(),
            dark: set.dark,
            last_dir: set.last_dir,
            rec,
            autosaved: Instant::now(),
            window: session::WindowStatus::new(&config_dir()),
            check_updates: set.check_updates,
            updater: update::Updater::default(),
            update_rx: None,
            install: None,
            child: None,
            lang: set.lang,
            live: Live::default(),
            focus_params: false,
            inbox: None,
            agent_seen: None,
            // past "Restart now"'s watch, so a crash while it watches can still be rolled back
            cleanup_at: Some(Instant::now() + 2 * RESTART_GRACE),
        };
        match session::ipc_dir(&config_dir()) {
            Ok(dir) => {
                let (tx, rx) = channel();
                let ctx = cc.egui_ctx.clone();
                session::listen(dir.clone(), tx, move || ctx.request_repaint());
                app.inbox = Some((dir, rx));
            }
            Err(e) => app.log.push(format!(
                "warning: coding agents can't reach this window ({e})"
            )),
        }
        if let Some((f, s)) = orphans
            .into_iter()
            .find_map(|f| Some((f.clone(), session::read(&f).ok()?)))
        {
            app.modals.push(Modal::Recover(f, s));
        }
        if !app.rec.locked() {
            app.log.push(format!("warning: {} can't hold file locks: crash recovery only offers copies older than 2 minutes", config_dir().display()));
        }
        app.ed.show_op = set.show_op;
        if app.check_updates {
            app.update_event(&cc.egui_ctx, update::Event::Check { manual: false });
        }
        match file {
            Some(f) => app.open_path(PathBuf::from(f)),
            None => app.replace_document(
                Doc::Schematic(examples()[0].1.clone()),
                None,
                Origin::Example,
            ),
        }
        app
    }

    /// Edit version of the open document's content.
    fn version(&self) -> u64 {
        match self.doc.mode {
            Mode::Schematic => self.ed.version(),
            Mode::Netlist => self.net_version,
        }
    }

    fn dirty(&self) -> bool {
        self.doc.dirty(self.version())
    }

    /// The one way another document becomes the open one: everything that belonged to the old
    /// document (its simulation, results, plots, import notes) goes with it.
    fn replace_document(&mut self, content: Doc, path: Option<PathBuf>, origin: Origin) {
        self.sim.on(SimEvent::DocChanged);
        self.live.clear();
        let mode = match content {
            Doc::Schematic(s) => {
                self.ed.load(s);
                Mode::Schematic
            }
            Doc::Netlist(t) => {
                self.net_text = t;
                self.net_version += 1;
                self.tab = Tab::Log;
                Mode::Netlist
            }
        };
        if mode == Mode::Schematic && self.tab == Tab::Netlist {
            self.tab = Tab::Waves;
        }
        self.doc.replace(mode, path, origin, self.version());
        self.notes.clear();
        self.results = None;
        self.waves.traces.clear();
        self.waves.generation += 1;
    }

    fn open_path(&mut self, p: PathBuf) {
        if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("asc")) {
            return self.import_asc(p);
        }
        let text = match std::fs::read(&p) {
            Ok(b) => swiftsim::netlist::decode(&b),
            Err(e) => {
                self.status = trf("Cannot open {}: {}", &[&p.display(), &e]);
                return;
            }
        };
        let content = if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("sws")) {
            match load_schematic(&text) {
                Ok(s) => Doc::Schematic(s),
                Err(e) => {
                    self.status = trf("Not a SwiftSim schematic: {}", &[&e]);
                    return;
                }
            }
        } else {
            Doc::Netlist(text)
        };
        self.status = trf("Opened {}", &[&p.display()]);
        self.remember(&p);
        self.replace_document(content, Some(p), Origin::Opened);
    }

    fn import_asc(&mut self, p: PathBuf) {
        let bytes = match std::fs::read(&p) {
            Ok(b) => b,
            Err(e) => {
                self.status = trf("Cannot open {}: {}", &[&p.display(), &e]);
                return;
            }
        };
        let (s, skipped) = asc::import(&asc::decode(&bytes), p.parent());
        self.remember(&p);
        // save next to the .asc as .sws (keeps relative .lib/.include paths working), never over the .asc itself
        let sws = p.with_extension("sws");
        self.replace_document(
            Doc::Schematic(s),
            (!sws.exists()).then_some(sws),
            Origin::Imported,
        );
        self.notes = skipped.iter().map(|n| format!("warning: LTspice symbol '{n}' has no SwiftSim equivalent and no subcircuit .asy for it was found (next to the schematic or in LTspice's lib/sym); it was left out (its nets are open)")).collect();
        self.log = self.notes.clone();
        self.status = if skipped.is_empty() {
            trf("Imported LTspice schematic {}", &[&p.display()])
        } else {
            self.tab = Tab::Log;
            trf(
                "Imported {} — WARNING: unsupported LTspice symbols left out (their nets are open): {}",
                &[&p.display(), &skipped.join(", ")],
            )
        };
    }

    fn save(&mut self, as_new: bool) {
        let schematic = self.doc.mode == Mode::Schematic;
        let path = match (&self.doc.path, as_new) {
            (Some(p), false) => p.clone(),
            _ => {
                let d = self.dialog();
                let d = if schematic {
                    d.add_filter(tr("SwiftSim schematic"), &["sws"])
                        .set_file_name("circuit.sws")
                } else {
                    d.add_filter(tr("SPICE netlist"), &["cir", "net", "sp", "spi"])
                        .set_file_name("circuit.cir")
                };
                match d.save_file() {
                    Some(p) => p,
                    None => return,
                }
            }
        };
        let data = if schematic {
            serde_json::to_string_pretty(&self.ed.sch).unwrap_or_default()
        } else {
            self.net_text.clone()
        };
        match session::atomic_write(&path, data.as_bytes()) {
            Ok(()) => {
                self.status = trf("Saved {}", &[&path.display()]);
                self.remember(&path);
                self.doc.saved_as(path, self.version());
            }
            Err(e) => self.status = trf("Save failed: {}", &[&e]),
        }
    }

    /// File dialog starting in the last folder used.
    fn dialog(&self) -> rfd::FileDialog {
        let d = rfd::FileDialog::new();
        match &self.last_dir {
            Some(dir) => d.set_directory(dir),
            None => d,
        }
    }

    fn remember(&mut self, p: &std::path::Path) {
        session::add_recent(&config_dir(), p);
        self.last_dir = p.parent().map(|d| d.to_path_buf());
    }

    /// Writes the recovery copy. Only ever to the config folder, never to the user's file.
    fn autosave(&mut self) {
        self.autosaved = Instant::now();
        let doc = match self.doc.mode {
            Mode::Schematic => Doc::Schematic(self.ed.sch.clone()),
            Mode::Netlist => Doc::Netlist(self.net_text.clone()),
        };
        if let Err(e) = self.rec.write(&Snapshot {
            path: self.doc.path.clone(),
            doc,
        }) {
            self.status = trf("Autosave failed: {}", &[&e]);
        }
    }

    fn restore(&mut self, s: Snapshot) {
        let content = match s.doc {
            Doc::Schematic(sch) => match checked(sch) {
                Ok(sch) => Doc::Schematic(sch),
                Err(e) => {
                    self.status = trf("Not a SwiftSim schematic: {}", &[&e]);
                    return;
                }
            },
            d => d,
        };
        let name = s
            .path
            .as_ref()
            .map_or(tr("untitled").into(), |p| p.display().to_string());
        self.status = trf(
            "Restored unsaved changes to {} — not saved yet (Ctrl+S)",
            &[&name],
        );
        self.replace_document(content, s.path, Origin::Restored);
    }

    fn do_pending(&mut self, p: Pending, ctx: &egui::Context) {
        match p {
            Pending::New => {
                self.replace_document(Doc::Schematic(Schematic::default()), None, Origin::New)
            }
            Pending::Open => {
                if let Some(p) = self
                    .dialog()
                    .add_filter(
                        tr("All supported"),
                        &["sws", "asc", "cir", "net", "sp", "spi", "txt"],
                    )
                    .add_filter(tr("SwiftSim schematic"), &["sws"])
                    .add_filter(tr("LTspice schematic"), &["asc"])
                    .add_filter(tr("SPICE netlist"), &["cir", "net", "sp", "spi", "txt"])
                    .pick_file()
                {
                    self.open_path(p);
                }
            }
            Pending::OpenPath(p) => self.open_path(p),
            Pending::Example(i) => self.replace_document(
                Doc::Schematic(examples()[i].1.clone()),
                None,
                Origin::Example,
            ),
            Pending::Restart(exe) => self.update_event(ctx, update::Event::Restart { exe }),
            Pending::Quit => {
                self.allow_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    fn request(&mut self, p: Pending, ctx: &egui::Context) {
        if self.dirty() {
            self.modals.push(Modal::Confirm(p))
        } else {
            self.do_pending(p, ctx)
        }
    }

    fn netlist_text(&mut self) -> String {
        match self.doc.mode {
            Mode::Schematic => self.ed.netlist().text.clone(),
            Mode::Netlist => self.net_text.clone(),
        }
    }

    fn run_sim(&mut self, ctx: &egui::Context) {
        if self.sim.busy() {
            return;
        }
        if self.dirty() {
            self.autosave();
        }
        let mut text = self.netlist_text();
        let over = self.live.overrides();
        if self.doc.mode == Mode::Schematic && !over.is_empty() {
            text = swiftsim::netlist::set_params(&text, &over);
        }
        let conflicts = if self.doc.mode == Mode::Schematic {
            self.ed.sch.name_conflicts()
        } else {
            vec![]
        };
        let base = self
            .doc
            .path
            .as_ref()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()));
        let ctl = Arc::new(Control::default());
        let (tx, rx) = channel();
        let (c2, ctx2, doc) = (ctl.clone(), ctx.clone(), self.doc.generation);
        let spawned = std::thread::Builder::new()
            .stack_size(swiftsim::sim::STACK)
            .spawn(move || {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    swiftsim::run_netlist(&text, base.as_deref(), &c2)
                }));
                let r = r.unwrap_or_else(|_| {
                    Err("internal simulator error (panic) — please report this netlist".into())
                });
                let _ = tx.send((
                    doc,
                    r.map(|mut res: SimResult| {
                        res.log.splice(0..0, conflicts);
                        res
                    }),
                ));
                ctx2.request_repaint();
            });
        if let Err(e) = spawned {
            self.status = trf("Simulation failed: {}", &[&e]);
            return;
        }
        self.runs += 1;
        self.sim.on(SimEvent::Start {
            doc,
            job: Job {
                ctl,
                rx,
                started: Instant::now(),
            },
        });
        self.status = tr("Simulating…").into();
    }

    fn stop_sim(&mut self) {
        self.live.rerun = false;
        self.sim.on(SimEvent::Cancel);
        if matches!(self.sim, Sim::Cancelling { .. }) {
            self.status = tr("Stopping the simulation…").into();
        }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let Some(job) = self.sim.job() else { return };
        let (doc, started) = (self.sim.doc().unwrap_or_default(), job.started);
        let (doc, r) = match job.rx.try_recv() {
            Err(TryRecvError::Empty) => {
                return ctx.request_repaint_after(Duration::from_millis(60));
            }
            Err(TryRecvError::Disconnected) => (
                doc,
                Err(tr("Simulation thread stopped unexpectedly").to_string()),
            ),
            Ok(m) => m,
        };
        // a run superseded by newer parameter values ends as cancelled, even if it finished first
        let r = if self.live.rerun && matches!(self.sim, Sim::Cancelling { .. }) {
            Err("superseded".to_string())
        } else {
            r
        };
        let secs = started.elapsed().as_secs_f64();
        let event = SimEvent::Done {
            doc,
            result: r.as_ref().map(|_| secs).map_err(|e| e.clone()),
        };
        if !self.sim.on(event) {
            if matches!(self.sim, Sim::Cancelled { .. }) {
                self.status = tr("Simulation stopped.").into();
            }
            return;
        }
        match r {
            Ok(res) => {
                self.log = self.notes.iter().chain(&res.log).cloned().collect();
                let kind = res.plots.first().map(|p| p.kind);
                self.ed.op_values.clear();
                let schematic = self.doc.mode == Mode::Schematic;
                if kind == Some(PlotKind::Op) {
                    let p = &res.plots[0];
                    for (n, v) in p.names.iter().zip(&p.re) {
                        if let Some(net) = n.strip_prefix("v(").and_then(|s| s.strip_suffix(')')) {
                            self.ed.op_values.insert(swiftsim::expr::fold(net), v[0]);
                        }
                    }
                    self.tab = Tab::Log;
                } else {
                    if kind == Some(PlotKind::Noise) {
                        if !self
                            .waves
                            .traces
                            .iter()
                            .any(|t| t.expr.to_ascii_lowercase().contains("noise"))
                        {
                            self.waves.add("V(onoise)");
                        }
                    } else if self.waves.traces.is_empty() && schematic {
                        for net in self.ed.sch.label_nets().into_iter().take(4) {
                            self.waves.add(&format!("V({net})"));
                        }
                    }
                    self.tab = Tab::Waves;
                    if self.ed.tool == Tool::Select && schematic {
                        self.ed.set_tool(Tool::Probe);
                    }
                }
                self.waves.generation += 1;
                let warn = res.log.iter().filter(|l| l.starts_with("warning")).count();
                let mut parts = vec![trf("Done in {} s", &[&format!("{secs:.2}")])];
                if warn > 0 {
                    parts.push(trf("{} warning(s), see Log", &[&warn]));
                }
                if self.ed.tool == Tool::Probe {
                    parts.push(
                        tr("Probe: click a wire for voltage, a part for current (Esc to edit)")
                            .into(),
                    );
                }
                self.status = parts.join(" · ");
                self.results = Some(res);
            }
            Err(e) => {
                self.log = self
                    .notes
                    .iter()
                    .cloned()
                    .chain([format!("error: {e}")])
                    .collect();
                self.tab = Tab::Log;
                self.status = trf("Simulation failed: {}", &[&e]);
            }
        }
    }

    /// Document and simulation state, as `gui_status` reports it to coding agents.
    fn status_json(&self) -> serde_json::Value {
        let (secs, error, progress) = match &self.sim {
            Sim::Finished { secs, .. } => (Some(*secs), None, 1.0),
            Sim::Failed { msg, .. } => (None, Some(msg.clone()), 0.0),
            Sim::Running { job, .. } | Sim::Cancelling { job, .. } => (
                None,
                None,
                (job.ctl.progress.load(Ordering::Relaxed) / 10) as f64 / 100.0,
            ),
            _ => (None, None, 0.0),
        };
        let steps = self.sim.job().map(|j| serde_json::json!({"done": j.ctl.points_done.load(Ordering::Relaxed), "total": j.ctl.points.load(Ordering::Relaxed)}));
        serde_json::json!({
            "pid": std::process::id(),
            "document": {
                "path": self.doc.path.as_ref().map(|p| std::path::absolute(p).unwrap_or_else(|_| p.clone())),
                "kind": if self.doc.mode == Mode::Schematic { "schematic" } else { "netlist" },
                "origin": self.doc.origin_name(),
                "dirty": self.dirty(),
                "generation": self.doc.generation,
            },
            "simulation": {
                "state": self.sim.name(),
                "run": self.runs,
                "document_generation": self.sim.doc(),
                "progress": progress,
                "seconds": secs,
                "error": error,
                "steps": steps,
            },
            "driven_by_agent": self.driven(),
            "live_params": self.live.overrides().into_iter().map(|(k, v)| (k, v.into())).collect::<serde_json::Map<String, serde_json::Value>>(),
        })
    }

    /// A Parameters value changed: the last simulation of this document runs again with it.
    /// A run in flight is stopped and superseded; however fast the values change, at most one
    /// new run waits for it (`Live::rerun`), and it takes the newest values.
    fn live_changed(&mut self, ctx: &egui::Context) {
        if self.sim.doc() != Some(self.doc.generation) {
            return; // nothing ran yet: the next run uses the values
        }
        if self.sim.busy() {
            self.sim.on(SimEvent::Cancel);
            self.live.rerun = true;
        } else {
            self.run_sim(ctx);
        }
    }

    /// The Parameters panel: numeric `.param`s with a slider and a value each. Only shown when
    /// the schematic has some.
    fn params_panel(&mut self, ui: &mut egui::Ui) {
        if self.doc.mode != Mode::Schematic || self.live.params.is_empty() {
            return;
        }
        let ctx = ui.ctx().clone();
        ui.add_space(12.0);
        ui.separator();
        ui.heading(tr("Parameters"));
        ui.weak(para(ui, "Moving a slider runs the simulation again. The schematic only changes when you apply the values."));
        fn num(v: &mut f64) -> egui::DragValue<'_> {
            let speed = v.abs().max(1e-12) * 0.01;
            egui::DragValue::new(v)
                .speed(speed)
                .custom_formatter(|x, _| swiftsim::netlist::fmt_spice(x))
                .custom_parser(|t| swiftsim::expr::parse_number(&t.trim().to_lowercase()))
        }
        let focus = std::mem::take(&mut self.focus_params);
        let mut changed = false;
        for (k, p) in self.live.params.iter_mut().enumerate() {
            ui.push_id(k, |ui| {
                let name = if p.value != p.base {
                    format!("{} ●", p.name)
                } else {
                    p.name.clone()
                };
                ui.label(RichText::new(name).monospace().strong())
                    .on_hover_text(trf(
                        "In the schematic: {}",
                        &[&swiftsim::netlist::fmt_spice(p.base)],
                    ));
                ui.horizontal(|ui| {
                    let r = ui.add(
                        egui::Slider::new(&mut p.value, p.min..=p.max)
                            .logarithmic(p.log)
                            .show_value(false)
                            .clamping(egui::SliderClamping::Never),
                    );
                    if focus && k == 0 {
                        r.request_focus();
                    }
                    changed |= r.changed();
                    changed |= ui.add(num(&mut p.value)).changed();
                });
                ui.horizontal(|ui| {
                    ui.weak(tr("Range"));
                    let edited =
                        ui.add(num(&mut p.min)).changed() | ui.add(num(&mut p.max)).changed();
                    if edited {
                        if p.min > p.max {
                            std::mem::swap(&mut p.min, &mut p.max);
                        }
                        // a log scale where the range spans orders of magnitude on one side of zero
                        p.log = p.min * p.max > 0.0
                            && (p.max / p.min).abs().max((p.min / p.max).abs()) >= 100.0;
                    }
                });
            });
        }
        let dirty = !self.live.overrides().is_empty();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(dirty, egui::Button::new(tr("Apply to schematic")))
                .on_hover_text(tr(
                    "Write these values into the schematic's .param lines (undoable)",
                ))
                .clicked()
            {
                self.apply_params();
            }
            if ui
                .add_enabled(dirty, egui::Button::new(tr("Reset")))
                .on_hover_text(tr("Back to the schematic's values"))
                .clicked()
                && self.live.reset()
            {
                changed = true;
            }
        });
        if changed {
            self.live_changed(&ctx);
        }
    }

    /// Writes the Parameters values into the schematic's `.param` lines, as one undoable edit.
    fn apply_params(&mut self) {
        let vals: Vec<(String, f64)> = self
            .live
            .params
            .iter()
            .map(|p| (p.name.clone(), p.value))
            .collect();
        let new: Vec<String> = self
            .ed
            .sch
            .texts
            .iter()
            .map(|t| swiftsim::netlist::set_params(&t.text, &vals))
            .collect();
        if new
            .iter()
            .zip(&self.ed.sch.texts)
            .all(|(n, t)| *n == t.text)
        {
            return;
        }
        self.ed.checkpoint();
        for (t, n) in self.ed.sch.texts.iter_mut().zip(new) {
            t.text = n;
        }
        self.ed.changed();
        self.status = tr("Parameter values written to the schematic (Ctrl+Z undoes it).").into();
    }

    /// Whether an agent drove this window recently.
    fn driven(&self) -> bool {
        self.agent_seen.is_some_and(|t| t.elapsed() < AGENT_SHOWN)
    }

    /// Answers the requests coding agents sent through `swiftsim --mcp`.
    fn agent_requests(&mut self, ctx: &egui::Context) {
        let Some((dir, rx)) = &self.inbox else { return };
        let (dir, reqs): (PathBuf, Vec<_>) = (dir.clone(), rx.try_iter().collect());
        for (id, req) in reqs {
            let r = self.agent(&req, ctx);
            session::answer(&dir, &id, r);
        }
        if self.driven() {
            ctx.request_repaint_after(Duration::from_secs(1));
        }
    }

    /// One agent request. The user comes first: nothing here saves, opens, closes or replaces
    /// the document, and requests are refused while a dialog is open or the mouse is held down.
    fn agent(
        &mut self,
        req: &serde_json::Value,
        ctx: &egui::Context,
    ) -> Result<serde_json::Value, String> {
        use serde_json::json;
        let (op, a) = (req["op"].as_str().unwrap_or(""), &req["args"]);
        if !session::live_request(req) {
            return Err(
                "the request expired before the window could take it; it was not carried out"
                    .into(),
            );
        }
        // a change is only made to the document the agent looked at
        if matches!(op, "run" | "set_params")
            && a["generation"].as_u64() != Some(self.doc.generation)
        {
            let now = self
                .doc
                .path
                .as_ref()
                .map_or(tr("untitled").to_string(), |p| p.display().to_string());
            return Err(match a["generation"].as_u64() {
                Some(g) => format!(
                    "the window's document changed since you looked (generation {g}, now {} for {now}); read it again with gui_netlist",
                    self.doc.generation
                ),
                None => "give `generation`: the document generation from gui_netlist or gui_status"
                    .into(),
            });
        }
        if self.modals.open().is_some() {
            return Err(
                "a dialog is open in the SwiftSim window; the user has to answer it first".into(),
            );
        }
        if ctx.input(|i| i.pointer.any_down()) {
            return Err(
                "the user is working in the SwiftSim window right now; try again in a moment"
                    .into(),
            );
        }
        self.agent_seen = Some(Instant::now());
        let params = |l: &Live| -> serde_json::Value {
            l.params
                .iter()
                .map(|p| json!({"name": p.name, "value": p.value, "schematic_value": p.base}))
                .collect()
        };
        match op {
            "netlist" => {
                let netlist = self.netlist_text();
                Ok(json!({
                    "path": self.doc.path,
                    "generation": self.doc.generation,
                    "kind": if self.doc.mode == Mode::Schematic { "schematic" } else { "netlist" },
                    "dirty": self.dirty(),
                    "netlist": netlist,
                    "params": params(&self.live),
                }))
            }
            "run" => {
                if self.sim.busy() {
                    return Err("a simulation is already running in the window".into());
                }
                let before = self.runs;
                self.run_sim(ctx);
                if self.runs == before {
                    return Err(self.status.clone());
                }
                self.status = format!("{} · {}", tr("Simulating…"), tr("Driven by an agent"));
                Ok(json!({"run": self.runs}))
            }
            "results" => {
                let r = self
                    .results
                    .as_ref()
                    .ok_or("no results in the window yet: run the simulation first (gui_run)")?;
                Ok(crate::mcp::results_json(r, self.log.clone(), a))
            }
            "set_params" => {
                if self.doc.mode != Mode::Schematic {
                    return Err(
                        "live parameters need a schematic; this window has a netlist open".into(),
                    );
                }
                let values = a["values"]
                    .as_object()
                    .ok_or("`values` must be an object of name: number")?;
                // all or nothing: check every value before changing any
                let mut trial = Live::default();
                trial.params = self.live.params.clone();
                for (k, v) in values {
                    trial.set(k, v.as_f64().ok_or(format!("{k}: not a number"))?)?;
                }
                let mut changed = false;
                for (k, v) in values {
                    changed |= self.live.set(k, v.as_f64().unwrap_or_default())?;
                }
                let before = self.runs;
                if changed {
                    self.live_changed(ctx);
                }
                // the re-run is the next run after `after_run`: it started now, or starts when the superseded one stops
                let rerun = self.runs > before || self.live.rerun;
                let after_run = if self.live.rerun { self.runs } else { before };
                Ok(json!({"params": params(&self.live), "rerun": rerun, "after_run": after_run}))
            }
            "last_error" => {
                let error = match &self.sim {
                    Sim::Failed { msg, .. } => Some(msg.clone()),
                    _ => None,
                };
                let lines: Vec<&String> =
                    self.log.iter().filter(|l| l.starts_with("error")).collect();
                Ok(
                    json!({"state": self.sim.name(), "run": self.runs, "error": error, "log_errors": lines}),
                )
            }
            _ => Err(format!("unknown request '{op}'")),
        }
    }

    fn menu(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        egui::MenuBar::new().ui(ui, |ui| i18n::bar(ui, |ui| {
            i18n::menu(ui, tr("File"), |ui| {
                if item(ui, tr("New schematic"), "Ctrl+N").clicked() {
                    self.request(Pending::New, &ctx);
                    ui.close();
                }
                if item(ui, tr("Open…"), "Ctrl+O").clicked() {
                    self.request(Pending::Open, &ctx);
                    ui.close();
                }
                i18n::submenu(ui, tr("Open recent"), |ui| {
                    let recent = session::recent(&config_dir());
                    if recent.is_empty() {
                        ui.weak(tr("No recent files"));
                    }
                    for p in recent {
                        let name = p.file_name().map_or(p.display().to_string(), |n| n.to_string_lossy().to_string());
                        if ui.button(name).on_hover_text(p.display().to_string()).clicked() {
                            self.request(Pending::OpenPath(p), &ctx);
                            ui.close();
                        }
                    }
                });
                if item(ui, tr("Save"), "Ctrl+S").clicked() {
                    self.save(false);
                    ui.close();
                }
                if ui.button(tr("Save as…")).clicked() {
                    self.save(true);
                    ui.close();
                }
                ui.separator();
                if self.doc.mode == Mode::Schematic && ui.button(tr("Export SPICE netlist…")).clicked() {
                    if let Some(p) = self.dialog().add_filter(tr("SPICE netlist"), &["cir"]).set_file_name("circuit.cir").save_file() {
                        let t = self.ed.netlist().text.clone();
                        self.status = match session::atomic_write(&p, t.as_bytes()) {
                            Ok(()) => trf("Exported {}", &[&p.display()]),
                            Err(e) => trf("Export failed: {}", &[&e]),
                        };
                    }
                    ui.close();
                }
                if ui.add_enabled(self.results.as_ref().is_some_and(|r| !r.plots.is_empty()), egui::Button::new(tr("Export results as LTspice .raw…"))).clicked() {
                    if let Some(p) = self.dialog().add_filter(tr("LTspice waveform"), &["raw"]).set_file_name("circuit.raw").save_file() {
                        let title = self.doc.path.as_ref().and_then(|p| p.file_name()).map_or("SwiftSim".into(), |n| n.to_string_lossy().to_string());
                        let raw = self.results.as_ref().map_or(Err(String::new()), |r| swiftsim::to_raw(&r.plots, &title));
                        self.status = match raw.and_then(|raw| session::atomic_write(&p, &raw).map_err(|e| e.to_string())) {
                            Ok(()) => trf("Exported {}", &[&p.display()]),
                            Err(e) => trf("Export failed: {}", &[&e]),
                        };
                    }
                    ui.close();
                }
                if ui.button(tr("Open netlist as text editor")).clicked() {
                    let t = self.netlist_text();
                    self.replace_document(Doc::Netlist(t), None, Origin::Converted);
                    ui.close();
                }
                ui.separator();
                if ui.button(tr("Quit")).clicked() {
                    self.request(Pending::Quit, &ctx);
                    ui.close();
                }
            });
            i18n::menu(ui, tr("Edit"), |ui| {
                let ed = &mut self.ed;
                if item(ui, tr("Undo"), "Ctrl+Z").clicked() {
                    ed.undo();
                }
                if item(ui, tr("Redo"), "Ctrl+Y").clicked() {
                    ed.redo();
                }
                ui.separator();
                if item(ui, tr("Cut"), "Ctrl+X").clicked() {
                    ed.copy();
                    ed.delete_selected();
                }
                if item(ui, tr("Copy"), "Ctrl+C").clicked() {
                    ed.copy();
                }
                if item(ui, tr("Paste"), "Ctrl+V").clicked() {
                    ed.paste();
                }
                if item(ui, tr("Delete"), "Del").clicked() {
                    ed.delete_selected();
                }
                if item(ui, tr("Select all"), "Ctrl+A").clicked() {
                    ed.select_all();
                }
                ui.separator();
                if item(ui, tr("Rotate"), "Ctrl+R").clicked() {
                    ed.rotate();
                }
                if item(ui, tr("Mirror"), "Ctrl+E").clicked() {
                    ed.mirror();
                }
            });
            i18n::menu(ui, tr("View"), |ui| {
                if item(ui, tr("Zoom to fit"), "Space").clicked() {
                    self.ed.fit();
                    ui.close();
                }
                ui.checkbox(&mut self.ed.show_op, tr("Show operating-point voltages"));
                if ui.checkbox(&mut self.dark, tr("Dark theme")).changed() {
                    ui.ctx().set_theme(if self.dark { egui::Theme::Dark } else { egui::Theme::Light });
                }
                i18n::submenu(ui, tr("Language"), |ui| {
                    let mut pick = None;
                    if ui.radio(self.lang.is_none(), tr("System default")).clicked() {
                        pick = Some(None);
                    }
                    for (code, name) in i18n::LANGS {
                        if ui.radio(self.lang.as_deref() == Some(code), name).clicked() {
                            pick = Some(Some(code.to_string()));
                        }
                    }
                    if let Some(l) = pick {
                        i18n::set(l.as_deref());
                        self.lang = l;
                        ui.close();
                    }
                });
                ui.separator();
                if ui.button(tr("Reset layout")).clicked() {
                    // panels fall back to their `default_size` once their saved state is gone
                    ctx.data_mut(|d| d.remove_by_type::<egui::containers::panel::PanelState>());
                    ui.close();
                }
            });
            i18n::menu(ui, tr("Simulate"), |ui| {
                if item(ui, tr("Run"), "F5").clicked() {
                    self.run_sim(&ctx);
                    ui.close();
                }
                if ui.add_enabled(matches!(self.sim, Sim::Running { .. }), egui::Button::new(tr("Stop"))).clicked() {
                    self.stop_sim();
                    ui.close();
                }
                ui.separator();
                let manual = lib_setting().is_some_and(|f| f.exists());
                ui.weak(match swiftsim::netlist::ltspice_lib() {
                    Some(p) if manual => trf("LTspice library: {}", &[&p.display()]),
                    Some(p) => trf("LTspice library: {} (auto-detected)", &[&p.display()]),
                    None => tr("LTspice library: not found (LTspice's own models are unavailable)").into(),
                });
                if ui.button(tr("Choose LTspice library folder…")).on_hover_text(i18n::para_in(ui, "LTspice's lib folder, with cmp/ and sub/ inside. Models missing from the netlist are read from it.", 300.0)).clicked() {
                    if let Some(d) = rfd::FileDialog::new().pick_folder() {
                        let saved = lib_setting().ok_or("no config folder".to_string()).and_then(|f| {
                            std::fs::create_dir_all(f.parent().unwrap_or(&f)).and_then(|_| std::fs::write(&f, d.to_string_lossy().as_bytes())).map_err(|e| e.to_string())
                        });
                        self.status = match saved {
                            Ok(()) => trf("LTspice library set to {}", &[&d.display()]),
                            Err(e) => trf("LTspice library set to {} for this session (not saved: {})", &[&d.display(), &e]),
                        };
                        swiftsim::netlist::set_ltspice_lib(Some(d));
                    }
                    ui.close();
                }
                if manual && ui.button(tr("Auto-detect LTspice library")).clicked() {
                    if let Some(f) = lib_setting() {
                        let _ = std::fs::remove_file(f);
                    }
                    swiftsim::netlist::set_ltspice_lib(None);
                    ui.close();
                }
            });
            i18n::menu(ui, tr("Examples"), |ui| {
                for (i, (name, _)) in examples().iter().enumerate() {
                    if ui.button(tr(*name)).clicked() {
                        self.request(Pending::Example(i), &ctx);
                        ui.close();
                    }
                }
            });
            i18n::menu(ui, tr("Help"), |ui| {
                ui.label(RichText::new(tr("Keyboard")).strong());
                let keys = [
                    ("R C L D V I Q M U", "place part"),
                    ("G / N", "ground / net label"),
                    ("W or F3", "wire (click points; click a pin to finish)"),
                    ("T or .", "SPICE directive text"),
                    ("P", "probe (after a run)"),
                    ("Ctrl+R / Ctrl+E", "rotate / mirror"),
                    ("Esc", "cancel / back to select"),
                    ("Space", "zoom to fit"),
                    ("Middle/right drag", "pan; wheel zooms"),
                    ("F5", "run simulation"),
                    ("F6", "Parameters panel (arrows move a slider)"),
                ];
                egui::Grid::new("keys").num_columns(2).show(ui, |ui| {
                    for (k, d) in keys {
                        i18n::form_row(ui, tr(d), |ui| {
                            ui.monospace(tr(k));
                        });
                    }
                });
                ui.separator();
                ui.label(format!("{} .op .tran .ac .dc .tf .noise .four .temp .step .meas .param .ic .model .subckt .include/.lib .options", tr("Supported:")));
                ui.label(format!("{} R C L K V I D Q M(level1/VDMOS) E F G H B S X", tr("Elements:")));
                ui.separator();
                ui.label(format!("SwiftSim {}", update::VERSION));
                if ui.link(tr("Support development")).clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(SPONSOR_URL));
                }
                if ui.button(tr("Check for updates now")).clicked() {
                    self.update_event(&ctx, update::Event::Check { manual: true });
                    ui.close();
                }
                ui.checkbox(&mut self.check_updates, tr("Check for updates at startup"));
                ui.separator();
                if ui.button(tr("Copy MCP server command")).on_hover_text(i18n::para_in(ui, "Lets a coding agent run simulations: add this command to it as an MCP server.", 300.0)).clicked() {
                    ui.ctx().copy_text(format!("\"{}\" --mcp", update::self_path().display()));
                    self.status = tr("MCP server command copied.").into();
                    ui.close();
                }
            });
        }));
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.horizontal(|ui| {
            // one look per simulation state
            let progress = |j: &Job| j.ctl.progress.load(Ordering::Relaxed) as f32 / 1000.0;
            // a .step run counts its points; anything else shows a percentage
            let bar = |j: &Job| {
                let b = egui::ProgressBar::new(progress(j)).desired_width(160.0);
                match j.ctl.points.load(Ordering::Relaxed) {
                    n if n > 1 => b.text(trf("{}/{} steps", &[&j.ctl.points_done.load(Ordering::Relaxed), &n])),
                    _ => b.show_percentage(),
                }
            };
            match &self.sim {
                Sim::Running { job, .. } => {
                    let b = bar(job);
                    if ui.button(RichText::new(i18n::with_symbol("■", tr("Stop"))).color(Color32::from_rgb(255, 99, 99))).clicked() {
                        self.stop_sim();
                    }
                    ui.add(b);
                }
                Sim::Cancelling { job, .. } => {
                    let b = bar(job);
                    ui.add_enabled(false, egui::Button::new(tr("Stopping…")));
                    ui.spinner();
                    ui.add(b);
                }
                Sim::Idle | Sim::Finished { .. } | Sim::Failed { .. } | Sim::Cancelled { .. } => {
                    if ui.button(RichText::new(i18n::with_symbol("▶", tr("Run"))).color(Color32::from_rgb(102, 220, 120)).strong()).on_hover_text(tr("Run simulation (F5)")).clicked() {
                        self.run_sim(&ctx);
                    }
                }
            }
            if self.driven() {
                let tip = i18n::para_in(ui, "A coding agent is using this window through SwiftSim's MCP server: it can run the simulation, read results and set parameter values, but never saves, opens or closes files. Your own input comes first.", 320.0);
                ui.label(RichText::new(i18n::with_symbol("🤖", tr("Driven by an agent"))).color(Color32::from_rgb(120, 170, 255)).strong()).on_hover_text(tip);
            }
            ui.separator();
            if self.doc.mode == Mode::Schematic {
                let t = self.ed.tool;
                for (label, tool, tip) in [
                    ("Select", Tool::Select, "Select / move (Esc)"),
                    ("Wire", Tool::WIRE, "Draw wires (W)"),
                    ("Ground", Tool::place(Kind::Ground), "Ground (G)"),
                    ("Label", Tool::place(Kind::Label), "Net label (N)"),
                    (".Text", Tool::Text, "SPICE directive (T)"),
                    ("Probe", Tool::Probe, "Click wire → voltage, part → current (P)"),
                ] {
                    if ui.selectable_label(t.same(tool), tr(label)).on_hover_text(tr(tip)).clicked() {
                        self.ed.set_tool(tool);
                    }
                }
                ui.separator();
                if ui.button("⟲").on_hover_text(tr("Undo (Ctrl+Z)")).clicked() {
                    self.ed.undo();
                }
                if ui.button("⟳").on_hover_text(tr("Redo (Ctrl+Y)")).clicked() {
                    self.ed.redo();
                }
                if ui.button(tr("Rotate")).on_hover_text("Ctrl+R").clicked() {
                    self.ed.rotate();
                }
                if ui.button(tr("Mirror")).on_hover_text("Ctrl+E").clicked() {
                    self.ed.mirror();
                }
                if ui.button(tr("Delete")).on_hover_text("Del").clicked() {
                    self.ed.delete_selected();
                }
                if ui.button(tr("Fit")).on_hover_text("Space").clicked() {
                    self.ed.fit();
                }
            } else {
                ui.label(RichText::new(tr("Netlist mode")).strong());
                if ui.button(tr("New schematic")).clicked() {
                    self.request(Pending::New, &ctx);
                }
            }
        });
    }

    fn palette(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        bidi::line(ui, &mut self.filter, Base::Auto, |t| {
            t.hint_text(tr("Search parts…"))
        });
        ui.add_space(4.0);
        let f = self.filter.to_lowercase();
        egui::ScrollArea::vertical().show(ui, |ui| {
            for cat in ["Passive", "Semiconductor", "Sources", "Blocks", "Wiring"] {
                let items: Vec<Kind> = Kind::ALL
                    .into_iter()
                    .filter(|k| {
                        k.category() == cat
                            && (tr(k.title()).to_lowercase().contains(&f)
                                || k.title().to_lowercase().contains(&f))
                    })
                    .collect();
                if items.is_empty() {
                    continue;
                }
                ui.label(RichText::new(tr(cat)).small().weak());
                for k in items {
                    let active = self.ed.tool.same(Tool::place(k));
                    let (rect, resp) = ui
                        .allocate_exact_size(Vec2::new(ui.available_width(), 34.0), Sense::click());
                    let vis = ui.style().interact_selectable(&resp, active);
                    if active || resp.hovered() {
                        ui.painter()
                            .rect_filled(rect, CornerRadius::same(4), vis.bg_fill);
                    }
                    // icon and title on the reading side, shortcut on the other
                    let rtl = i18n::is_rtl_ui();
                    let icon_x = if rtl {
                        rect.right() - 32.0
                    } else {
                        rect.left() + 4.0
                    };
                    let icon = egui::Rect::from_min_size(
                        egui::pos2(icon_x, rect.top() + 3.0),
                        Vec2::splat(28.0),
                    );
                    draw_icon(ui.painter(), icon, k, vis.text_color());
                    let (title_at, title_align) = if rtl {
                        (rect.right() - 40.0, egui::Align2::RIGHT_CENTER)
                    } else {
                        (rect.left() + 40.0, egui::Align2::LEFT_CENTER)
                    };
                    ui.painter().text(
                        egui::pos2(title_at, rect.top() + 17.0),
                        title_align,
                        tr(k.title()),
                        egui::FontId::proportional(13.0),
                        vis.text_color(),
                    );
                    if let Some(s) = k.shortcut() {
                        let (key_at, key_align) = if rtl {
                            (rect.left() + 8.0, egui::Align2::LEFT_CENTER)
                        } else {
                            (rect.right() - 8.0, egui::Align2::RIGHT_CENTER)
                        };
                        ui.painter().text(
                            egui::pos2(key_at, rect.center().y),
                            key_align,
                            s,
                            egui::FontId::monospace(11.0),
                            ui.visuals().weak_text_color(),
                        );
                    }
                    if resp.clicked() {
                        self.ed.set_tool(Tool::place(k));
                    }
                }
                ui.add_space(6.0);
            }
        });
    }

    fn properties(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        let focus = std::mem::take(&mut self.ed.focus_value);
        let sel = self.ed.sel.clone();
        if sel.comps.len() == 1 && sel.wires.is_empty() && sel.texts.is_empty() {
            let i = *sel.comps.iter().next().unwrap();
            self.comp_props(ui, i, focus);
        } else if sel.texts.len() == 1 && sel.comps.is_empty() {
            let i = *sel.texts.iter().next().unwrap();
            ui.heading(tr("SPICE directive"));
            ui.weak(para(ui, "Lines are added to the netlist as-is."));
            let mut t = self.ed.sch.texts[i].text.clone();
            let r = bidi::code(ui, &mut t, |e| {
                e.desired_rows(6).desired_width(f32::INFINITY)
            });
            if focus {
                r.request_focus();
            }
            if r.gained_focus() {
                self.ed.checkpoint();
            }
            if r.changed() {
                self.ed.sch.texts[i].text = t;
                self.ed.changed();
            }
        } else if !sel.is_empty() {
            ui.heading(tr("Selection"));
            ui.label(trf(
                "{} part(s), {} wire(s), {} text(s)",
                &[&sel.comps.len(), &sel.wires.len(), &sel.texts.len()],
            ));
        } else {
            ui.heading(tr("Properties"));
            ui.weak(para(
                ui,
                "Select a part to edit it. Double-click jumps to its value.",
            ));
        }
        ui.add_space(12.0);
        ui.separator();
        self.sim_card(ui);
        self.params_panel(ui);
    }

    fn comp_props(&mut self, ui: &mut egui::Ui, i: usize, focus: bool) {
        let c = self.ed.sch.comps[i].clone();
        ui.heading(tr(c.kind.title()));
        let mut changed: Option<Component> = None;
        egui::Grid::new("props")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                if !c.kind.prefix().is_empty() {
                    i18n::form_row(ui, tr("Name"), |ui| {
                        let mut n = c.name.clone();
                        if bidi::line(ui, &mut n, Base::Auto, |t| t).changed() {
                            changed = Some(Component {
                                name: n,
                                ..c.clone()
                            });
                        }
                    });
                }
                if c.kind != Kind::Ground {
                    i18n::form_row(
                        ui,
                        if c.kind == Kind::Label {
                            tr("Net name")
                        } else {
                            tr("Value")
                        },
                        |ui| {
                            let mut v = c.value.clone();
                            let r = bidi::line(ui, &mut v, Base::Auto, |t| t.desired_width(180.0));
                            if focus {
                                r.request_focus();
                            }
                            if r.changed() {
                                changed = Some(Component {
                                    value: v,
                                    ..changed.clone().unwrap_or(c.clone())
                                });
                            }
                        },
                    );
                }
                let kinds = c.kind.model_kinds();
                if !kinds.is_empty() {
                    i18n::form_row(ui, tr("Model"), |ui| {
                        let mut names = swiftsim::netlist::builtin_names(kinds);
                        for t in &self.ed.sch.texts {
                            for l in t.text.lines() {
                                let w: Vec<String> = tokenize(&l.to_ascii_lowercase());
                                if w.first().is_some_and(|f| f == ".model")
                                    && w.len() > 2
                                    && kinds.contains(&w[2].as_str())
                                {
                                    names.insert(
                                        0,
                                        l.split_whitespace().nth(1).unwrap_or("").to_string(),
                                    );
                                }
                            }
                        }
                        let cur = c.value.split_whitespace().next().unwrap_or("").to_string();
                        egui::ComboBox::from_id_salt("model")
                            .selected_text(&cur)
                            .width(180.0)
                            .show_ui(ui, |ui| {
                                for n in names {
                                    if ui
                                        .selectable_label(n.eq_ignore_ascii_case(&cur), &n)
                                        .clicked()
                                    {
                                        let rest: Vec<&str> =
                                            c.value.split_whitespace().skip(1).collect();
                                        changed = Some(Component {
                                            value: [vec![n.as_str()], rest].concat().join(" "),
                                            ..c.clone()
                                        });
                                    }
                                }
                            });
                    });
                }
            });
        if matches!(c.kind, Kind::Voltage | Kind::Current) {
            ui.add_space(8.0);
            if let Some(v) = source_form(ui, &c.value, c.kind == Kind::Voltage) {
                changed = Some(Component {
                    value: v,
                    ..c.clone()
                });
            }
        }
        if matches!(c.kind, Kind::Nmos | Kind::Pmos | Kind::Nmos4 | Kind::Pmos4) {
            ui.weak(para(
                ui,
                "Level-1 models accept W=… L=… after the model name.",
            ));
        }
        if c.kind == Kind::Behavioral {
            ui.weak(tr("e.g. V=V(a)*V(b)  or  I=if(V(in)>1, 1m, 0)"));
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(tr("Rotate")).clicked() {
                self.ed.rotate();
            }
            if ui.button(tr("Mirror")).clicked() {
                self.ed.mirror();
            }
            if ui.button(tr("Delete")).clicked() {
                self.ed.delete_selected();
            }
        });
        if let Some(n) = changed {
            if self.ed.sch.comps.get(i).is_some() {
                self.ed.checkpoint();
                self.ed.sch.comps[i] = n;
                self.ed.changed();
            }
        }
    }

    fn sim_card(&mut self, ui: &mut egui::Ui) {
        ui.heading(tr("Simulation"));
        let (ti, line) = self
            .ed
            .sch
            .texts
            .iter()
            .enumerate()
            .find_map(|(i, t)| {
                t.text
                    .lines()
                    .find(|l| is_analysis(l))
                    .map(|l| (Some(i), l.trim().to_string()))
            })
            .unwrap_or((None, String::new()));
        let mut f = SimForm::parse(&line);
        let before = f.clone();
        egui::ComboBox::from_id_salt("an")
            .selected_text(tr(SimForm::KINDS[f.kind]))
            .show_ui(ui, |ui| {
                for (i, k) in SimForm::KINDS.iter().enumerate() {
                    ui.selectable_value(&mut f.kind, i, tr(*k));
                }
            });
        egui::Grid::new("simf")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                let field = |ui: &mut egui::Ui, l: &'static str, v: &mut String| {
                    i18n::form_row(ui, tr(l), |ui| {
                        ui.add(egui::TextEdit::singleline(v).desired_width(120.0));
                    });
                };
                match f.kind {
                    0 => {
                        field(ui, "Stop time", &mut f.a);
                        field(ui, "Start saving", &mut f.b);
                        field(ui, "Max step", &mut f.c);
                        i18n::form_row(ui, tr("Start from"), |ui| {
                            ui.checkbox(&mut f.uic, tr("initial conditions (UIC)"));
                        });
                    }
                    1 | 5 => {
                        if f.kind == 5 {
                            field(ui, "Output", &mut f.out);
                            field(ui, "Input source", &mut f.src);
                        }
                        i18n::form_row(ui, tr("Sweep"), |ui| {
                            egui::ComboBox::from_id_salt("acs")
                                .selected_text(&f.d)
                                .show_ui(ui, |ui| {
                                    for s in ["dec", "oct", "lin"] {
                                        ui.selectable_value(&mut f.d, s.to_string(), s);
                                    }
                                });
                        });
                        field(
                            ui,
                            if f.d == "lin" {
                                "Points"
                            } else {
                                "Points/decade"
                            },
                            &mut f.a,
                        );
                        field(ui, "Start freq", &mut f.b);
                        field(ui, "Stop freq", &mut f.c);
                    }
                    2 => {
                        field(ui, "Source", &mut f.d);
                        field(ui, "Start value", &mut f.a);
                        field(ui, "Stop value", &mut f.b);
                        field(ui, "Step", &mut f.c);
                    }
                    4 => {
                        field(ui, "Output", &mut f.a);
                        field(ui, "Input source", &mut f.d);
                    }
                    _ => {}
                }
            });
        if f.kind == 3 {
            ui.weak(para(
                ui,
                "DC operating point: voltages appear on the schematic.",
            ));
        }
        if f.kind == 4 {
            ui.weak(para(
                ui,
                "Small-signal gain, input and output impedance; results in the Log.",
            ));
        }
        if f.kind == 5 {
            ui.weak(para(ui, "V(onoise), V(inoise) and each part's contribution in V/√Hz; integrated totals in the Log."));
        }
        if f.kind != before.kind {
            let src = self
                .ed
                .sch
                .comps
                .iter()
                .find(|c| c.kind == Kind::Voltage)
                .map_or("V1".to_string(), |c| c.spice_name());
            f = SimForm::defaults(f.kind, &src);
        }
        if f != before {
            let new = f.line();
            self.ed.checkpoint();
            match ti {
                Some(i) => {
                    let t = &mut self.ed.sch.texts[i];
                    t.text = t
                        .text
                        .lines()
                        .map(|l| {
                            if is_analysis(l) {
                                new.clone()
                            } else {
                                l.to_string()
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                }
                None => self.ed.place_text(&new),
            }
            self.ed.changed();
        }
        ui.add_space(6.0);
        ui.weak(para(
            ui,
            "Add .step, .meas, .param, .model … with the .Text tool.",
        ));
    }

    fn bottom(&mut self, ui: &mut egui::Ui) {
        i18n::mirrored(ui, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Waves, tr("Waveforms"));
                if self.doc.mode == Mode::Schematic {
                    ui.selectable_value(&mut self.tab, Tab::Netlist, tr("Netlist"));
                }
                let errs = self.log.iter().any(|l| l.starts_with("error"));
                ui.selectable_value(
                    &mut self.tab,
                    Tab::Log,
                    if errs {
                        RichText::new(i18n::with_symbol("●", tr("Log")))
                            .color(Color32::from_rgb(255, 99, 99))
                    } else {
                        RichText::new(tr("Log"))
                    },
                );
            })
        });
        ui.separator();
        match self.tab {
            Tab::Waves => self.waves.ui(ui, self.results.as_ref()),
            Tab::Netlist => {
                let mut t = self.ed.netlist().text.clone();
                ui.horizontal(|ui| {
                    if ui.button(tr("Copy")).clicked() {
                        ui.ctx().copy_text(t.clone());
                    }
                    ui.weak(tr("Generated from the schematic (read-only)."));
                });
                egui::ScrollArea::both().show(ui, |ui| {
                    bidi::code(ui, &mut t, |e| {
                        e.interactive(false).desired_width(f32::INFINITY)
                    });
                });
            }
            Tab::Log => {
                egui::ScrollArea::both()
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        if self.log.is_empty() {
                            ui.weak(tr("No messages."));
                        }
                        for l in &self.log {
                            let col = if l.starts_with("error") {
                                Color32::from_rgb(255, 99, 99)
                            } else if l.starts_with("warning") {
                                Color32::from_rgb(255, 196, 0)
                            } else {
                                ui.visuals().text_color()
                            };
                            ui.label(RichText::new(l).monospace().color(col));
                        }
                    });
            }
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // a GUI panic still leaves an up-to-date recovery copy for the next start
        if let Err(p) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.frame(ui))) {
            if self.dirty() {
                self.autosave();
            }
            std::panic::resume_unwind(p);
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        let set = Settings {
            dark: self.dark,
            show_op: self.ed.show_op,
            last_dir: self.last_dir.clone(),
            check_updates: self.check_updates,
            lang: self.lang.clone(),
        };
        eframe::set_value(storage, eframe::APP_KEY, &set);
    }

    fn on_exit(&mut self) {
        self.rec.close();
        self.window.close();
        if let Some((dir, _)) = &self.inbox {
            session::forget_requests(dir);
        }
    }
}

impl App {
    /// Feeds the update state machine and carries out what it asks for. Automatic checks stay
    /// quiet unless they find a release; checks the user starts also report "up to date" and errors.
    fn update_event(&mut self, ctx: &egui::Context, ev: update::Event) {
        let Some(out) = self.updater.on(ev) else {
            return;
        };
        match out {
            update::Out::StartCheck => {
                let (tx, rx) = channel();
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(update::check());
                    ctx.request_repaint();
                });
                self.update_rx = Some(rx);
            }
            update::Out::StartInstall { tag, plan } => {
                self.cleanup_at = None;
                let (progress, cancel) = (
                    Arc::new(AtomicU32::new(0)),
                    Arc::new(AtomicBool::new(false)),
                );
                let (tx, rx) = channel();
                let (p, c, ctx) = (progress.clone(), cancel.clone(), ctx.clone());
                std::thread::spawn(move || {
                    let _ = tx.send(update::install(&tag, &plan, &p, &c));
                    ctx.request_repaint();
                });
                self.install = Some((progress, cancel, rx));
            }
            update::Out::CancelInstall => {
                if let Some((_, c, _)) = &self.install {
                    c.store(true, Ordering::Relaxed);
                }
            }
            update::Out::AskRestart { tag, exe } => {
                self.status = trf("SwiftSim {} is installed.", &[&tag]);
                self.modals.push(Modal::Restart(tag, exe));
            }
            update::Out::UpToDate => {
                self.status = trf("SwiftSim {} is up to date.", &[&update::VERSION])
            }
            update::Out::AlreadyInstalled(tag) => {
                self.status = trf("SwiftSim {} is installed.", &[&tag])
            }
            update::Out::Found(tag) => {
                self.status = trf(
                    "SwiftSim {} is available (you have {}).",
                    &[&tag, &update::VERSION],
                )
            }
            update::Out::Cancelled => self.status = tr("Update cancelled.").into(),
            update::Out::Spawn { exe } => {
                let mut c = std::process::Command::new(&exe);
                c.args(&self.doc.path);
                match c.spawn() {
                    Ok(child) => self.child = Some((child, Instant::now())),
                    Err(e) => self.restart_failed(ctx, &exe, e.to_string()),
                }
            }
            update::Out::Quit => self.do_pending(Pending::Quit, ctx),
        }
    }

    /// The new version didn't start: the old program goes back in place.
    fn restart_failed(&mut self, ctx: &egui::Context, exe: &std::path::Path, why: String) {
        let back = match update::rollback(exe) {
            Ok(()) => tr("the previous version was restored").to_string(),
            Err(e) => e,
        };
        self.update_event(
            ctx,
            update::Event::Started(Err(trf(
                "Couldn't start {}: {}",
                &[&exe.display(), &format!("{why}; {back}")],
            ))),
        );
    }

    fn poll_updates(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.update_rx {
            let r = match rx.try_recv() {
                Ok(r) => Some(r),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    Some(Err("the update check stopped unexpectedly".into()))
                }
            };
            if let Some(r) = r {
                self.update_rx = None;
                let r = r.map(|o| {
                    o.map(|rel| {
                        let plan = update::plan(&rel);
                        (rel, plan)
                    })
                });
                self.update_event(ctx, update::Event::Checked(r));
            }
        }
        if let Some((_, _, rx)) = &self.install {
            ctx.request_repaint_after(Duration::from_millis(100));
            let r = match rx.try_recv() {
                Ok(r) => Some(r),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    Some(Err("the update stopped unexpectedly".into()))
                }
            };
            if let Some(r) = r {
                self.install = None;
                self.update_event(ctx, update::Event::Installed(r));
            }
        }
        if let Some((child, since)) = &mut self.child {
            ctx.request_repaint_after(Duration::from_millis(200));
            let exited = child.try_wait();
            let ok = since.elapsed() >= RESTART_GRACE;
            if let update::State::Restarting { exe } = &self.updater.state {
                let exe = exe.clone();
                match exited {
                    Ok(Some(st)) if !st.success() => {
                        self.child = None;
                        self.restart_failed(ctx, &exe, format!("exit status {st}"));
                    }
                    Ok(Some(_)) => {
                        self.child = None;
                        self.update_event(ctx, update::Event::Started(Ok(())));
                    }
                    _ if ok => {
                        self.child = None;
                        self.update_event(ctx, update::Event::Started(Ok(())));
                    }
                    _ => {}
                }
            } else {
                self.child = None;
            }
        }
    }

    /// The update banner: one look per update state.
    fn update_banner(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let mut ev = None;
        match &self.updater.state {
            update::State::Idle => {}
            update::State::Checking { manual } => {
                if *manual {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(tr("Checking for updates…"));
                    });
                }
            }
            update::State::Available { rel, plan } => {
                ui.horizontal(|ui| {
                    ui.colored_label(
                        Color32::from_rgb(102, 220, 120),
                        trf(
                            "Update available: SwiftSim {} (you have {})",
                            &[&rel.tag, &update::VERSION],
                        ),
                    );
                    if plan.is_some() && ui.button(tr("Install update")).clicked() {
                        ev = Some(update::Event::Install);
                    }
                    if ui.button(tr("View release")).clicked() {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(&rel.url));
                    }
                    if ui.button(tr("Dismiss")).clicked() {
                        ev = Some(update::Event::Dismiss);
                    }
                });
            }
            update::State::Installing {
                rel, cancelling, ..
            } => {
                let p = self
                    .install
                    .as_ref()
                    .map_or(0.0, |(p, _, _)| p.load(Ordering::Relaxed) as f32 / 1000.0);
                ui.horizontal(|ui| {
                    ui.colored_label(
                        Color32::from_rgb(102, 220, 120),
                        trf(
                            "Update available: SwiftSim {} (you have {})",
                            &[&rel.tag, &update::VERSION],
                        ),
                    );
                    ui.add(
                        egui::ProgressBar::new(p)
                            .desired_width(200.0)
                            .show_percentage(),
                    );
                    if *cancelling {
                        ui.add_enabled(false, egui::Button::new(tr("Stopping…")));
                        ui.spinner();
                    } else if ui.button(tr("Cancel")).clicked() {
                        ev = Some(update::Event::Cancel);
                    }
                });
            }
            update::State::Restarting { .. } => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(tr("Restarting…"));
                });
            }
            update::State::Failed { msg } => {
                ui.horizontal(|ui| {
                    ui.colored_label(
                        Color32::from_rgb(255, 99, 99),
                        trf("Update failed: {}", &[msg]),
                    );
                    if ui.button(tr("Dismiss")).clicked() {
                        ev = Some(update::Event::Dismiss);
                    }
                });
            }
        }
        if let Some(ev) = ev {
            self.update_event(&ctx, ev);
        }
    }

    /// The open dialog, if any (one at a time; see `state::Modals`).
    fn modal(&mut self, ctx: &egui::Context) {
        let mut close = false;
        match self.modals.open() {
            None => {}
            Some(Modal::Recover(f, s)) => {
                let (f, path) = (f.clone(), s.path.clone());
                let mut answer = None;
                egui::Modal::new(egui::Id::unique("recover")).show(ctx, |ui| i18n::mirrored(ui, |ui| {
                    ui.heading(tr("Recover unsaved work?"));
                    ui.label(para(ui, "SwiftSim closed without saving last time. A recovery copy of the unsaved document was found:"));
                    ui.monospace(path.as_ref().map_or(tr("untitled").into(), |p| p.display().to_string()));
                    ui.weak(para(ui, "Restoring opens it with unsaved changes; your file on disk stays as it was until you save."));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button(tr("Restore")).clicked() {
                            answer = Some(true);
                        }
                        if ui.button(tr("Discard")).clicked() {
                            answer = Some(false);
                        }
                    });
                }));
                match answer {
                    Some(true) => {
                        close = true;
                        match self.rec.adopt(&f) {
                            Ok(s) => self.restore(s),
                            Err(e) => {
                                self.status = trf("Cannot restore {}: {}", &[&f.display(), &e])
                            }
                        }
                    }
                    Some(false) => {
                        close = true;
                        self.rec.discard(&f);
                    }
                    None => {}
                }
            }
            Some(Modal::Restart(tag, exe)) => {
                let (tag, exe) = (tag.clone(), exe.clone());
                let mut answer = None;
                egui::Modal::new(egui::Id::unique("restart")).show(ctx, |ui| {
                    i18n::mirrored(ui, |ui| {
                        ui.heading(tr("Update installed"));
                        ui.label(trf(
                            "SwiftSim {} is installed. Restart now to start using it?",
                            &[&tag],
                        ));
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.button(tr("Restart now")).clicked() {
                                answer = Some(true);
                            }
                            if ui.button(tr("Later")).clicked() {
                                answer = Some(false);
                            }
                        });
                    })
                });
                if let Some(now) = answer {
                    self.modals.close();
                    if now {
                        self.request(Pending::Restart(exe), ctx);
                    }
                }
            }
            Some(Modal::Confirm(p)) => {
                let p = p.clone();
                egui::Modal::new(egui::Id::unique("confirm")).show(ctx, |ui| {
                    i18n::mirrored(ui, |ui| {
                        ui.heading(tr("Unsaved changes"));
                        ui.label(tr("Discard changes to the current document?"));
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.button(tr("Save")).clicked() {
                                self.save(false);
                                close = true;
                                if !self.dirty() {
                                    self.do_pending(p.clone(), ctx);
                                }
                            }
                            if ui.button(tr("Discard")).clicked() {
                                close = true;
                                self.do_pending(p.clone(), ctx);
                            }
                            if ui.button(tr("Cancel")).clicked() {
                                close = true;
                            }
                        });
                    })
                });
            }
        }
        if close {
            self.modals.close();
        }
    }

    fn frame(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.poll(&ctx);
        if self.doc.mode == Mode::Schematic {
            let v = self.ed.version();
            self.live.sync(v, || {
                swiftsim::netlist::numeric_params(&self.ed.netlist().text)
            });
        }
        // a run superseded by newer parameter values has stopped: start the newest
        if self.live.rerun && !self.sim.busy() {
            self.live.rerun = false;
            self.run_sim(&ctx);
        }
        self.agent_requests(&ctx);
        self.poll_updates(&ctx);
        if let Some(t) = self.cleanup_at {
            if Instant::now() >= t {
                self.cleanup_at = None;
                update::cleanup();
            } else {
                ctx.request_repaint_after(t - Instant::now());
            }
        }
        if let Some(s) = self.waves.status.take() {
            self.status = s;
        }
        if !self.dirty() {
            self.rec.remove();
        } else {
            if self.autosaved.elapsed() >= AUTOSAVE {
                self.autosave();
            }
            ctx.request_repaint_after(AUTOSAVE);
        }
        // global shortcuts
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F5)) {
            self.run_sim(&ctx);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F6)) {
            self.focus_params = true;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::S)) {
            self.save(false);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::O)) {
            self.request(Pending::Open, &ctx);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::N)) {
            self.request(Pending::New, &ctx);
        }
        if ctx.input(|i| i.viewport().close_requested()) && self.dirty() && !self.allow_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.modals.push(Modal::Confirm(Pending::Quit));
        }
        if let Some(mut sig) = self.ed.probe.take() {
            // after .noise a part's probe shows its noise contribution, V(<part>)
            let noise = self
                .results
                .as_ref()
                .and_then(|r| r.plots.first())
                .is_some_and(|p| p.kind == PlotKind::Noise);
            if let Some((_, r)) = sig
                .split_once('(')
                .filter(|(h, _)| noise && !h.eq_ignore_ascii_case("v"))
            {
                sig = format!("V({r}");
            }
            self.waves.add(&sig);
            self.tab = Tab::Waves;
        }
        let name = self
            .doc
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map_or(tr("untitled").into(), |n| n.to_string_lossy().to_string());
        let title = format!("{}{name} — SwiftSim", if self.dirty() { "● " } else { "" });
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }

        egui::Panel::top("menu").show(ui, |ui| {
            self.menu(ui);
            i18n::mirrored(ui, |ui| {
                self.toolbar(ui);
                self.update_banner(ui);
            });
            ui.add_space(2.0);
        });
        egui::Panel::bottom("status")
            .exact_size(22.0)
            .show(ui, |ui| {
                i18n::mirrored(ui, |ui| {
                    ui.horizontal_centered(|ui| {
                        ui.small(&self.status);
                    });
                });
            });
        egui::Panel::bottom("results")
            .resizable(true)
            .default_size(300.0)
            .min_size(120.0)
            .show(ui, |ui| {
                // egui panels adopt their content's size each frame; pin it to the panel rect so it can't creep
                let r = ui.max_rect();
                ui.allocate_rect(r, Sense::hover());
                self.bottom(
                    &mut ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(r)
                            .layout(egui::Layout::top_down(egui::Align::Min)),
                    ),
                );
            });
        if self.doc.mode == Mode::Schematic {
            // the parts list is on the reading side
            let (parts, props) = if i18n::is_rtl_ui() {
                (egui::Panel::right("palette"), egui::Panel::left("props"))
            } else {
                (egui::Panel::left("palette"), egui::Panel::right("props"))
            };
            parts
                .resizable(true)
                .default_size(200.0)
                .min_size(150.0)
                .show(ui, |ui| i18n::mirrored(ui, |ui| self.palette(ui)));
            props
                .resizable(true)
                .default_size(280.0)
                .min_size(220.0)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .show(ui, |ui| i18n::mirrored(ui, |ui| self.properties(ui)));
                });
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| match self.doc.mode {
                Mode::Schematic => self.ed.ui(ui),
                Mode::Netlist => {
                    egui::Frame::central_panel(ui.style()).show(ui, |ui| {
                        egui::ScrollArea::both().show(ui, |ui| {
                            let r = bidi::code(ui, &mut self.net_text, |e| {
                                e.desired_width(f32::INFINITY).desired_rows(40)
                            });
                            if r.changed() {
                                self.net_version += 1;
                            }
                        });
                    });
                }
            });

        self.modal(&ctx);
        let next = self.window.update(&self.status_json());
        ctx.request_repaint_after(next);
    }
}

/// Netlist of a saved schematic (`.sws`), for batch mode.
pub fn sws_netlist(text: &str) -> Result<(String, Vec<String>), String> {
    let s = load_schematic(text).map_err(|e| format!("not a SwiftSim schematic: {e}"))?;
    Ok((s.netlist().text, s.name_conflicts()))
}

/// A schematic from `.sws` text, brought to the current format and checked.
fn load_schematic(text: &str) -> Result<Schematic, String> {
    checked(serde_json::from_str(text).map_err(|e| e.to_string())?)
}

fn checked(mut s: Schematic) -> Result<Schematic, String> {
    s.upgrade();
    s.check()?;
    Ok(s)
}

/// Sends a request to the SwiftSim window of `pid` and waits for its answer (`gui_*` MCP tools).
pub fn ask_window(
    pid: u32,
    req: &serde_json::Value,
    timeout: Duration,
    cancelled: impl Fn() -> bool,
) -> Result<serde_json::Value, String> {
    let dir = session::ipc_dir(&config_dir()).map_err(|e| format!("no request folder: {e}"))?;
    session::ask(&dir, pid, req, timeout, cancelled)
}

/// States of the running SwiftSim windows (`gui_status` in the MCP server).
pub fn windows() -> Vec<serde_json::Value> {
    session::windows(&config_dir())
}

/// Settings folder: `%APPDATA%\SwiftSim` or `$XDG_CONFIG_HOME`/`~/.config/swiftsim` (temp folder as a last resort).
fn config_dir() -> PathBuf {
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    env("APPDATA")
        .map(|d| d.join("SwiftSim"))
        .or_else(|| {
            env("XDG_CONFIG_HOME")
                .or_else(|| env("HOME").map(|h| h.join(".config")))
                .map(|d| d.join("swiftsim"))
        })
        .unwrap_or_else(|| std::env::temp_dir().join("swiftsim"))
}

/// Saved LTspice library folder.
fn lib_setting() -> Option<PathBuf> {
    Some(config_dir().join("ltspice-lib.txt"))
}

/// Menu entry with its shortcut at the right edge.
fn item(ui: &mut egui::Ui, label: &str, shortcut: &str) -> egui::Response {
    if i18n::is_rtl_ui() {
        ui.add(egui::Button::new((
            RichText::new(shortcut).weak(),
            egui::Atom::grow(),
            label,
        )))
    } else {
        ui.add(egui::Button::new(label).shortcut_text(shortcut))
    }
}

fn is_analysis(l: &str) -> bool {
    let w = l.trim().to_ascii_lowercase();
    [".tran", ".ac", ".dc", ".op", ".tf", ".noise"]
        .iter()
        .any(|k| w == *k || w.starts_with(&format!("{k} ")))
}

#[derive(Clone, PartialEq, Default)]
struct SimForm {
    kind: usize,
    a: String,
    b: String,
    c: String,
    d: String,
    uic: bool,
    /// Noise: output and input source.
    out: String,
    src: String,
    /// Transient: the print step (kept as written; the form doesn't show it).
    step: String,
    /// Words the form doesn't model (a second .dc sweep, `startup`, …), kept at the end.
    extra: Vec<String>,
}

/// Words of an analysis line; a parenthesized group with spaces (`V(out, ref)`) stays one word.
fn analysis_words(line: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let open = |w: &str| w.matches('(').count() > w.matches(')').count();
    for w in line.split_whitespace() {
        match out.last_mut() {
            Some(last) if open(last) => {
                last.push(' ');
                last.push_str(w);
            }
            _ => out.push(w.to_string()),
        }
    }
    out
}

impl SimForm {
    const KINDS: [&str; 6] = [
        "Transient",
        "AC analysis",
        "DC sweep",
        "Operating point",
        "DC transfer function",
        "Noise",
    ];

    fn parse(line: &str) -> SimForm {
        let t = analysis_words(line);
        let g = |i: usize| t.get(i).cloned().unwrap_or_default();
        let rest = |i: usize| t.get(i..).unwrap_or_default().to_vec();
        match t.first().map(|s| s.to_ascii_lowercase()).as_deref() {
            Some(".ac") => SimForm {
                kind: 1,
                d: g(1),
                a: g(2),
                b: g(3),
                c: g(4),
                extra: rest(5),
                ..Default::default()
            },
            Some(".dc") => SimForm {
                kind: 2,
                d: g(1),
                a: g(2),
                b: g(3),
                c: g(4),
                extra: rest(5),
                ..Default::default()
            },
            Some(".op") => SimForm {
                kind: 3,
                extra: rest(1),
                ..Default::default()
            },
            Some(".tf") => SimForm {
                kind: 4,
                a: g(1),
                d: g(2),
                extra: rest(3),
                ..Default::default()
            },
            Some(".noise") => SimForm {
                kind: 5,
                out: g(1),
                src: g(2),
                d: g(3),
                a: g(4),
                b: g(5),
                c: g(6),
                extra: rest(7),
                ..Default::default()
            },
            Some(".tran") => {
                let uic = t.iter().any(|s| s.eq_ignore_ascii_case("uic"));
                let flag = |s: &String| {
                    s.parse::<f64>().is_err()
                        && swiftsim::expr::parse_number(s).is_none()
                        && !s.starts_with('{')
                };
                let n: Vec<String> = t[1..].iter().filter(|s| !flag(s)).cloned().collect();
                let extra = t[1..]
                    .iter()
                    .filter(|s| flag(s) && !s.eq_ignore_ascii_case("uic"))
                    .cloned()
                    .collect();
                let (step, stop, start, max) = match n.len() {
                    0 => (String::new(), String::new(), String::new(), String::new()),
                    1 => (String::new(), n[0].clone(), String::new(), String::new()),
                    _ => (
                        n[0].clone(),
                        n[1].clone(),
                        n.get(2).cloned().unwrap_or_default(),
                        n.get(3).cloned().unwrap_or_default(),
                    ),
                };
                SimForm {
                    kind: 0,
                    a: stop,
                    b: start,
                    c: max,
                    uic,
                    step,
                    extra,
                    ..Default::default()
                }
            }
            _ => SimForm {
                kind: 0,
                a: "10m".into(),
                ..Default::default()
            },
        }
    }

    fn defaults(kind: usize, src: &str) -> SimForm {
        let s = |v: &str| v.to_string();
        let (a, b, c, d) = match kind {
            0 => (s("10m"), s(""), s(""), s("")),
            1 | 5 => (s("100"), s("1"), s("1meg"), s("dec")),
            2 => (s("0"), s("5"), s("0.1"), s(src)),
            4 => (s("V(out)"), s(""), s(""), s(src)),
            _ => (s(""), s(""), s(""), s("")),
        };
        let (out, src) = if kind == 5 {
            (s("V(out)"), s(src))
        } else {
            (s(""), s(""))
        };
        SimForm {
            kind,
            a,
            b,
            c,
            d,
            out,
            src,
            ..Default::default()
        }
    }

    fn line(&self) -> String {
        let mut l = self.modeled();
        for w in &self.extra {
            l += " ";
            l += w;
        }
        l
    }

    fn modeled(&self) -> String {
        let or = |s: &str, d: &str| {
            if s.trim().is_empty() {
                d.to_string()
            } else {
                s.trim().to_string()
            }
        };
        match self.kind {
            0 => {
                let mut l = format!(".tran {} {}", or(&self.step, "0"), or(&self.a, "10m"));
                if !self.b.trim().is_empty() || !self.c.trim().is_empty() {
                    l += &format!(" {}", or(&self.b, "0"));
                }
                if !self.c.trim().is_empty() {
                    l += &format!(" {}", self.c.trim());
                }
                if self.uic {
                    l += " uic";
                }
                l
            }
            1 => format!(
                ".ac {} {} {} {}",
                or(&self.d, "dec"),
                or(&self.a, "100"),
                or(&self.b, "1"),
                or(&self.c, "1meg")
            ),
            2 => format!(
                ".dc {} {} {} {}",
                or(&self.d, "V1"),
                or(&self.a, "0"),
                or(&self.b, "5"),
                or(&self.c, "0.1")
            ),
            4 => format!(".tf {} {}", or(&self.a, "V(out)"), or(&self.d, "V1")),
            5 => format!(
                ".noise {} {} {} {} {} {}",
                or(&self.out, "V(out)"),
                or(&self.src, "V1"),
                or(&self.d, "dec"),
                or(&self.a, "100"),
                or(&self.b, "1"),
                or(&self.c, "1meg")
            ),
            _ => ".op".into(),
        }
    }
}

const PULSE: [&str; 8] = [
    "V initial",
    "V on",
    "T delay",
    "T rise",
    "T fall",
    "T on",
    "T period",
    "N cycles",
];
const SINE: [&str; 7] = [
    "DC offset",
    "Amplitude",
    "Freq",
    "T delay",
    "Theta (1/s)",
    "Phase (°)",
    "N cycles",
];
const EXP: [&str; 6] = ["V1", "V2", "T delay 1", "Tau 1", "T delay 2", "Tau 2"];

/// Structured editor for V/I source values. Returns a new value on change.
fn source_form(ui: &mut egui::Ui, value: &str, volt: bool) -> Option<String> {
    let toks = tokenize(value);
    let (mut func, mut args, mut dc, mut ac) =
        (String::from("dc"), vec![], String::new(), String::new());
    let mut i = 0;
    while i < toks.len() {
        let t = toks[i].to_ascii_lowercase();
        match t.as_str() {
            "dc" => {
                dc = toks.get(i + 1).cloned().unwrap_or_default();
                i += 2;
            }
            "ac" => {
                ac = toks.get(i + 1).cloned().unwrap_or_default();
                i += 2;
                if toks
                    .get(i)
                    .is_some_and(|x| swiftsim::expr::parse_number(x).is_some())
                {
                    i += 1;
                }
            }
            "pulse" | "sin" | "sine" | "exp" | "pwl" => {
                func = if t == "sin" { "sine".into() } else { t.clone() };
                i += 1;
                if toks.get(i).is_some_and(|x| x == "(") {
                    i += 1;
                }
                while i < toks.len() && toks[i] != ")" {
                    args.push(toks[i].clone());
                    i += 1;
                }
                i += 1;
            }
            _ => {
                dc = toks[i].clone();
                i += 1;
            }
        }
    }
    let (f0, a0, d0, ac0) = (func.clone(), args.clone(), dc.clone(), ac.clone());
    let unit = if volt { "V" } else { "A" };
    ui.label(RichText::new(tr("Waveform")).strong());
    ui.horizontal(|ui| {
        for (k, l) in [
            ("dc", "DC"),
            ("pulse", "Pulse"),
            ("sine", "Sine"),
            ("exp", "Exp"),
            ("pwl", "PWL"),
        ] {
            ui.selectable_value(&mut func, k.to_string(), tr(l));
        }
    });
    egui::Grid::new("srcf")
        .num_columns(2)
        .spacing([8.0, 4.0])
        .show(ui, |ui| {
            let labels: &[&str] = match func.as_str() {
                "pulse" => &PULSE,
                "sine" => &SINE,
                "exp" => &EXP,
                _ => &[],
            };
            if func == "dc" {
                i18n::form_row(ui, &trf("DC value ({})", &[&unit]), |ui| {
                    ui.text_edit_singleline(&mut dc);
                });
            } else if func == "pwl" {
                i18n::form_row(ui, "t1 v1 t2 v2 …", |ui| {
                    let mut s = args.join(" ");
                    if ui.text_edit_singleline(&mut s).changed() {
                        args = s.split_whitespace().map(|x| x.to_string()).collect();
                    }
                });
            } else {
                args.resize(args.len().max(labels.len().min(3)), String::new());
                for (k, l) in labels.iter().enumerate() {
                    i18n::form_row(ui, tr(*l), |ui| {
                        let mut v = args.get(k).cloned().unwrap_or_default();
                        if ui.text_edit_singleline(&mut v).changed() {
                            if args.len() <= k {
                                args.resize(k + 1, String::new());
                            }
                            args[k] = v;
                        }
                    });
                }
            }
            i18n::form_row(ui, tr("AC amplitude"), |ui| {
                ui.text_edit_singleline(&mut ac);
            });
        });
    if (&func, &args, &dc, &ac) == (&f0, &a0, &d0, &ac0) {
        return None;
    }
    while args.last().is_some_and(|a| a.trim().is_empty()) {
        args.pop();
    }
    let mut out = vec![];
    if func == "dc" || !dc.trim().is_empty() && func == "dc" {
        out.push(if dc.trim().is_empty() {
            "0".to_string()
        } else {
            dc.trim().to_string()
        });
    } else {
        let a: Vec<String> = args
            .iter()
            .map(|a| {
                if a.trim().is_empty() {
                    "0".to_string()
                } else {
                    a.trim().to_string()
                }
            })
            .collect();
        out.push(format!("{}({})", func.to_uppercase(), a.join(" ")));
    }
    if !ac.trim().is_empty() {
        out.push(format!("AC {}", ac.trim()));
    }
    Some(out.join(" "))
}

fn draw_icon(p: &egui::Painter, r: egui::Rect, k: Kind, col: Color32) {
    use schematic::Prim;
    let (a, b) = schematic::bbox(k);
    let s = (r.width() / (b[0] - a[0]).max(b[1] - a[1]).max(2.5)) * 0.9;
    let c = r.center() - Vec2::new((a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0) * s;
    let tf = |q: [f32; 2]| c + Vec2::new(q[0], q[1]) * s;
    let st = Stroke::new(1.3, col);
    for prim in schematic::symbol(k) {
        match prim {
            Prim::Line(v) => {
                p.add(Shape::line(v.into_iter().map(tf).collect(), st));
            }
            Prim::Poly(v) => {
                p.add(Shape::convex_polygon(
                    v.into_iter().map(tf).collect(),
                    col,
                    Stroke::NONE,
                ));
            }
            Prim::Circle(ctr, rad) => {
                p.circle_stroke(tf(ctr), rad * s, st);
            }
            Prim::Text(..) => {}
        }
    }
    if k == Kind::Label {
        p.text(
            r.center(),
            egui::Align2::CENTER_CENTER,
            "net",
            egui::FontId::proportional(11.0),
            col,
        );
    }
}

fn comp(kind: Kind, name: &str, value: &str, pos: [i32; 2], rot: u8) -> Component {
    Component {
        kind,
        name: name.into(),
        value: value.into(),
        pos,
        rot,
        mirror: false,
        pins: vec![],
    }
}

fn wires(pts: &[[i32; 2]]) -> Vec<Wire> {
    pts.windows(2).map(|w| Wire { a: w[0], b: w[1] }).collect()
}

fn examples() -> Vec<(&'static str, Schematic)> {
    let g = |x, y| comp(Kind::Ground, "", "", [x, y], 0);
    let lbl = |v: &str, x, y| comp(Kind::Label, "", v, [x, y], 0);
    let txt = |x, y, t: &str| Text {
        pos: [x, y],
        text: t.into(),
    };
    let rc = Schematic {
        comps: vec![
            comp(
                Kind::Voltage,
                "V1",
                "PULSE(0 1 0 1u 1u 5m 10m) AC 1",
                [0, 0],
                0,
            ),
            comp(Kind::Resistor, "R1", "1k", [7, -4], 1),
            comp(Kind::Capacitor, "C1", "1u", [14, 0], 0),
            g(0, 2),
            g(14, 2),
            lbl("in", 0, -4),
            lbl("out", 14, -4),
        ],
        wires: [
            wires(&[[0, -2], [0, -4], [5, -4]]),
            wires(&[[9, -4], [14, -4], [14, -2]]),
        ]
        .concat(),
        texts: vec![txt(0, 6, ".tran 20m\n* switch to AC: .ac dec 50 1 100k")],
        ..Default::default()
    };
    let ce = Schematic {
        comps: vec![
            comp(Kind::Voltage, "Vcc", "12", [-4, -6], 0),
            g(-4, -4),
            comp(Kind::Resistor, "R1", "47k", [4, -4], 0),
            comp(Kind::Resistor, "R2", "10k", [4, 4], 0),
            g(4, 6),
            comp(Kind::Resistor, "Rc", "4.7k", [11, -6], 0),
            comp(Kind::Resistor, "Re", "1k", [11, 6], 0),
            g(11, 8),
            comp(Kind::Npn, "Q1", "2N3904", [10, 0], 0),
            comp(Kind::Capacitor, "Cin", "10u", [0, 0], 1),
            comp(Kind::Voltage, "Vin", "SINE(0 10m 1k) AC 1", [-4, 4], 0),
            g(-4, 6),
            lbl("in", -4, 0),
            lbl("out", 13, -3),
        ],
        wires: [
            wires(&[[-4, -8], [4, -8], [11, -8]]),
            wires(&[[4, -6], [4, -8]]),
            wires(&[[4, -2], [4, 2]]),
            wires(&[[4, 0], [8, 0]]),
            wires(&[[11, -4], [11, -2]]),
            wires(&[[11, 2], [11, 4]]),
            wires(&[[-4, 2], [-4, 0], [-2, 0]]),
            wires(&[[2, 0], [4, 0]]),
            wires(&[[11, -3], [13, -3]]),
        ]
        .concat(),
        texts: vec![txt(-4, 11, ".tran 5m")],
        ..Default::default()
    };
    // inverting amplifier, gain -10: + (top) to ground, Rin and Rf on − (bottom)
    let opamp = Schematic {
        comps: vec![
            comp(Kind::OpAmp, "U1", "Aol=100k GBW=10Meg", [10, 0], 0),
            comp(Kind::Resistor, "Rin", "1k", [4, 1], 1),
            comp(Kind::Resistor, "Rf", "10k", [10, 5], 1),
            comp(Kind::Ground, "", "", [8, -3], 2),
            comp(Kind::Voltage, "Vin", "SINE(0 0.5 1k) AC 1", [0, 3], 0),
            g(0, 5),
            lbl("in", 0, 1),
            lbl("out", 14, 0),
        ],
        wires: [
            wires(&[[0, 1], [2, 1]]),
            wires(&[[6, 1], [8, 1]]),
            wires(&[[8, 5], [8, 1]]),
            wires(&[[12, 5], [12, 0]]),
            wires(&[[8, -1], [8, -3]]),
            wires(&[[12, 0], [14, 0]]),
        ]
        .concat(),
        texts: vec![txt(0, 9, ".tran 3m\n.meas tran gain PARAM -10")],
        ..Default::default()
    };
    let rect = Schematic {
        comps: vec![
            comp(Kind::Voltage, "V1", "SINE(0 10 50)", [0, 0], 0),
            g(0, 2),
            comp(Kind::Diode, "D1", "1N4007", [4, -4], 1),
            comp(Kind::Capacitor, "C1", "100u", [8, 0], 0),
            g(8, 2),
            comp(Kind::Resistor, "R1", "1k", [12, 0], 0),
            g(12, 2),
            lbl("out", 10, -4),
        ],
        wires: [
            wires(&[[0, -2], [0, -4], [2, -4]]),
            wires(&[[6, -4], [8, -4], [8, -2]]),
            wires(&[[8, -4], [12, -4], [12, -2]]),
        ]
        .concat(),
        texts: vec![txt(
            0,
            6,
            ".tran 100m\n.meas tran ripple PP V(out) FROM=60m TO=100m",
        )],
        ..Default::default()
    };
    vec![
        ("RC low-pass filter", rc),
        ("BJT common-emitter amplifier", ce),
        ("Op-amp inverting amplifier", opamp),
        ("Half-wave rectifier", rect),
    ]
}

#[cfg(test)]
mod tests {
    #[test]
    fn sponsor_link_is_only_in_the_help_menu() {
        assert_eq!(
            super::SPONSOR_URL,
            "https://github.com/sponsors/RamazanBerk20"
        );
        let src = include_str!("mod.rs");
        let help = src.find("i18n::menu(ui, tr(\"Help\")").unwrap();
        let end = help + src[help..].find("Copy MCP server command").unwrap();
        let uses: Vec<_> = src
            .match_indices(concat!("new_tab(SPONSOR", "_URL)"))
            .collect();
        assert!(uses.len() == 1 && (help..end).contains(&uses[0].0));
    }

    #[test]
    fn examples_simulate() {
        for (name, s) in super::examples() {
            let n = s.netlist();
            let r = swiftsim::run_netlist(&n.text, None, &swiftsim::Control::default())
                .unwrap_or_else(|e| panic!("{name}: {e}\n{}", n.text));
            assert!(!r.plots.is_empty(), "{name}");
            let p = &r.plots[0];
            assert!(
                p.index("v(out)").is_some(),
                "{name}: {:?}\n{}",
                p.names,
                n.text
            );
            if name.starts_with("Op-amp") {
                // inverting gain -10: at the input's positive peak (t = 0.25 ms) the output is -5 V
                let k = p.x.iter().position(|&t| t >= 2.5e-4).unwrap();
                let (vin, vout) = (
                    p.re[p.index("v(in)").unwrap()][k],
                    p.re[p.index("v(out)").unwrap()][k],
                );
                assert!(
                    (vout / vin + 10.0).abs() < 0.05,
                    "{name}: gain {}",
                    vout / vin
                );
            }
        }
    }

    #[test]
    fn schematic_file_roundtrip() {
        for (_, s) in super::examples() {
            let json = serde_json::to_string(&s).unwrap();
            let back: super::Schematic = serde_json::from_str(&json).unwrap();
            assert_eq!(back.netlist().text, s.netlist().text);
        }
    }

    #[test]
    fn sim_form_roundtrip() {
        let f = super::SimForm::parse(".tran 0 5m 1m 1u uic");
        assert_eq!(f.line(), ".tran 0 5m 1m 1u uic");
        assert_eq!(
            super::SimForm::parse(".ac dec 20 10 1meg").line(),
            ".ac dec 20 10 1meg"
        );
        assert_eq!(
            super::SimForm::defaults(1, "V1").line(),
            ".ac dec 100 1 1meg"
        );
        assert_eq!(super::SimForm::defaults(2, "Vin").line(), ".dc Vin 0 5 0.1");
        assert_eq!(
            super::SimForm::parse(".tf V(out) Vin").line(),
            ".tf V(out) Vin"
        );
        assert_eq!(
            super::SimForm::parse(".noise V(out) Vin oct 10 10 100k").line(),
            ".noise V(out) Vin oct 10 10 100k"
        );
        assert_eq!(
            super::SimForm::defaults(5, "Vin").line(),
            ".noise V(out) Vin dec 100 1 1meg"
        );
        // what the form doesn't show survives an edit of what it does
        let edit = |l: &str| {
            let mut f = super::SimForm::parse(l);
            f.a = "7m".into();
            f.line()
        };
        assert_eq!(edit(".tran 1u 5m"), ".tran 1u 7m");
        assert_eq!(
            edit(".tran 1u 5m 0 2u startup uic"),
            ".tran 1u 7m 0 2u uic startup"
        );
        assert_eq!(
            edit(".dc V1 0 5 0.1 V2 0 1 0.5"),
            ".dc V1 7m 5 0.1 V2 0 1 0.5"
        );
        assert_eq!(
            edit(".noise V(out, ref) Vin dec 10 1 1k"),
            ".noise V(out, ref) Vin dec 7m 1 1k"
        );
        assert_eq!(edit(".tran {tstop}"), ".tran 0 7m");
    }
}
