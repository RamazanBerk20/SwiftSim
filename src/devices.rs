//! Device models. Each device stamps its Newton-linearized companion model
//! into the MNA matrix (DC/transient) or its small-signal model (AC).
//!
//! Sign conventions follow SPICE: a device current is positive when it flows
//! into the first terminal (I(R1) from n+ to n-, I(V1) into the + pin,
//! Ic(Q1) into the collector, Id(M1) into the drain).

use crate::expr::{Env, Expr};
use crate::solver::{C64, Matrix};
use std::collections::HashMap;

/// Boltzmann constant over electron charge, V/K (CODATA 2014, the values ngspice uses;
/// SPICE3's 1.3806226e-23/1.6021918e-19 shifts junction currents by ~3e-4).
pub const KOQ: f64 = 1.38064852e-23 / 1.6021766208e-19;
/// Electron charge, C (CODATA 2014, as ngspice).
pub const Q: f64 = 1.6021766208e-19;
/// Thermal voltage kT/q at 27 °C.
pub const VT: f64 = KOQ * 300.15;
/// SPICE reference temperature for the bandgap and junction formulas (27 °C).
const TREF: f64 = 300.15;

/// Silicon bandgap (eV) at `t` kelvin.
fn egap(t: f64) -> f64 {
    1.16 - 7.02e-4 * t * t / (t + 1108.0)
}

fn pbfact(t: f64) -> f64 {
    -3.0 * KOQ * t * (t / TREF).ln() + egap(t) - t / TREF * egap(TREF)
}

/// Junction potential and zero-bias capacitance moved from `tnom` to `t`
/// (kelvin), SPICE3 rules.
pub fn junction_at(vj: f64, cj: f64, m: f64, t: f64, tnom: f64) -> (f64, f64) {
    let pbo = (vj - pbfact(tnom)) * TREF / tnom;
    let cj0 = cj / (1.0 + m * (4e-4 * (tnom - TREF) - (vj - pbo) / pbo));
    let vjt = pbfact(t) + t / TREF * pbo;
    (vjt, cj0 * (1.0 + m * (4e-4 * (t - TREF) - (vjt - pbo) / pbo)))
}

pub struct LoadCtx<'a> {
    /// Current Newton iterate, x[0] = ground = 0.
    pub x: &'a [f64],
    /// Device state at the last accepted time point.
    pub st0: &'a [f64],
    pub time: f64,
    /// DC solution: capacitors open, inductors short.
    pub dc: bool,
    /// Part of a transient analysis (sources follow their waveform).
    pub tran: bool,
    /// Integration: i = a0*(q - q0) - b1*i0 (BE: a0=1/h,b1=0; trap: a0=2/h,b1=1).
    pub a0: f64,
    pub b1: f64,
    /// Independent-source scale for source stepping.
    pub src: f64,
    pub gmin: f64,
    /// First Newton iteration: start junctions at a sane bias.
    pub init: bool,
    /// Transient start with UIC: capacitors take their IC.
    pub uic: bool,
}

pub struct Setup {
    pub n: usize,
    pub names: Vec<String>,
    pub is_current: Vec<bool>,
    pub nstate: usize,
    pub nodes: HashMap<String, usize>,
    pub branches: HashMap<String, usize>,
}

impl Setup {
    pub fn branch(&mut self, dev: &str) -> usize {
        self.n += 1;
        self.names.push(format!("i({dev})"));
        self.is_current.push(true);
        self.branches.insert(dev.to_string(), self.n);
        self.n
    }
    pub fn state(&mut self, k: usize) -> usize {
        let s = self.nstate;
        self.nstate += k;
        s
    }
    pub fn find_branch(&self, dev: &str) -> Result<usize, String> {
        self.branches
            .get(dev)
            .copied()
            .ok_or_else(|| format!("'{dev}' is not a voltage source, inductor or other branch device"))
    }
}

/// Noise current generator between nodes `a` and `b` with power spectral density
/// `white + flicker / f^ef` (A²/Hz). Generators are uncorrelated.
pub struct Noise {
    pub a: usize,
    pub b: usize,
    pub white: f64,
    pub flicker: f64,
    pub ef: f64,
}

impl Noise {
    pub fn psd(&self, f: f64) -> f64 {
        if self.flicker > 0.0 { self.white + self.flicker / f.powf(self.ef) } else { self.white }
    }
    /// Thermal noise 4kT·g of conductance `g` at `temp` kelvin.
    fn thermal(a: usize, b: usize, g: f64, temp: f64) -> Noise {
        Noise { a, b, white: 4.0 * KOQ * Q * temp * g.abs(), flicker: 0.0, ef: 1.0 }
    }
    /// Shot noise 2q|i| plus flicker kf·|i|^af / f.
    fn shot(a: usize, b: usize, i: f64, kf: f64, af: f64) -> Noise {
        Noise { a, b, white: 2.0 * Q * i.abs(), flicker: kf * i.abs().powf(af), ef: 1.0 }
    }
}

pub trait Device: Send {
    fn name(&self) -> &str;
    fn setup(&mut self, _s: &mut Setup) {}
    fn bind(&mut self, _s: &Setup) -> Result<(), String> {
        Ok(())
    }
    fn nonlinear(&self) -> bool {
        false
    }
    /// Stamp the linearized model. Returns true if a voltage was limited
    /// (the iteration must not be declared converged).
    fn load(&mut self, m: &mut Matrix<f64>, c: &LoadCtx, st: &mut [f64]) -> bool;
    /// Small-signal stamp around operating point `x`.
    fn load_ac(&self, m: &mut Matrix<C64>, x: &[f64], w: f64, st: &[f64]);
    fn init_x(&self, _x: &mut [f64]) {}
    fn breakpoints(&self, _tstop: f64, _out: &mut Vec<f64>) {}
    fn set_dc(&mut self, _v: f64) -> bool {
        false
    }
    /// Independent sources: set the AC excitation and return the (n+, n-) nodes.
    fn set_ac(&mut self, _v: C64) -> Option<(usize, usize)> {
        None
    }
    /// Noise generators at operating point `x`, circuit temperature `temp` (kelvin).
    fn noise(&self, _x: &[f64], _st: &[f64], _temp: f64, _out: &mut Vec<Noise>) {}
    fn current_names(&self) -> Vec<String>;
    fn currents(&self, x: &[f64], st: &[f64]) -> Vec<f64>;
    fn ac_currents(&self, x: &[f64], v: &[C64], w: f64, st: &[f64]) -> Vec<C64>;
}

/// Linearized current I(v) flowing from node `a` to node `b` through a device:
/// I ≈ i0 + Σ g·(V(p) - V(n) - v0) for each (p, n, g, v0).
#[inline]
fn nl(m: &mut Matrix<f64>, a: usize, b: usize, i0: f64, terms: &[(usize, usize, f64, f64)]) {
    let mut ieq = i0;
    for &(p, n, g, v0) in terms {
        m.vccs(a, b, p, n, g);
        ieq -= g * v0;
    }
    m.i(a, b, ieq);
}

/// Integrate charge `q` (capacitance `cap`) at state slot `s`.
/// Returns (geq, current) at the present iterate.
#[inline]
fn integ(c: &LoadCtx, st: &mut [f64], s: usize, q: f64, cap: f64) -> (f64, f64) {
    st[s] = q;
    if c.dc {
        st[s + 1] = 0.0;
        return (0.0, 0.0);
    }
    let i = c.a0 * (q - c.st0[s]) - c.b1 * c.st0[s + 1];
    st[s + 1] = i;
    (c.a0 * cap, i)
}

/// Stamp charge q(v) between a and b; v is V(a)-V(b) at the iterate.
#[inline]
fn stamp_q(m: &mut Matrix<f64>, c: &LoadCtx, st: &mut [f64], s: usize, a: usize, b: usize, q: f64, cap: f64, v: f64) {
    let (g, i) = integ(c, st, s, q, cap);
    if !c.dc {
        nl(m, a, b, i, &[(a, b, g, v)]);
    }
}

fn jw(w: f64, c: f64) -> C64 {
    C64::new(0.0, w * c)
}

