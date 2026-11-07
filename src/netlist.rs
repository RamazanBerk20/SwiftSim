//! SPICE netlist parsing (LTSpice dialect): includes/libs, subcircuits with
//! parameters, models, `.param`, directives, and flattening into a Circuit.

use crate::devices::*;
use crate::expr::{Expr, Funcs, Op, VarEnv, parse_func, parse_number};
use crate::sim::{Circuit, Options};
use crate::solver::C64;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Mutex, OnceLock, RwLock};

/// Parameter values plus the `.func`s visible at that level.
#[derive(Clone, Debug, Default)]
pub struct Scope {
    pub vars: HashMap<String, f64>,
    pub funcs: Rc<Funcs>,
}

impl std::ops::Deref for Scope {
    type Target = HashMap<String, f64>;
    fn deref(&self) -> &Self::Target {
        &self.vars
    }
}

impl std::ops::DerefMut for Scope {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.vars
    }
}

#[derive(Clone, Debug)]
pub struct Line {
    pub no: usize,
    pub text: String,
}

#[derive(Clone, Debug, Default)]
pub struct Model {
    pub kind: String,
    pub params: HashMap<String, String>,
}

#[derive(Clone, Debug)]
pub struct Subckt {
    pub ports: Vec<String>,
    pub params: Vec<(String, String)>,
    pub funcs: Funcs,
    pub body: Vec<Line>,
}

#[derive(Clone, Debug)]
pub enum Analysis {
    Op,
    /// (source name, sweep values); one or two nested sweeps (first is inner).
    Dc(Vec<(String, Vec<f64>)>),
    Ac(Vec<f64>),
    Tran { tstep: f64, tstop: f64, tstart: f64, tmax: f64, uic: bool },
    /// `.tf V(a[,b]) src` or `.tf I(vx) src`: output (current?, node/source, ref node).
    Tf { cur: bool, a: String, b: String, src: String },
    /// `.noise V(out[,ref]) src dec|oct|lin n fstart fstop`.
    Noise { out: String, refn: String, src: String, freqs: Vec<f64> },
}

#[derive(Clone, Debug)]
pub struct Step {
    pub name: String,
    pub values: Vec<f64>,
}

#[derive(Clone, Debug, Default)]
pub struct Deck {
    pub title: String,
    pub cards: Vec<Line>,
    pub subckts: HashMap<String, Subckt>,
    pub models: HashMap<String, Model>,
    pub params: Vec<(String, String)>,
    pub funcs: Funcs,
    pub analysis: Option<Line>,
    pub steps: Vec<Line>,
    pub meas: Vec<Line>,
    pub ics: Vec<Line>,
    pub options: Vec<Line>,
    pub temps: Vec<Line>,
    pub four: Vec<Line>,
    pub warnings: Vec<String>,
    /// `.lib`/`.include` files skipped because they are vendor-encrypted.
    pub encrypted: Vec<String>,
}

/// SiO2 permittivity, F/m (3.9·ε0, as ngspice's noise models).
const EPS_OX: f64 = 3.9 * 8.854214871e-12;

/// Generic parts available without `.model` lines. Values are typical
/// datasheet-level approximations.
const BUILTIN_LIB: &str = "
.model 1N4148 D(Is=2.52n Rs=.568 N=1.752 Cjo=4p M=.4 tt=20n Bv=100 Ibv=100u)
.model 1N914 D(Is=2.52n Rs=.568 N=1.752 Cjo=4p M=.4 tt=20n Bv=100 Ibv=100u)
.model 1N4007 D(Is=7.03n Rs=.034 N=1.81 Cjo=10p M=.333 tt=4.3u Bv=1000 Ibv=5u)
.model 1N5819 D(Is=31.7u Rs=.051 N=1.373 Cjo=110p M=.35 Bv=40 Ibv=1m)
.model 1N5408 D(Is=63n Rs=.014 N=1.7 Cjo=53p M=.333 tt=5u Bv=1000 Ibv=10u)
.model 1N4733 D(Is=1n Rs=1 N=1.1 Cjo=100p Bv=5.1 Ibv=49m Nbv=1)
.model 1N4742 D(Is=1n Rs=3 N=1.1 Cjo=60p Bv=12 Ibv=21m Nbv=1)
.model 1N750 D(Is=1n Rs=2 N=1.1 Cjo=80p Bv=4.7 Ibv=20m Nbv=1)
.model LED_RED D(Is=1e-18 N=1.9 Rs=3 Cjo=20p Bv=5 Ibv=10u)
.model LED_GREEN D(Is=1e-22 N=1.9 Rs=3 Cjo=20p Bv=5 Ibv=10u)
.model LED_BLUE D(Is=1e-27 N=2 Rs=5 Cjo=20p Bv=5 Ibv=10u)
.model LED_WHITE D(Is=1e-27 N=2 Rs=5 Cjo=20p Bv=5 Ibv=10u)
.model 2N3904 NPN(Is=6.734f Vaf=74.03 Bf=416.4 Ne=1.259 Ise=6.734f Ikf=66.78m Br=.7371 Nc=2 Rc=1 Cjc=3.638p Mjc=.3085 Vjc=.75 Fc=.5 Cje=4.493p Mje=.2593 Vje=.75 Tr=239.5n Tf=301.2p Rb=10)
.model 2N3906 PNP(Is=1.41f Vaf=18.7 Bf=180.7 Ne=1.5 Ikf=80m Br=4.977 Nc=2 Rc=2.5 Cjc=9.728p Mjc=.5776 Vjc=.75 Fc=.5 Cje=8.063p Mje=.3677 Vje=.75 Tr=33.42n Tf=179.3p Rb=10)
.model 2N2222 NPN(Is=14.34f Vaf=74.03 Bf=255.9 Ne=1.307 Ise=14.34f Ikf=.2847 Br=6.092 Nc=2 Rc=1 Cjc=7.306p Mjc=.3416 Vjc=.75 Fc=.5 Cje=22.01p Mje=.377 Vje=.75 Tr=46.91n Tf=411.1p Rb=10)
.model 2N2907 PNP(Is=650.6E-18 Vaf=115.7 Bf=231.7 Ne=1.829 Ise=54.81f Ikf=1.079 Br=3.563 Nc=2 Rc=.715 Cjc=14.76p Mjc=.5383 Vjc=.75 Fc=.5 Cje=19.82p Mje=.3357 Vje=.75 Tr=111.3n Tf=603.7p Rb=10)
.model BC547B NPN(Is=2.39E-14 Nf=1.008 Ise=3.545E-15 Ne=1.541 Bf=294.3 Ikf=.1357 Vaf=63.2 Nr=1.004 Isc=6.272E-14 Nc=1.243 Br=7.946 Ikr=.1144 Var=25.9 Rb=1 Re=.4683 Rc=.85 Cje=13.58p Vje=.65 Mje=.3279 Tf=439.1p Cjc=3.728p Vjc=.3997 Mjc=.2955 Fc=.9579)
.model BC557B PNP(Is=3.83E-14 Nf=1.008 Ise=1.22E-14 Ne=1.528 Bf=344.4 Ikf=.08039 Vaf=21.11 Nr=1.005 Isc=2.85E-13 Nc=1.28 Br=14.84 Ikr=.047 Var=32.02 Rb=1 Re=.6202 Rc=.5713 Cje=12.37p Vje=.7245 Mje=.3877 Tf=530.3p Cjc=11.22p Vjc=.4924 Mjc=.4192 Fc=.8)
.model TIP31C NPN(Is=1.8p Bf=60 Vaf=100 Ikf=3 Ise=40p Ne=1.6 Br=3 Rb=5 Re=.05 Rc=.1 Cje=300p Cjc=100p Tf=12n Tr=1u)
.model TIP32C PNP(Is=1.8p Bf=60 Vaf=100 Ikf=3 Ise=40p Ne=1.6 Br=3 Rb=5 Re=.05 Rc=.1 Cje=300p Cjc=150p Tf=12n Tr=1u)
.model 2N7000 VDMOS(Vto=2.1 Kp=.08 Lambda=.02 Rd=.4 Rs=.2 Rg=10 Cgs=20p Cgdmax=10p Cgdmin=3p Cjo=20p Is=1e-13 Bv=60)
.model BS170 VDMOS(Vto=2 Kp=.1 Lambda=.02 Rd=.5 Rs=.2 Rg=10 Cgs=24p Cgdmax=10p Cgdmin=3p Cjo=25p Is=1e-13 Bv=60)
.model BSS84 VDMOS(pchan Vto=-1.7 Kp=.12 Lambda=.02 Rd=1.5 Rs=.5 Rg=10 Cgs=20p Cgdmax=10p Cgdmin=3p Cjo=20p Is=1e-13 Bv=50)
.model IRF540N VDMOS(Vto=3.5 Kp=20 Lambda=.01 Rd=20m Rs=5m Rg=2 Cgs=1.6n Cgdmax=600p Cgdmin=60p Cjo=500p Is=1e-11 Rb=5m N=1.2 Bv=100)
.model IRF9540N VDMOS(pchan Vto=-3.5 Kp=8 Lambda=.01 Rd=60m Rs=10m Rg=2 Cgs=1.2n Cgdmax=500p Cgdmin=50p Cjo=500p Is=1e-11 Rb=10m N=1.2 Bv=100)
.model IRLZ44N VDMOS(Vto=1.5 Kp=40 Lambda=.01 Rd=10m Rs=3m Rg=2 Cgs=1.7n Cgdmax=500p Cgdmin=60p Cjo=600p Is=1e-11 Rb=5m N=1.2 Bv=55)
.model NMOS_GENERIC NMOS(Vto=0.7 Kp=110u Lambda=.04 Gamma=.4 Phi=.7 Cgso=.3n Cgdo=.3n)
.model PMOS_GENERIC PMOS(Vto=-0.7 Kp=40u Lambda=.05 Gamma=.5 Phi=.7 Cgso=.3n Cgdo=.3n)
";

