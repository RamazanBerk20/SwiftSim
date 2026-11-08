//! Analyses: operating point, DC sweep, AC, noise, transient (adaptive trapezoidal
//! with LTE control and breakpoints), plus `.step` and `.meas` orchestration.

use crate::devices::{Device, LoadCtx, Noise};
use crate::expr::fmt_si;
use crate::netlist::{Analysis, Deck};
use crate::solver::{C64, Matrix};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Instant;

#[derive(Clone, Debug)]
pub struct Options {
    pub reltol: f64,
    pub abstol: f64,
    pub vntol: f64,
    pub gmin: f64,
    pub itl1: usize,
    pub itl4: usize,
    pub trtol: f64,
    pub maxstep: f64,
    /// Nominal temperature of model parameters, °C.
    pub tnom: f64,
}

impl Default for Options {
    fn default() -> Self {
        Options { reltol: 1e-3, abstol: 1e-12, vntol: 1e-6, gmin: 1e-12, itl1: 150, itl4: 25, trtol: 7.0, maxstep: 0.0, tnom: 27.0 }
    }
}

pub struct Circuit {
    pub devices: Vec<Box<dyn Device>>,
    pub n: usize,
    pub names: Vec<String>,
    pub is_current: Vec<bool>,
    pub out_nodes: Vec<(String, usize)>,
    pub nstate: usize,
    pub ics: Vec<(usize, f64)>,
    pub opts: Options,
    /// Circuit temperature, kelvin.
    pub temp: f64,
    pub nonlinear: bool,
    pub warnings: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlotKind {
    Op,
    Dc,
    Ac,
    Tran,
    /// Noise densities (V/√Hz) versus frequency.
    Noise,
}

/// One analysis run (one `.step` point): a sweep variable and signals.
#[derive(Clone, Debug)]
pub struct Plot {
    pub kind: PlotKind,
    pub xname: String,
    pub x: Vec<f64>,
    pub names: Vec<String>,
    pub re: Vec<Vec<f64>>,
    /// Imaginary parts (AC only).
    pub im: Vec<Vec<f64>>,
    pub step: String,
}

impl Plot {
    pub fn index(&self, name: &str) -> Option<usize> {
        let n = crate::expr::fold(name);
        self.names.iter().position(|s| if s.is_ascii() { *s == n } else { crate::expr::fold(s) == n })
    }
}

#[derive(Default)]
pub struct SimResult {
    pub plots: Vec<Plot>,
    pub log: Vec<String>,
    /// (step label, name, value)
    pub meas: Vec<(String, String, f64)>,
}

/// Shared with the UI thread: cancel flag and progress in 1/1000.
#[derive(Default)]
pub struct Control {
    pub cancel: AtomicBool,
    pub progress: AtomicU32,
}

pub fn run_netlist(text: &str, base: Option<&Path>, ctl: &Control) -> Result<SimResult, String> {
    let deck = Deck::parse(text, base)?;
    run(&deck, ctl)
}

pub fn run(deck: &Deck, ctl: &Control) -> Result<SimResult, String> {
    let t0 = Instant::now();
    let mut res = SimResult::default();
    res.log.extend(deck.warnings.iter().map(|w| format!("warning: {w}")));
    // .param lines may use .step variables, which only get values per step: provisionally 0 here
    // (enough to read the .step lines); each step below evaluates everything with its own values
    let provisional: HashMap<String, f64> = deck.step_names().into_iter().map(|n| (n, 0.0)).collect();
    let global = deck.eval_params(&provisional)?;
    let (steps, sw) = deck.steps(&global)?;
    res.log.extend(sw.into_iter().map(|w| format!("warning: {w}")));
    let mut combos: Vec<Vec<(String, f64)>> = vec![vec![]];
    for s in &steps {
        combos = combos
            .into_iter()
            .flat_map(|c| s.values.iter().map(move |&v| [c.clone(), vec![(s.name.clone(), v)]].concat()))
            .collect();
    }
    let total = combos.len();
    // each plot's own parameter values, for .meas
    let mut scopes = vec![];
    for (ci, combo) in combos.iter().enumerate() {
        let over: HashMap<String, f64> = combo.iter().cloned().collect();
        let label = combo.iter().map(|(k, v)| format!("{k}={}", fmt_si(*v))).collect::<Vec<_>>().join(" ");
        let scope = deck.eval_params(&over)?;
        let an = deck.analysis(&scope)?;
        let ckt = deck.build(&over)?;
        if ci == 0 {
            res.log.extend(ckt.warnings.iter().map(|w| format!("warning: {w}")));
        }
        let mut eng = Engine::new(ckt);
        let prog = Prog { ctl, base: ci as f64 / total as f64, span: 1.0 / total as f64 };
        let plots = eng.analyze(&an, &prog, &label).map_err(|e| if label.is_empty() { e } else { format!("{e} (step {label})") })?;
        match an {
            Analysis::Op => res.log.extend(op_report(&plots[0], &label)),
            Analysis::Tf { .. } => {
                res.log.push(format!("--- Transfer function {label} ---"));
                for (n, v) in plots[0].names.iter().zip(&plots[0].re) {
                    res.log.push(format!("{n:>30} = {}{}", fmt_si(v[0]), if n.contains("impedance") { "Ω" } else { "" }));
                }
            }
            Analysis::Noise { .. } => res.log.extend(noise_report(&plots[0], &label)),
            Analysis::Tran { .. } => {
                for l in &deck.four {
                    match crate::measure::fourier(&l.text, &plots[0], &scope) {
                        Ok(lines) => res.log.extend(lines),
                        Err(e) => res.log.push(format!("line {}: .four: {e}", l.no)),
                    }
                }
            }
            _ => {}
        }
        scopes.extend(std::iter::repeat_n(scope, plots.len()));
        res.plots.extend(plots);
    }
    for l in &deck.meas {
        for (p, scope) in res.plots.iter().zip(&scopes) {
            match crate::measure::measure(&l.text, p, &res.meas, scope) {
                Ok(Some((name, v))) => res.meas.push((p.step.clone(), name, v)),
                Ok(None) => {}
                Err(e) => res.log.push(format!("line {}: .meas: {e}", l.no)),
            }
        }
    }
    for (step, name, v) in &res.meas {
        let at = if step.is_empty() { String::new() } else { format!("   [{step}]") };
        res.log.push(format!("{name}: {}{at}", fmt_si(*v)));
    }
    res.plots.iter_mut().flat_map(|p| &mut p.names).for_each(|n| *n = deck.spelled(n));
    res.log.iter_mut().filter(|l| !l.starts_with("warning")).for_each(|l| *l = deck.spelled(l));
    res.meas.iter_mut().for_each(|m| m.1 = deck.spelled(&m.1));
    let pts: usize = res.plots.iter().map(|p| p.x.len()).sum();
    res.log.push(format!("Simulation done in {:.3} s ({} points, {} run(s))", t0.elapsed().as_secs_f64(), pts, total));
    ctl.progress.store(1000, Ordering::Relaxed);
    Ok(res)
}

fn op_report(p: &Plot, label: &str) -> Vec<String> {
    let mut out = vec![format!("--- Operating point {label} ---")];
    for (n, v) in p.names.iter().zip(&p.re) {
        let unit = if n.starts_with('v') { "V" } else { "A" };
        out.push(format!("{n:>20} = {}{unit}", fmt_si(v[0])));
    }
    out
}

/// ∫ v(f)² df over the sweep, each interval taken as a power law between its end points
/// (SPICE's noise integration): total power, V².
pub fn integrate_psd(f: &[f64], v: &[f64]) -> f64 {
    f.windows(2)
        .zip(v.windows(2))
        .map(|(f, v)| {
            let (s0, s1) = (v[0] * v[0], v[1] * v[1]);
            if s0 <= 0.0 || s1 <= 0.0 || f[0] <= 0.0 {
                return (s0 + s1) / 2.0 * (f[1] - f[0]);
            }
            let r = (f[1] / f[0]).ln();
            let e = (s1 / s0).ln() / r + 1.0;
            if e.abs() < 1e-10 { s0 * f[0] * r } else { s0 * f[0] / e * ((f[1] / f[0]).powf(e) - 1.0) }
        })
        .sum()
}

/// Integrated output / input noise and each part's share of the output noise.
fn noise_report(p: &Plot, label: &str) -> Vec<String> {
    let (lo, hi) = (fmt_si(p.x[0]), fmt_si(p.x[p.x.len() - 1]));
    let tot = |k: usize| integrate_psd(&p.x, &p.re[k]);
    let on = tot(0);
    let mut out = vec![
        format!("--- Noise {}({lo}Hz to {hi}Hz) ---", if label.is_empty() { String::new() } else { format!("{label} ") }),
        format!("Total output noise: {}V rms", fmt_si(on.sqrt())),
        format!("Total input-referred noise: {}V rms", fmt_si(tot(1).sqrt())),
    ];
    let mut parts: Vec<(f64, &str)> = (2..p.names.len()).map(|k| (tot(k), p.names[k].as_str())).collect();
    parts.sort_by(|a, b| b.0.total_cmp(&a.0));
    for (v, n) in parts.into_iter().filter(|(v, _)| *v > 0.0) {
        out.push(format!("{:>20}: {}V rms ({:.3}%)", n.trim_start_matches("v(").trim_end_matches(')'), fmt_si(v.sqrt()), 100.0 * v / on));
    }
    out
}

struct Prog<'a> {
    ctl: &'a Control,
    base: f64,
    span: f64,
}
impl Prog<'_> {
    fn set(&self, f: f64) {
        self.ctl.progress.store(((self.base + self.span * f.clamp(0.0, 1.0)) * 1000.0) as u32, Ordering::Relaxed);
    }
    fn check(&self) -> Result<(), String> {
        if self.ctl.cancel.load(Ordering::Relaxed) { Err("simulation cancelled".into()) } else { Ok(()) }
    }
}