/// Depletion charge and capacitance with forward-bias linearization (FC).
pub fn qdep(v: f64, cj: f64, vj: f64, m: f64, fc: f64) -> (f64, f64) {
    if cj == 0.0 {
        return (0.0, 0.0);
    }
    let m = m.min(0.95);
    let fcv = fc * vj;
    if v < fcv {
        let a = 1.0 - v / vj;
        let s = a.powf(-m);
        (cj * vj * (1.0 - a * s) / (1.0 - m), cj * s)
    } else {
        let f1 = vj / (1.0 - m) * (1.0 - (1.0 - fc).powf(1.0 - m));
        let f2 = (1.0 - fc).powf(1.0 + m);
        let f3 = 1.0 - fc * (1.0 + m);
        let q = cj * (f1 + (f3 * (v - fcv) + m / (2.0 * vj) * (v * v - fcv * fcv)) / f2);
        (q, cj / f2 * (f3 + m * v / vj))
    }
}

/// SPICE3 pn-junction voltage limiting.
pub fn pnjlim(vnew: f64, vold: f64, vt: f64, vcrit: f64) -> f64 {
    if vnew > vcrit && (vnew - vold).abs() > 2.0 * vt {
        if vold > 0.0 {
            let arg = 1.0 + (vnew - vold) / vt;
            if arg > 0.0 { vold + vt * arg.ln() } else { vcrit }
        } else {
            vt * (vnew / vt).ln()
        }
    } else {
        vnew
    }
}

/// SPICE3 FET gate-voltage limiting.
pub fn fetlim(vnew: f64, vold: f64, vto: f64) -> f64 {
    let vtsthi = (2.0 * (vold - vto)).abs() + 2.0;
    let vtstlo = vtsthi / 2.0 + 2.0;
    let vtox = vto + 3.5;
    let delv = vnew - vold;
    if vold >= vto {
        if vold >= vtox {
            if delv <= 0.0 {
                if vnew >= vtox {
                    if -delv > vtstlo {
                        return vold - vtstlo;
                    }
                } else {
                    return vnew.max(vto + 2.0);
                }
            } else if delv >= vtsthi {
                return vold + vtsthi;
            }
        } else if delv <= 0.0 {
            return vnew.max(vto - 0.5);
        } else {
            return vnew.min(vto + 4.0);
        }
    } else if delv <= 0.0 {
        if -delv > vtsthi {
            return vold - vtsthi;
        }
    } else {
        let vtemp = vto + 0.5;
        if vnew <= vtemp {
            if delv > vtstlo {
                return vold + vtstlo;
            }
        } else {
            return vtemp;
        }
    }
    vnew
}

pub fn limvds(vnew: f64, vold: f64) -> f64 {
    if vold >= 3.5 {
        if vnew > vold {
            vnew.min(3.0 * vold + 2.0)
        } else if vnew < 3.5 {
            vnew.max(2.0)
        } else {
            vnew
        }
    } else if vnew > vold {
        vnew.min(4.0)
    } else {
        vnew.max(-0.5)
    }
}

// ---------------------------------------------------------------- passives

pub struct Resistor {
    pub name: String,
    pub a: usize,
    pub b: usize,
    pub r: f64,
}

impl Device for Resistor {
    fn name(&self) -> &str {
        &self.name
    }
    fn load(&mut self, m: &mut Matrix<f64>, _c: &LoadCtx, _st: &mut [f64]) -> bool {
        m.g(self.a, self.b, 1.0 / self.r);
        false
    }
    fn load_ac(&self, m: &mut Matrix<C64>, _x: &[f64], _w: f64, _st: &[f64]) {
        m.g(self.a, self.b, C64::from(1.0 / self.r));
    }
    fn noise(&self, _x: &[f64], _st: &[f64], temp: f64, out: &mut Vec<Noise>) {
        out.push(Noise::thermal(self.a, self.b, 1.0 / self.r, temp));
    }
    fn current_names(&self) -> Vec<String> {
        vec![format!("i({})", self.name)]
    }
    fn currents(&self, x: &[f64], _st: &[f64]) -> Vec<f64> {
        vec![(x[self.a] - x[self.b]) / self.r]
    }
    fn ac_currents(&self, _x: &[f64], v: &[C64], _w: f64, _st: &[f64]) -> Vec<C64> {
        vec![(v[self.a] - v[self.b]) * C64::from(1.0 / self.r)]
    }
}

pub struct Capacitor {
    pub name: String,
    pub a: usize,
    pub b: usize,
    pub c: f64,
    pub ic: Option<f64>,
    pub s: usize,
}

impl Device for Capacitor {
    fn name(&self) -> &str {
        &self.name
    }
    fn setup(&mut self, s: &mut Setup) {
        self.s = s.state(2);
    }
    fn load(&mut self, m: &mut Matrix<f64>, c: &LoadCtx, st: &mut [f64]) -> bool {
        let v = match (c.uic, self.ic) {
            (true, Some(ic)) => ic,
            _ => c.x[self.a] - c.x[self.b],
        };
        stamp_q(m, c, st, self.s, self.a, self.b, self.c * v, self.c, v);
        false
    }
    fn load_ac(&self, m: &mut Matrix<C64>, _x: &[f64], w: f64, _st: &[f64]) {
        m.g(self.a, self.b, jw(w, self.c));
    }
    fn current_names(&self) -> Vec<String> {
        vec![format!("i({})", self.name)]
    }
    fn currents(&self, _x: &[f64], st: &[f64]) -> Vec<f64> {
        vec![st[self.s + 1]]
    }
    fn ac_currents(&self, _x: &[f64], v: &[C64], w: f64, _st: &[f64]) -> Vec<C64> {
        vec![(v[self.a] - v[self.b]) * jw(w, self.c)]
    }
}

pub struct Inductor {
    pub name: String,
    pub a: usize,
    pub b: usize,
    pub l: f64,
    pub ic: Option<f64>,
    pub k: usize,
    pub s: usize,
    /// Mutual couplings by inductor name and M (henry).
    pub coupling: Vec<(String, f64)>,
    pub coup: Vec<(usize, f64)>,
}

impl Device for Inductor {
    fn name(&self) -> &str {
        &self.name
    }
    fn setup(&mut self, s: &mut Setup) {
        self.k = s.branch(&self.name);
        self.s = s.state(2);
    }
    fn bind(&mut self, s: &Setup) -> Result<(), String> {
        self.coup = self.coupling.iter().map(|(n, m)| Ok((s.find_branch(n)?, *m))).collect::<Result<_, String>>()?;
        Ok(())
    }
    fn init_x(&self, x: &mut [f64]) {
        if let Some(ic) = self.ic {
            x[self.k] = ic;
        }
    }
    fn load(&mut self, m: &mut Matrix<f64>, c: &LoadCtx, st: &mut [f64]) -> bool {
        let k = self.k;
        m.branch(self.a, self.b, k);
        let phi = self.l * c.x[k] + self.coup.iter().map(|&(j, mm)| mm * c.x[j]).sum::<f64>();
        st[self.s] = phi;
        if c.dc {
            st[self.s + 1] = 0.0;
        } else {
            m.add(k, k, -c.a0 * self.l);
            for &(j, mm) in &self.coup {
                m.add(k, j, -c.a0 * mm);
            }
            m.rhs(k, -c.a0 * c.st0[self.s] - c.b1 * c.st0[self.s + 1]);
            st[self.s + 1] = c.a0 * (phi - c.st0[self.s]) - c.b1 * c.st0[self.s + 1];
        }
        false
    }
    fn load_ac(&self, m: &mut Matrix<C64>, _x: &[f64], w: f64, _st: &[f64]) {
        m.branch(self.a, self.b, self.k);
        m.add(self.k, self.k, -jw(w, self.l));
        for &(j, mm) in &self.coup {
            m.add(self.k, j, -jw(w, mm));
        }
    }
    fn current_names(&self) -> Vec<String> {
        vec![format!("i({})", self.name)]
    }
    fn currents(&self, x: &[f64], _st: &[f64]) -> Vec<f64> {
        vec![x[self.k]]
    }
    fn ac_currents(&self, _x: &[f64], v: &[C64], _w: f64, _st: &[f64]) -> Vec<C64> {
        vec![v[self.k]]
    }
}

// ---------------------------------------------------------------- sources

