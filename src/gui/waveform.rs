//! Waveform viewer: traces from expressions, stepped runs, Bode plots,
//! noise spectra (log-log), FFT, two cursors with readout, CSV export.

use super::bidi::{self, Base};
use super::i18n::{self, tr};
use eframe::egui;
use egui::{Color32, RichText};
use egui_plot::{GridInput, GridMark, HoverPosition, Line, Plot, PlotPoint, PlotPoints, VLine};
use swiftsim::expr::fmt_si;
use swiftsim::{PlotKind, SimResult};

const COLORS: [Color32; 10] = [
    Color32::from_rgb(64, 196, 255),
    Color32::from_rgb(255, 112, 67),
    Color32::from_rgb(102, 220, 120),
    Color32::from_rgb(255, 213, 79),
    Color32::from_rgb(186, 104, 255),
    Color32::from_rgb(255, 82, 160),
    Color32::from_rgb(77, 230, 210),
    Color32::from_rgb(255, 160, 0),
    Color32::from_rgb(160, 200, 255),
    Color32::from_rgb(200, 230, 80),
];

pub struct Trace {
    pub expr: String,
    pub color: Color32,
    pub visible: bool,
}

struct Series {
    trace: usize,
    step: String,
    a: Vec<PlotPoint>,
    /// Phase (AC) — empty otherwise.
    b: Vec<PlotPoint>,
}

#[derive(Default)]
pub struct WaveView {
    pub traces: Vec<Trace>,
    input: String,
    key: String,
    series: Vec<Series>,
    errors: Vec<String>,
    c1: Option<f64>,
    c2: Option<f64>,
    fft: bool,
    /// Bumped by the app on new results.
    pub generation: u64,
    next_color: usize,
}

#[derive(Clone, Copy, PartialEq)]
enum Pane {
    All,
    Phase,
    Volt,
    Amp,
}

/// `I(R1)`, `Ic(Q1)`, … plot on the current pane.
fn is_current(e: &str) -> bool {
    let l = e.trim().to_ascii_lowercase();
    l.split_once('(').is_some_and(|(h, _)| matches!(h.trim(), "i" | "ic" | "ib" | "ie" | "id" | "ig" | "is" | "ix"))
}

pub fn pretty(sig: &str) -> String {
    match sig.find('(') {
        Some(i) => {
            let (f, r) = sig.split_at(i);
            let mut c = f.chars();
            let head: String = c.next().map(|x| x.to_ascii_uppercase()).into_iter().chain(c).collect();
            format!("{head}{r}")
        }
        None => sig.to_string(),
    }
}

/// min/max decimation to keep huge runs interactive
fn decimate(v: Vec<PlotPoint>) -> Vec<PlotPoint> {
    const MAX: usize = 20_000;
    if v.len() <= MAX {
        return v;
    }
    let chunk = v.len().div_ceil(MAX / 2);
    let mut out = Vec::with_capacity(MAX + 2);
    for c in v.chunks(chunk) {
        let (mut lo, mut hi) = (c[0], c[0]);
        for p in c {
            if p.y < lo.y {
                lo = *p;
            }
            if p.y > hi.y {
                hi = *p;
            }
        }
        if lo.x <= hi.x {
            out.extend([lo, hi]);
        } else {
            out.extend([hi, lo]);
        }
    }
    out
}

fn fft_db(pts: &[PlotPoint]) -> Vec<PlotPoint> {
    let n0 = pts.len();
    if n0 < 16 {
        return vec![];
    }
    let (t0, t1) = (pts[0].x, pts[n0 - 1].x);
    let mut n = 256;
    while n < n0 && n < 1 << 16 {
        n <<= 1;
    }
    let dt = (t1 - t0) / n as f64;
    let (mut re, mut im) = (vec![0.0; n], vec![0.0; n]);
    let mut j = 0;
    for k in 0..n {
        let t = t0 + k as f64 * dt;
        while j + 2 < n0 && pts[j + 1].x < t {
            j += 1;
        }
        let (a, b) = (pts[j], pts[j + 1]);
        let y = if b.x > a.x { a.y + (b.y - a.y) * (t - a.x) / (b.x - a.x) } else { a.y };
        let w = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * k as f64 / n as f64).cos();
        re[k] = y * w;
    }
    // iterative radix-2 FFT
    let mut jj = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while jj & bit != 0 {
            jj ^= bit;
            bit >>= 1;
        }
        jj |= bit;
        if i < jj {
            re.swap(i, jj);
            im.swap(i, jj);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * std::f64::consts::PI / len as f64;
        for s in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (wr, wi) = ((ang * k as f64).cos(), (ang * k as f64).sin());
                let (a, b) = (s + k, s + k + len / 2);
                let (xr, xi) = (re[b] * wr - im[b] * wi, re[b] * wi + im[b] * wr);
                re[b] = re[a] - xr;
                im[b] = im[a] - xi;
                re[a] += xr;
                im[a] += xi;
            }
        }
        len <<= 1;
    }
    (1..n / 2)
        .map(|k| {
            let f = k as f64 / (n as f64 * dt);
            let mag = 2.0 * re[k].hypot(im[k]) / (n as f64 * 0.5);
            PlotPoint::new(f.log10(), 20.0 * mag.max(1e-30).log10())
        })
        .collect()
}