#[derive(Clone, Copy)]
struct Mode {
    time: f64,
    dc: bool,
    tran: bool,
    a0: f64,
    b1: f64,
    src: f64,
    uic: bool,
}

impl Mode {
    fn dc(tran: bool) -> Mode {
        Mode { time: 0.0, dc: true, tran, a0: 0.0, b1: 0.0, src: 1.0, uic: false }
    }
}

/// Newton update (relative to its tolerance) below which the converged iterate's device state is kept.
const STATE_REFRESH: f64 = 1e-2;

struct Engine {
    c: Circuit,
    m: Matrix<f64>,
    x: Vec<f64>,
    xn: Vec<f64>,
    st: Vec<f64>,
    st0: Vec<f64>,
    gshunt: f64,
    force_ic: bool,
}

impl Engine {
    fn new(c: Circuit) -> Engine {
        let n = c.n;
        let ns = c.nstate;
        Engine { m: Matrix::new(n), x: vec![0.0; n + 1], xn: vec![0.0; n + 1], st: vec![0.0; ns], st0: vec![0.0; ns], gshunt: 0.0, force_ic: false, c }
    }

    fn load(&mut self, md: &Mode, init: bool) -> bool {
        self.m.clear();
        let ctx = LoadCtx {
            x: &self.x,
            st0: &self.st0,
            time: md.time,
            dc: md.dc,
            tran: md.tran,
            a0: md.a0,
            b1: md.b1,
            src: md.src,
            gmin: self.c.opts.gmin,
            init,
            uic: md.uic,
        };
        let mut lim = false;
        for d in self.c.devices.iter_mut() {
            lim |= d.load(&mut self.m, &ctx, &mut self.st);
        }
        let g = self.c.opts.gmin.max(self.gshunt);
        for i in 1..=self.c.n {
            if !self.c.is_current[i - 1] {
                self.m.add(i, i, g);
            }
        }
        if self.force_ic {
            for &(i, v) in &self.c.ics {
                self.m.add(i, i, 1e6);
                self.m.rhs(i, 1e6 * v);
            }
        }
        lim
    }