#[derive(Clone, Debug)]
pub enum Wave {
    Pulse { v1: f64, v2: f64, td: f64, tr: f64, tf: f64, pw: f64, per: f64, ncyc: f64 },
    Sin { vo: f64, va: f64, freq: f64, td: f64, theta: f64, phi: f64, ncyc: f64 },
    Exp { v1: f64, v2: f64, td1: f64, tau1: f64, td2: f64, tau2: f64 },
    Pwl(Vec<(f64, f64)>),
    Sffm { vo: f64, va: f64, fc: f64, mdi: f64, fs: f64 },
}

/// PULSE rise/fall time used when the netlist gives 0 (explicit edges are kept as written).
const MIN_EDGE: f64 = 1e-9;

fn edge(t: f64) -> f64 {
    if t > 0.0 { t } else { MIN_EDGE }
}

impl Wave {
    pub fn parse(kind: &str, a: &[f64]) -> Result<Wave, String> {
        let g = |i: usize, d: f64| a.get(i).copied().unwrap_or(d);
        Ok(match kind {
            "pulse" => Wave::Pulse {
                v1: g(0, 0.0),
                v2: g(1, 0.0),
                td: g(2, 0.0),
                tr: g(3, 0.0),
                tf: g(4, 0.0),
                pw: g(5, f64::INFINITY),
                per: g(6, 0.0),
                ncyc: g(7, 0.0),
            },
            "sin" | "sine" => Wave::Sin {
                vo: g(0, 0.0),
                va: g(1, 0.0),
                freq: g(2, 0.0),
                td: g(3, 0.0),
                theta: g(4, 0.0),
                phi: g(5, 0.0),
                ncyc: g(6, 0.0),
            },
            "exp" => Wave::Exp {
                v1: g(0, 0.0),
                v2: g(1, 0.0),
                td1: g(2, 0.0),
                tau1: g(3, 1e-9).max(1e-18),
                td2: g(4, f64::INFINITY),
                tau2: g(5, 1e-9).max(1e-18),
            },
            "pwl" => {
                if a.len() < 2 || a.len() % 2 != 0 {
                    return Err("PWL needs time/value pairs".into());
                }
                Wave::Pwl(a.chunks(2).map(|p| (p[0], p[1])).collect())
            }
            "sffm" => Wave::Sffm { vo: g(0, 0.0), va: g(1, 0.0), fc: g(2, 0.0), mdi: g(3, 0.0), fs: g(4, 0.0) },
            _ => return Err(format!("unknown source function '{kind}'")),
        })
    }

    pub fn at(&self, t: f64) -> f64 {
        match *self {
            Wave::Pulse { v1, v2, td, tr, tf, pw, per, ncyc } => {
                if t < td {
                    return v1;
                }
                let (tr, tf) = (edge(tr), edge(tf));
                let mut tt = t - td;
                if per > 0.0 {
                    if ncyc > 0.0 && tt >= per * ncyc {
                        return v1;
                    }
                    tt -= (tt / per).floor() * per;
                }
                if tt < tr {
                    v1 + (v2 - v1) * tt / tr
                } else if tt < tr + pw {
                    v2
                } else if tt < tr + pw + tf {
                    v2 + (v1 - v2) * (tt - tr - pw) / tf
                } else {
                    v1
                }
            }
            Wave::Sin { vo, va, freq, td, theta, phi, ncyc } => {
                let ph = phi.to_radians();
                if t < td || (ncyc > 0.0 && freq > 0.0 && t > td + ncyc / freq) {
                    return vo + va * ph.sin();
                }
                let tt = t - td;
                vo + va * (-theta * tt).exp() * (2.0 * std::f64::consts::PI * freq * tt + ph).sin()
            }
            Wave::Exp { v1, v2, td1, tau1, td2, tau2 } => {
                let mut v = v1;
                if t > td1 {
                    v += (v2 - v1) * (1.0 - (-(t - td1) / tau1).exp());
                }
                if t > td2 {
                    v += (v1 - v2) * (1.0 - (-(t - td2) / tau2).exp());
                }
                v
            }
            Wave::Pwl(ref p) => {
                if t <= p[0].0 {
                    return p[0].1;
                }
                let i = p.partition_point(|q| q.0 <= t);
                if i >= p.len() {
                    return p[p.len() - 1].1;
                }
                let (t0, v0) = p[i - 1];
                let (t1, v1) = p[i];
                if t1 <= t0 { v1 } else { v0 + (v1 - v0) * (t - t0) / (t1 - t0) }
            }
            Wave::Sffm { vo, va, fc, mdi, fs } => {
                let w = 2.0 * std::f64::consts::PI;
                vo + va * (w * fc * t + mdi * (w * fs * t).sin()).sin()
            }
        }
    }

    pub fn breakpoints(&self, tstop: f64, out: &mut Vec<f64>) {
        match *self {
            Wave::Pulse { td, tr, tf, pw, per, ncyc, .. } => {
                let (tr, tf) = (edge(tr), edge(tf));
                let mut k = 0.0;
                loop {
                    let b = td + k * per;
                    if b > tstop || out.len() > 2_000_000 {
                        break;
                    }
                    for t in [b, b + tr, b + tr + pw, b + tr + pw + tf] {
                        if t.is_finite() {
                            out.push(t);
                        }
                    }
                    k += 1.0;
                    if per <= 0.0 || (ncyc > 0.0 && k >= ncyc) {
                        break;
                    }
                }
            }
            Wave::Sin { td, .. } => out.push(td),
            Wave::Exp { td1, td2, .. } => out.extend([td1, td2].into_iter().filter(|t| t.is_finite())),
            Wave::Pwl(ref p) => out.extend(p.iter().map(|q| q.0)),
            Wave::Sffm { .. } => {}
        }
    }
}

pub struct VSource {
    pub name: String,
    pub a: usize,
    pub b: usize,
    pub k: usize,
    pub dc: f64,
    pub wave: Option<Wave>,
    pub ac: C64,
    pub rser: f64,
}

impl VSource {
    fn value(&self, c: &LoadCtx) -> f64 {
        c.src
            * match (&self.wave, c.tran) {
                (Some(w), true) => w.at(c.time),
                _ => self.dc,
            }
    }
}

impl Device for VSource {
    fn name(&self) -> &str {
        &self.name
    }
    fn setup(&mut self, s: &mut Setup) {
        self.k = s.branch(&self.name);
    }
    fn load(&mut self, m: &mut Matrix<f64>, c: &LoadCtx, _st: &mut [f64]) -> bool {
        m.branch(self.a, self.b, self.k);
        if self.rser > 0.0 {
            m.add(self.k, self.k, -self.rser);
        }
        m.rhs(self.k, self.value(c));
        false
    }
    fn load_ac(&self, m: &mut Matrix<C64>, _x: &[f64], _w: f64, _st: &[f64]) {
        m.branch(self.a, self.b, self.k);
        if self.rser > 0.0 {
            m.add(self.k, self.k, C64::from(-self.rser));
        }
        m.rhs(self.k, self.ac);
    }
    fn breakpoints(&self, tstop: f64, out: &mut Vec<f64>) {
        if let Some(w) = &self.wave {
            w.breakpoints(tstop, out);
        }
    }
    fn set_dc(&mut self, v: f64) -> bool {
        self.dc = v;
        self.wave = None;
        true
    }
    fn set_ac(&mut self, v: C64) -> Option<(usize, usize)> {
        self.ac = v;
        Some((self.a, self.b))
    }
    fn current_names(&self) -> Vec<String> {
        vec![format!("i({})", self.name)]
    }
    fn currents(&self, x: &[f64], _st: &[f64]) -> Vec<f64> {
        vec![x[self.k]]
    }
    fn ac_currents(&self, _x: &[f64], v: &[C64], _w: f64, _st: &[f64]) -> Vec<C64> {
        vec![v[self.k]]
    }
}

pub struct ISource {
    pub name: String,
    pub a: usize,
    pub b: usize,
    pub dc: f64,
    pub wave: Option<Wave>,
    pub ac: C64,
}

impl ISource {
    fn value(&self, c: &LoadCtx) -> f64 {
        c.src
            * match (&self.wave, c.tran) {
                (Some(w), true) => w.at(c.time),
                _ => self.dc,
            }
    }
}