fn log_spacer(input: GridInput) -> Vec<GridMark> {
    let (lo, hi) = input.bounds;
    let mut v = vec![];
    if !(hi - lo).is_finite() || hi - lo > 40.0 {
        return v;
    }
    for d in lo.floor() as i32..=hi.ceil() as i32 {
        v.push(GridMark { value: d as f64, step_size: 1.0 });
        if hi - lo < 8.0 {
            for m in 2..10 {
                v.push(GridMark { value: d as f64 + (m as f64).log10(), step_size: 0.1 });
            }
        }
    }
    v
}

fn interp(p: &[PlotPoint], x: f64) -> Option<f64> {
    if p.is_empty() || x < p[0].x || x > p[p.len() - 1].x {
        return None;
    }
    let i = p.partition_point(|q| q.x < x).max(1).min(p.len() - 1);
    let (a, b) = (p[i - 1], p[i]);
    Some(if b.x > a.x { a.y + (b.y - a.y) * (x - a.x) / (b.x - a.x) } else { a.y })
}

impl WaveView {
    pub fn add(&mut self, expr: &str) {
        let e = expr.trim();
        if e.is_empty() || self.traces.iter().any(|t| t.expr.eq_ignore_ascii_case(e)) {
            return;
        }
        self.traces.push(Trace { expr: e.to_string(), color: COLORS[self.next_color % COLORS.len()], visible: true });
        self.next_color += 1;
    }

