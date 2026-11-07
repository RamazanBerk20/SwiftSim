//! `.meas` statements and expression traces (e.g. `V(out)-V(in)`) over results.

use crate::expr::{Env, Expr};
use crate::sim::{Plot, PlotKind};
use crate::solver::C64;
use std::collections::HashMap;

struct PEnv<'a> {
    p: &'a Plot,
    k: usize,
    vars: &'a dyn Fn(&str) -> Option<f64>,
}

impl Env for PEnv<'_> {
    fn x(&self, i: usize) -> f64 {
        let re = self.p.re[i][self.k];
        if self.p.kind == PlotKind::Ac { re.hypot(self.p.im[i][self.k]) } else { re }
    }
    fn time(&self) -> f64 {
        self.p.x[self.k]
    }
    fn var(&self, n: &str) -> Option<f64> {
        match n {
            "freq" | "frequency" | "x" => Some(self.p.x[self.k]),
            _ => (self.vars)(n),
        }
    }
}

fn bind(p: &Plot, e: &Expr) -> Result<Expr, String> {
    e.bind(&|s| p.index(s).ok_or(format!("unknown signal '{}'", s.to_uppercase())))
}

/// Evaluate a real expression for every point of the plot (AC: magnitudes).
pub fn eval_trace(p: &Plot, src: &str) -> Result<Vec<f64>, String> {
    let e = bind(p, &Expr::parse(src)?)?;
    (0..p.x.len()).map(|k| e.eval(&PEnv { p, k, vars: &|_| None })).collect()
}

/// Evaluate a complex expression for an AC plot.
pub fn eval_trace_c(p: &Plot, src: &str) -> Result<Vec<C64>, String> {
    let e = bind(p, &Expr::parse(src)?)?;
    (0..p.x.len()).map(|k| ceval(&e, p, k)).collect()
}

fn ceval(e: &Expr, p: &Plot, k: usize) -> Result<C64, String> {
    Ok(match e {
        Expr::Num(v) => C64::from(*v),
        Expr::X(i) => C64::new(p.re[*i][k], p.im[*i][k]),
        Expr::Neg(a) => -ceval(a, p, k)?,
        Expr::Bin(op, a, b) => {
            let (x, y) = (ceval(a, p, k)?, ceval(b, p, k)?);
            use crate::expr::Op::*;
            match op {
                Add => x + y,
                Sub => x - y,
                Mul => x * y,
                Div => x / y,
                _ => return Err("only + - * / are supported in AC expressions".into()),
            }
        }
        Expr::Call(f, a) if a.len() == 1 => {
            let z = ceval(&a[0], p, k)?;
            match f.as_str() {
                "mag" | "abs" => C64::from(z.norm()),
                "db" => C64::from(20.0 * z.norm().log10()),
                "ph" | "phase" | "arg" => C64::from(z.arg_deg()),
                "re" | "real" => C64::from(z.re),
                "im" | "imag" => C64::from(z.im),
                _ => return Err(format!("{f}() is not supported in AC expressions")),
            }
        }
        _ => return Err("unsupported AC expression".into()),
    })
}

fn interp(xs: &[f64], ys: &[f64], x: f64) -> f64 {
    if xs.is_empty() {
        return f64::NAN;
    }
    let i = xs.partition_point(|&v| v < x);
    if i == 0 {
        return ys[0];
    }
    if i >= xs.len() {
        return ys[xs.len() - 1];
    }
    let (x0, x1) = (xs[i - 1], xs[i]);
    if x1 == x0 { ys[i] } else { ys[i - 1] + (ys[i] - ys[i - 1]) * (x - x0) / (x1 - x0) }
}

#[derive(Clone, Copy)]
enum Count {
    N(usize),
    Last,
}