impl Device for ISource {
    fn name(&self) -> &str {
        &self.name
    }
    fn load(&mut self, m: &mut Matrix<f64>, c: &LoadCtx, _st: &mut [f64]) -> bool {
        m.i(self.a, self.b, self.value(c));
        false
    }
    fn load_ac(&self, m: &mut Matrix<C64>, _x: &[f64], _w: f64, _st: &[f64]) {
        m.i(self.a, self.b, self.ac);
    }
    fn breakpoints(&self, tstop: f64, out: &mut Vec<f64>) {
        if let Some(w) = &self.wave {
            w.breakpoints(tstop, out);
        }
    }
    fn set_dc(&mut self, v: f64) -> bool {
        self.dc = v;
        self.wave = None;
        true
    }
    fn set_ac(&mut self, v: C64) -> Option<(usize, usize)> {
        self.ac = v;
        Some((self.a, self.b))
    }
    fn current_names(&self) -> Vec<String> {
        vec![format!("i({})", self.name)]
    }
    fn currents(&self, _x: &[f64], _st: &[f64]) -> Vec<f64> {
        vec![self.wave.as_ref().map_or(self.dc, |_| f64::NAN)]
    }
    fn ac_currents(&self, _x: &[f64], _v: &[C64], _w: f64, _st: &[f64]) -> Vec<C64> {
        vec![self.ac]
    }
}

/// E (VCVS), F (CCCS), G (VCCS), H (CCVS).
pub struct Controlled {
    pub name: String,
    pub kind: char,
    pub a: usize,
    pub b: usize,
    pub cp: usize,
    pub cn: usize,
    pub ctrl: String,
    pub kc: usize,
    pub gain: f64,
    pub k: usize,
}

impl Controlled {
    fn stamp<T: crate::solver::Scalar>(&self, m: &mut Matrix<T>) {
        let g = T::from(self.gain);
        match self.kind {
            'e' => {
                m.branch(self.a, self.b, self.k);
                m.add(self.k, self.cp, -g);
                m.add(self.k, self.cn, g);
            }
            'g' => m.vccs(self.a, self.b, self.cp, self.cn, g),
            'f' => {
                m.add(self.a, self.kc, g);
                m.add(self.b, self.kc, -g);
            }
            _ => {
                m.branch(self.a, self.b, self.k);
                m.add(self.k, self.kc, -g);
            }
        }
    }
    fn cur<T: crate::solver::Scalar>(&self, x: &[T]) -> T {
        match self.kind {
            'g' => (x[self.cp] - x[self.cn]) * T::from(self.gain),
            'f' => x[self.kc] * T::from(self.gain),
            _ => x[self.k],
        }
    }
}

impl Device for Controlled {
    fn name(&self) -> &str {
        &self.name
    }
    fn setup(&mut self, s: &mut Setup) {
        if matches!(self.kind, 'e' | 'h') {
            self.k = s.branch(&self.name);
        }
    }
    fn bind(&mut self, s: &Setup) -> Result<(), String> {
        if matches!(self.kind, 'f' | 'h') {
            self.kc = s.find_branch(&self.ctrl)?;
        }
        Ok(())
    }
    fn load(&mut self, m: &mut Matrix<f64>, _c: &LoadCtx, _st: &mut [f64]) -> bool {
        self.stamp(m);
        false
    }
    fn load_ac(&self, m: &mut Matrix<C64>, _x: &[f64], _w: f64, _st: &[f64]) {
        self.stamp(m);
    }
    fn current_names(&self) -> Vec<String> {
        vec![format!("i({})", self.name)]
    }
    fn currents(&self, x: &[f64], _st: &[f64]) -> Vec<f64> {
        vec![self.cur(x)]
    }
    fn ac_currents(&self, _x: &[f64], v: &[C64], _w: f64, _st: &[f64]) -> Vec<C64> {
        vec![self.cur(v)]
    }
}

/// Behavioral source: `B1 n+ n- V={expr}` or `I={expr}`.
pub struct BSource {
    pub name: String,
    pub a: usize,
    pub b: usize,
    pub is_v: bool,
    pub expr: Expr,
    pub bound: Expr,
    pub deps: Vec<usize>,
    pub k: usize,
}

struct XEnv<'a> {
    x: &'a [f64],
    t: f64,
    pj: usize,
    pv: f64,
}
impl Env for XEnv<'_> {
    fn x(&self, i: usize) -> f64 {
        if i == self.pj { self.pv } else { self.x[i] }
    }
    fn time(&self) -> f64 {
        self.t
    }
}

impl BSource {
    /// Value and numeric partial derivatives w.r.t. each dependency.
    fn eval(&self, x: &[f64], t: f64, g: &mut Vec<f64>) -> f64 {
        let f = |pj: usize, pv: f64| self.bound.eval(&XEnv { x, t, pj, pv }).unwrap_or(f64::NAN);
        let f0 = f(usize::MAX, 0.0);
        g.clear();
        for &j in &self.deps {
            let h = 1e-6 * x[j].abs().max(1e-3);
            g.push((f(j, x[j] + h) - f0) / h);
        }
        f0
    }
}

impl Device for BSource {
    fn name(&self) -> &str {
        &self.name
    }
    fn nonlinear(&self) -> bool {
        !self.deps.is_empty()
    }
    fn setup(&mut self, s: &mut Setup) {
        if self.is_v {
            self.k = s.branch(&self.name);
        }
    }
    fn bind(&mut self, s: &Setup) -> Result<(), String> {
        self.bound = self.expr.bind(&|sig: &str| {
            if let Some(n) = sig.strip_prefix("v(").and_then(|r| r.strip_suffix(')')) {
                if n == "0" || n == "gnd" {
                    return Ok(0);
                }
                return s.nodes.get(n).copied().ok_or_else(|| format!("unknown node '{n}' in {}", self.name));
            }
            if let Some(d) = sig.strip_prefix("i(").and_then(|r| r.strip_suffix(')')) {
                return s.find_branch(d);
            }
            Err(format!("signal '{sig}' not supported in behavioral source {}", self.name))
        })?;
        let mut deps = vec![];
        collect_x(&self.bound, &mut deps);
        deps.retain(|&j| j != 0);
        deps.sort_unstable();
        deps.dedup();
        self.deps = deps;
        let probe = vec![0.0; s.n + 1];
        self.bound.eval(&XEnv { x: &probe, t: 0.0, pj: usize::MAX, pv: 0.0 }).map_err(|e| format!("{}: {e}", self.name))?;
        Ok(())
    }
    fn load(&mut self, m: &mut Matrix<f64>, c: &LoadCtx, _st: &mut [f64]) -> bool {
        let mut g = Vec::with_capacity(self.deps.len());
        let f0 = self.eval(c.x, c.time, &mut g);
        let lin: f64 = self.deps.iter().zip(&g).map(|(&j, &gj)| gj * c.x[j]).sum();
        if self.is_v {
            m.branch(self.a, self.b, self.k);
            for (&j, &gj) in self.deps.iter().zip(&g) {
                m.add(self.k, j, -gj);
            }
            m.rhs(self.k, f0 - lin);
        } else {
            for (&j, &gj) in self.deps.iter().zip(&g) {
                m.add(self.a, j, gj);
                m.add(self.b, j, -gj);
            }
            m.i(self.a, self.b, f0 - lin);
        }
        false
    }
    fn load_ac(&self, m: &mut Matrix<C64>, x: &[f64], _w: f64, _st: &[f64]) {
        let mut g = vec![];
        self.eval(x, 0.0, &mut g);
        if self.is_v {
            m.branch(self.a, self.b, self.k);
            for (&j, &gj) in self.deps.iter().zip(&g) {
                m.add(self.k, j, C64::from(-gj));
            }
        } else {
            for (&j, &gj) in self.deps.iter().zip(&g) {
                m.add(self.a, j, C64::from(gj));
                m.add(self.b, j, C64::from(-gj));
            }
        }
    }
    fn current_names(&self) -> Vec<String> {
        vec![format!("i({})", self.name)]
    }
    fn currents(&self, x: &[f64], _st: &[f64]) -> Vec<f64> {
        if self.is_v {
            vec![x[self.k]]
        } else {
            vec![self.eval(x, f64::NAN, &mut vec![])]
        }
    }
    fn ac_currents(&self, x: &[f64], v: &[C64], _w: f64, _st: &[f64]) -> Vec<C64> {
        if self.is_v {
            return vec![v[self.k]];
        }
        let mut g = vec![];
        self.eval(x, 0.0, &mut g);
        vec![self.deps.iter().zip(&g).fold(C64::default(), |s, (&j, &gj)| s + v[j] * C64::from(gj))]
    }
}