    fn newton(&mut self, md: &Mode, maxit: usize, mut init: bool) -> Result<usize, String> {
        self.st.copy_from_slice(&self.st0);
        let o = &self.c.opts;
        let (reltol, vntol, abstol) = (o.reltol, o.vntol, o.abstol);
        for it in 0..maxit {
            let lim = self.load(md, init);
            init = false;
            if let Err(k) = self.m.solve(&mut self.xn) {
                return Err(format!(
                    "singular matrix at {}: check for floating nodes, or loops of voltage sources / inductors",
                    self.c.names[k - 1]
                ));
            }
            if !self.xn.iter().all(|v| v.is_finite()) {
                return Err("numerical overflow".into());
            }
            // largest update relative to its tolerance (<= 1: converged)
            let step = (0..self.c.n).fold(0.0f64, |w, i| {
                let (a, b) = (self.xn[i + 1], self.x[i + 1]);
                w.max((a - b).abs() / (reltol * a.abs().max(b.abs()) + if self.c.is_current[i] { abstol } else { vntol }))
            });
            let conv = !lim && step <= 1.0;
            std::mem::swap(&mut self.x, &mut self.xn);
            if !self.c.nonlinear {
                // linear: exact after one solve; refresh state at the solution
                self.load(md, false);
                return Ok(1);
            }
            if conv && it > 0 {
                // device state (charges, capacitor currents) is still from the previous iterate; unless the last
                // update was negligible, re-evaluate it at the solution, or the time step leaves a0·C·Δx in the
                // trapezoidal current history (diode recovery and BJT storage times drifted with step count)
                if step > STATE_REFRESH {
                    self.load(md, false);
                }
                return Ok(it + 1);
            }
        }
        Err("Newton iteration did not converge".into())
    }