pub fn builtin_models() -> &'static HashMap<String, Model> {
    static LIB: OnceLock<HashMap<String, Model>> = OnceLock::new();
    LIB.get_or_init(|| {
        BUILTIN_LIB
            .lines()
            .filter(|l| l.starts_with(".model"))
            .filter_map(|l| parse_model(&l.to_ascii_lowercase()).ok())
            .collect()
    })
}

/// Built-in model names of the given kinds (e.g. ["npn"]), in library order.
pub fn builtin_names(kinds: &[&str]) -> Vec<String> {
    BUILTIN_LIB
        .lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            (w.next() == Some(".model")).then_some(())?;
            let name = w.next()?;
            let kind = w.next()?.split('(').next()?.to_ascii_lowercase();
            kinds.contains(&kind.as_str()).then(|| name.to_string())
        })
        .collect()
}

static LTSPICE_LIB: RwLock<Option<PathBuf>> = RwLock::new(None);

/// Sets LTspice's `lib` folder (the GUI setting). `None` falls back to the
/// `SWIFTSIM_LTSPICE_LIB` environment variable, then to auto-detection.
pub fn set_ltspice_lib(p: Option<PathBuf>) {
    *LTSPICE_LIB.write().unwrap_or_else(|e| e.into_inner()) = p;
}

/// LTspice's `lib` folder (holds `cmp/standard.*` and `sub/`), if any is set or found.
pub fn ltspice_lib() -> Option<PathBuf> {
    let set = LTSPICE_LIB.read().unwrap_or_else(|e| e.into_inner()).clone();
    set.or_else(|| std::env::var_os("SWIFTSIM_LTSPICE_LIB").map(PathBuf::from)).or_else(|| detect_ltspice_lib().clone())
}

/// The first LTspice install found in the usual places (native Windows, Wine, macOS).
pub fn detect_ltspice_lib() -> &'static Option<PathBuf> {
    static FOUND: OnceLock<Option<PathBuf>> = OnceLock::new();
    FOUND.get_or_init(|| {
        let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
        let mut dirs = vec![];
        if let Some(d) = env("LOCALAPPDATA") {
            dirs.push(d.join("LTspice/lib")); // LTspice 24+
        }
        if let Some(d) = env("USERPROFILE") {
            dirs.push(d.join("Documents/LTspiceXVII/lib"));
        }
        if let Some(h) = env("HOME") {
            dirs.push(h.join("Library/Application Support/LTspice/lib"));
            let user = env("USER").unwrap_or_default();
            let users = env("WINEPREFIX").unwrap_or_else(|| h.join(".wine")).join("drive_c/users").join(user);
            for d in ["AppData/Local/LTspice/lib", "Documents/LTspiceXVII/lib", "My Documents/LTspiceXVII/lib"] {
                dirs.push(users.join(d));
            }
        }
        dirs.into_iter().find(|d| d.join("cmp").is_dir())
    })
}

/// `.model`s of LTspice's `cmp/standard.*` files, read once per folder. Only
/// read at run time: these files belong to LTspice and are not shipped.
fn ltspice_models(lib: &Path) -> &'static HashMap<String, Model> {
    static CACHE: Mutex<Vec<(PathBuf, &'static HashMap<String, Model>)>> = Mutex::new(vec![]);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, m)) = cache.iter().find(|(p, _)| p == lib) {
        return m;
    }
    let mut text = String::new();
    for f in ["standard.dio", "standard.bjt", "standard.mos", "standard.jft"] {
        if let Ok(b) = std::fs::read(lib.join("cmp").join(f)) {
            text += &decode(&b);
            text.push('\n');
        }
    }
    let (raw, mut lines) = (text.lines().collect::<Vec<_>>(), vec![]);
    let _ = read_lines(&raw, 0, None, &mut lines, &mut vec![], &mut vec![], 0);
    let mut models: HashMap<String, Model> = lines.iter().filter_map(|l| parse_model(&l.text).ok()).collect();
    resolve_ako(&mut models);
    // leaked on purpose: one small map per library folder used in this process
    let m: &'static HashMap<String, Model> = Box::leak(Box::new(models));
    cache.push((lib.to_path_buf(), m));
    m
}

/// `.model x ako:base kind(…)` ("a kind of", LTspice): base's parameters plus x's own.
fn resolve_ako(models: &mut HashMap<String, Model>) {
    let akos: Vec<String> = models.iter().filter(|(_, m)| m.params.contains_key("ako")).map(|(k, _)| k.clone()).collect();
    for k in akos {
        let mut m = models.remove(&k).unwrap_or_default();
        let base = m.params.remove("ako").unwrap_or_default();
        if let Some(b) = models.get(&base) {
            let mut p = b.params.clone();
            p.remove("ako");
            p.extend(m.params);
            m.params = p;
        }
        models.insert(k, m);
    }
}

/// Text of a model/schematic file: UTF-16LE (newer LTspice), UTF-8, or Latin-1 (µ as 0xB5).
pub fn decode(b: &[u8]) -> String {
    if b.starts_with(&[0xFF, 0xFE]) || b.get(1) == Some(&0) {
        let u: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        return String::from_utf16_lossy(&u).trim_start_matches('\u{feff}').to_string();
    }
    String::from_utf8(b.to_vec()).unwrap_or_else(|_| b.iter().map(|&c| c as char).collect())
}

/// Vendor-encrypted model files (LTspice's own format, Cadence/PSpice) cannot be simulated.
fn is_encrypted(t: &str) -> bool {
    t.contains("LTspice Encrypted File") || t.contains("$CDNENCSTART")
}

/// `.param a=1 b={a*2}`, or LTspice's `.param a 1` (no `=`).
fn param_list(s: &str) -> Vec<(String, String)> {
    if s.contains('=') {
        return parse_assignments(s);
    }
    let w: Vec<&str> = s.split_whitespace().collect();
    w.chunks_exact(2).map(|p| (p[0].to_string(), p[1].to_string())).collect()
}

/// Split a card into tokens: whitespace/comma separated, with `(`, `)` and
/// `=` as their own tokens, `{...}` kept whole, quotes stripped.
pub fn tokenize(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '{' => {
                flush(&mut cur, &mut out);
                let mut depth = 1;
                let mut t = String::from("{");
                for c in it.by_ref() {
                    t.push(c);
                    match c {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                out.push(t);
            }
            '"' => {
                flush(&mut cur, &mut out);
                out.push(it.by_ref().take_while(|&c| c != '"').collect());
            }
            ' ' | '\t' | ',' => flush(&mut cur, &mut out),
            '(' | ')' | '=' => {
                flush(&mut cur, &mut out);
                out.push(c.to_string());
            }
            _ => cur.push(c),
        }
    }
    flush(&mut cur, &mut out);
    out
}

fn flush(cur: &mut String, out: &mut Vec<String>) {
    if !cur.is_empty() {
        out.push(std::mem::take(cur));
    }
}

/// `a=1 b = {x*2} c=3` → [(a,"1"), (b,"{x*2}"), (c,"3")]
pub fn parse_assignments(s: &str) -> Vec<(String, String)> {
    let b = s.as_bytes();
    let mut eqs = vec![];
    let mut depth = 0i32;
    for i in 0..b.len() {
        match b[i] {
            b'{' | b'(' => depth += 1,
            b'}' | b')' => depth -= 1,
            b'=' if depth == 0 => {
                let prev = if i > 0 { b[i - 1] } else { b' ' };
                let next = b.get(i + 1).copied().unwrap_or(b' ');
                if !matches!(prev, b'=' | b'<' | b'>' | b'!') && next != b'=' {
                    eqs.push(i);
                }
            }
            _ => {}
        }
    }
    let key_start = |eq: usize| {
        let mut j = eq;
        while j > 0 && b[j - 1] == b' ' {
            j -= 1;
        }
        let end = j;
        while j > 0 && !matches!(b[j - 1], b' ' | b'(' | b',') {
            j -= 1;
        }
        (j, end)
    };
    let mut out = vec![];
    for (n, &eq) in eqs.iter().enumerate() {
        let (ks, ke) = key_start(eq);
        let vend = eqs.get(n + 1).map_or(s.len(), |&e| key_start(e).0);
        let key = s[ks..ke].trim().to_string();
        let val = s[eq + 1..vend].trim().trim_end_matches(')').trim().to_string();
        if !key.is_empty() {
            out.push((key, val));
        }
    }
    out
}

fn parse_model(t: &str) -> Result<(String, Model), String> {
    let rest = t.trim_start().strip_prefix(".model").ok_or("not a .model line")?.trim_start();
    let (name, rest) = rest.split_once(char::is_whitespace).ok_or("incomplete .model")?;
    let rest = rest.trim_start();
    let (ako, rest) = match rest.strip_prefix("ako:") {
        Some(r) => {
            let (b, r) = r.split_once(char::is_whitespace).unwrap_or((r, ""));
            (Some(b), r.trim_start())
        }
        None => (None, rest),
    };
    let kend = rest.find(|c: char| !c.is_ascii_alphanumeric()).unwrap_or(rest.len());
    let kind = rest[..kend].to_string();
    let body = rest[kend..].trim().trim_start_matches('(').trim_end().trim_end_matches(')');
    let mut params: HashMap<String, String> = parse_assignments(body).into_iter().collect();
    // bare flags such as `pchan`
    let toks = tokenize(body);
    for (i, t) in toks.iter().enumerate() {
        let near_eq = toks.get(i + 1).is_some_and(|n| n == "=") || (i > 0 && toks[i - 1] == "=");
        if !near_eq && t.chars().all(|c| c.is_ascii_alphabetic()) {
            params.insert(t.clone(), "1".into());
        }
    }
    if let Some(b) = ako {
        params.insert("ako".into(), b.to_string()); // merged by resolve_ako
    }
    Ok((name.to_string(), Model { kind, params }))
}