fn collect_x(e: &Expr, out: &mut Vec<usize>) {
    match e {
        Expr::X(i) => out.push(*i),
        Expr::Neg(a) | Expr::Not(a) => collect_x(a, out),
        Expr::Bin(_, a, b) => {
            collect_x(a, out);
            collect_x(b, out);
        }
        Expr::Cond(c, a, b) => {
            collect_x(c, out);
            collect_x(a, out);
            collect_x(b, out);
        }
        Expr::Call(_, a) => a.iter().for_each(|e| collect_x(e, out)),
        _ => {}
    }
}

/// Voltage-controlled switch with hysteresis (LTSpice `SW` model).
pub struct Switch {
    pub name: String,
    pub a: usize,
    pub b: usize,
    pub cp: usize,
    pub cn: usize,
    pub ron: f64,
    pub roff: f64,
    pub vt: f64,
    pub vh: f64,
    pub s: usize,
}

impl Switch {
    fn g(&self, on: bool) -> f64 {
        if on { 1.0 / self.ron } else { 1.0 / self.roff }
    }
}

impl Device for Switch {
    fn name(&self) -> &str {
        &self.name
    }
    fn nonlinear(&self) -> bool {
        true
    }
    fn setup(&mut self, s: &mut Setup) {
        self.s = s.state(1);
    }
    fn load(&mut self, m: &mut Matrix<f64>, c: &LoadCtx, st: &mut [f64]) -> bool {
        let v = c.x[self.cp] - c.x[self.cn];
        let acc = c.st0[self.s] != 0.0;
        let cur = st[self.s] != 0.0;
        // once a step flips the switch, keep it flipped for the rest of that step
        let on = if cur != acc {
            cur
        } else if v > self.vt + self.vh {
            true
        } else if v < self.vt - self.vh {
            false
        } else {
            acc
        };
        st[self.s] = if on { 1.0 } else { 0.0 };
        m.g(self.a, self.b, self.g(on));
        false
    }
    fn load_ac(&self, m: &mut Matrix<C64>, _x: &[f64], _w: f64, st: &[f64]) {
        m.g(self.a, self.b, C64::from(self.g(st[self.s] != 0.0)));
    }
    fn current_names(&self) -> Vec<String> {
        vec![format!("i({})", self.name)]
    }
    fn currents(&self, x: &[f64], st: &[f64]) -> Vec<f64> {
        vec![(x[self.a] - x[self.b]) * self.g(st[self.s] != 0.0)]
    }
    fn ac_currents(&self, _x: &[f64], v: &[C64], _w: f64, st: &[f64]) -> Vec<C64> {
        vec![(v[self.a] - v[self.b]) * C64::from(self.g(st[self.s] != 0.0))]
    }
}

// ---------------------------------------------------------------- diode

#[derive(Clone, Debug)]
pub struct DiodeModel {
    pub is: f64,
    pub n: f64,
    pub cjo: f64,
    pub vj: f64,
    pub m: f64,
    pub tt: f64,
    pub bv: f64,
    pub ibv: f64,
    pub nbv: f64,
    pub fc: f64,
    /// Thermal voltage at the device temperature.
    pub vt: f64,
    /// Flicker noise coefficient and current exponent.
    pub kf: f64,
    pub af: f64,
}

impl DiodeModel {
    /// Move IS, VJ and CJO from `tnom` to `t` (kelvin), SPICE3 rules.
    pub fn set_temp(&mut self, t: f64, tnom: f64, eg: f64, xti: f64) {
        self.vt = KOQ * t;
        self.is *= ((t / tnom - 1.0) * eg / (self.n * self.vt)).exp() * (t / tnom).powf(xti / self.n);
        (self.vj, self.cjo) = junction_at(self.vj, self.cjo, self.m, t, tnom);
    }
}

pub struct Diode {
    pub name: String,
    pub a: usize,
    pub k: usize,
    pub md: DiodeModel,
    pub s: usize,
    pub vd_old: f64,
}

impl Diode {
    fn vcrit(&self) -> f64 {
        let nvt = self.md.n * self.md.vt;
        nvt * (nvt / (std::f64::consts::SQRT_2 * self.md.is)).ln()
    }
    /// (current, conductance, charge, capacitance)
    fn eval(&self, vd: f64, gmin: f64) -> (f64, f64, f64, f64) {
        let md = &self.md;
        let nvt = md.n * md.vt;
        let (mut id, mut gd);
        if vd >= -3.0 * nvt {
            let e = (vd / nvt).min(80.0).exp();
            id = md.is * (e - 1.0);
            gd = md.is * e / nvt;
        } else {
            let a = 3.0 * nvt / (vd * std::f64::consts::E);
            let a3 = a * a * a;
            id = -md.is * (1.0 + a3);
            gd = md.is * 3.0 * a3 / vd;
        }
        if md.bv.is_finite() {
            let bvt = md.nbv * md.vt;
            let e = (-(md.bv + vd) / bvt).min(80.0).exp();
            id -= md.ibv * e;
            gd += md.ibv * e / bvt;
        }
        id += gmin * vd;
        gd += gmin;
        let (qj, cj) = qdep(vd, md.cjo, md.vj, md.m, md.fc);
        (id, gd, qj + md.tt * id, cj + md.tt * gd)
    }
}

impl Device for Diode {
    fn name(&self) -> &str {
        &self.name
    }
    fn nonlinear(&self) -> bool {
        true
    }
    fn setup(&mut self, s: &mut Setup) {
        self.s = s.state(2);
    }
    fn load(&mut self, m: &mut Matrix<f64>, c: &LoadCtx, st: &mut [f64]) -> bool {
        let raw = c.x[self.a] - c.x[self.k];
        let nvt = self.md.n * self.md.vt;
        let vd = if c.init {
            self.vcrit()
        } else {
            let mut v = pnjlim(raw, self.vd_old, nvt, self.vcrit());
            if self.md.bv.is_finite() && v < (-self.md.bv + 10.0 * nvt).min(0.0) {
                let bvt = self.md.nbv * self.md.vt;
                let vt = -(v + self.md.bv);
                let vto = -(self.vd_old + self.md.bv);
                v = -(pnjlim(vt, vto, bvt, self.vcrit()) + self.md.bv);
            }
            v
        };
        self.vd_old = vd;
        let (id, gd, q, cap) = self.eval(vd, c.gmin);
        let (gc, ic) = integ(c, st, self.s, q, cap);
        nl(m, self.a, self.k, id + ic, &[(self.a, self.k, gd + gc, vd)]);
        !c.init && (vd - raw).abs() > 1e-9
    }
    fn load_ac(&self, m: &mut Matrix<C64>, x: &[f64], w: f64, _st: &[f64]) {
        let (_, gd, _, cap) = self.eval(x[self.a] - x[self.k], 0.0);
        m.g(self.a, self.k, C64::new(gd, w * cap));
    }
    fn noise(&self, x: &[f64], _st: &[f64], _temp: f64, out: &mut Vec<Noise>) {
        let id = self.eval(x[self.a] - x[self.k], 0.0).0;
        out.push(Noise::shot(self.a, self.k, id, self.md.kf, self.md.af));
    }
    fn current_names(&self) -> Vec<String> {
        vec![format!("i({})", self.name)]
    }
    fn currents(&self, x: &[f64], st: &[f64]) -> Vec<f64> {
        vec![self.eval(x[self.a] - x[self.k], 0.0).0 + st[self.s + 1]]
    }
    fn ac_currents(&self, x: &[f64], v: &[C64], w: f64, _st: &[f64]) -> Vec<C64> {
        let (_, gd, _, cap) = self.eval(x[self.a] - x[self.k], 0.0);
        vec![(v[self.a] - v[self.k]) * C64::new(gd, w * cap)]
    }
}

