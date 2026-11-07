//! Desktop application shell.

mod editor;
mod schematic;
mod waveform;

use eframe::egui;
use editor::{Editor, Tool};
use egui::{Color32, CornerRadius, Key, Modifiers, RichText, Sense, Shape, Stroke, Vec2};
use schematic::{Component, Kind, Schematic, Text, Wire};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::Instant;
use swiftsim::netlist::tokenize;
use swiftsim::{Control, PlotKind, SimResult};
use waveform::WaveView;

pub fn run(file: Option<String>) -> eframe::Result {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1440.0, 900.0]).with_min_inner_size([900.0, 600.0]).with_title("SwiftSim"),
        ..Default::default()
    };
    eframe::run_native("SwiftSim", opts, Box::new(move |cc| Ok(Box::new(App::new(cc, file)))))
}

#[derive(PartialEq, Clone, Copy)]
enum Mode {
    Schematic,
    Netlist,
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Waves,
    Netlist,
    Log,
}

#[derive(Clone, Copy)]
enum Pending {
    New,
    Open,
    Example(usize),
    Quit,
}

struct Job {
    ctl: Arc<Control>,
    rx: Receiver<Result<SimResult, String>>,
    started: Instant,
}

struct App {
    ed: Editor,
    mode: Mode,
    net_text: String,
    net_dirty: bool,
    path: Option<PathBuf>,
    tab: Tab,
    waves: WaveView,
    results: Option<SimResult>,
    log: Vec<String>,
    job: Option<Job>,
    filter: String,
    confirm: Option<Pending>,
    allow_close: bool,
    status: String,
    title: String,
    dark: bool,
}

impl App {
    fn new(cc: &eframe::CreationContext, file: Option<String>) -> App {
        cc.egui_ctx.set_theme(egui::Theme::Dark);
        let mut app = App {
            ed: Editor::default(),
            mode: Mode::Schematic,
            net_text: String::new(),
            net_dirty: false,
            path: None,
            tab: Tab::Waves,
            waves: WaveView::default(),
            results: None,
            log: vec![],
            job: None,
            filter: String::new(),
            confirm: None,
            allow_close: false,
            status: "Ready. Pick a part on the left (or press R, C, L, V, G, W…), press F5 to simulate.".into(),
            title: String::new(),
            dark: true,
        };
        match file {
            Some(f) => app.open_path(PathBuf::from(f)),
            None => app.ed.load(examples()[0].1.clone()),
        }
        app.ed.dirty = false;
        app
    }

    fn dirty(&self) -> bool {
        match self.mode {
            Mode::Schematic => self.ed.dirty,
            Mode::Netlist => self.net_dirty,
        }
    }