fn eval_str(s: &str, sc: &Scope) -> Result<f64, String> {
    let s = s.trim();
    if let Some(v) = parse_number(s) {
        return Ok(v);
    }
    let inner = s.trim_matches('\'').trim_matches('"');
    Expr::parse(inner)?.inline(&sc.funcs)?.eval(&VarEnv(&|n| sc.get(n).copied())).map_err(|e| format!("'{s}': {e}"))
}

/// Skip `n` whitespace-separated words and return the remainder.
fn after_words(s: &str, n: usize) -> &str {
    let mut r = s.trim_start();
    for _ in 0..n {
        r = r.find(char::is_whitespace).map_or("", |i| r[i..].trim_start());
    }
    r
}

/// Values of a linear/log sweep: lin (start, stop, step), dec/oct (start, stop, points).
pub fn sweep(kind: &str, a: f64, b: f64, c: f64) -> Result<Vec<f64>, String> {
    let mut v = vec![];
    match kind {
        "dec" | "oct" => {
            if a <= 0.0 || b < a || c <= 0.0 {
                return Err(format!("bad {kind} sweep {a} {b} {c}"));
            }
            let base: f64 = if kind == "dec" { 10.0 } else { 2.0 };
            let mut k = 0.0;
            loop {
                let x = a * base.powf(k / c);
                if x > b * (1.0 + 1e-9) || v.len() > 10_000_000 {
                    break;
                }
                v.push(x);
                k += 1.0;
            }
        }
        _ => {
            if c == 0.0 || (b - a) / c < -1e-12 {
                return Err(format!("bad sweep {a} to {b} step {c}"));
            }
            let n = ((b - a) / c + 1e-9).floor() as usize + 1;
            if n > 10_000_000 {
                return Err("sweep has too many points".into());
            }
            v.extend((0..n).map(|i| a + c * i as f64));
        }
    }
    Ok(v)
}

impl Deck {
    pub fn parse(text: &str, base: Option<&Path>) -> Result<Deck, String> {
        let mut d = Deck::default();
        let raw: Vec<&str> = text.lines().collect();
        let mut start = 0;
        if let Some(first) = raw.first() {
            let t = first.trim();
            if !t.starts_with('.') {
                d.title = t.trim_start_matches('*').trim().to_string();
                start = 1;
            }
        }
        let mut lines = vec![];
        read_lines(&raw[start..], start, base, &mut lines, &mut d.warnings, &mut d.encrypted, 0)?;
        d.classify(lines)?;
        Ok(d)
    }

    fn classify(&mut self, lines: Vec<Line>) -> Result<(), String> {
        let mut stack: Vec<(String, Subckt)> = vec![];
        for l in lines {
            let first = l.text.split_whitespace().next().unwrap_or("").to_string();
            match first.as_str() {
                ".subckt" => {
                    let toks = tokenize(&l.text);
                    let name = toks.get(1).ok_or(format!("line {}: .subckt needs a name", l.no))?.clone();
                    let (mut ports, mut params) = (vec![], vec![]);
                    let mut i = 2;
                    while i < toks.len() {
                        if toks[i] == "params:" {
                            i += 1;
                        } else if toks.get(i + 1).is_some_and(|t| t == "=") {
                            params.push((toks[i].clone(), toks.get(i + 2).cloned().unwrap_or_default()));
                            i += 3;
                        } else {
                            ports.push(toks[i].clone());
                            i += 1;
                        }
                    }
                    stack.push((name, Subckt { ports, params, funcs: Funcs::new(), body: vec![] }));
                    continue;
                }
                ".ends" => {
                    let (n, s) = stack.pop().ok_or(format!("line {}: .ends without .subckt", l.no))?;
                    self.subckts.insert(n, s);
                    continue;
                }
                ".model" => {
                    let (n, m) = parse_model(&l.text).map_err(|e| format!("line {}: {e}", l.no))?;
                    self.models.insert(n, m);
                    continue;
                }
                ".func" => {
                    let (n, f) = parse_func(after_words(&l.text, 1)).map_err(|e| format!("line {}: {e}", l.no))?;
                    stack.last_mut().map_or(&mut self.funcs, |(_, s)| &mut s.funcs).insert(n, f);
                    continue;
                }
                _ => {}
            }
            if let Some((_, sub)) = stack.last_mut() {
                if first == ".param" || first == ".params" {
                    sub.params.extend(param_list(after_words(&l.text, 1)));
                } else {
                    sub.body.push(l);
                }
                continue;
            }
            match first.as_str() {
                ".param" | ".params" => self.params.extend(param_list(after_words(&l.text, 1))),
                ".op" | ".tran" | ".ac" | ".dc" | ".tf" | ".noise" => {
                    if self.analysis.is_none() {
                        self.analysis = Some(l);
                    } else {
                        self.warnings.push(format!("line {}: extra analysis '{}' ignored", l.no, l.text));
                    }
                }
                ".step" => self.steps.push(l),
                ".meas" | ".measure" => self.meas.push(l),
                ".ic" => self.ics.push(l),
                ".temp" => self.temps.push(l),
                ".four" | ".fourier" => self.four.push(l),
                ".options" | ".option" | ".opt" | ".opts" => self.options.push(l),
                ".backanno" | ".save" | ".probe" | ".plot" | ".print" | ".width" | ".title" | ".end" | ".endl" => {}
                f if f.starts_with('.') => self.warnings.push(format!("line {}: '{f}' is not supported yet and was ignored", l.no)),
                _ => self.cards.push(l),
            }
        }
        if let Some((n, _)) = stack.last() {
            return Err(format!(".subckt {n} has no matching .ends"));
        }
        resolve_ako(&mut self.models);
        Ok(())
    }

    /// Temperatures (°C) listed on `.temp` lines.
    fn temps(&self) -> Result<Vec<f64>, String> {
        let mut v = vec![];
        for l in &self.temps {
            for t in l.text.split_whitespace().skip(1) {
                v.push(eval_str(t, &Scope::default()).map_err(|e| format!("line {}: .temp: {e}", l.no))?);
            }
        }
        Ok(v)
    }

    pub fn eval_params(&self, over: &HashMap<String, f64>) -> Result<Scope, String> {
        let mut sc = Scope { vars: over.clone(), funcs: Rc::new(self.funcs.clone()) };
        // circuit temperature (°C): stepped value, single `.temp`, or 27
        if !sc.contains_key("temp") {
            sc.insert("temp".into(), self.temps()?.first().copied().unwrap_or(27.0));
        }
        let mut pending: Vec<&(String, String)> = self.params.iter().filter(|(k, _)| !over.contains_key(k)).collect();
        while !pending.is_empty() {
            let before = pending.len();
            let mut last_err = String::new();
            pending.retain(|(k, v)| match eval_str(v, &sc) {
                Ok(x) => {
                    sc.insert(k.clone(), x);
                    false
                }
                Err(e) => {
                    last_err = format!(".param {k}: {e}");
                    true
                }
            });
            if pending.len() == before {
                return Err(last_err);
            }
        }
        Ok(sc)
    }

    pub fn options(&self) -> Options {
        let mut o = Options::default();
        for l in &self.options {
            for (k, v) in parse_assignments(after_words(&l.text, 1)) {
                let Some(x) = parse_number(&v) else { continue };
                match k.as_str() {
                    "reltol" => o.reltol = x,
                    "abstol" => o.abstol = x,
                    "vntol" => o.vntol = x,
                    "gmin" => o.gmin = x,
                    "itl1" => o.itl1 = x as usize,
                    "itl4" => o.itl4 = x as usize,
                    "trtol" => o.trtol = x,
                    "maxstep" => o.maxstep = x,
                    "tnom" => o.tnom = x,
                    _ => {}
                }
            }
        }
        o
    }