// ---------------------------------------------------------------- BJT

#[derive(Clone, Debug)]
pub struct BjtModel {
    pub pol: f64,
    pub is: f64,
    pub bf: f64,
    pub nf: f64,
    pub vaf: f64,
    pub ikf: f64,
    pub ise: f64,
    pub ne: f64,
    pub br: f64,
    pub nr: f64,
    pub var: f64,
    pub ikr: f64,
    pub isc: f64,
    pub nc: f64,
    pub cje: f64,
    pub vje: f64,
    pub mje: f64,
    pub cjc: f64,
    pub vjc: f64,
    pub mjc: f64,
    pub tf: f64,
    pub tr: f64,
    pub fc: f64,
    /// Thermal voltage at the device temperature.
    pub vt: f64,
    /// Base-current flicker noise coefficient and exponent.
    pub kf: f64,
    pub af: f64,
}

impl BjtModel {
    /// Move IS, ISE, ISC, BF, BR and the junctions from `tnom` to `t` (kelvin), SPICE3 rules.
    pub fn set_temp(&mut self, t: f64, tnom: f64, eg: f64, xti: f64, xtb: f64) {
        self.vt = KOQ * t;
        let ratlog = (t / tnom).ln();
        let factlog = (t / tnom - 1.0) * eg / self.vt + xti * ratlog;
        let bfactor = (xtb * ratlog).exp();
        self.is *= factlog.exp();
        self.bf *= bfactor;
        self.br *= bfactor;
        self.ise *= (factlog / self.ne).exp() / bfactor;
        self.isc *= (factlog / self.nc).exp() / bfactor;
        (self.vje, self.cje) = junction_at(self.vje, self.cje, self.mje, t, tnom);
        (self.vjc, self.cjc) = junction_at(self.vjc, self.cjc, self.mjc, t, tnom);
    }
}

/// Gummel-Poon DC + junction/diffusion charges.
pub struct Bjt {
    pub name: String,
    pub c: usize,
    pub b: usize,
    pub e: usize,
    pub md: BjtModel,
    pub s: usize,
    pub vbe_old: f64,
    pub vbc_old: f64,
}

struct BjtOp {
    ic: f64,
    ib: f64,
    gce: f64, // d ic / d vbe
    gcc: f64, // d ic / d vbc
    gpi: f64, // d ib / d vbe
    gmu: f64, // d ib / d vbc
    qbe: f64,
    cbe: f64,
    qbc: f64,
    cbc: f64,
}

impl Bjt {
    fn vcrit(&self) -> f64 {
        let nvt = self.md.nf * self.md.vt;
        nvt * (nvt / (std::f64::consts::SQRT_2 * self.md.is)).ln()
    }
    fn eval(&self, vbe: f64, vbc: f64, gmin: f64) -> BjtOp {
        let md = &self.md;
        let ex = |v: f64, n: f64| {
            let e = (v / (n * md.vt)).min(80.0).exp();
            (e - 1.0, e / (n * md.vt))
        };
        let (fe, fg) = ex(vbe, md.nf);
        let cbe = md.is * fe + gmin * vbe;
        let gbe = md.is * fg + gmin;
        let (cben, gben) = if md.ise > 0.0 {
            let (a, b) = ex(vbe, md.ne);
            (md.ise * a, md.ise * b)
        } else {
            (0.0, 0.0)
        };
        let (re, rg) = ex(vbc, md.nr);
        let cbc = md.is * re + gmin * vbc;
        let gbc = md.is * rg + gmin;
        let (cbcn, gbcn) = if md.isc > 0.0 {
            let (a, b) = ex(vbc, md.nc);
            (md.isc * a, md.isc * b)
        } else {
            (0.0, 0.0)
        };
        let inv = |v: f64| if v.is_finite() && v != 0.0 { 1.0 / v } else { 0.0 };
        let (ova, ovb, oik, oikr) = (inv(md.vaf), inv(md.var), inv(md.ikf), inv(md.ikr));
        let q1 = 1.0 / (1.0 - ova * vbc - ovb * vbe).max(1e-3);
        let (qb, dqbdve, dqbdvc) = if oik == 0.0 && oikr == 0.0 {
            (q1, q1 * q1 * ovb, q1 * q1 * ova)
        } else {
            let q2 = oik * cbe + oikr * cbc;
            let arg = (1.0 + 4.0 * q2).max(1e-12);
            let sq = arg.sqrt();
            let qb = q1 * (1.0 + sq) / 2.0;
            (qb, q1 * (qb * ovb + oik * gbe / sq), q1 * (qb * ova + oikr * gbc / sq))
        };
        let ic = (cbe - cbc) / qb - cbc / md.br - cbcn;
        let ib = cbe / md.bf + cben + cbc / md.br + cbcn;
        let gpi = gbe / md.bf + gben;
        let gmu = gbc / md.br + gbcn;
        let go = (gbc + (cbe - cbc) * dqbdvc / qb) / qb;
        let gm = (gbe - (cbe - cbc) * dqbdve / qb) / qb - go;
        let (qje, cje) = qdep(vbe, md.cje, md.vje, md.mje, md.fc);
        let (qjc, cjc) = qdep(vbc, md.cjc, md.vjc, md.mjc, md.fc);
        BjtOp {
            ic,
            ib,
            gce: gm + go,
            gcc: -go - gmu,
            gpi,
            gmu,
            qbe: qje + md.tf * cbe,
            cbe: cje + md.tf * gbe,
            qbc: qjc + md.tr * cbc,
            cbc: cjc + md.tr * gbc,
        }
    }
    fn op_at(&self, x: &[f64]) -> (BjtOp, f64, f64) {
        let p = self.md.pol;
        let vbe = p * (x[self.b] - x[self.e]);
        let vbc = p * (x[self.b] - x[self.c]);
        (self.eval(vbe, vbc, 0.0), vbe, vbc)
    }
}

impl Device for Bjt {
    fn name(&self) -> &str {
        &self.name
    }
    fn nonlinear(&self) -> bool {
        true
    }
    fn setup(&mut self, s: &mut Setup) {
        self.s = s.state(4);
    }
    fn load(&mut self, m: &mut Matrix<f64>, c: &LoadCtx, st: &mut [f64]) -> bool {
        let p = self.md.pol;
        let rbe = p * (c.x[self.b] - c.x[self.e]);
        let rbc = p * (c.x[self.b] - c.x[self.c]);
        let (vbe, vbc) = if c.init {
            (self.vcrit(), 0.0)
        } else {
            let vc = self.vcrit();
            let nvt = self.md.nf * self.md.vt;
            (pnjlim(rbe, self.vbe_old, nvt, vc), pnjlim(rbc, self.vbc_old, self.md.nr * self.md.vt, vc))
        };
        self.vbe_old = vbe;
        self.vbc_old = vbc;
        let o = self.eval(vbe, vbc, c.gmin);
        let (b, cc, e) = (self.b, self.c, self.e);
        let (ube, ubc) = (p * vbe, p * vbc);
        nl(m, cc, e, p * o.ic, &[(b, e, o.gce, ube), (b, cc, o.gcc, ubc)]);
        nl(m, b, e, p * o.ib, &[(b, e, o.gpi, ube), (b, cc, o.gmu, ubc)]);
        stamp_q(m, c, st, self.s, b, e, p * o.qbe, o.cbe, ube);
        stamp_q(m, c, st, self.s + 2, b, cc, p * o.qbc, o.cbc, ubc);
        !c.init && ((vbe - rbe).abs() > 1e-9 || (vbc - rbc).abs() > 1e-9)
    }
    fn load_ac(&self, m: &mut Matrix<C64>, x: &[f64], w: f64, _st: &[f64]) {
        let (o, _, _) = self.op_at(x);
        let (b, c, e) = (self.b, self.c, self.e);
        m.vccs(c, e, b, e, C64::from(o.gce));
        m.vccs(c, e, b, c, C64::from(o.gcc));
        m.vccs(b, e, b, e, C64::from(o.gpi));
        m.vccs(b, e, b, c, C64::from(o.gmu));
        m.g(b, e, jw(w, o.cbe));
        m.g(b, c, jw(w, o.cbc));
    }
    /// Collector and base shot noise, base flicker noise (RB/RC/RE are separate resistors).
    fn noise(&self, x: &[f64], _st: &[f64], _temp: f64, out: &mut Vec<Noise>) {
        let (o, _, _) = self.op_at(x);
        out.push(Noise::shot(self.c, self.e, o.ic, 0.0, 1.0));
        out.push(Noise::shot(self.b, self.e, o.ib, self.md.kf, self.md.af));
    }
    fn current_names(&self) -> Vec<String> {
        ["ic", "ib", "ie"].iter().map(|p| format!("{p}({})", self.name)).collect()
    }
    fn currents(&self, x: &[f64], st: &[f64]) -> Vec<f64> {
        let (o, _, _) = self.op_at(x);
        let p = self.md.pol;
        let (ibe, ibc) = (st[self.s + 1], st[self.s + 3]);
        let ic = p * o.ic - ibc;
        let ib = p * o.ib + ibe + ibc;
        vec![ic, ib, -(ic + ib)]
    }
    fn ac_currents(&self, x: &[f64], v: &[C64], w: f64, _st: &[f64]) -> Vec<C64> {
        let (o, _, _) = self.op_at(x);
        let vbe = v[self.b] - v[self.e];
        let vbc = v[self.b] - v[self.c];
        let ic = vbe * C64::from(o.gce) + vbc * C64::new(o.gcc, -w * o.cbc);
        let ib = vbe * C64::new(o.gpi, w * o.cbe) + vbc * C64::new(o.gmu, w * o.cbc);
        vec![ic, ib, -(ic + ib)]
    }
}