    fn open_path(&mut self, p: PathBuf) {
        let text = match std::fs::read_to_string(&p) {
            Ok(t) => t,
            Err(e) => {
                self.status = format!("Cannot open {}: {e}", p.display());
                return;
            }
        };
        if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("sws")) {
            match serde_json::from_str::<Schematic>(&text) {
                Ok(s) => {
                    self.mode = Mode::Schematic;
                    self.ed.load(s);
                }
                Err(e) => {
                    self.status = format!("Not a SwiftSim schematic: {e}");
                    return;
                }
            }
        } else {
            self.mode = Mode::Netlist;
            self.net_text = text;
            self.net_dirty = false;
            self.tab = Tab::Log;
        }
        self.status = format!("Opened {}", p.display());
        self.path = Some(p);
        self.results = None;
        self.waves.traces.clear();
    }

    fn save(&mut self, as_new: bool) {
        let schematic = self.mode == Mode::Schematic;
        let path = match (&self.path, as_new) {
            (Some(p), false) => p.clone(),
            _ => {
                let d = rfd::FileDialog::new();
                let d = if schematic { d.add_filter("SwiftSim schematic", &["sws"]).set_file_name("circuit.sws") } else { d.add_filter("SPICE netlist", &["cir", "net", "sp", "spi"]).set_file_name("circuit.cir") };
                match d.save_file() {
                    Some(p) => p,
                    None => return,
                }
            }
        };
        let data = if schematic { serde_json::to_string_pretty(&self.ed.sch).unwrap_or_default() } else { self.net_text.clone() };
        match std::fs::write(&path, data) {
            Ok(()) => {
                self.status = format!("Saved {}", path.display());
                self.path = Some(path);
                self.ed.dirty = false;
                self.net_dirty = false;
            }
            Err(e) => self.status = format!("Save failed: {e}"),
        }
    }

    fn do_pending(&mut self, p: Pending, ctx: &egui::Context) {
        match p {
            Pending::New => {
                self.mode = Mode::Schematic;
                self.ed.load(Schematic::default());
                self.path = None;
                self.results = None;
                self.waves.traces.clear();
            }
            Pending::Open => {
                if let Some(p) = rfd::FileDialog::new()
                    .add_filter("All supported", &["sws", "cir", "net", "sp", "spi", "txt"])
                    .add_filter("SwiftSim schematic", &["sws"])
                    .add_filter("SPICE netlist", &["cir", "net", "sp", "spi", "txt"])
                    .pick_file()
                {
                    self.open_path(p);
                }
            }
            Pending::Example(i) => {
                self.mode = Mode::Schematic;
                self.ed.load(examples()[i].1.clone());
                self.path = None;
                self.results = None;
                self.waves.traces.clear();
            }
            Pending::Quit => {
                self.allow_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    fn request(&mut self, p: Pending, ctx: &egui::Context) {
        if self.dirty() { self.confirm = Some(p) } else { self.do_pending(p, ctx) }
    }

    fn netlist_text(&mut self) -> String {
        match self.mode {
            Mode::Schematic => self.ed.netlist().text.clone(),
            Mode::Netlist => self.net_text.clone(),
        }
    }

    fn run_sim(&mut self, ctx: &egui::Context) {
        if self.job.is_some() {
            return;
        }
        let text = self.netlist_text();
        let base = self.path.as_ref().and_then(|p| p.parent().map(|d| d.to_path_buf()));
        let ctl = Arc::new(Control::default());
        let (tx, rx) = channel();
        let (c2, ctx2) = (ctl.clone(), ctx.clone());
        std::thread::spawn(move || {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| swiftsim::run_netlist(&text, base.as_deref(), &c2)));
            let _ = tx.send(r.unwrap_or_else(|_| Err("internal simulator error (panic) — please report this netlist".into())));
            ctx2.request_repaint();
        });
        self.job = Some(Job { ctl, rx, started: Instant::now() });
        self.status = "Simulating…".into();
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let Some(job) = &self.job else { return };
        match job.rx.try_recv() {
            Err(TryRecvError::Empty) => ctx.request_repaint_after(std::time::Duration::from_millis(60)),
            Err(TryRecvError::Disconnected) => {
                self.job = None;
                self.status = "Simulation thread stopped unexpectedly".into();
            }
            Ok(r) => {
                let secs = job.started.elapsed().as_secs_f64();
                self.job = None;
                match r {
                    Ok(res) => {
                        self.log = res.log.clone();
                        let kind = res.plots.first().map(|p| p.kind);
                        self.ed.op_values.clear();
                        if kind == Some(PlotKind::Op) {
                            let p = &res.plots[0];
                            for (n, v) in p.names.iter().zip(&p.re) {
                                if let Some(net) = n.strip_prefix("v(").and_then(|s| s.strip_suffix(')')) {
                                    self.ed.op_values.insert(net.to_string(), v[0]);
                                }
                            }
                            self.tab = Tab::Log;
                        } else {
                            if self.waves.traces.is_empty() && self.mode == Mode::Schematic {
                                let labels: Vec<String> = self.ed.sch.comps.iter().filter(|c| c.kind == Kind::Label).map(|c| format!("V({})", c.value.trim())).collect();
                                for l in labels.into_iter().take(4) {
                                    self.waves.add(&l);
                                }
                            }
                            self.tab = Tab::Waves;
                            if self.ed.tool == Tool::Select && self.mode == Mode::Schematic {
                                self.ed.set_tool(Tool::Probe);
                            }
                        }
                        self.waves.generation += 1;
                        let warn = res.log.iter().filter(|l| l.starts_with("warning")).count();
                        self.status = format!(
                            "Done in {secs:.2} s{}{}",
                            if warn > 0 { format!(" · {warn} warning(s), see Log") } else { String::new() },
                            if self.ed.tool == Tool::Probe { " · Probe: click a wire for voltage, a part for current (Esc to edit)" } else { "" }
                        );
                        self.results = Some(res);
                    }
                    Err(e) => {
                        self.log = vec![format!("error: {e}")];
                        self.tab = Tab::Log;
                        self.status = format!("Simulation failed: {e}");
                    }
                }
            }
        }
    }

    fn menu(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("New schematic       Ctrl+N").clicked() {
                    self.request(Pending::New, &ctx);
                    ui.close();
                }
                if ui.button("Open…               Ctrl+O").clicked() {
                    self.request(Pending::Open, &ctx);
                    ui.close();
                }
                if ui.button("Save                Ctrl+S").clicked() {
                    self.save(false);
                    ui.close();
                }
                if ui.button("Save as…").clicked() {
                    self.save(true);
                    ui.close();
                }
                ui.separator();
                if self.mode == Mode::Schematic && ui.button("Export SPICE netlist…").clicked() {
                    if let Some(p) = rfd::FileDialog::new().add_filter("SPICE netlist", &["cir"]).set_file_name("circuit.cir").save_file() {
                        let t = self.ed.netlist().text.clone();
                        self.status = match std::fs::write(&p, t) {
                            Ok(()) => format!("Exported {}", p.display()),
                            Err(e) => format!("Export failed: {e}"),
                        };
                    }
                    ui.close();
                }
                if ui.button("Open netlist as text editor").clicked() {
                    let t = self.netlist_text();
                    self.mode = Mode::Netlist;
                    self.net_text = t;
                    self.net_dirty = true;
                    self.path = None;
                    ui.close();
                }
                ui.separator();
                if ui.button("Quit").clicked() {
                    self.request(Pending::Quit, &ctx);
                    ui.close();
                }
            });
            ui.menu_button("Edit", |ui| {
                let ed = &mut self.ed;
                if ui.button("Undo            Ctrl+Z").clicked() {
                    ed.undo();
                }
                if ui.button("Redo            Ctrl+Y").clicked() {
                    ed.redo();
                }
                ui.separator();
                if ui.button("Cut             Ctrl+X").clicked() {
                    ed.copy();
                    ed.delete_selected();
                }
                if ui.button("Copy            Ctrl+C").clicked() {
                    ed.copy();
                }
                if ui.button("Paste           Ctrl+V").clicked() {
                    ed.paste();
                }
                if ui.button("Delete          Del").clicked() {
                    ed.delete_selected();
                }
                if ui.button("Select all      Ctrl+A").clicked() {
                    ed.select_all();
                }
                ui.separator();
                if ui.button("Rotate          Ctrl+R").clicked() {
                    ed.rotate();
                }
                if ui.button("Mirror          Ctrl+E").clicked() {
                    ed.mirror();
                }
            });
            ui.menu_button("View", |ui| {
                if ui.button("Zoom to fit     Space").clicked() {
                    self.ed.fit();
                    ui.close();
                }
                ui.checkbox(&mut self.ed.show_op, "Show operating-point voltages");
                if ui.checkbox(&mut self.dark, "Dark theme").changed() {
                    ui.ctx().set_theme(if self.dark { egui::Theme::Dark } else { egui::Theme::Light });
                }
            });
            ui.menu_button("Simulate", |ui| {
                if ui.button("Run             F5").clicked() {
                    self.run_sim(&ctx);
                    ui.close();
                }
                if ui.add_enabled(self.job.is_some(), egui::Button::new("Stop")).clicked() {
                    if let Some(j) = &self.job {
                        j.ctl.cancel.store(true, Ordering::Relaxed);
                    }
                    ui.close();
                }
            });
            ui.menu_button("Examples", |ui| {
                for (i, (name, _)) in examples().iter().enumerate() {
                    if ui.button(*name).clicked() {
                        self.request(Pending::Example(i), &ctx);
                        ui.close();
                    }
                }
            });
            ui.menu_button("Help", |ui| {
                ui.label(RichText::new("Keyboard").strong());
                for (k, d) in [
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
                ] {
                    ui.horizontal(|ui| {
                        ui.monospace(format!("{k:<18}"));
                        ui.label(d);
                    });
                }
                ui.separator();
                ui.label("Supported: .op .tran .ac .dc .step .meas .param .ic .model .subckt .include/.lib .options");
                ui.label("Elements: R C L K V I D Q M(level1/VDMOS) E F G H B S X");
            });
        });
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.horizontal(|ui| {
            if let Some(j) = &self.job {
                if ui.button(RichText::new("■ Stop").color(Color32::from_rgb(255, 99, 99))).clicked() {
                    j.ctl.cancel.store(true, Ordering::Relaxed);
                }
                let p = j.ctl.progress.load(Ordering::Relaxed) as f32 / 1000.0;
                ui.add(egui::ProgressBar::new(p).desired_width(160.0).show_percentage());
            } else if ui.button(RichText::new("▶ Run").color(Color32::from_rgb(102, 220, 120)).strong()).on_hover_text("Run simulation (F5)").clicked() {
                self.run_sim(&ctx);
            }
            ui.separator();
            if self.mode == Mode::Schematic {
                let t = self.ed.tool;
                for (label, tool, tip) in [
                    ("Select", Tool::Select, "Select / move (Esc)"),
                    ("Wire", Tool::Wire, "Draw wires (W)"),
                    ("Ground", Tool::Place(Kind::Ground), "Ground (G)"),
                    ("Label", Tool::Place(Kind::Label), "Net label (N)"),
                    (".Text", Tool::Text, "SPICE directive (T)"),
                    ("Probe", Tool::Probe, "Click wire → voltage, part → current (P)"),
                ] {
                    if ui.selectable_label(t == tool, label).on_hover_text(tip).clicked() {
                        self.ed.set_tool(tool);
                    }
                }
                ui.separator();
                if ui.button("⟲").on_hover_text("Undo (Ctrl+Z)").clicked() {
                    self.ed.undo();
                }
                if ui.button("⟳").on_hover_text("Redo (Ctrl+Y)").clicked() {
                    self.ed.redo();
                }
                if ui.button("Rotate").on_hover_text("Ctrl+R").clicked() {
                    self.ed.rotate();
                }
                if ui.button("Mirror").on_hover_text("Ctrl+E").clicked() {
                    self.ed.mirror();
                }
                if ui.button("Delete").on_hover_text("Del").clicked() {
                    self.ed.delete_selected();
                }
                if ui.button("Fit").on_hover_text("Space").clicked() {
                    self.ed.fit();
                }
            } else {
                ui.label(RichText::new("Netlist mode").strong());
                if ui.button("New schematic").clicked() {
                    self.request(Pending::New, &ctx);
                }
            }
        });
    }

    fn palette(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Search parts…"));
        ui.add_space(4.0);
        let f = self.filter.to_ascii_lowercase();
        egui::ScrollArea::vertical().show(ui, |ui| {
            for cat in ["Passive", "Semiconductor", "Sources", "Blocks", "Wiring"] {
                let items: Vec<Kind> = Kind::ALL.into_iter().filter(|k| k.category() == cat && k.title().to_ascii_lowercase().contains(&f)).collect();
                if items.is_empty() {
                    continue;
                }
                ui.label(RichText::new(cat).small().weak());
                for k in items {
                    let active = self.ed.tool == Tool::Place(k);
                    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 34.0), Sense::click());
                    let vis = ui.style().interact_selectable(&resp, active);
                    if active || resp.hovered() {
                        ui.painter().rect_filled(rect, CornerRadius::same(4), vis.bg_fill);
                    }
                    let icon = egui::Rect::from_min_size(rect.min + Vec2::new(4.0, 3.0), Vec2::splat(28.0));
                    draw_icon(ui.painter(), icon, k, vis.text_color());
                    ui.painter().text(rect.min + Vec2::new(40.0, 17.0), egui::Align2::LEFT_CENTER, k.title(), egui::FontId::proportional(13.0), vis.text_color());
                    if let Some(s) = k.shortcut() {
                        ui.painter().text(rect.right_center() - Vec2::new(8.0, 0.0), egui::Align2::RIGHT_CENTER, s, egui::FontId::monospace(11.0), ui.visuals().weak_text_color());
                    }
                    if resp.clicked() {
                        self.ed.set_tool(Tool::Place(k));
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
            ui.heading("SPICE directive");
            ui.weak("Lines are added to the netlist as-is.");
            let mut t = self.ed.sch.texts[i].text.clone();
            let r = ui.add(egui::TextEdit::multiline(&mut t).code_editor().desired_rows(6).desired_width(f32::INFINITY));
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
            ui.heading("Selection");
            ui.label(format!("{} part(s), {} wire(s), {} text(s)", sel.comps.len(), sel.wires.len(), sel.texts.len()));
        } else {
            ui.heading("Properties");
            ui.weak("Select a part to edit it. Double-click jumps to its value.");
        }
        ui.add_space(12.0);
        ui.separator();
        self.sim_card(ui);
    }

    fn comp_props(&mut self, ui: &mut egui::Ui, i: usize, focus: bool) {
        let c = self.ed.sch.comps[i].clone();
        ui.heading(c.kind.title());
        let mut changed: Option<Component> = None;
        egui::Grid::new("props").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            if !c.kind.prefix().is_empty() {
                ui.label("Name");
                let mut n = c.name.clone();
                let r = ui.text_edit_singleline(&mut n);
                if r.changed() {
                    changed = Some(Component { name: n, ..c.clone() });
                }
                ui.end_row();
            }
            if c.kind != Kind::Ground {
                ui.label(if c.kind == Kind::Label { "Net name" } else { "Value" });
                let mut v = c.value.clone();
                let r = ui.add(egui::TextEdit::singleline(&mut v).desired_width(180.0));
                if focus {
                    r.request_focus();
                }
                if r.changed() {
                    changed = Some(Component { value: v, ..changed.clone().unwrap_or(c.clone()) });
                }
                ui.end_row();
            }
            let kinds = c.kind.model_kinds();
            if !kinds.is_empty() {
                ui.label("Model");
                let mut names = swiftsim::netlist::builtin_names(kinds);
                for t in &self.ed.sch.texts {
                    for l in t.text.lines() {
                        let w: Vec<String> = tokenize(&l.to_ascii_lowercase());
                        if w.first().is_some_and(|f| f == ".model") && w.len() > 2 && kinds.contains(&w[2].as_str()) {
                            names.insert(0, l.split_whitespace().nth(1).unwrap_or("").to_string());
                        }
                    }
                }
                let cur = c.value.split_whitespace().next().unwrap_or("").to_string();
                egui::ComboBox::from_id_salt("model").selected_text(&cur).width(180.0).show_ui(ui, |ui| {
                    for n in names {
                        if ui.selectable_label(n.eq_ignore_ascii_case(&cur), &n).clicked() {
                            let rest: Vec<&str> = c.value.split_whitespace().skip(1).collect();
                            changed = Some(Component { value: [vec![n.as_str()], rest].concat().join(" "), ..c.clone() });
                        }
                    }
                });
                ui.end_row();
            }
        });
        if matches!(c.kind, Kind::Voltage | Kind::Current) {
            ui.add_space(8.0);
            if let Some(v) = source_form(ui, &c.value, c.kind == Kind::Voltage) {
                changed = Some(Component { value: v, ..c.clone() });
            }
        }
        if matches!(c.kind, Kind::Nmos | Kind::Pmos) {
            ui.weak("Level-1 models accept W=… L=… after the model name.");
        }
        if c.kind == Kind::Behavioral {
            ui.weak("e.g. V=V(a)*V(b)  or  I=if(V(in)>1, 1m, 0)");
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Rotate").clicked() {
                self.ed.rotate();
            }
            if ui.button("Mirror").clicked() {
                self.ed.mirror();
            }
            if ui.button("Delete").clicked() {
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
        ui.heading("Simulation");
        let (ti, line) = self
            .ed
            .sch
            .texts
            .iter()
            .enumerate()
            .find_map(|(i, t)| t.text.lines().find(|l| is_analysis(l)).map(|l| (Some(i), l.trim().to_string())))
            .unwrap_or((None, String::new()));
        let mut f = SimForm::parse(&line);
        let before = f.clone();
        egui::ComboBox::from_id_salt("an").selected_text(SimForm::KINDS[f.kind]).show_ui(ui, |ui| {
            for (i, k) in SimForm::KINDS.iter().enumerate() {
                ui.selectable_value(&mut f.kind, i, *k);
            }
        });
        egui::Grid::new("simf").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
            let field = |ui: &mut egui::Ui, l: &str, v: &mut String| {
                ui.label(l);
                ui.add(egui::TextEdit::singleline(v).desired_width(120.0));
                ui.end_row();
            };
            match f.kind {
                0 => {
                    field(ui, "Stop time", &mut f.a);
                    field(ui, "Start saving", &mut f.b);
                    field(ui, "Max step", &mut f.c);
                    ui.label("Start from");
                    ui.checkbox(&mut f.uic, "initial conditions (UIC)");
                    ui.end_row();
                }
                1 => {
                    ui.label("Sweep");
                    egui::ComboBox::from_id_salt("acs").selected_text(&f.d).show_ui(ui, |ui| {
                        for s in ["dec", "oct", "lin"] {
                            ui.selectable_value(&mut f.d, s.to_string(), s);
                        }
                    });
                    ui.end_row();
                    field(ui, if f.d == "lin" { "Points" } else { "Points/decade" }, &mut f.a);
                    field(ui, "Start freq", &mut f.b);
                    field(ui, "Stop freq", &mut f.c);
                }
                2 => {
                    field(ui, "Source", &mut f.d);
                    field(ui, "Start", &mut f.a);
                    field(ui, "Stop", &mut f.b);
                    field(ui, "Step", &mut f.c);
                }
                _ => {}
            }
        });
        if f.kind == 3 {
            ui.weak("DC operating point: voltages appear on the schematic.");
        }
        if f.kind != before.kind {
            let src = self.ed.sch.comps.iter().find(|c| c.kind == Kind::Voltage).map_or("V1".to_string(), |c| c.spice_name());
            f = SimForm::defaults(f.kind, &src);
        }
        if f != before {
            let new = f.line();
            self.ed.checkpoint();
            match ti {
                Some(i) => {
                    let t = &mut self.ed.sch.texts[i];
                    t.text = t.text.lines().map(|l| if is_analysis(l) { new.clone() } else { l.to_string() }).collect::<Vec<_>>().join("\n");
                }
                None => self.ed.place_text(&new),
            }
            self.ed.changed();
        }
        ui.add_space(6.0);
        ui.weak("Add .step, .meas, .param, .model … with the .Text tool.");
    }

    fn bottom(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.tab, Tab::Waves, "Waveforms");
            if self.mode == Mode::Schematic {
                ui.selectable_value(&mut self.tab, Tab::Netlist, "Netlist");
            }
            let errs = self.log.iter().any(|l| l.starts_with("error"));
            ui.selectable_value(&mut self.tab, Tab::Log, if errs { RichText::new("Log ●").color(Color32::from_rgb(255, 99, 99)) } else { RichText::new("Log") });
        });
        ui.separator();
        match self.tab {
            Tab::Waves => self.waves.ui(ui, self.results.as_ref()),
            Tab::Netlist => {
                let mut t = self.ed.netlist().text.clone();
                ui.horizontal(|ui| {
                    if ui.button("Copy").clicked() {
                        ui.ctx().copy_text(t.clone());
                    }
                    ui.weak("Generated from the schematic (read-only).");
                });
                egui::ScrollArea::both().show(ui, |ui| {
                    ui.add(egui::TextEdit::multiline(&mut t).code_editor().interactive(false).desired_width(f32::INFINITY));
                });
            }
            Tab::Log => {
                egui::ScrollArea::both().stick_to_bottom(true).show(ui, |ui| {
                    if self.log.is_empty() {
                        ui.weak("No messages.");
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
        let ctx = ui.ctx().clone();
        self.poll(&ctx);
        // global shortcuts
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F5)) {
            self.run_sim(&ctx);
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
            self.confirm = Some(Pending::Quit);
        }
        if let Some(sig) = self.ed.probe.take() {
            self.waves.add(&sig);
            self.tab = Tab::Waves;
        }
        let name = self.path.as_ref().and_then(|p| p.file_name()).map_or("untitled".into(), |n| n.to_string_lossy().to_string());
        let title = format!("{}{name} — SwiftSim", if self.dirty() { "● " } else { "" });
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }

        egui::Panel::top("menu").show(ui, |ui| {
            self.menu(ui);
            self.toolbar(ui);
            ui.add_space(2.0);
        });
        egui::Panel::bottom("status").exact_size(22.0).show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.small(&self.status);
            });
        });
        egui::Panel::bottom("results").resizable(true).default_size(300.0).min_size(120.0).show(ui, |ui| {
            // egui panels adopt their content's size each frame; pin it to the panel rect so it can't creep
            let r = ui.max_rect();
            ui.allocate_rect(r, Sense::hover());
            self.bottom(&mut ui.new_child(egui::UiBuilder::new().max_rect(r).layout(egui::Layout::top_down(egui::Align::Min))));
        });
        if self.mode == Mode::Schematic {
            egui::Panel::left("palette").resizable(true).default_size(200.0).min_size(150.0).show(ui, |ui| self.palette(ui));
            egui::Panel::right("props").resizable(true).default_size(280.0).min_size(220.0).show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.properties(ui));
            });
        }
        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| match self.mode {
            Mode::Schematic => self.ed.ui(ui),
            Mode::Netlist => {
                egui::Frame::central_panel(ui.style()).show(ui, |ui| {
                    egui::ScrollArea::both().show(ui, |ui| {
                        let r = ui.add(egui::TextEdit::multiline(&mut self.net_text).code_editor().desired_width(f32::INFINITY).desired_rows(40));
                        if r.changed() {
                            self.net_dirty = true;
                        }
                    });
                });
            }
        });

        if let Some(p) = self.confirm {
            let mut close = false;
            egui::Modal::new(egui::Id::unique("confirm")).show(&ctx, |ui| {
                ui.heading("Unsaved changes");
                ui.label("Discard changes to the current document?");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        self.save(false);
                        close = true;
                        if !self.dirty() {
                            self.do_pending(p, &ctx);
                        }
                    }
                    if ui.button("Discard").clicked() {
                        close = true;
                        self.ed.dirty = false;
                        self.net_dirty = false;
                        self.do_pending(p, &ctx);
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });
            if close {
                self.confirm = None;
            }
        }
    }
}