    pub fn analysis(&self, sc: &Scope) -> Result<Analysis, String> {
        let l = self.analysis.as_ref().ok_or("no analysis command: add .op, .tran, .ac, .dc, .tf or .noise")?;
        let toks = tokenize(&l.text);
        let e = |s: &str| eval_str(s, sc).map_err(|er| format!("line {}: {er}", l.no));
        let tk = |i: usize| toks.get(i).map(|s| s.as_str()).ok_or(format!("line {}: incomplete '{}'", l.no, l.text));
        // frequency sweep `dec|oct|lin <points> <fstart> <fstop>` starting at token `i`
        let freqs = |i: usize| -> Result<Vec<f64>, String> {
            let kind = tk(i)?;
            let (n, f1, f2) = (e(tk(i + 1)?)?, e(tk(i + 2)?)?, e(tk(i + 3)?)?);
            if kind == "lin" {
                let n = n.max(1.0) as usize;
                Ok((0..n).map(|i| if n == 1 { f1 } else { f1 + (f2 - f1) * i as f64 / (n - 1) as f64 }).collect())
            } else {
                sweep(kind, f1, f2, n).map_err(|er| format!("line {}: {er}", l.no))
            }
        };
        Ok(match toks[0].as_str() {
            ".op" => Analysis::Op,
            ".tran" => {
                let uic = toks.iter().any(|t| t == "uic");
                let nums = toks[1..]
                    .iter()
                    .filter(|t| !matches!(t.as_str(), "uic" | "startup" | "steady" | "nodiscard" | "step"))
                    .map(|t| e(t))
                    .collect::<Result<Vec<_>, _>>()?;
                let (tstep, tstop, tstart, tmax) = match nums.len() {
                    0 => return Err(format!("line {}: .tran needs a stop time", l.no)),
                    1 => (0.0, nums[0], 0.0, 0.0),
                    _ => (nums[0], nums[1], nums.get(2).copied().unwrap_or(0.0), nums.get(3).copied().unwrap_or(0.0)),
                };
                if !(tstop > 0.0) || tstart >= tstop {
                    return Err(format!("line {}: bad .tran times", l.no));
                }
                Analysis::Tran { tstep, tstop, tstart, tmax, uic }
            }
            ".ac" => Analysis::Ac(freqs(1)?),
            ".noise" => {
                let t: Vec<&str> = toks[1..].iter().map(|s| s.as_str()).collect();
                let (out, refn, src, at) = match t[..] {
                    ["v", "(", a, ")", src, ..] => (a, "", src, 6),
                    ["v", "(", a, b, ")", src, ..] => (a, b, src, 7),
                    _ => return Err(format!("line {}: expected .noise V(out[,ref]) <source> dec|oct|lin <points> <fstart> <fstop>", l.no)),
                };
                Analysis::Noise { out: out.into(), refn: refn.into(), src: src.into(), freqs: freqs(at)? }
            }
            ".dc" => {
                let args: Vec<&String> = toks[1..].iter().filter(|t| t.as_str() != "lin").collect();
                let mut sw = vec![];
                for g in args.chunks(4) {
                    if g.len() < 4 {
                        return Err(format!("line {}: .dc needs source start stop step", l.no));
                    }
                    let v = sweep("lin", e(g[1])?, e(g[2])?, e(g[3])?).map_err(|er| format!("line {}: {er}", l.no))?;
                    sw.push((g[0].clone(), v));
                }
                if sw.is_empty() || sw.len() > 2 {
                    return Err(format!("line {}: .dc supports one or two sweeps", l.no));
                }
                Analysis::Dc(sw)
            }
            ".tf" => {
                let t: Vec<&str> = toks[1..].iter().map(|s| s.as_str()).collect();
                let (cur, a, b, src) = match t[..] {
                    ["v", "(", a, ")", src] => (false, a, "", src),
                    ["v", "(", a, b, ")", src] => (false, a, b, src),
                    ["i", "(", a, ")", src] => (true, a, "", src),
                    _ => return Err(format!("line {}: expected .tf V(out[,ref]) <source> or .tf I(<vsource>) <source>", l.no)),
                };
                Analysis::Tf { cur, a: a.into(), b: b.into(), src: src.into() }
            }
            _ => unreachable!(),
        })
    }

    pub fn steps(&self, sc: &Scope) -> Result<(Vec<Step>, Vec<String>), String> {
        let (mut out, mut warn) = (vec![], vec![]);
        for l in &self.steps {
            let toks = tokenize(&l.text);
            let e = |s: &str| eval_str(s, sc).map_err(|er| format!("line {}: {er}", l.no));
            let mut i = 1;
            let mut mode = "lin";
            if let Some(m) = toks.get(i).filter(|t| matches!(t.as_str(), "lin" | "dec" | "oct")) {
                mode = m;
                i += 1;
            }
            if toks.get(i).is_some_and(|t| t == "param") {
                i += 1;
            }
            let name = toks.get(i).ok_or(format!("line {}: incomplete .step", l.no))?.clone();
            i += 1;
            let values = if toks.get(i).is_some_and(|t| t == "list") {
                toks[i + 1..].iter().map(|t| e(t)).collect::<Result<Vec<_>, _>>()?
            } else {
                if toks.len() < i + 3 {
                    return Err(format!("line {}: .step needs start stop step", l.no));
                }
                sweep(mode, e(&toks[i])?, e(&toks[i + 1])?, e(&toks[i + 2])?).map_err(|er| format!("line {}: {er}", l.no))?
            };
            out.push(Step { name, values });
        }
        let temps = self.temps()?;
        if temps.len() > 1 {
            if out.iter().any(|s| s.name == "temp") {
                warn.push(".temp ignored: .step temp sets the temperature".into());
            } else {
                out.push(Step { name: "temp".into(), values: temps });
            }
        }
        Ok((out, warn))
    }

    /// Netlist `.model`s, then SwiftSim's built-ins, then LTspice's `cmp/standard.*`.
    fn find_model(&self, name: &str) -> Option<&Model> {
        self.models.get(name).or_else(|| builtin_models().get(name)).or_else(|| ltspice_models(&ltspice_lib()?).get(name))
    }

    /// Names the encrypted libraries a missing model/subcircuit may be in.
    fn enc_hint(&self) -> String {
        if self.encrypted.is_empty() { String::new() } else { format!("; it may be in {}, which is encrypted and cannot be read", self.encrypted.join(", ")) }
    }

    fn model(&self, name: &str, kinds: &[&str]) -> Result<Model, String> {
        if let Some(m) = self.find_model(name) {
            if !kinds.contains(&m.kind.as_str()) {
                return Err(format!("model '{name}' is type {}, expected {}", m.kind, kinds.join("/")));
            }
            return Ok(m.clone());
        }
        if kinds.contains(&name) {
            return Ok(Model { kind: name.into(), params: HashMap::new() });
        }
        Err(match ltspice_lib() {
            Some(p) => format!("unknown model '{name}' (not in the netlist, the built-ins or the LTspice library {})", p.display()),
            None => format!("unknown model '{name}' (not built in, and no LTspice library folder found: choose it under Simulate, or set SWIFTSIM_LTSPICE_LIB)"),
        } + &self.enc_hint())
    }

    fn is_model(&self, name: &str) -> bool {
        self.find_model(name).is_some()
            || matches!(name, "d" | "npn" | "pnp" | "nmos" | "pmos" | "njf" | "pjf" | "sw")
    }