// ---------------------------------------------------------------- MOSFET

#[derive(Clone, Debug)]
pub struct MosModel {
    pub pol: f64,
    pub vto: f64,
    pub kp: f64,
    pub gamma: f64,
    pub phi: f64,
    pub lambda: f64,
    pub cgs: f64,
    pub cgd: f64,
    pub cbd: f64,
    pub cbs: f64,
    /// VDMOS subthreshold slope KSUBTHRES (V); 0 = hard cutoff.
    pub ksub: f64,
    /// Flicker noise: kf·|Id|^af / (f·fden), or kf·gm² / (f^af·fden) when `fgm` (ngspice NLEV 2/3).
    pub kf: f64,
    pub af: f64,
    pub fden: f64,
    pub fgm: bool,
}

impl MosModel {
    /// Move VTO and PHI from `tnom` to `t` (kelvin), SPICE3 level-1 rules
    /// (KP scales as (t/tnom)^-1.5 on the device's beta).
    pub fn set_temp(&mut self, t: f64, tnom: f64) {
        let (phit, _) = junction_at(self.phi, 0.0, 0.0, t, tnom);
        let v0 = self.pol * self.vto;
        let sq = |p: f64| p.max(0.1).sqrt(); // same floor as eval()
        let v = v0 + self.gamma * (sq(phit) - sq(self.phi)) + 0.5 * (phit - self.phi) + self.pol * 0.5 * (egap(tnom) - egap(t));
        self.vto = self.pol * v;
        self.phi = phit;
    }
}

/// Shichman-Hodges (SPICE level 1) MOSFET with constant capacitances.
/// shortcut: no Meyer/BSIM charge model; switching edges are approximate.
pub struct Mos {
    pub name: String,
    pub d: usize,
    pub g: usize,
    pub s: usize,
    pub b: usize,
    pub beta: f64,
    pub md: MosModel,
    pub st: usize,
    pub vgs_old: f64,
    pub vds_old: f64,
    /// Level-1 bulk-drain and bulk-source junctions, see `mos_junction` (empty for VDMOS/JFET).
    pub bulk: Vec<Diode>,
}

/// SPICE level-1 bulk junction from `a` to `k` (NMOS: bulk → drain/source): IS moved from
/// `tnom` to `t` (kelvin) with the MOS1 rule IS·exp(Eg(tnom)/vt(tnom) − Eg(t)/vt(t)).
pub fn mos_junction(name: String, a: usize, k: usize, is: f64, t: f64, tnom: f64) -> Diode {
    let vt = KOQ * t;
    let is = is * (egap(tnom) / (KOQ * tnom) - egap(t) / vt).exp();
    let md = DiodeModel { is, n: 1.0, cjo: 0.0, vj: 1.0, m: 0.5, tt: 0.0, bv: f64::INFINITY, ibv: 1e-3, nbv: 1.0, fc: 0.5, vt, kf: 0.0, af: 1.0 };
    Diode { name, a, k, md, s: 0, vd_old: 0.0 }
}

impl Mos {
    /// Normalized (n-channel, vds >= 0): (ids, gm, gds, gmbs)
    fn eval(&self, vgs: f64, vds: f64, vbs: f64) -> (f64, f64, f64, f64) {
        let md = &self.md;
        let vto = md.pol * md.vto;
        let sphi = md.phi.max(0.1).sqrt();
        let (sarg, dsarg) = if vbs <= 0.0 {
            let s = (md.phi.max(0.1) - vbs).sqrt();
            (s, 1.0 / (2.0 * s))
        } else {
            ((sphi - vbs / (2.0 * sphi)).max(0.0), 1.0 / (2.0 * sphi))
        };
        let vth = vto + md.gamma * (sarg - sphi);
        let mut vgst = vgs - vth;
        // ngspice VDMOS subthreshold: Vgst -> k·ln(1 + exp(Vgst/k))
        let mut dvgst = 1.0;
        if md.ksub > 0.0 {
            let x = vgst / md.ksub;
            if x < 40.0 {
                let e = x.exp();
                vgst = md.ksub * e.ln_1p();
                dvgst = e / (1.0 + e);
            }
        } else if vgst <= 0.0 {
            return (0.0, 0.0, 0.0, 0.0);
        }
        let l = 1.0 + md.lambda * vds;
        let b = self.beta;
        let (ids, gm, gds) = if vds < vgst {
            (b * (vgst - vds / 2.0) * vds * l, b * vds * l, b * ((vgst - vds) * l + md.lambda * (vgst - vds / 2.0) * vds))
        } else {
            (b / 2.0 * vgst * vgst * l, b * vgst * l, b / 2.0 * vgst * vgst * md.lambda)
        };
        let gm = gm * dvgst;
        (ids, gm, gds, gm * md.gamma * dsarg)
    }
    /// Returns ((drain', source') nodes, normalized voltages, model outputs).
    fn bias(&self, vgs: f64, vds: f64, vbs: f64) -> ((usize, usize), (f64, f64, f64), (f64, f64, f64, f64)) {
        if vds >= 0.0 {
            ((self.d, self.s), (vgs, vds, vbs), self.eval(vgs, vds, vbs))
        } else {
            let v = (vgs - vds, -vds, vbs - vds);
            ((self.s, self.d), v, self.eval(v.0, v.1, v.2))
        }
    }
    fn raw(&self, x: &[f64]) -> (f64, f64, f64) {
        let p = self.md.pol;
        (p * (x[self.g] - x[self.s]), p * (x[self.d] - x[self.s]), p * (x[self.b] - x[self.s]))
    }
}