fn is_analysis(l: &str) -> bool {
    let w = l.trim().to_ascii_lowercase();
    [".tran", ".ac", ".dc", ".op"].iter().any(|k| w == *k || w.starts_with(&format!("{k} ")))
}

#[derive(Clone, PartialEq)]
struct SimForm {
    kind: usize,
    a: String,
    b: String,
    c: String,
    d: String,
    uic: bool,
}

impl SimForm {
    const KINDS: [&str; 4] = ["Transient", "AC analysis", "DC sweep", "Operating point"];

    fn parse(line: &str) -> SimForm {
        let t: Vec<String> = line.split_whitespace().map(|s| s.to_string()).collect();
        let g = |i: usize| t.get(i).cloned().unwrap_or_default();
        match t.first().map(|s| s.to_ascii_lowercase()).as_deref() {
            Some(".ac") => SimForm { kind: 1, d: g(1), a: g(2), b: g(3), c: g(4), uic: false },
            Some(".dc") => SimForm { kind: 2, d: g(1), a: g(2), b: g(3), c: g(4), uic: false },
            Some(".op") => SimForm { kind: 3, a: String::new(), b: String::new(), c: String::new(), d: String::new(), uic: false },
            Some(".tran") => {
                let uic = t.iter().any(|s| s.eq_ignore_ascii_case("uic"));
                let n: Vec<String> = t[1..].iter().filter(|s| !s.eq_ignore_ascii_case("uic")).cloned().collect();
                let (stop, start, max) = match n.len() {
                    0 => (String::new(), String::new(), String::new()),
                    1 => (n[0].clone(), String::new(), String::new()),
                    _ => (n[1].clone(), n.get(2).cloned().unwrap_or_default(), n.get(3).cloned().unwrap_or_default()),
                };
                SimForm { kind: 0, a: stop, b: start, c: max, d: String::new(), uic }
            }
            _ => SimForm { kind: 0, a: "10m".into(), b: String::new(), c: String::new(), d: String::new(), uic: false },
        }
    }