    /// DC operating point with gmin stepping and source stepping fallbacks.
    fn op(&mut self, md: &Mode, init: bool) -> Result<(), String> {
        let maxit = self.c.opts.itl1;
        let x0 = self.x.clone();
        let first = match self.newton(md, maxit, init) {
            Ok(_) => return Ok(()),
            Err(e) => e,
        };
        if first.starts_with("singular") {
            return Err(first);
        }
        // gmin stepping
        self.x = x0.clone();
        let mut ok = true;
        for k in 0..=10 {
            self.gshunt = 10f64.powi(-2 - k);
            if self.newton(md, maxit, k == 0).is_err() {
                ok = false;
                break;
            }
        }
        self.gshunt = 0.0;
        if ok && self.newton(md, maxit, false).is_ok() {
            return Ok(());
        }
        // source stepping
        self.x.fill(0.0);
        let (mut s, mut ds, mut first_it) = (0.0f64, 0.1f64, true);
        while s < 1.0 {
            let ns = (s + ds).min(1.0);
            let saved = self.x.clone();
            if self.newton(&Mode { src: ns, ..*md }, maxit, first_it).is_ok() {
                s = ns;
                ds = (ds * 1.5).min(0.3);
                first_it = false;
            } else {
                self.x = saved;
                ds /= 4.0;
                if ds < 1e-7 {
                    return Err(format!("DC operating point did not converge ({first}); gmin and source stepping failed"));
                }
            }
        }
        Ok(())
    }