impl Device for Mos {
    fn name(&self) -> &str {
        &self.name
    }
    fn nonlinear(&self) -> bool {
        true
    }
    fn setup(&mut self, s: &mut Setup) {
        self.st = s.state(8);
        for j in &mut self.bulk {
            j.setup(s);
        }
    }
    fn load(&mut self, m: &mut Matrix<f64>, c: &LoadCtx, st: &mut [f64]) -> bool {
        let p = self.md.pol;
        let (rgs, rds, vbs) = self.raw(c.x);
        let (vgs, vds) = if c.init {
            (p * self.md.vto, 0.0)
        } else {
            let von = p * self.md.vto;
            let vgd = rgs - rds;
            if self.vds_old >= 0.0 {
                let vgs = fetlim(rgs, self.vgs_old, von);
                (vgs, limvds(vgs - vgd, self.vds_old))
            } else {
                let vgd = fetlim(vgd, self.vgs_old - self.vds_old, von);
                let vds = -limvds(-(rgs - vgd), -self.vds_old);
                (vgd + vds, vds)
            }
        };
        self.vgs_old = vgs;
        self.vds_old = vds;
        let ((dn, sn), (egs, eds, ebs), (ids, gm, gds, gmbs)) = self.bias(vgs, vds, vbs);
        nl(m, dn, sn, p * ids, &[(self.g, sn, gm, p * egs), (dn, sn, gds, p * eds), (self.b, sn, gmbs, p * ebs)]);
        m.g(self.d, self.s, c.gmin);
        let (d, g, s, b) = (self.d, self.g, self.s, self.b);
        let md = &self.md;
        let vg_s = p * vgs;
        let vg_d = p * (vgs - vds);
        stamp_q(m, c, st, self.st, g, s, md.cgs * vg_s, md.cgs, vg_s);
        stamp_q(m, c, st, self.st + 2, g, d, md.cgd * vg_d, md.cgd, vg_d);
        let vbd = c.x[b] - c.x[d];
        let vbs_a = c.x[b] - c.x[s];
        stamp_q(m, c, st, self.st + 4, b, d, md.cbd * vbd, md.cbd, vbd);
        stamp_q(m, c, st, self.st + 6, b, s, md.cbs * vbs_a, md.cbs, vbs_a);
        // junctions start from zero bias, not the diode's forward vcrit (SPICE starts MOS at vbs = -1)
        let jc = LoadCtx { init: false, ..*c };
        let mut lim = false;
        for j in &mut self.bulk {
            lim |= j.load(m, &jc, st);
        }
        lim || !c.init && ((vgs - rgs).abs() > 1e-9 || (vds - rds).abs() > 1e-9)
    }
    fn load_ac(&self, m: &mut Matrix<C64>, x: &[f64], w: f64, st: &[f64]) {
        let (vgs, vds, vbs) = self.raw(x);
        let ((dn, sn), _, (_, gm, gds, gmbs)) = self.bias(vgs, vds, vbs);
        m.vccs(dn, sn, self.g, sn, C64::from(gm));
        m.vccs(dn, sn, dn, sn, C64::from(gds));
        m.vccs(dn, sn, self.b, sn, C64::from(gmbs));
        m.g(self.g, self.s, jw(w, self.md.cgs));
        m.g(self.g, self.d, jw(w, self.md.cgd));
        m.g(self.b, self.d, jw(w, self.md.cbd));
        m.g(self.b, self.s, jw(w, self.md.cbs));
        for j in &self.bulk {
            j.load_ac(m, x, w, st);
        }
    }
    /// Channel thermal noise 8/3·kT·gm and flicker noise (RD/RS are separate resistors;
    /// like SPICE, the bulk junctions are noiseless).
    fn noise(&self, x: &[f64], _st: &[f64], temp: f64, out: &mut Vec<Noise>) {
        let (vgs, vds, vbs) = self.raw(x);
        let ((dn, sn), _, (ids, gm, ..)) = self.bias(vgs, vds, vbs);
        let md = &self.md;
        let mut n = Noise::thermal(dn, sn, 2.0 / 3.0 * gm, temp);
        if md.fgm {
            (n.flicker, n.ef) = (md.kf * gm * gm / md.fden, md.af);
        } else {
            n.flicker = md.kf * ids.abs().powf(md.af) / md.fden;
        }
        out.push(n);
    }
    fn current_names(&self) -> Vec<String> {
        ["id", "ig", "is"].iter().map(|p| format!("{p}({})", self.name)).collect()
    }
    fn currents(&self, x: &[f64], st: &[f64]) -> Vec<f64> {
        let (vgs, vds, vbs) = self.raw(x);
        let ((dn, _), _, (ids, ..)) = self.bias(vgs, vds, vbs);
        let ich = if dn == self.d { self.md.pol * ids } else { -self.md.pol * ids };
        let (igs, igd, mut ibd, mut ibs) = (st[self.st + 1], st[self.st + 3], st[self.st + 5], st[self.st + 7]);
        // bulk junction currents, bulk -> drain / source
        if let [jd, js] = &self.bulk[..] {
            ibd += self.md.pol * jd.currents(x, st)[0];
            ibs += self.md.pol * js.currents(x, st)[0];
        }
        let id = ich - igd - ibd;
        let ig = igs + igd;
        let is = -ich - igs - ibs;
        vec![id, ig, is]
    }
    fn ac_currents(&self, x: &[f64], v: &[C64], w: f64, st: &[f64]) -> Vec<C64> {
        let (vgs, vds, vbs) = self.raw(x);
        let ((dn, sn), _, (_, gm, gds, gmbs)) = self.bias(vgs, vds, vbs);
        let mut ich = (v[self.g] - v[sn]) * C64::from(gm) + (v[dn] - v[sn]) * C64::from(gds) + (v[self.b] - v[sn]) * C64::from(gmbs);
        if dn != self.d {
            ich = -ich;
        }
        let igs = (v[self.g] - v[self.s]) * jw(w, self.md.cgs);
        let igd = (v[self.g] - v[self.d]) * jw(w, self.md.cgd);
        let mut ibd = (v[self.b] - v[self.d]) * jw(w, self.md.cbd);
        let mut ibs = (v[self.b] - v[self.s]) * jw(w, self.md.cbs);
        if let [jd, js] = &self.bulk[..] {
            ibd += jd.ac_currents(x, v, w, st)[0] * C64::from(self.md.pol);
            ibs += js.ac_currents(x, v, w, st)[0] * C64::from(self.md.pol);
        }
        vec![ich - igd - ibd, igs + igd, -ich - igs - ibs]
    }
}

// ---------------------------------------------------------------- JFET

/// SPICE NJF/PJF JFET: the Shichman-Hodges channel is the level-1 MOSFET
/// law with β_mos = 2·BETA and no body effect; the gate is two pn junctions
/// (gate-source, gate-drain) carrying IS and the depletion caps CGS/CGD.
pub struct Jfet {
    pub name: String,
    pub ch: Mos,
    pub gs: Diode,
    pub gd: Diode,
}

impl Jfet {
    /// Terminal currents [id, ig, is] from channel [id, ig, is] and the junction currents (anode → cathode).
    fn terminals<T: crate::solver::Scalar>(&self, ch: Vec<T>, gs: T, gd: T) -> Vec<T> {
        // n-channel junctions point gate → channel, p-channel channel → gate
        let (gs, gd) = if self.ch.md.pol > 0.0 { (gs, gd) } else { (-gs, -gd) };
        vec![ch[0] - gd, gs + gd, ch[2] - gs]
    }
}

impl Device for Jfet {
    fn name(&self) -> &str {
        &self.name
    }
    fn nonlinear(&self) -> bool {
        true
    }
    fn setup(&mut self, s: &mut Setup) {
        self.ch.setup(s);
        self.gs.setup(s);
        self.gd.setup(s);
    }
    fn load(&mut self, m: &mut Matrix<f64>, c: &LoadCtx, st: &mut [f64]) -> bool {
        let a = self.ch.load(m, c, st);
        let b = self.gs.load(m, c, st);
        self.gd.load(m, c, st) | a | b
    }
    fn load_ac(&self, m: &mut Matrix<C64>, x: &[f64], w: f64, st: &[f64]) {
        self.ch.load_ac(m, x, w, st);
        self.gs.load_ac(m, x, w, st);
        self.gd.load_ac(m, x, w, st);
    }
    /// Channel noise only: SPICE leaves the gate junctions noiseless.
    fn noise(&self, x: &[f64], st: &[f64], temp: f64, out: &mut Vec<Noise>) {
        self.ch.noise(x, st, temp, out);
    }
    fn current_names(&self) -> Vec<String> {
        ["id", "ig", "is"].iter().map(|p| format!("{p}({})", self.name)).collect()
    }
    fn currents(&self, x: &[f64], st: &[f64]) -> Vec<f64> {
        self.terminals(self.ch.currents(x, st), self.gs.currents(x, st)[0], self.gd.currents(x, st)[0])
    }
    fn ac_currents(&self, x: &[f64], v: &[C64], w: f64, st: &[f64]) -> Vec<C64> {
        self.terminals(self.ch.ac_currents(x, v, w, st), self.gs.ac_currents(x, v, w, st)[0], self.gd.ac_currents(x, v, w, st)[0])
    }
}