    fn node_count(&self, kind: char, toks: &[String]) -> Result<usize, String> {
        Ok(match kind {
            'r' | 'c' | 'l' | 'd' | 'v' | 'i' | 'b' | 'f' | 'h' => 2,
            'j' => 3,
            'e' | 'g' => {
                let t2 = toks.get(2).map(|s| s.as_str()).unwrap_or("");
                if matches!(t2, "value" | "table" | "laplace" | "poly") { 2 } else { 4 }
            }
            's' => 4,
            'q' => {
                if toks.len() >= 5 && !self.is_model(&toks[3]) { 4 } else { 3 }
            }
            'm' => {
                if toks.len() >= 4 && self.is_model(&toks[3]) { 3 } else { 4 }
            }
            'k' => 0,
            _ => return Err(format!("element type '{}' is not supported yet", kind.to_ascii_uppercase())),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn flatten(
        &self,
        line: &Line,
        prefix: &str,
        nmap: &HashMap<String, String>,
        scope: &Rc<Scope>,
        global: &Rc<Scope>,
        out: &mut Vec<Flat>,
        depth: usize,
    ) -> Result<(), String> {
        if depth > 40 {
            return Err("subcircuit nesting too deep (recursive .subckt?)".into());
        }
        let err = |e: String| format!("line {}: {e}", line.no);
        let toks = tokenize(&line.text);
        let Some(name) = toks.first() else { return Ok(()) };
        let kind = name.chars().next().unwrap_or(' ');
        let full = format!("{prefix}{name}");
        let mapn = |n: &str| -> String {
            if n == "0" || n == "gnd" {
                "0".into()
            } else if let Some(m) = nmap.get(n) {
                m.clone()
            } else {
                format!("{prefix}{n}")
            }
        };
        let rest = &toks[1..];
        if kind == 'x' {
            let end = rest
                .iter()
                .position(|t| t == "params:")
                .or_else(|| rest.iter().position(|t| t == "=").map(|p| p.saturating_sub(1)))
                .unwrap_or(rest.len());
            if end == 0 {
                return Err(err(format!("{name}: missing subcircuit name")));
            }
            let sname = &rest[end - 1];
            let sub = self.subckts.get(sname).ok_or_else(|| err(format!("unknown subcircuit '{sname}' (no .subckt for it in the netlist or its .lib/.include files){}", self.enc_hint())))?;
            let nodes = &rest[..end - 1];
            if nodes.len() != sub.ports.len() {
                return Err(err(format!("{name}: {} nodes given, subcircuit '{sname}' has {} ports", nodes.len(), sub.ports.len())));
            }
            let mut sc: Scope = (**global).clone();
            if !sub.funcs.is_empty() {
                sc.funcs = Rc::new(global.funcs.iter().chain(&sub.funcs).map(|(k, v)| (k.clone(), v.clone())).collect());
            }
            let mut inst = vec![];
            let mut i = end;
            while i < rest.len() {
                if rest.get(i + 1).is_some_and(|t| t == "=") {
                    inst.push((rest[i].clone(), rest.get(i + 2).cloned().unwrap_or_default()));
                    i += 3;
                } else {
                    i += 1;
                }
            }
            for (k, v) in &inst {
                sc.insert(k.clone(), eval_str(v, scope).map_err(err)?);
            }
            for (k, v) in &sub.params {
                if !inst.iter().any(|(ik, _)| ik == k) {
                    let x = eval_str(v, &sc).map_err(err)?;
                    sc.insert(k.clone(), x);
                }
            }
            let cmap: HashMap<String, String> = sub.ports.iter().cloned().zip(nodes.iter().map(|n| mapn(n))).collect();
            let sc = Rc::new(sc);
            let cpre = format!("{full}.");
            for l in &sub.body {
                self.flatten(l, &cpre, &cmap, &sc, global, out, depth + 1)?;
            }
            return Ok(());
        }
        let nn = self.node_count(kind, rest).map_err(err)?;
        if rest.len() < nn {
            return Err(err(format!("{name}: expected {nn} nodes")));
        }
        let nodes: Vec<String> = rest[..nn].iter().map(|n| mapn(n)).collect();
        let r = &rest[nn..];
        let sigv = |a: &str, b: &str| Expr::Bin(Op::Sub, Box::new(Expr::Sig(format!("v({a})"))), Box::new(Expr::Sig(format!("v({b})"))));
        let parse = |t: &String| Expr::parse(t).map_err(err);
        let src = if matches!(kind, 'e' | 'f' | 'g' | 'h') && r.first().is_some_and(|t| t == "poly") {
            // POLY(n) (a,b)… | vname… c0 c1 …: parentheses and commas are only separators
            let t: Vec<&String> = r[1..].iter().filter(|t| *t != "(" && *t != ")").collect();
            let n = t.first().and_then(|n| n.parse::<usize>().ok()).filter(|&n| n > 0).ok_or_else(|| err(format!("{name}: bad POLY(n)")))?;
            let vc = matches!(kind, 'e' | 'g');
            let k = 1 + if vc { 2 * n } else { n };
            if t.len() < k {
                return Err(err(format!("{name}: POLY({n}) needs {} controls", if vc { "node pairs as" } else { "source names as" })));
            }
            let x: Vec<Expr> = if vc { t[1..k].chunks(2).map(|p| sigv(p[0], p[1])).collect() } else { t[1..k].iter().map(|v| Expr::Sig(format!("i({v})"))).collect() };
            let c = t[k..].iter().map(|c| parse(c)).collect::<Result<Vec<_>, _>>()?;
            Some((matches!(kind, 'e' | 'h'), poly(&x, &c)))
        } else if let (true, Some(i)) = (matches!(kind, 'e' | 'g'), r.iter().position(|t| t == "table")) {
            // nc+ nc- table=(x,y …)  or  table {expr} = (x,y) …
            let (x, pts) = if nn == 4 { (sigv(&rest[2], &rest[3]), i + 2) } else { (parse(r.get(i + 1).ok_or_else(|| err(format!("{name}: table needs an input")))?)?, i + 3) };
            let mut a = vec![x];
            for t in r.iter().skip(pts).filter(|t| *t != "(" && *t != ")") {
                a.push(parse(t)?);
            }
            Some((kind == 'e', Expr::Call("table".into(), a)))
        } else if kind == 'b' || (matches!(kind, 'e' | 'g') && nn == 2) {
            let r = after_words(&line.text, 3);
            let is_v = if kind == 'b' { r.starts_with('v') } else { kind == 'e' };
            if (kind == 'b' && !(r.starts_with('v') || r.starts_with('i'))) || (kind != 'b' && !r.starts_with("value")) {
                return Err(err(format!("{name}: expected V=<expr> or I=<expr>")));
            }
            let eq = r.find('=').ok_or_else(|| err(format!("{name}: missing '='")))?;
            Some((is_v, Expr::parse(r[eq + 1..].trim()).map_err(err)?))
        } else {
            None
        };
        let sig = |s: &str| -> String {
            if let Some(n) = s.strip_prefix("v(").and_then(|r| r.strip_suffix(')')) {
                format!("v({})", mapn(n))
            } else if let Some(o) = s.find('(') {
                format!("{}({prefix}{}", &s[..o], &s[o + 1..])
            } else {
                s.to_string()
            }
        };
        let expr = match src {
            Some((is_v, e)) => Some((is_v, e.inline(&scope.funcs).map_err(err)?.resolve(&|n| scope.get(n).copied(), &sig))),
            None => None,
        };
        let mut toks: Vec<String> = r.to_vec();
        match kind {
            'f' | 'h' if !toks.is_empty() => toks[0] = format!("{prefix}{}", toks[0]),
            'k' if toks.len() >= 2 => {
                toks[0] = format!("{prefix}{}", toks[0]);
                toks[1] = format!("{prefix}{}", toks[1]);
            }
            _ => {}
        }
        out.push(Flat { line: line.no, kind, name: full, nodes, toks, scope: scope.clone(), expr });
        Ok(())
    }

    /// Flatten and build a simulatable circuit. `over` overrides parameters
    /// and element values (for `.step`).
    pub fn build(&self, over: &HashMap<String, f64>) -> Result<Circuit, String> {
        let global = Rc::new(self.eval_params(over)?);
        let mut flat = vec![];
        for l in &self.cards {
            self.flatten(l, "", &HashMap::new(), &global, &global, &mut flat, 0)?;
        }
        if flat.is_empty() {
            return Err("netlist has no circuit elements".into());
        }
        let opts = self.options();
        let temp = global["temp"] + 273.15;
        let tnom = opts.tnom + 273.15;
        let mut b = Builder { deck: self, over, temp, tnom, nodes: HashMap::new(), node_names: vec![], internal: vec![], devs: vec![], inds: vec![], ks: vec![], warnings: vec![] };
        for f in &flat {
            b.element(f).map_err(|e| format!("line {} ({}): {e}", f.line, f.name))?;
        }
        for (l1, l2, k) in std::mem::take(&mut b.ks) {
            let lv = |n: &str| b.inds.iter().find(|i| i.name == n).map(|i| i.l).ok_or(format!("K: unknown inductor '{n}'"));
            let m = k * (lv(&l1)? * lv(&l2)?).sqrt();
            for ind in b.inds.iter_mut() {
                if ind.name == l1 {
                    ind.coupling.push((l2.clone(), m));
                } else if ind.name == l2 {
                    ind.coupling.push((l1.clone(), m));
                }
            }
        }
        for ind in std::mem::take(&mut b.inds) {
            b.devs.push(Box::new(ind));
        }
        let nn = b.node_names.len();
        let mut s = Setup {
            n: nn,
            names: b.node_names.iter().map(|n| format!("v({n})")).collect(),
            is_current: vec![false; nn],
            nstate: 0,
            nodes: b.nodes.clone(),
            branches: HashMap::new(),
        };
        let mut devs = b.devs;
        for d in devs.iter_mut() {
            d.setup(&mut s);
        }
        for d in devs.iter_mut() {
            d.bind(&s).map_err(|e| format!("{}: {e}", d.name()))?;
        }
        let mut ics = vec![];
        for l in &self.ics {
            let t = tokenize(&l.text);
            let mut i = 1;
            while i < t.len() {
                if t[i] == "v" && t.get(i + 1).is_some_and(|x| x == "(") && t.get(i + 4).is_some_and(|x| x == "=") {
                    let node = &t[i + 2];
                    let idx = *s.nodes.get(node.as_str()).ok_or(format!("line {}: .ic unknown node '{node}'", l.no))?;
                    let v = eval_str(t.get(i + 5).ok_or(format!("line {}: .ic missing value", l.no))?, &global)?;
                    ics.push((idx, v));
                    i += 6;
                } else {
                    return Err(format!("line {}: expected .ic V(node)=value", l.no));
                }
            }
        }
        let out_nodes = b
            .node_names
            .iter()
            .enumerate()
            .filter(|(i, _)| !b.internal[*i])
            .map(|(i, n)| (format!("v({n})"), i + 1))
            .collect();
        Ok(Circuit {
            nonlinear: devs.iter().any(|d| d.nonlinear()),
            n: s.n,
            names: s.names,
            is_current: s.is_current,
            nstate: s.nstate,
            devices: devs,
            out_nodes,
            ics,
            opts,
            temp: b.temp,
            warnings: b.warnings,
        })
    }
}

fn read_lines(raw: &[&str], offset: usize, base: Option<&Path>, out: &mut Vec<Line>, warn: &mut Vec<String>, enc: &mut Vec<String>, depth: usize) -> Result<(), String> {
    if depth > 10 {
        return Err("includes nested too deeply".into());
    }
    for (i, l) in raw.iter().enumerate() {
        let no = offset + i + 1;
        let mut t = l.trim();
        if let Some(p) = t.find(';') {
            t = t[..p].trim_end();
        }
        if t.is_empty() || t.starts_with('*') {
            continue;
        }
        if let Some(rest) = t.strip_prefix('+') {
            if let Some(last) = out.last_mut() {
                last.text.push(' ');
                last.text.push_str(&rest.to_ascii_lowercase());
            }
            continue;
        }
        let low = t.to_ascii_lowercase();
        let word = low.split_whitespace().next().unwrap_or("");
        if matches!(word, ".include" | ".inc" | ".lib") {
            let arg = t[word.len()..].trim();
            let (path, section) = if arg.starts_with('"') {
                let end = arg[1..].find('"').map_or(arg.len(), |e| e + 1);
                (arg[1..end].to_string(), arg[(end + 1).min(arg.len())..].trim().to_ascii_lowercase())
            } else {
                match arg.rsplit_once(char::is_whitespace) {
                    Some((p, s)) if word == ".lib" => (p.trim().to_string(), s.to_ascii_lowercase()),
                    _ => (arg.to_string(), String::new()),
                }
            };
            // LTspice netlists may use Windows separators
            let rel = PathBuf::from(if cfg!(windows) { path.clone() } else { path.replace('\\', "/") });
            let mut tries = vec![if rel.is_absolute() { rel.clone() } else { base.map_or(rel.clone(), |b| b.join(&rel)) }];
            // like LTspice, relative names also resolve in its lib/sub and lib/cmp folders
            if let (false, Some(lt)) = (rel.is_absolute(), ltspice_lib()) {
                tries.extend(["sub", "cmp", ""].map(|d| lt.join(d).join(&rel)));
            }
            let Some((text, p)) = tries.into_iter().find_map(|p| Some((decode(&std::fs::read(&p).ok()?), p))) else {
                warn.push(format!("line {no}: library '{path}' not found, ignored (built-in models are still available)"));
                continue;
            };
            if is_encrypted(&text) {
                warn.push(format!("line {no}: library '{path}' is vendor-encrypted; SwiftSim cannot read encrypted models, so parts using it will fail"));
                enc.push(format!("'{path}'"));
                continue;
            }
            let lines: Vec<&str> = text.lines().collect();
            let pick: Vec<&str> = if section.is_empty() {
                lines
            } else {
                let mut on = false;
                lines
                    .into_iter()
                    .filter(|l| {
                        let w: Vec<String> = l.split_whitespace().map(|s| s.to_ascii_lowercase()).collect();
                        if w.first().is_some_and(|f| f == ".lib") && w.get(1) == Some(&section) {
                            on = true;
                            return false;
                        }
                        if w.first().is_some_and(|f| f == ".endl") {
                            on = false;
                            return false;
                        }
                        on
                    })
                    .collect()
            };
            read_lines(&pick, 0, p.parent(), out, warn, enc, depth + 1)?;
            continue;
        }
        if low == ".end" {
            break;
        }
        out.push(Line { no, text: low });
    }
    Ok(())
}

struct Flat {
    line: usize,
    kind: char,
    name: String,
    nodes: Vec<String>,
    toks: Vec<String>,
    scope: Rc<Scope>,
    expr: Option<(bool, Expr)>,
}

struct Builder<'a> {
    deck: &'a Deck,
    over: &'a HashMap<String, f64>,
    /// Circuit and nominal temperature, kelvin.
    temp: f64,
    tnom: f64,
    nodes: HashMap<String, usize>,
    node_names: Vec<String>,
    internal: Vec<bool>,
    devs: Vec<Box<dyn Device>>,
    inds: Vec<Inductor>,
    ks: Vec<(String, String, f64)>,
    warnings: Vec<String>,
}

/// Positional tokens and `key=value` pairs.
fn split_kv(toks: &[String]) -> (Vec<&str>, HashMap<&str, &str>) {
    let (mut pos, mut kv) = (vec![], HashMap::new());
    let mut i = 0;
    while i < toks.len() {
        if toks.get(i + 1).is_some_and(|t| t == "=") {
            kv.insert(toks[i].as_str(), toks.get(i + 2).map_or("", |s| s.as_str()));
            i += 3;
        } else {
            pos.push(toks[i].as_str());
            i += 1;
        }
    }
    (pos, kv)
}

fn source_spec(toks: &[String], sc: &Scope) -> Result<(Option<f64>, Option<Wave>, C64, HashMap<String, f64>), String> {
    let (mut dc, mut wave, mut ac, mut kv) = (None, None, C64::default(), HashMap::new());
    let num = |s: &str| eval_str(s, sc);
    let mut i = 0;
    while i < toks.len() {
        let t = toks[i].as_str();
        if toks.get(i + 1).is_some_and(|x| x == "=") {
            kv.insert(t.to_string(), num(toks.get(i + 2).ok_or("missing value")?)?);
            i += 3;
            continue;
        }
        match t {
            "dc" => {
                dc = Some(num(toks.get(i + 1).ok_or("DC needs a value")?)?);
                i += 2;
            }
            "ac" => {
                let mag = num(toks.get(i + 1).ok_or("AC needs a magnitude")?)?;
                i += 2;
                let mut ph = 0.0;
                if let Some(p) = toks.get(i).filter(|_| toks.get(i + 1).is_none_or(|x| x != "=")) {
                    if let Ok(v) = num(p) {
                        ph = v;
                        i += 1;
                    }
                }
                ac = C64::polar(mag, ph);
            }
            "pulse" | "sin" | "sine" | "exp" | "pwl" | "sffm" => {
                i += 1;
                let mut args = vec![];
                if toks.get(i).is_some_and(|x| x == "(") {
                    i += 1;
                    while i < toks.len() && toks[i] != ")" {
                        args.push(num(&toks[i]).map_err(|e| format!("{} argument: {e}", t.to_uppercase()))?);
                        i += 1;
                    }
                    i += 1;
                } else {
                    while let Some(v) = toks.get(i).and_then(|x| num(x).ok()) {
                        args.push(v);
                        i += 1;
                    }
                }
                wave = Some(Wave::parse(t, &args)?);
            }
            _ => {
                dc = Some(num(t)?);
                i += 1;
            }
        }
    }
    Ok((dc, wave, ac, kv))
}

impl Builder<'_> {
    fn node(&mut self, n: &str) -> usize {
        if n == "0" || n == "gnd" {
            return 0;
        }
        if let Some(&i) = self.nodes.get(n) {
            return i;
        }
        self.node_names.push(n.to_string());
        self.internal.push(false);
        let i = self.node_names.len();
        self.nodes.insert(n.to_string(), i);
        i
    }
    fn internal(&mut self, n: String) -> usize {
        let i = self.node(&n);
        self.internal[i - 1] = true;
        i
    }
    fn res(&mut self, name: String, a: usize, b: usize, r: f64) {
        self.devs.push(Box::new(Resistor { name, a, b, r }));
    }
    /// Optional series resistor: returns the inner node.
    fn series(&mut self, owner: &str, tag: &str, ext: usize, r: f64) -> usize {
        if r > 0.0 {
            let n = self.internal(format!("{owner}#{tag}"));
            self.res(format!("{owner}#r{tag}"), ext, n, r);
            n
        } else {
            ext
        }
    }

    fn element(&mut self, f: &Flat) -> Result<(), String> {
        let sc = f.scope.clone();
        let num = |s: &str| eval_str(s, &sc);
        let (pos, kv) = split_kv(&f.toks);
        let kvn = |k: &str, d: f64| kv.get(k).map_or(Ok(d), |v| num(v));
        let ov = self.over.get(&f.name).copied();
        let nd: Vec<usize> = f.nodes.clone().iter().map(|n| self.node(n)).collect();
        let value = |key: &str| -> Result<f64, String> {
            if let Some(v) = ov {
                return Ok(v);
            }
            num(pos.first().copied().or(kv.get(key).copied()).ok_or("missing value")?)
        };
        let name = f.name.clone();
        match f.kind {
            'r' => {
                // LTspice also writes `tc=tc1,tc2`
                let tc1 = kv.get("tc1").or(kv.get("tc")).map_or(Ok(0.0), |v| num(v))?;
                let tc2 = kv.get("tc2").or(pos.get(1).filter(|_| kv.contains_key("tc"))).map_or(Ok(0.0), |v| num(v))?;
                let dt = self.temp - self.tnom;
                let r = value("r")? * (1.0 + tc1 * dt + tc2 * dt * dt);
                let r = if r.abs() < 1e-9 { 1e-9 } else { r };
                self.res(name, nd[0], nd[1], r);
            }
            'c' => {
                let c = value("c")?;
                let ic = kv.get("ic").map(|v| num(v)).transpose()?;
                let a = self.series(&name, "s", nd[0], kvn("rser", 0.0)?);
                let rpar = kvn("rpar", 0.0)?;
                if rpar > 0.0 {
                    self.res(format!("{name}#rpar"), nd[0], nd[1], rpar);
                }
                self.devs.push(Box::new(Capacitor { name, a, b: nd[1], c, ic, s: 0 }));
            }
            'l' => {
                let l = value("l")?;
                let ic = kv.get("ic").map(|v| num(v)).transpose()?;
                // LTSpice gives inductors 1 mΩ series resistance by default
                let b = self.series(&name, "s", nd[1], kvn("rser", 1e-3)?);
                self.inds.push(Inductor { name, a: nd[0], b, l, ic, k: 0, s: 0, coupling: vec![], coup: vec![] });
            }
            'v' | 'i' => {
                let (dc, mut wave, ac, ex) = source_spec(&f.toks, &sc)?;
                let mut dc = dc.unwrap_or_else(|| wave.as_ref().map_or(0.0, |w| w.at(0.0)));
                if let Some(v) = ov {
                    dc = v;
                    wave = None;
                }
                if f.kind == 'v' {
                    let rser = ex.get("rser").copied().unwrap_or(0.0);
                    self.devs.push(Box::new(VSource { name, a: nd[0], b: nd[1], k: 0, dc, wave, ac, rser }));
                } else {
                    self.devs.push(Box::new(ISource { name, a: nd[0], b: nd[1], dc, wave, ac }));
                }
            }
            'd' => {
                let m = self.deck.model(pos.first().ok_or("missing model")?, &["d"])?;
                let area = pos.get(1).map_or(kvn("area", 1.0), |v| num(v))?;
                let g = |k: &[&str], d: f64| mparam(&m, k, d, &sc);
                let n = g(&["n"], 1.0)?;
                let bv = g(&["bv"], 0.0)?;
                let mut md = DiodeModel {
                    is: g(&["is"], 1e-14)? * area,
                    n,
                    cjo: g(&["cjo", "cj0", "cj"], 0.0)? * area,
                    vj: g(&["vj", "pb"], 1.0)?,
                    m: g(&["m", "mj"], 0.5)?,
                    tt: g(&["tt"], 0.0)?,
                    bv: if bv > 0.0 { bv } else { f64::INFINITY },
                    ibv: g(&["ibv"], 1e-3)? * area,
                    nbv: g(&["nbv"], n)?,
                    fc: g(&["fc"], 0.5)?,
                    vt: VT,
                    kf: g(&["kf"], 0.0)?,
                    af: g(&["af"], 1.0)?,
                };
                md.set_temp(self.temp, self.tnom, g(&["eg"], 1.11)?, g(&["xti"], 3.0)?);
                let a = self.series(&name, "a", nd[0], g(&["rs"], 0.0)? / area);
                self.devs.push(Box::new(Diode { name, a, k: nd[1], md, s: 0, vd_old: 0.0 }));
            }
            'q' => {
                let m = self.deck.model(pos.first().ok_or("missing model")?, &["npn", "pnp"])?;
                let area = pos.get(1).map_or(kvn("area", 1.0), |v| num(v))?;
                let g = |k: &[&str], d: f64| mparam(&m, k, d, &sc);
                let inf0 = |v: f64| if v > 0.0 { v } else { f64::INFINITY };
                let mut md = BjtModel {
                    pol: if m.kind == "pnp" { -1.0 } else { 1.0 },
                    is: g(&["is"], 1e-16)? * area,
                    bf: g(&["bf"], 100.0)?,
                    nf: g(&["nf"], 1.0)?,
                    vaf: inf0(g(&["vaf", "va"], 0.0)?),
                    ikf: inf0(g(&["ikf", "ik"], 0.0)? * area),
                    ise: g(&["ise"], 0.0)? * area,
                    ne: g(&["ne"], 1.5)?,
                    br: g(&["br"], 1.0)?,
                    nr: g(&["nr"], 1.0)?,
                    var: inf0(g(&["var", "vb"], 0.0)?),
                    ikr: inf0(g(&["ikr"], 0.0)? * area),
                    isc: g(&["isc"], 0.0)? * area,
                    nc: g(&["nc"], 2.0)?,
                    cje: g(&["cje"], 0.0)? * area,
                    vje: g(&["vje", "pe"], 0.75)?,
                    mje: g(&["mje", "me"], 0.33)?,
                    cjc: g(&["cjc"], 0.0)? * area,
                    vjc: g(&["vjc", "pc"], 0.75)?,
                    mjc: g(&["mjc", "mc"], 0.33)?,
                    tf: g(&["tf"], 0.0)?,
                    tr: g(&["tr"], 0.0)?,
                    fc: g(&["fc"], 0.5)?,
                    vt: VT,
                    kf: g(&["kf"], 0.0)?,
                    af: g(&["af"], 1.0)?,
                };
                md.set_temp(self.temp, self.tnom, g(&["eg"], 1.11)?, g(&["xti", "pt"], 3.0)?, g(&["xtb"], 0.0)?);
                let c = self.series(&name, "c", nd[0], g(&["rc"], 0.0)? / area);
                let b = self.series(&name, "b", nd[1], g(&["rb"], 0.0)? / area);
                let e = self.series(&name, "e", nd[2], g(&["re"], 0.0)? / area);
                self.devs.push(Box::new(Bjt { name, c, b, e, md, s: 0, vbe_old: 0.0, vbc_old: 0.0 }));
            }
            'm' => {
                let m = self.deck.model(pos.first().ok_or("missing model")?, &["nmos", "pmos", "vdmos"])?;
                let g = |k: &[&str], d: f64| mparam(&m, k, d, &sc);
                let (d, gt, s) = (nd[0], nd[1], nd[2]);
                if m.kind == "vdmos" {
                    let pol = if m.params.contains_key("pchan") { -1.0 } else { 1.0 };
                    let mult = kvn("m", 1.0)?;
                    let cmax = g(&["cgdmax"], 0.0)?;
                    let cmin = g(&["cgdmin"], cmax)?;
                    // shortcut: constant Cgd (geometric mean of Cgdmax/Cgdmin) instead of LTSpice's nonlinear Cgd(Vgd)
                    let cgd = (cmax * cmin).sqrt();
                    let di = self.series(&name, "d", d, g(&["rd"], 0.0)? / mult);
                    let si = self.series(&name, "s", s, g(&["rs"], 0.0)? / mult);
                    let gi = self.series(&name, "g", gt, g(&["rg"], 0.0)?);
                    let md = MosModel {
                        pol,
                        vto: g(&["vto", "vt0"], 0.0)?,
                        kp: 0.0,
                        gamma: 0.0,
                        phi: 0.6,
                        lambda: g(&["lambda"], 0.0)?,
                        cgs: g(&["cgs"], 0.0)? * mult,
                        cgd: cgd * mult,
                        cbd: 0.0,
                        cbs: 0.0,
                        ksub: g(&["ksubthres"], 0.0)?,
                        kf: g(&["kf"], 0.0)?,
                        af: g(&["af"], 1.0)?,
                        // ngspice (and SPICE3 with W = L = 1): kf·|Id|^af / (f·Cox²), Cox at TOX = 100 nm
                        fden: (EPS_OX / 1e-7).powi(2),
                        fgm: false,
                    };
                    // shortcut: VDMOS only scales KP with temperature (VTO fixed, no Vtotc/Bex)
                    let beta = g(&["kp"], 1.0)? * mult * (self.temp / self.tnom).powf(-1.5);
                    self.devs.push(Box::new(Mos { name: name.clone(), d: di, g: gi, s: si, b: si, beta, md, st: 0, vgs_old: 0.0, vds_old: 0.0, bulk: vec![] }));
                    let bv = g(&["bv"], 0.0)?;
                    let n = g(&["n"], 1.0)?;
                    let mut dm = DiodeModel {
                        is: g(&["is"], 1e-14)? * mult,
                        n,
                        cjo: g(&["cjo"], 0.0)? * mult,
                        vj: g(&["vj"], 1.0)?,
                        m: g(&["m"], 0.5)?,
                        tt: g(&["tt"], 0.0)?,
                        bv: if bv > 0.0 { bv } else { f64::INFINITY },
                        ibv: g(&["ibv"], 1e-3)?,
                        nbv: g(&["nbv"], n)?,
                        fc: 0.5,
                        vt: VT,
                        kf: 0.0,
                        af: 1.0,
                    };
                    dm.set_temp(self.temp, self.tnom, g(&["eg"], 1.11)?, g(&["xti"], 3.0)?);
                    let (an, ca) = if pol > 0.0 { (s, d) } else { (d, s) };
                    let an = self.series(&name, "body", an, g(&["rb"], 0.0)? / mult);
                    self.devs.push(Box::new(Diode { name: format!("{name}#body"), a: an, k: ca, md: dm, s: 0, vd_old: 0.0 }));
                } else {
                    let level = g(&["level"], 1.0)?;
                    if level != 1.0 {
                        let w = format!("model '{}' is level {level}; simulated with the level-1 equations (approximate)", pos[0]);
                        if !self.warnings.contains(&w) {
                            self.warnings.push(w);
                        }
                    }
                    let b = if nd.len() > 3 { nd[3] } else { s };
                    let w = kvn("w", 100e-6)?;
                    let l = kvn("l", 100e-6)?;
                    let mult = kvn("m", 1.0)?;
                    let tox = g(&["tox"], 0.0)?;
                    let kp_def = if tox > 0.0 { g(&["u0", "uo"], 600.0)? * 1e-4 * 3.453e-11 / tox } else { 2e-5 };
                    let mut md = MosModel {
                        pol: if m.kind == "pmos" { -1.0 } else { 1.0 },
                        vto: g(&["vto", "vt0"], 0.0)?,
                        kp: g(&["kp"], kp_def)?,
                        gamma: g(&["gamma"], 0.0)?,
                        phi: g(&["phi"], 0.6)?,
                        lambda: g(&["lambda"], 0.0)?,
                        cgs: g(&["cgso"], 0.0)? * w * mult + g(&["cgs"], 0.0)?,
                        cgd: g(&["cgdo"], 0.0)? * w * mult + g(&["cgd"], 0.0)?,
                        cbd: g(&["cbd"], 0.0)?,
                        cbs: g(&["cbs"], 0.0)?,
                        ksub: 0.0,
                        kf: g(&["kf"], 0.0)?,
                        af: g(&["af"], 1.0)?,
                        fden: 0.0,
                        fgm: false,
                    };
                    // flicker noise denominator by ngspice's NLEV; without NLEV the SPICE3 form
                    // kf·|Id|^af / (f·W·L·Cox²) (ngspice's own default is NLEV=2)
                    let cox = EPS_OX / if tox > 0.0 { tox } else { 1e-7 };
                    let mf = mult.powf(md.af - 1.0); // m·kf·|Id/m|^af
                    (md.fden, md.fgm) = match g(&["nlev"], -1.0)? as i32 {
                        0 => (l * l * cox * mf, false),
                        1 => (w * l * cox * mf, false),
                        2 | 3 => (mult * w * l * cox, true),
                        _ => (w * l * cox * cox * mf, false),
                    };
                    md.set_temp(self.temp, self.tnom);
                    let beta = md.kp * w / l * mult * (self.temp / self.tnom).powf(-1.5);
                    let di = self.series(&name, "d", d, g(&["rd"], 0.0)?);
                    let si = self.series(&name, "s", s, g(&["rs"], 0.0)?);
                    // shortcut: bulk junctions use IS only (no JS·AD/AS area scaling, no N)
                    let is = g(&["is"], 1e-14)? * mult;
                    let (temp, tnom) = (self.temp, self.tnom);
                    let junction = |tag: &str, ch: usize| {
                        let (a, k) = if md.pol > 0.0 { (b, ch) } else { (ch, b) };
                        mos_junction(format!("{name}#{tag}"), a, k, is, temp, tnom)
                    };
                    let bulk = vec![junction("bd", di), junction("bs", si)];
                    self.devs.push(Box::new(Mos { name, d: di, g: gt, s: si, b, beta, md, st: 0, vgs_old: 0.0, vds_old: 0.0, bulk }));
                }
            }
            'j' => {
                let m = self.deck.model(pos.first().ok_or("missing model")?, &["njf", "pjf"])?;
                let area = pos.get(1).map_or(kvn("area", 1.0), |v| num(v))?;
                let g = |k: &[&str], d: f64| mparam(&m, k, d, &sc);
                let pol = if m.kind == "pjf" { -1.0 } else { 1.0 };
                let gt = nd[1];
                let di = self.series(&name, "d", nd[0], g(&["rd"], 0.0)? / area);
                let si = self.series(&name, "s", nd[2], g(&["rs"], 0.0)? / area);
                // SPICE uses VTO unsigned for both polarities; Mos multiplies it by pol
                let md = MosModel { pol, vto: pol * g(&["vto", "vt0"], -2.0)?, kp: 0.0, gamma: 0.0, phi: 0.6, lambda: g(&["lambda"], 0.0)?, cgs: 0.0, cgd: 0.0, cbd: 0.0, cbs: 0.0, ksub: 0.0, kf: g(&["kf"], 0.0)?, af: g(&["af"], 1.0)?, fden: 1.0, fgm: false };
                let beta = 2.0 * g(&["beta"], 1e-4)? * area;
                let ch = Mos { name: name.clone(), d: di, g: gt, s: si, b: si, beta, md, st: 0, vgs_old: 0.0, vds_old: 0.0, bulk: vec![] };
                let (temp, tnom) = (self.temp, self.tnom);
                // gate junctions scale with temperature like diodes; channel VTO/BETA stay fixed (SPICE3)
                let junction = |tag: &str, ch: usize, c: f64| -> Result<Diode, String> {
                    let mut md = DiodeModel { is: g(&["is"], 1e-14)? * area, n: g(&["n"], 1.0)?, cjo: c * area, vj: g(&["pb"], 1.0)?, m: g(&["m"], 0.5)?, tt: 0.0, bv: f64::INFINITY, ibv: 1e-3, nbv: 1.0, fc: g(&["fc"], 0.5)?, vt: VT, kf: 0.0, af: 1.0 };
                    md.set_temp(temp, tnom, g(&["eg"], 1.11)?, g(&["xti"], 3.0)?);
                    let (a, k) = if pol > 0.0 { (gt, ch) } else { (ch, gt) };
                    Ok(Diode { name: format!("{name}#{tag}"), a, k, md, s: 0, vd_old: 0.0 })
                };
                let (gs, gd) = (junction("gs", si, g(&["cgs"], 0.0)?)?, junction("gd", di, g(&["cgd"], 0.0)?)?);
                self.devs.push(Box::new(Jfet { name, ch, gs, gd }));
            }
            'e' | 'g' | 'f' | 'h' => {
                if let Some((is_v, e)) = &f.expr {
                    self.devs.push(Box::new(BSource { name, a: nd[0], b: nd[1], is_v: *is_v, expr: e.clone(), bound: Expr::Num(0.0), deps: vec![], k: 0 }));
                    return Ok(());
                }
                let (cp, cn, ctrl, gain) = if matches!(f.kind, 'e' | 'g') {
                    (nd[2], nd[3], String::new(), num(pos.first().ok_or("missing gain")?)?)
                } else {
                    (0, 0, pos.first().ok_or("missing controlling source")?.to_string(), num(pos.get(1).ok_or("missing gain")?)?)
                };
                self.devs.push(Box::new(Controlled { name, kind: f.kind, a: nd[0], b: nd[1], cp, cn, ctrl, kc: 0, gain, k: 0 }));
            }
            'b' => {
                let (is_v, e) = f.expr.clone().ok_or("missing expression")?;
                self.devs.push(Box::new(BSource { name, a: nd[0], b: nd[1], is_v, expr: e, bound: Expr::Num(0.0), deps: vec![], k: 0 }));
            }
            's' => {
                let m = self.deck.model(pos.first().ok_or("missing model")?, &["sw"])?;
                let g = |k: &[&str], d: f64| mparam(&m, k, d, &sc);
                let ron = g(&["ron"], 1.0)?.max(1e-9);
                let roff = g(&["roff"], 1e12)?;
                self.devs.push(Box::new(Switch {
                    name,
                    a: nd[0],
                    b: nd[1],
                    cp: nd[2],
                    cn: nd[3],
                    ron,
                    roff,
                    vt: g(&["vt"], 0.0)?,
                    vh: g(&["vh"], 0.0)?.abs(),
                    s: 0,
                }));
            }
            'k' => {
                if pos.len() < 3 {
                    return Err("K needs two inductors and a coupling factor".into());
                }
                let k = num(pos[2])?;
                if !(-1.0..=1.0).contains(&k) {
                    return Err("coupling factor must be within -1..1".into());
                }
                self.ks.push((pos[0].to_string(), pos[1].to_string(), k));
            }
            _ => return Err("unsupported element".into()),
        }
        Ok(())
    }
}

/// SPICE POLY polynomial: c0 + Σ ci·xi + Σ cij·xi·xj (i ≤ j, lexicographic) + … cubic …
fn poly(x: &[Expr], c: &[Expr]) -> Expr {
    // a lone POLY(1) coefficient is the linear gain
    let c = if x.len() == 1 && c.len() == 1 { vec![Expr::Num(0.0), c[0].clone()] } else { c.to_vec() };
    let (mut terms, mut level): (Vec<Vec<usize>>, Vec<Vec<usize>>) = (vec![], vec![vec![]]);
    while terms.len() < c.len() {
        terms.extend(level.iter().cloned());
        level = level.iter().flat_map(|t| (t.last().copied().unwrap_or(0)..x.len()).map(move |j| [t.as_slice(), &[j]].concat())).collect();
    }
    c.iter()
        .zip(&terms)
        .filter(|(k, _)| !matches!(k, Expr::Num(v) if *v == 0.0))
        .map(|(k, t)| t.iter().fold(k.clone(), |a, &j| Expr::Bin(Op::Mul, Box::new(a), Box::new(x[j].clone()))))
        .reduce(|a, b| Expr::Bin(Op::Add, Box::new(a), Box::new(b)))
        .unwrap_or(Expr::Num(0.0))
}

fn mparam(m: &Model, keys: &[&str], def: f64, sc: &Scope) -> Result<f64, String> {
    for k in keys {
        if let Some(v) = m.params.get(*k) {
            return eval_str(v, sc).map_err(|e| format!("model parameter {k}: {e}"));
        }
    }
    Ok(def)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tokens_and_assignments() {
        assert_eq!(tokenize("v1 a 0 pulse(0 {vh*2} 1n)"), ["v1", "a", "0", "pulse", "(", "0", "{vh*2}", "1n", ")"]);
        let a = parse_assignments("r=1k c = {1/(2*pi*f*r)} f=1meg");
        assert_eq!(a[0], ("r".into(), "1k".into()));
        assert_eq!(a[1], ("c".into(), "{1/(2*pi*f*r)}".into()));
        assert_eq!(a[2], ("f".into(), "1meg".into()));
        let (n, m) = parse_model(".model irf vdmos(pchan vto=-2 kp=5)").unwrap();
        assert_eq!(n, "irf");
        assert_eq!(m.kind, "vdmos");
        assert!(m.params.contains_key("pchan"));
        assert_eq!(m.params["vto"], "-2");
    }

    #[test]
    fn ltspice_param_without_equals_and_ako() {
        let d = Deck::parse("t\n.param vid 0.001\n.param f 1k\n.model base npn(bf=50 vaf=80)\n.model q2 ako:base npn(bf=200)\n", None).unwrap();
        let sc = d.eval_params(&HashMap::new()).unwrap();
        assert_eq!((sc["vid"], sc["f"]), (0.001, 1000.0));
        let m = d.model("q2", &["npn"]).unwrap();
        assert_eq!((m.params["bf"].as_str(), m.params["vaf"].as_str(), m.params.get("ako")), ("200", "80", None));
    }

    // a stand-in LTspice lib folder written by the test (LTspice's own files are never copied here)
    #[test]
    fn ltspice_library_lookup() {
        let lib = std::env::temp_dir().join(format!("swiftsim-ltlib-{}", std::process::id()));
        std::fs::create_dir_all(lib.join("cmp")).unwrap();
        std::fs::create_dir_all(lib.join("sub")).unwrap();
        std::fs::write(lib.join("cmp/standard.bjt"), ".model LTONLY1 NPN(IS=1E-14 BF=123\n+ VAF=100) ; comment\n.model LTONLY2 ako:LTONLY1 NPN(VAF=50 mfg=Test)\n").unwrap();
        std::fs::write(lib.join("sub/ltdiv.sub"), ".subckt ltdiv a b\nR1 a b 1k\n.ends ltdiv\n").unwrap();
        std::fs::write(lib.join("sub/secret.lib"), "* LTspice Encrypted File\n*\n* Begin:\n  A3 1D 22 9F\n").unwrap();
        assert!(is_encrypted("$CDNENCSTART\nabc\n$CDNENCFINISH"));

        set_ltspice_lib(Some(lib.clone()));
        let net = "t\n.lib ltdiv.sub\n.lib secret.lib\nV1 c 0 5\nQ1 c c 0 LTONLY2\nX1 c 0 ltdiv\n.op\n";
        let d = Deck::parse(net, None).unwrap();
        let built = d.build(&HashMap::new()).map(|_| ());
        let m = d.model("ltonly2", &["npn"]);
        let missing = Deck::parse("t\nX1 a 0 secretpart\nD1 a 0 nosuchdiode\n", None).unwrap();
        let (sub_err, model_err) = (missing.build(&HashMap::new()).err().unwrap_or_default(), missing.model("nosuchdiode", &["d"]).err().unwrap_or_default());
        let missing_enc = Deck::parse("t\n.lib secret.lib\nX1 a 0 secretpart\n", None).unwrap().build(&HashMap::new()).err().unwrap_or_default();
        set_ltspice_lib(None);
        std::fs::remove_dir_all(&lib).ok();

        built.unwrap();
        let m = m.unwrap();
        assert_eq!((m.params["bf"].as_str(), m.params["vaf"].as_str()), ("123", "50"));
        assert!(d.subckts.contains_key("ltdiv"), "{:?}", d.warnings);
        assert!(d.warnings.iter().any(|w| w.contains("'secret.lib' is vendor-encrypted")), "{:?}", d.warnings);
        assert!(sub_err.contains("unknown subcircuit 'secretpart'") && !sub_err.contains("encrypted"), "{sub_err}");
        assert!(model_err.contains("unknown model 'nosuchdiode'") && model_err.contains("swiftsim-ltlib"), "{model_err}");
        assert!(missing_enc.contains("'secret.lib', which is encrypted"), "{missing_enc}");
    }
}