/// x where f crosses zero; dir 1 rising, -1 falling, 0 either.
fn crossing(xs: &[f64], f: &[f64], td: f64, dir: i32, cnt: Count) -> Option<f64> {
    let mut c = 0;
    let mut last = None;
    for k in 1..xs.len() {
        if xs[k] < td {
            continue;
        }
        let (a, b) = (f[k - 1], f[k]);
        let rise = a < 0.0 && b >= 0.0;
        let fall = a > 0.0 && b <= 0.0;
        if (dir >= 0 && rise) || (dir <= 0 && fall) {
            let x = if b == a { xs[k] } else { xs[k - 1] + (xs[k] - xs[k - 1]) * (-a) / (b - a) };
            c += 1;
            last = Some(x);
            if let Count::N(n) = cnt {
                if c == n {
                    return last;
                }
            }
        }
    }
    if matches!(cnt, Count::Last) { last } else { None }
}

/// Top-level `=` that is not part of `==`, `<=`, `>=`, `!=`.
fn split_eq(s: &str) -> Option<(&str, &str)> {
    let b = s.as_bytes();
    let mut depth = 0;
    for i in 0..b.len() {
        match b[i] {
            b'(' | b'{' => depth += 1,
            b')' | b'}' => depth -= 1,
            b'=' if depth == 0 => {
                let prev = if i > 0 { b[i - 1] } else { b' ' };
                let next = b.get(i + 1).copied().unwrap_or(b' ');
                if !matches!(prev, b'=' | b'<' | b'>' | b'!') && next != b'=' {
                    return Some((&s[..i], &s[i + 1..]));
                }
            }
            _ => {}
        }
    }
    None
}

const OPTS: [&str; 8] = ["from", "to", "at", "td", "val", "rise", "fall", "cross"];

struct Part {
    expr: String,
    opts: HashMap<String, String>,
}

fn part(words: &[&str]) -> Part {
    let mut expr = vec![];
    let mut opts = HashMap::new();
    for w in words {
        match w.split_once('=') {
            Some((k, v)) if OPTS.contains(&k) => {
                opts.insert(k.to_string(), v.to_string());
            }
            _ => expr.push(*w),
        }
    }
    Part { expr: expr.join(" "), opts }
}