    fn signal_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.c.out_nodes.iter().map(|(n, _)| n.clone()).collect();
        for d in &self.c.devices {
            if !d.name().contains('#') {
                v.extend(d.current_names());
            }
        }
        v
    }

    fn record(&self, cols: &mut [Vec<f64>]) {
        let mut k = 0;
        for &(_, i) in &self.c.out_nodes {
            cols[k].push(self.x[i]);
            k += 1;
        }
        for d in &self.c.devices {
            if !d.name().contains('#') {
                for v in d.currents(&self.x, &self.st) {
                    cols[k].push(v);
                    k += 1;
                }
            }
        }
    }

    fn plot(&self, kind: PlotKind, xname: &str, step: &str) -> Plot {
        let names = self.signal_names();
        let n = names.len();
        Plot { kind, xname: xname.into(), x: vec![], names, re: vec![vec![]; n], im: vec![], step: step.into() }
    }

    fn analyze(&mut self, an: &Analysis, prog: &Prog, step: &str) -> Result<Vec<Plot>, String> {
        match an {
            Analysis::Op => {
                self.op(&Mode::dc(false), true)?;
                let mut p = self.plot(PlotKind::Op, "op", step);
                p.x.push(0.0);
                self.record(&mut p.re);
                Ok(vec![p])
            }
            Analysis::Dc(sw) => self.dc(sw, prog, step),
            Analysis::Ac(f) => self.ac(f, prog, step).map(|p| vec![p]),
            &Analysis::Tran { tstep, tstop, tstart, tmax, uic } => self.tran(tstep, tstop, tstart, tmax, uic, prog, step).map(|p| vec![p]),
            Analysis::Tf { cur, a, b, src } => self.tf(*cur, a, b, src, step).map(|p| vec![p]),
            Analysis::Noise { out, refn, src, freqs } => self.noise(out, refn, src, freqs, prog, step).map(|p| vec![p]),
        }
    }

    /// Small-signal matrix at angular frequency `w` around the operating point (RHS: AC sources).
    fn ac_load(&self, m: &mut Matrix<C64>, w: f64) {
        m.clear();
        for d in &self.c.devices {
            d.load_ac(m, &self.x, w, &self.st);
        }
        for i in 1..=self.c.n {
            if !self.c.is_current[i - 1] {
                m.add(i, i, C64::from(self.c.opts.gmin));
            }
        }
    }

    /// Small-signal solve at angular frequency `w` around the operating point,
    /// driven by the sources' AC values plus 1 A injected into node `inj.0`
    /// and drawn from `inj.1`.
    fn ac_solve(&self, m: &mut Matrix<C64>, w: f64, inj: (usize, usize), x: &mut [C64]) -> Result<(), usize> {
        self.ac_load(m, w);
        m.i(inj.1, inj.0, C64::from(1.0));
        m.solve(x)
    }

    /// `.tf`: DC small-signal gain, input and output impedance at the operating point.
    fn tf(&mut self, cur: bool, a: &str, b: &str, src: &str, step: &str) -> Result<Plot, String> {
        self.op(&Mode::dc(false), true)?;
        let idx = |n: String| self.c.names.iter().position(|s| *s == n).map(|i| i + 1);
        let node = |n: &str| if matches!(n, "" | "0" | "gnd") { Some(0) } else { idx(format!("v({n})")) };
        let (op, on) = if cur {
            (idx(format!("i({a})")).ok_or(format!(".tf: '{a}' is not a voltage source"))?, 0)
        } else {
            (node(a).ok_or(format!(".tf: unknown node '{a}'"))?, node(b).ok_or(format!(".tf: unknown node '{b}'"))?)
        };
        let ksrc = idx(format!("i({src})"));
        let (mut sn, mut out_src) = (None, false);
        for d in self.c.devices.iter_mut() {
            if let Some(nodes) = d.set_ac(C64::default()) {
                if d.name() == src {
                    sn = Some(nodes);
                }
                out_src |= cur && d.name() == a;
            }
        }
        let (sa, sb) = sn.ok_or(format!(".tf: '{src}' is not an independent source"))?;
        if cur && !out_src {
            return Err(format!(".tf: I({a}) needs an independent voltage source"));
        }
        let n = self.c.n;
        let mut m = Matrix::<C64>::new(n);
        let mut x = vec![C64::default(); n + 1];
        let names = self.c.names.clone();
        let sing = |k: usize| format!(".tf: singular matrix at {}", names[k - 1]);
        let set = |eng: &mut Self, name: &str, v: f64| {
            for d in eng.c.devices.iter_mut().filter(|d| d.name() == name) {
                d.set_ac(C64::from(v));
            }
        };
        // 1 V (or 1 A) at the input: gain and input impedance
        set(self, src, 1.0);
        self.ac_solve(&mut m, 0.0, (0, 0), &mut x).map_err(sing)?;
        let gain = x[op].re - x[on].re;
        let zin = match ksrc {
            Some(k) => -1.0 / x[k].re,
            None => x[sb].re - x[sa].re,
        };
        set(self, src, 0.0);
        // output impedance: 1 A into the output node, or 1 V in the output source
        let zout = if cur {
            set(self, a, 1.0);
            self.ac_solve(&mut m, 0.0, (0, 0), &mut x).map_err(sing)?;
            -1.0 / x[op].re
        } else {
            self.ac_solve(&mut m, 0.0, (op, on), &mut x).map_err(sing)?;
            x[op].re - x[on].re
        };
        let out = match (cur, b) {
            (true, _) => format!("i({a})"),
            (false, "") => format!("v({a})"),
            _ => format!("v({a},{b})"),
        };
        let names = vec!["transfer_function".to_string(), format!("{src}#input_impedance"), format!("output_impedance_at_{out}")];
        Ok(Plot { kind: PlotKind::Op, xname: "tf".into(), x: vec![0.0], names, re: vec![vec![gain], vec![zin], vec![zout]], im: vec![], step: step.into() })
    }

    /// `.noise`: output noise density of V(out, refn) and each part's contribution, by the
    /// adjoint method: one solve of the transposed small-signal system per frequency gives the
    /// transfer from every noise current (and from the input source) to the output.
    fn noise(&mut self, out: &str, refn: &str, src: &str, freqs: &[f64], prog: &Prog, step: &str) -> Result<Plot, String> {
        self.op(&Mode::dc(false), true)?;
        let idx = |n: String| self.c.names.iter().position(|s| *s == n).map(|i| i + 1);
        let node = |n: &str| if matches!(n, "" | "0" | "gnd") { Some(0) } else { idx(format!("v({n})")) };
        let op = node(out).ok_or(format!(".noise: unknown node '{out}'"))?;
        let on = node(refn).ok_or(format!(".noise: unknown node '{refn}'"))?;
        let ksrc = idx(format!("i({src})"));
        let d = self.c.devices.iter_mut().find(|d| d.name() == src).ok_or(format!(".noise: unknown source '{src}'"))?;
        // the forward right-hand side is never used, so the source's AC value doesn't matter
        let (sa, sb) = d.set_ac(C64::from(1.0)).ok_or(format!(".noise: '{src}' is not an independent source"))?;
        // generators grouped by part (internal series resistors `Q1#rb` count for Q1)
        let (mut parts, mut gens): (Vec<String>, Vec<(usize, Noise)>) = (vec![], vec![]);
        let mut buf = vec![];
        for d in &self.c.devices {
            d.noise(&self.x, &self.st, self.c.temp, &mut buf);
            if buf.is_empty() {
                continue;
            }
            let name = d.name().split('#').next().unwrap_or_default();
            let k = parts.iter().position(|p| p == name).unwrap_or_else(|| {
                parts.push(name.to_string());
                parts.len() - 1
            });
            gens.extend(buf.drain(..).map(|g| (k, g)));
        }
        let n = self.c.n;
        let (mut m, mut t) = (Matrix::<C64>::new(n), Matrix::<C64>::new(n));
        let mut y = vec![C64::default(); n + 1];
        let mut names = vec!["v(onoise)".to_string(), "v(inoise)".to_string()];
        names.extend(parts.iter().map(|p| format!("v({p})")));
        let mut p = Plot { kind: PlotKind::Noise, xname: "frequency".into(), x: vec![], re: vec![vec![]; names.len()], names, im: vec![], step: step.into() };
        let mut dev = vec![0.0; parts.len()];
        for (fi, &f) in freqs.iter().enumerate() {
            prog.check()?;
            self.ac_load(&mut m, 2.0 * std::f64::consts::PI * f);
            m.transpose_into(&mut t);
            t.rhs(op, C64::from(1.0));
            t.rhs(on, C64::from(-1.0));
            t.solve(&mut y).map_err(|k| format!(".noise: singular matrix at {} (f={})", self.c.names[k - 1], fmt_si(f)))?;
            // unit input: a voltage source drives its branch row, a current source flows sa → sb
            let gain = match ksrc {
                Some(k) => y[k],
                None => y[sb] - y[sa],
            }
            .norm()
            .max(1e-10); // ngspice's N_MINGAIN (on gain²)
            dev.fill(0.0);
            for (k, g) in &gens {
                let h = y[g.b] - y[g.a];
                dev[*k] += (h.re * h.re + h.im * h.im) * g.psd(f);
            }
            let tot = dev.iter().sum::<f64>().sqrt();
            p.x.push(f);
            p.re[0].push(tot);
            p.re[1].push(tot / gain);
            for (col, v) in p.re[2..].iter_mut().zip(&dev) {
                col.push(v.sqrt());
            }
            prog.set((fi + 1) as f64 / freqs.len() as f64);
        }
        Ok(p)
    }

    fn set_src(&mut self, name: &str, v: f64) -> Result<(), String> {
        let d = self.c.devices.iter_mut().find(|d| d.name() == name).ok_or(format!(".dc: unknown source '{name}'"))?;
        if d.set_dc(v) { Ok(()) } else { Err(format!(".dc: '{name}' is not an independent source")) }
    }

    fn dc(&mut self, sw: &[(String, Vec<f64>)], prog: &Prog, step: &str) -> Result<Vec<Plot>, String> {
        let (inner, iv) = (&sw[0].0, &sw[0].1);
        let outer: Vec<Option<f64>> = sw.get(1).map_or(vec![None], |s| s.1.iter().map(|&v| Some(v)).collect());
        let mut plots = vec![];
        let total = (outer.len() * iv.len()) as f64;
        let mut done = 0.0;
        for ov in outer {
            let mut label = step.to_string();
            if let Some(v) = ov {
                self.set_src(&sw[1].0, v)?;
                label = format!("{label} {}={}", sw[1].0, fmt_si(v)).trim().to_string();
            }
            let mut p = self.plot(PlotKind::Dc, inner, &label);
            for (k, &v) in iv.iter().enumerate() {
                prog.check()?;
                self.set_src(inner, v)?;
                let md = Mode::dc(false);
                if k == 0 || self.newton(&md, self.c.opts.itl1, false).is_err() {
                    self.op(&md, k == 0).map_err(|e| format!("{e} at {inner}={}", fmt_si(v)))?;
                }
                p.x.push(v);
                self.record(&mut p.re);
                done += 1.0;
                prog.set(done / total);
            }
            plots.push(p);
        }
        Ok(plots)
    }

    fn ac(&mut self, freqs: &[f64], prog: &Prog, step: &str) -> Result<Plot, String> {
        self.op(&Mode::dc(false), true)?;
        let st = self.st.clone();
        let n = self.c.n;
        let mut mc = Matrix::<C64>::new(n);
        let mut xc = vec![C64::default(); n + 1];
        let mut p = self.plot(PlotKind::Ac, "frequency", step);
        p.im = vec![vec![]; p.names.len()];
        for (fi, &f) in freqs.iter().enumerate() {
            prog.check()?;
            let w = 2.0 * std::f64::consts::PI * f;
            self.ac_solve(&mut mc, w, (0, 0), &mut xc).map_err(|k| format!("AC: singular matrix at {} (f={})", self.c.names[k - 1], fmt_si(f)))?;
            p.x.push(f);
            let mut k = 0;
            for &(_, i) in &self.c.out_nodes {
                p.re[k].push(xc[i].re);
                p.im[k].push(xc[i].im);
                k += 1;
            }
            for d in &self.c.devices {
                if !d.name().contains('#') {
                    for v in d.ac_currents(&self.x, &xc, w, &st) {
                        p.re[k].push(v.re);
                        p.im[k].push(v.im);
                        k += 1;
                    }
                }
            }
            prog.set((fi + 1) as f64 / freqs.len() as f64);
        }
        Ok(p)
    }

    #[allow(clippy::too_many_arguments)]
    fn tran(&mut self, tstep: f64, tstop: f64, tstart: f64, tmax: f64, uic: bool, prog: &Prog, step: &str) -> Result<Plot, String> {
        let o = self.c.opts.clone();
        let mut tmax = if tmax > 0.0 { tmax } else { (tstop - tstart) / 250.0 };
        if o.maxstep > 0.0 {
            tmax = tmax.min(o.maxstep);
        }
        // initial conditions
        if uic {
            self.x.fill(0.0);
            for &(i, v) in &self.c.ics {
                self.x[i] = v;
            }
            for d in &self.c.devices {
                d.init_x(&mut self.x);
            }
            self.load(&Mode { uic: true, ..Mode::dc(true) }, false);
        } else {
            self.force_ic = !self.c.ics.is_empty();
            let r = self.op(&Mode::dc(true), true);
            self.force_ic = false;
            r.map_err(|e| format!("transient initial operating point: {e}"))?;
        }
        self.st0.copy_from_slice(&self.st);

        let mut bps = vec![tstop];
        for d in &self.c.devices {
            d.breakpoints(tstop, &mut bps);
        }
        bps.retain(|&t| t > 0.0 && t <= tstop);
        bps.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let eps = tstop * 1e-12;
        bps.dedup_by(|a, b| (*a - *b).abs() < eps);

        let mut p = self.plot(PlotKind::Tran, "time", step);
        if tstart <= 0.0 {
            p.x.push(0.0);
            self.record(&mut p.re);
        }
        let n = self.c.n;
        let dtmin = (tstop * 1e-14).max(1e-18);
        let mut dt = ((tstop / 100.0).min(if tstep > 0.0 { tstep } else { tstop }) / 10.0).min(tmax);
        let mut t = 0.0;
        let mut bi = 0;
        // history of accepted points, most recent first
        let mut hist: Vec<(f64, Vec<f64>)> = vec![(0.0, self.x.clone())];
        let mut saved = self.x.clone();
        let mut xp = vec![0.0; n + 1];
        while t < tstop - eps {
            prog.check()?;
            while bi < bps.len() && bps[bi] <= t + eps {
                bi += 1;
            }
            let bp = bps.get(bi).copied().unwrap_or(tstop);
            let mut h = dt.min(tmax);
            let mut hit = false;
            if t + h >= bp - eps {
                h = bp - t;
                hit = true;
            } else if t + 1.6 * h > bp {
                h = (bp - t) / 2.0;
            }
            let be = hist.len() < 3;
            let md = Mode { time: t + h, dc: false, tran: true, a0: if be { 1.0 / h } else { 2.0 / h }, b1: if be { 0.0 } else { 1.0 }, src: 1.0, uic: false };
            saved.copy_from_slice(&self.x);
            let iters = match self.newton(&md, o.itl4, false) {
                Ok(i) => i,
                Err(e) => {
                    self.x.copy_from_slice(&saved);
                    dt = h / 8.0;
                    if dt < dtmin {
                        return Err(format!("time step too small at t={}s: {e}", fmt_si(t)));
                    }
                    continue;
                }
            };
            // local truncation error estimate from a polynomial predictor
            let mut grow = 2.0;
            if hist.len() >= 2 {
                let (t0, x0) = (&hist[0].0, &hist[0].1);
                let (t1, x1) = (&hist[1].0, &hist[1].1);
                let tn = t + h;
                let h1 = t0 - t1;
                let (kappa, order) = if be || hist.len() < 3 {
                    for i in 1..=n {
                        xp[i] = x0[i] + (x0[i] - x1[i]) * h / h1;
                    }
                    (h / (h + h1), 2.0)
                } else {
                    let (t2, x2) = (&hist[2].0, &hist[2].1);
                    let h2 = t1 - t2;
                    let l0 = (tn - t1) * (tn - t2) / ((t0 - t1) * (t0 - t2));
                    let l1 = (tn - t0) * (tn - t2) / ((t1 - t0) * (t1 - t2));
                    let l2 = (tn - t0) * (tn - t1) / ((t2 - t0) * (t2 - t1));
                    for i in 1..=n {
                        xp[i] = l0 * x0[i] + l1 * x1[i] + l2 * x2[i];
                    }
                    (h * h / (2.0 * (h + h1) * (h + h1 + h2)), 3.0)
                };
                let mut r: f64 = 0.0;
                for i in 1..=n {
                    let xc = self.x[i];
                    let abs = if self.c.is_current[i - 1] { o.abstol * 1e3 } else { o.vntol };
                    let tol = o.trtol * (o.reltol * xc.abs().max(x0[i].abs()) + abs);
                    r = r.max(kappa * (xc - xp[i]).abs() / tol);
                }
                if r > 1.0 && h > dtmin * 10.0 {
                    self.x.copy_from_slice(&saved);
                    dt = h * (0.9 * r.powf(-1.0 / order)).max(0.2);
                    continue;
                }
                grow = if r > 0.0 { (0.9 * r.powf(-1.0 / order)).clamp(0.5, 2.0) } else { 2.0 };
            }
            if iters > o.itl4 / 2 {
                grow = grow.min(1.0);
            }
            // accept
            t = if hit { bp } else { t + h };
            self.st0.copy_from_slice(&self.st);
            if t >= tstart - eps {
                p.x.push(t);
                self.record(&mut p.re);
            }
            if hit {
                hist.clear();
                hist.push((t, self.x.clone()));
                let next = bps.get(bi + 1).copied().unwrap_or(tstop);
                dt = h.min(0.1 * (next - t).max(dtmin * 10.0));
            } else {
                if hist.len() >= 3 {
                    let mut old = hist.pop().unwrap();
                    old.0 = t;
                    old.1.copy_from_slice(&self.x);
                    hist.insert(0, old);
                } else {
                    hist.insert(0, (t, self.x.clone()));
                }
                dt = h * grow;
            }
            prog.set(t / tstop);
        }
        Ok(p)
    }
}