    fn defaults(kind: usize, src: &str) -> SimForm {
        let s = |v: &str| v.to_string();
        let (a, b, c, d) = match kind {
            0 => (s("10m"), s(""), s(""), s("")),
            1 => (s("100"), s("1"), s("1meg"), s("dec")),
            2 => (s("0"), s("5"), s("0.1"), s(src)),
            _ => (s(""), s(""), s(""), s("")),
        };
        SimForm { kind, a, b, c, d, uic: false }
    }

    fn line(&self) -> String {
        let or = |s: &str, d: &str| if s.trim().is_empty() { d.to_string() } else { s.trim().to_string() };
        match self.kind {
            0 => {
                let mut l = format!(".tran 0 {}", or(&self.a, "10m"));
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
            1 => format!(".ac {} {} {} {}", or(&self.d, "dec"), or(&self.a, "100"), or(&self.b, "1"), or(&self.c, "1meg")),
            2 => format!(".dc {} {} {} {}", or(&self.d, "V1"), or(&self.a, "0"), or(&self.b, "5"), or(&self.c, "0.1")),
            _ => ".op".into(),
        }
    }
}

const PULSE: [&str; 8] = ["V initial", "V on", "T delay", "T rise", "T fall", "T on", "T period", "N cycles"];
const SINE: [&str; 7] = ["DC offset", "Amplitude", "Freq", "T delay", "Theta (1/s)", "Phase (°)", "N cycles"];
const EXP: [&str; 6] = ["V1", "V2", "T delay 1", "Tau 1", "T delay 2", "Tau 2"];

/// Structured editor for V/I source values. Returns a new value on change.
fn source_form(ui: &mut egui::Ui, value: &str, volt: bool) -> Option<String> {
    let toks = tokenize(value);
    let (mut func, mut args, mut dc, mut ac) = (String::from("dc"), vec![], String::new(), String::new());
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
                if toks.get(i).is_some_and(|x| swiftsim::expr::parse_number(x).is_some()) {
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
    ui.label(RichText::new("Waveform").strong());
    ui.horizontal(|ui| {
        for (k, l) in [("dc", "DC"), ("pulse", "Pulse"), ("sine", "Sine"), ("exp", "Exp"), ("pwl", "PWL")] {
            ui.selectable_value(&mut func, k.to_string(), l);
        }
    });
    egui::Grid::new("srcf").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
        let labels: &[&str] = match func.as_str() {
            "pulse" => &PULSE,
            "sine" => &SINE,
            "exp" => &EXP,
            _ => &[],
        };
        if func == "dc" {
            ui.label(format!("DC value ({unit})"));
            ui.text_edit_singleline(&mut dc);
            ui.end_row();
        } else if func == "pwl" {
            ui.label("t1 v1 t2 v2 …");
            let mut s = args.join(" ");
            if ui.text_edit_singleline(&mut s).changed() {
                args = s.split_whitespace().map(|x| x.to_string()).collect();
            }
            ui.end_row();
        } else {
            args.resize(args.len().max(labels.len().min(3)), String::new());
            for (k, l) in labels.iter().enumerate() {
                ui.label(*l);
                let mut v = args.get(k).cloned().unwrap_or_default();
                if ui.text_edit_singleline(&mut v).changed() {
                    if args.len() <= k {
                        args.resize(k + 1, String::new());
                    }
                    args[k] = v;
                }
                ui.end_row();
            }
        }
        ui.label("AC amplitude");
        ui.text_edit_singleline(&mut ac);
        ui.end_row();
    });
    if (&func, &args, &dc, &ac) == (&f0, &a0, &d0, &ac0) {
        return None;
    }
    while args.last().is_some_and(|a| a.trim().is_empty()) {
        args.pop();
    }
    let mut out = vec![];
    if func == "dc" || !dc.trim().is_empty() && func == "dc" {
        out.push(if dc.trim().is_empty() { "0".to_string() } else { dc.trim().to_string() });
    } else {
        let a: Vec<String> = args.iter().map(|a| if a.trim().is_empty() { "0".to_string() } else { a.trim().to_string() }).collect();
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
                p.add(Shape::convex_polygon(v.into_iter().map(tf).collect(), col, Stroke::NONE));
            }
            Prim::Circle(ctr, rad) => {
                p.circle_stroke(tf(ctr), rad * s, st);
            }
            Prim::Text(..) => {}
        }
    }
    if k == Kind::Label {
        p.text(r.center(), egui::Align2::CENTER_CENTER, "net", egui::FontId::proportional(11.0), col);
    }
}