/// Evaluate one `.meas` line on one plot. Returns None if the measurement
/// targets another analysis type.
pub fn measure(line: &str, p: &Plot, prior: &[(String, String, f64)], global: &HashMap<String, f64>) -> Result<Option<(String, f64)>, String> {
    let mut words: Vec<&str> = line.split_whitespace().skip(1).collect();
    let kinds = [("tran", PlotKind::Tran), ("ac", PlotKind::Ac), ("dc", PlotKind::Dc), ("op", PlotKind::Op), ("noise", PlotKind::Noise)];
    if let Some(&(_, k)) = kinds.iter().find(|(n, _)| words.first() == Some(n)) {
        if k != p.kind {
            return Ok(None);
        }
        words.remove(0);
    }
    let name = words.first().ok_or("missing measurement name")?.to_string();
    // glue "key = value" into "key=value"
    let joined = words[1..].join(" ").replace(" =", "=").replace("= ", "=");
    let words: Vec<&str> = joined.split_whitespace().collect();
    let vars = |n: &str| -> Option<f64> {
        prior.iter().rev().find(|(s, m, _)| *s == p.step && m == n).map(|t| t.2).or_else(|| global.get(n).copied())
    };
    let num = |s: &str| -> Result<f64, String> { Expr::parse(s)?.eval(&PEnv { p, k: 0, vars: &vars }) };
    let series = |src: &str| -> Result<Vec<f64>, String> {
        let e = bind(p, &Expr::parse(src)?)?;
        (0..p.x.len()).map(|k| e.eval(&PEnv { p, k, vars: &vars })).collect()
    };
    let xs = &p.x;
    let kind = *words.first().ok_or("missing measurement type")?;
    let cnt = |o: &HashMap<String, String>| -> Result<(i32, Count), String> {
        for (k, d) in [("rise", 1), ("fall", -1), ("cross", 0)] {
            if let Some(v) = o.get(k) {
                let c = if v == "last" { Count::Last } else { Count::N(num(v)?.max(1.0) as usize) };
                return Ok((d, c));
            }
        }
        Ok((0, Count::N(1)))
    };
    // condition "lhs=rhs" (or lhs with val=)
    let cond = |pt: &Part| -> Result<(Vec<f64>, f64), String> {
        let (l, r) = match split_eq(&pt.expr) {
            Some((l, r)) => (l.to_string(), r.to_string()),
            None => (pt.expr.clone(), pt.opts.get("val").cloned().ok_or("missing VAL=")?),
        };
        let fl = series(&l)?;
        let fr = series(&r)?;
        Ok((fl.iter().zip(&fr).map(|(a, b)| a - b).collect(), 0.0))
    };
    let td = |pt: &Part| pt.opts.get("td").map_or(Ok(f64::NEG_INFINITY), |v| num(v));
    let v = match kind {
        "avg" | "max" | "min" | "pp" | "rms" | "integ" | "integral" => {
            let pt = part(&words[1..]);
            let ys = series(&pt.expr)?;
            let from = pt.opts.get("from").map_or(Ok(f64::NEG_INFINITY), |v| num(v))?;
            let to = pt.opts.get("to").map_or(Ok(f64::INFINITY), |v| num(v))?;
            let mut w: Vec<(f64, f64)> = vec![];
            if from > xs[0] && from < *xs.last().unwrap() {
                w.push((from, interp(xs, &ys, from)));
            }
            w.extend(xs.iter().zip(&ys).filter(|(x, _)| **x >= from && **x <= to).map(|(x, y)| (*x, *y)));
            if to < *xs.last().unwrap() && to > xs[0] {
                w.push((to, interp(xs, &ys, to)));
            }
            if w.is_empty() {
                return Err(format!("{name}: empty FROM/TO window"));
            }
            let integ = |f: &dyn Fn(f64) -> f64| w.windows(2).map(|s| (s[1].0 - s[0].0) * (f(s[0].1) + f(s[1].1)) / 2.0).sum::<f64>();
            let span = w.last().unwrap().0 - w[0].0;
            let mx = w.iter().map(|q| q.1).fold(f64::NEG_INFINITY, f64::max);
            let mn = w.iter().map(|q| q.1).fold(f64::INFINITY, f64::min);
            match kind {
                "max" => mx,
                "min" => mn,
                "pp" => mx - mn,
                "integ" | "integral" => integ(&|y| y),
                "avg" => if span > 0.0 { integ(&|y| y) / span } else { w[0].1 },
                _ => if span > 0.0 { (integ(&|y| y * y) / span).sqrt() } else { w[0].1.abs() },
            }
        }
        "find" => {
            let rest = &words[1..];
            let wi = rest.iter().position(|w| *w == "when");
            let pt = part(&rest[..wi.unwrap_or(rest.len())]);
            let ys = series(&pt.expr)?;
            if let Some(wi) = wi {
                let c = part(&rest[wi + 1..]);
                let (f, _) = cond(&c)?;
                let (dir, n) = cnt(&c.opts)?;
                let x = crossing(xs, &f, td(&c)?, dir, n).ok_or(format!("{name}: condition never met"))?;
                interp(xs, &ys, x)
            } else if let Some(at) = pt.opts.get("at") {
                interp(xs, &ys, num(at)?)
            } else {
                ys[0]
            }
        }
        "when" => {
            let c = part(&words[1..]);
            let (f, _) = cond(&c)?;
            let (dir, n) = cnt(&c.opts)?;
            crossing(xs, &f, td(&c)?, dir, n).ok_or(format!("{name}: condition never met"))?
        }
        "trig" => {
            let ti = words.iter().position(|w| *w == "targ").ok_or("TRIG needs TARG")?;
            let (a, b) = (part(&words[1..ti]), part(&words[ti + 1..]));
            let (fa, _) = cond(&a)?;
            let (fb, _) = cond(&b)?;
            let (da, na) = cnt(&a.opts)?;
            let (db, nb) = cnt(&b.opts)?;
            let xa = crossing(xs, &fa, td(&a)?, da, na).ok_or(format!("{name}: TRIG never met"))?;
            let xb = crossing(xs, &fb, td(&b)?, db, nb).ok_or(format!("{name}: TARG never met"))?;
            xb - xa
        }
        "param" => num(&words[1..].join(" "))?,
        other => return Err(format!("'{other}' measurements are not supported")),
    };
    Ok(Some((name, v)))
}