    fn rebuild(&mut self, res: &SimResult) {
        self.series.clear();
        self.errors.clear();
        for (ti, t) in self.traces.iter().enumerate() {
            for p in &res.plots {
                if p.kind == PlotKind::Op {
                    continue;
                }
                let r = if p.kind == PlotKind::Ac {
                    swiftsim::measure::eval_trace_c(p, &t.expr).map(|v| {
                        let lx: Vec<f64> = p.x.iter().map(|f| f.log10()).collect();
                        let a = lx.iter().zip(&v).map(|(x, z)| PlotPoint::new(*x, 20.0 * z.norm().max(1e-300).log10())).collect();
                        let b = lx.iter().zip(&v).map(|(x, z)| PlotPoint::new(*x, z.arg_deg())).collect();
                        (a, b)
                    })
                } else if p.kind == PlotKind::Noise {
                    swiftsim::measure::eval_trace(p, &t.expr)
                        .map(|v| (p.x.iter().zip(&v).map(|(x, y)| PlotPoint::new(x.log10(), y.abs().max(1e-300).log10())).collect(), vec![]))
                } else {
                    swiftsim::measure::eval_trace(p, &t.expr).map(|v| {
                        let pts: Vec<PlotPoint> = p.x.iter().zip(&v).map(|(x, y)| PlotPoint::new(*x, *y)).collect();
                        if self.fft && p.kind == PlotKind::Tran { (fft_db(&pts), vec![]) } else { (decimate(pts), vec![]) }
                    })
                };
                match r {
                    Ok((a, b)) => self.series.push(Series { trace: ti, step: p.step.clone(), a, b }),
                    Err(e) => {
                        self.errors.push(format!("{}: {e}", t.expr));
                        break;
                    }
                }
            }
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, res: Option<&SimResult>) {
        let kind = res.and_then(|r| r.plots.first()).map(|p| p.kind);
        // toolbar
        i18n::mirrored(ui, |ui| ui.horizontal(|ui| {
            let r = bidi::line(ui, &mut self.input, Base::Ltr, |t| t.hint_text(tr("Add trace: V(out), V(a)-V(b), I(R1)*V(out) …")).desired_width(260.0));
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                let s = std::mem::take(&mut self.input);
                self.add(&s);
                r.request_focus();
            }
            if ui.button(tr("Add")).clicked() {
                let s = std::mem::take(&mut self.input);
                self.add(&s);
            }
            if let Some(p) = res.and_then(|r| r.plots.first()) {
                ui.menu_button(format!("{} ▾", tr("Signals")), |ui| {
                    egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                        for n in &p.names {
                            if ui.button(pretty(n)).clicked() {
                                self.add(&pretty(n));
                            }
                        }
                    });
                });
            }
            ui.separator();
            if kind == Some(PlotKind::Tran) {
                ui.toggle_value(&mut self.fft, "FFT");
            }
            if ui.button(tr("Clear")).clicked() {
                self.traces.clear();
                self.c1 = None;
                self.c2 = None;
            }
            if let Some(r) = res {
                if ui.button(tr("Export CSV…")).clicked() {
                    if let Some(path) = rfd::FileDialog::new().add_filter("CSV", &["csv"]).set_file_name("waveforms.csv").save_file() {
                        let _ = std::fs::write(path, swiftsim::to_csv(&r.plots));
                    }
                }
            }
            ui.weak(tr("Left-click: cursor 1 · Right-click: cursor 2 · Drag: pan · Scroll: zoom · Double-click: reset"));
        }));
        // trace chips
        let mut remove = None;
        ui.horizontal_wrapped(|ui| {
            for (i, t) in self.traces.iter_mut().enumerate() {
                let txt = RichText::new(format!("■ {}", t.expr)).color(if t.visible { t.color } else { ui.visuals().weak_text_color() });
                if ui.selectable_label(false, txt).on_hover_text(tr("Click to show/hide")).clicked() {
                    t.visible = !t.visible;
                }
                if ui.small_button("×").clicked() {
                    remove = Some(i);
                }
            }
        });
        if let Some(i) = remove {
            self.traces.remove(i);
        }
        for e in &self.errors {
            ui.colored_label(Color32::from_rgb(255, 100, 100), e);
        }
        let Some(res) = res else {
            ui.centered_and_justified(|ui| ui.weak(tr("Run a simulation (F5) to see waveforms. Use the probe to click wires and parts.")));
            return;
        };
        if kind == Some(PlotKind::Op) || kind.is_none() {
            ui.centered_and_justified(|ui| ui.weak(tr("Operating point results are shown on the schematic and in the Log tab.")));
            return;
        }
        if self.traces.is_empty() {
            ui.centered_and_justified(|ui| ui.weak(tr("Pick signals from the Signals menu, type an expression above, or probe the schematic (P).")));
            return;
        }
        let key = format!("{}|{}|{}", self.generation, self.fft, self.traces.iter().map(|t| t.expr.as_str()).collect::<Vec<_>>().join("\u{1}"));
        if key != self.key {
            self.key = key;
            self.rebuild(res);
        }

        // noise: log frequency and log density axes, like LTspice
        let logy = kind == Some(PlotKind::Noise);
        let logx = logy || kind == Some(PlotKind::Ac) || (self.fft && kind == Some(PlotKind::Tran));
        let fy = move |y: f64| if logy { 10f64.powf(y) } else { y };
        let xname = res.plots[0].xname.clone();
        let xunit = match kind {
            Some(PlotKind::Tran) if !self.fft => "s",
            _ if logx => "Hz",
            _ => "",
        };
        let unit_of = move |e: &str| if logy { "V/√Hz" } else if logx { "dB" } else if is_current(e) { "A" } else { "V" };
        let fmt_x = move |x: f64| if logx { format!("{}{xunit}", fmt_si(10f64.powf(x))) } else { format!("{}{xunit}", fmt_si(x)) };
        let readout_h = if self.c1.is_some() || self.c2.is_some() { 24.0 + 20.0 * self.traces.len().min(6) as f32 } else { 0.0 };
        // claim exactly the remaining area; plots live in a child so they can never grow the panel
        let area = ui.available_rect_before_wrap();
        ui.allocate_rect(area, egui::Sense::hover());
        let ui = &mut ui.new_child(egui::UiBuilder::new().max_rect(area).layout(egui::Layout::top_down(egui::Align::Min)));
        let avail = (area.height() - readout_h).max(80.0);
        // voltages and currents get their own panes (shared time axis), like separate axes in LTSpice
        let vis = || self.traces.iter().filter(|t| t.visible);
        let (has_v, has_i) = (vis().any(|t| !is_current(&t.expr)), vis().any(|t| is_current(&t.expr)));
        let panes: Vec<(Pane, &'static str, f32)> = if kind == Some(PlotKind::Ac) {
            vec![(Pane::All, "dB", 0.62), (Pane::Phase, "°", 0.38)]
        } else if logy {
            vec![(Pane::All, "V/√Hz", 1.0)]
        } else if logx {
            vec![(Pane::All, "dB", 1.0)]
        } else if has_v && has_i {
            vec![(Pane::Volt, "V", 0.5), (Pane::Amp, "A", 0.5)]
        } else if has_i {
            vec![(Pane::Amp, "A", 1.0)]
        } else {
            vec![(Pane::Volt, "V", 1.0)]
        };
        let (mut c1, mut c2) = (self.c1, self.c2);
        let link = egui::Id::unique("wave_link");
        let make = |id: &str, h: f32, unit: &'static str| {
            let mut p = Plot::new(id.to_string())
                .height(h)
                .legend(egui_plot::Legend::default())
                .link_axis(link, [true, false])
                .link_cursor(link, [true, false])
                .y_axis_formatter(move |m, _| format!("{}{unit}", fmt_si(fy(m.value))))
                .x_axis_formatter(move |m, _| fmt_x(m.value))
                .label_formatter(move |h: &HoverPosition| {
                    Some(match h {
                        HoverPosition::NearDataPoint { plot_name, position, .. } => {
                            format!("{plot_name}\n{}\n{}{unit}", fmt_x(position.x), fmt_si(fy(position.y)))
                        }
                        HoverPosition::Elsewhere { position } => format!("{}\n{}{unit}", fmt_x(position.x), fmt_si(fy(position.y))),
                    })
                });
            if logx {
                p = p.x_grid_spacer(log_spacer);
            }
            if logy {
                p = p.y_grid_spacer(log_spacer);
            }
            p
        };
        let traces = &self.traces;
        let series = &self.series;
        let show = |ui: &mut egui::Ui, plot: Plot, pane: Pane, c1: &mut Option<f64>, c2: &mut Option<f64>| {
            plot.show(ui, |pu| {
                for s in series {
                    let t = &traces[s.trace];
                    let wanted = match pane {
                        Pane::Volt => !is_current(&t.expr),
                        Pane::Amp => is_current(&t.expr),
                        _ => true,
                    };
                    if !t.visible || !wanted {
                        continue;
                    }
                    let pts = if pane == Pane::Phase { &s.b } else { &s.a };
                    let name = if s.step.is_empty() { t.expr.clone() } else { format!("{} [{}]", t.expr, s.step) };
                    pu.line(Line::new(name, PlotPoints::Borrowed(pts)).color(t.color).width(1.5));
                }
                for (c, col) in [(*c1, Color32::from_rgb(255, 235, 59)), (*c2, Color32::from_rgb(255, 64, 129))] {
                    if let Some(x) = c {
                        pu.vline(VLine::new("", x).color(col).width(1.0));
                    }
                }
                if pu.response().clicked() {
                    *c1 = pu.pointer_coordinate().map(|p| p.x);
                }
                if pu.response().secondary_clicked() {
                    *c2 = pu.pointer_coordinate().map(|p| p.x);
                }
            });
        };
        let gaps = 2.0 * ui.spacing().item_spacing.y * panes.len() as f32;
        for (i, &(pane, unit, frac)) in panes.iter().enumerate() {
            show(ui, make(&format!("wave_pane{i}"), (avail - gaps) * frac, unit), pane, &mut c1, &mut c2);
        }
        self.c1 = c1;
        self.c2 = c2;
        let mut close = false;
        if c1.is_some() || c2.is_some() {
            egui::Grid::new("readout").striped(true).show(ui, |ui| {
                ui.strong(&xname);
                ui.strong(c1.map_or("—".into(), fmt_x));
                ui.strong(c2.map_or("—".into(), fmt_x));
                match (c1, c2) {
                    (Some(a), Some(b)) if !logx => {
                        let d = b - a;
                        ui.strong(format!("Δ {}{xunit}  (1/Δ = {}Hz)", fmt_si(d), fmt_si(1.0 / d.abs())));
                    }
                    _ => {
                        ui.strong("Δ");
                    }
                }
                close = ui.small_button(i18n::with_symbol("×", tr("Close"))).on_hover_text(tr("Remove both cursors (traces stay)")).clicked();
                ui.end_row();
                for s in self.series.iter().filter(|s| self.traces[s.trace].visible).take(6) {
                    let t = &self.traces[s.trace];
                    let name = if s.step.is_empty() { t.expr.clone() } else { format!("{} [{}]", t.expr, s.step) };
                    ui.colored_label(t.color, name);
                    let (y1, y2) = (c1.and_then(|x| interp(&s.a, x)).map(fy), c2.and_then(|x| interp(&s.a, x)).map(fy));
                    let yunit = unit_of(&t.expr);
                    let f = |y: Option<f64>| y.map_or("—".into(), |v| format!("{}{yunit}", fmt_si(v)));
                    ui.label(f(y1));
                    ui.label(f(y2));
                    ui.label(match (y1, y2) {
                        (Some(a), Some(b)) => format!("{}{yunit}", fmt_si(b - a)),
                        _ => "—".into(),
                    });
                    ui.end_row();
                }
            });
        }
        if close {
            self.c1 = None;
            self.c2 = None;
        }
    }
}