fn comp(kind: Kind, name: &str, value: &str, pos: [i32; 2], rot: u8) -> Component {
    Component { kind, name: name.into(), value: value.into(), pos, rot, mirror: false }
}

fn wires(pts: &[[i32; 2]]) -> Vec<Wire> {
    pts.windows(2).map(|w| Wire { a: w[0], b: w[1] }).collect()
}

fn examples() -> Vec<(&'static str, Schematic)> {
    let g = |x, y| comp(Kind::Ground, "", "", [x, y], 0);
    let lbl = |v: &str, x, y| comp(Kind::Label, "", v, [x, y], 0);
    let txt = |x, y, t: &str| Text { pos: [x, y], text: t.into() };
    let rc = Schematic {
        comps: vec![
            comp(Kind::Voltage, "V1", "PULSE(0 1 0 1u 1u 5m 10m) AC 1", [0, 0], 0),
            comp(Kind::Resistor, "R1", "1k", [7, -4], 1),
            comp(Kind::Capacitor, "C1", "1u", [14, 0], 0),
            g(0, 2),
            g(14, 2),
            lbl("in", 0, -4),
            lbl("out", 14, -4),
        ],
        wires: [wires(&[[0, -2], [0, -4], [5, -4]]), wires(&[[9, -4], [14, -4], [14, -2]])].concat(),
        texts: vec![txt(0, 6, ".tran 20m\n* switch to AC: .ac dec 50 1 100k")],
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
    };
    let opamp = Schematic {
        comps: vec![
            comp(Kind::OpAmp, "U1", "Aol=100k GBW=10Meg", [10, 0], 0),
            comp(Kind::Resistor, "Rin", "1k", [4, -1], 1),
            comp(Kind::Resistor, "Rf", "10k", [10, -4], 1),
            g(8, 3),
            comp(Kind::Voltage, "Vin", "SINE(0 0.5 1k) AC 1", [0, 2], 0),
            g(0, 4),
            lbl("in", 0, -1),
            lbl("out", 14, 0),
        ],
        wires: [
            wires(&[[6, -1], [8, -1]]),
            wires(&[[8, -4], [8, -1]]),
            wires(&[[12, -4], [12, 0]]),
            wires(&[[8, 1], [8, 3]]),
            wires(&[[0, 0], [0, -1], [2, -1]]),
            wires(&[[12, 0], [14, 0]]),
        ]
        .concat(),
        texts: vec![txt(0, 8, ".tran 3m\n.meas tran gain PARAM -10")],
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
        wires: [wires(&[[0, -2], [0, -4], [2, -4]]), wires(&[[6, -4], [8, -4], [8, -2]]), wires(&[[8, -4], [12, -4], [12, -2]])].concat(),
        texts: vec![txt(0, 6, ".tran 100m\n.meas tran ripple PP V(out) FROM=60m TO=100m")],
    };
    vec![("RC low-pass filter", rc), ("BJT common-emitter amplifier", ce), ("Op-amp inverting amplifier", opamp), ("Half-wave rectifier", rect)]
}

#[cfg(test)]
mod tests {
    #[test]
    fn examples_simulate() {
        for (name, s) in super::examples() {
            let n = s.netlist();
            let r = swiftsim::run_netlist(&n.text, None, &swiftsim::Control::default()).unwrap_or_else(|e| panic!("{name}: {e}\n{}", n.text));
            assert!(!r.plots.is_empty(), "{name}");
            let p = &r.plots[0];
            assert!(p.index("v(out)").is_some(), "{name}: {:?}\n{}", p.names, n.text);
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
        assert_eq!(super::SimForm::parse(".ac dec 20 10 1meg").line(), ".ac dec 20 10 1meg");
        assert_eq!(super::SimForm::defaults(1, "V1").line(), ".ac dec 100 1 1meg");
        assert_eq!(super::SimForm::defaults(2, "Vin").line(), ".dc Vin 0 5 0.1");
    }
}