/// `.four f0 [Nharmonics [Nperiods]] trace…` on a transient plot: Fourier
/// components over the last `Nperiods` periods (default 9 harmonics, 1 period),
/// as Log lines. Phases are sine-referenced at t = 0, as in SPICE.
pub fn fourier(line: &str, p: &Plot, global: &HashMap<String, f64>) -> Result<Vec<String>, String> {
    let words: Vec<&str> = line.split_whitespace().skip(1).collect();
    let split = words.iter().position(|w| w.contains('(')).unwrap_or(words.len());
    let (args, traces) = words.split_at(split);
    if args.is_empty() || args.len() > 3 || traces.is_empty() {
        return Err("expected .four <freq> [Nharmonics] [Nperiods] <trace> …".into());
    }
    let vars = |n: &str| global.get(n).copied();
    let a = args
        .iter()
        .map(|s| Expr::parse(s.trim_matches(['{', '}']))?.eval(&PEnv { p, k: 0, vars: &vars }))
        .collect::<Result<Vec<f64>, String>>()?;
    let f0 = a[0];
    let nharm = a.get(1).map_or(9, |&n| n.max(1.0) as usize);
    let nper = a.get(2).copied().unwrap_or(1.0);
    if !(f0 > 0.0 && nper > 0.0) {
        return Err("frequency and Nperiods must be positive".into());
    }
    let (Some(&t1), Some(&tfirst)) = (p.x.last(), p.x.first()) else { return Err("empty transient".into()) };
    let span = nper / f0;
    let t0 = t1 - span;
    if t0 < tfirst - 1e-9 * span {
        return Err(format!("the saved transient is shorter than {nper} period(s) of {}Hz", crate::expr::fmt_si(f0)));
    }
    let step = if p.step.is_empty() { String::new() } else { format!("   [{}]", p.step) };
    let mut out = vec![];
    for tr in traces {
        let y = eval_trace(p, tr)?;
        // shortcut: linear interpolation onto a uniform grid, so accuracy follows the transient time step
        const N: usize = 8192;
        let w = 2.0 * std::f64::consts::PI * f0;
        let (mut dc, mut cs, mut sn) = (0.0, vec![0.0; nharm + 1], vec![0.0; nharm + 1]);
        for i in 0..N {
            let t = t0 + span * i as f64 / N as f64;
            let v = interp(&p.x, &y, t);
            dc += v;
            for h in 1..=nharm {
                let (s, c) = (w * h as f64 * t).sin_cos();
                cs[h] += v * c;
                sn[h] += v * s;
            }
        }
        let k = 2.0 / N as f64;
        let mag: Vec<f64> = (0..=nharm).map(|h| k * cs[h].hypot(sn[h])).collect();
        let ph: Vec<f64> = (0..=nharm).map(|h| cs[h].atan2(sn[h]).to_degrees()).collect();
        out.push(format!("Fourier components of {tr}{step}"));
        out.push(format!("DC component: {:.6e}", dc / N as f64));
        out.push("Harmonic   Frequency     Fourier      Normalized    Phase      Normalized".into());
        out.push(" Number       [Hz]       Component    Component    [degree]    Phase [deg]".into());
        for h in 1..=nharm {
            out.push(format!(
                "{h:>5}      {:.4e}   {:.5e}   {:.5e}   {:>8.2}°   {:>8.2}°",
                f0 * h as f64,
                mag[h],
                mag[h] / mag[1],
                ph[h],
                ph[h] - ph[1]
            ));
        }
        let thd = mag[2..].iter().map(|m| m * m).sum::<f64>().sqrt() / mag[1] * 100.0;
        out.push(format!("Total Harmonic Distortion: {thd:.6}%"));
    }
    Ok(out)
}
