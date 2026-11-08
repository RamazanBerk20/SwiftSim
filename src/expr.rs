//! SPICE numbers (with SI suffixes) and an expression language shared by
//! `.param`, `{...}` values, behavioral sources, `.meas` and plot traces.

/// Comparison key of a node or signal name: two names are the same net when their keys are equal.
/// Independent of the locale, so a file means the same on every machine: normalized (NFC, so a
/// decomposed `ç` equals a composed one), case-folded, and Turkish `ı` and `İ` count as `i`, because
/// SPICE needs `I` to equal `i` while Turkish pairs `I` with `ı`.
pub fn fold(s: &str) -> String {
    if s.is_ascii() {
        return s.to_ascii_lowercase();
    }
    normalized(s).to_lowercase().replace("i\u{307}", "i").replace('ı', "i").replace('ς', "σ").replace('ß', "ss")
}

/// NFC, without the variation selectors some keyboards add to emoji (❤ and ❤️ are one name).
fn normalized(s: &str) -> String {
    icu_normalizer::ComposingNormalizerBorrowed::new_nfc().normalize(s).replace(['\u{FE0E}', '\u{FE0F}'], "")
}

/// True when two spellings with the same `fold` key look like different words, such as `kır` and
/// `kir`: they differ in the plain lowercase and in the Turkish one. Case variants never count
/// (`KIR` is `kır` or `kir`, depending on who wrote it).
pub fn distinct_spellings(a: &str, b: &str) -> bool {
    let plain = |s: &str| normalized(s).to_lowercase();
    let turkish = |s: &str| normalized(s).replace('I', "ı").replace('İ', "i").to_lowercase();
    plain(a) != plain(b) && turkish(a) != turkish(b)
}

/// Parse a SPICE number: `1k`, `4.7u`, `1meg`, `10uF`, `4k7`, `1e-3`.
/// Trailing unit letters are ignored, as in SPICE.
pub fn parse_number(s: &str) -> Option<f64> {
    let (v, n) = parse_number_prefix(s)?;
    (n == s.len()).then_some(v)
}

/// Parse a number at the start of `s`; returns value and bytes consumed
/// (including suffix and unit letters).
pub fn parse_number_prefix(s: &str) -> Option<(f64, usize)> {
    let b = s.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let d0 = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let mut plain = true;
    if i < b.len() && b[i] == b'.' {
        plain = false;
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
    }
    let digits = b[d0..i].iter().filter(|c| c.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        if j < b.len() && b[j].is_ascii_digit() {
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            i = j;
            plain = false;
        }
    }
    let mantissa = &s[..i];
    let mut val: f64 = mantissa.parse().ok()?;
    let rest = &s[i..];
    let lower = rest.to_ascii_lowercase();
    let (mult, slen) = if lower.starts_with("meg") {
        (1e6, 3)
    } else if lower.starts_with("mil") {
        (25.4e-6, 3)
    } else {
        match lower.chars().next() {
            Some('t') => (1e12, 1),
            Some('g') => (1e9, 1),
            Some('k') => (1e3, 1),
            Some('m') => (1e-3, 1),
            Some('u') => (1e-6, 1),
            Some('µ') => (1e-6, 'µ'.len_utf8()),
            Some('n') => (1e-9, 1),
            Some('p') => (1e-12, 1),
            Some('f') => (1e-15, 1),
            _ => (1.0, 0),
        }
    };
    let mut j = i + slen;
    if slen > 0 && plain {
        // "4k7" == 4.7k
        let fstart = j;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if j > fstart {
            val = format!("{}.{}", mantissa, &s[fstart..j]).parse().ok()?;
        }
    }
    // swallow unit letters (V, A, F, H, Hz, ohm...)
    while j < b.len() && (b[j].is_ascii_alphabetic()) {
        j += 1;
    }
    Some((val * mult, j))
}

/// Format a value with an SI suffix, e.g. 4700 -> "4.7k".
pub fn fmt_si(v: f64) -> String {
    if v == 0.0 || !v.is_finite() {
        return format!("{v}");
    }
    let a = v.abs();
    let (m, s) = [
        (1e12, "T"),
        (1e9, "G"),
        (1e6, "M"),
        (1e3, "k"),
        (1.0, ""),
        (1e-3, "m"),
        (1e-6, "µ"),
        (1e-9, "n"),
        (1e-12, "p"),
        (1e-15, "f"),
    ]
    .into_iter()
    .find(|(m, _)| a >= *m * 0.9999)
    .unwrap_or((1e-15, "f"));
    let x = v / m;
    let t = format!("{:.4}", x);
    let t = t.trim_end_matches('0').trim_end_matches('.');
    format!("{t}{s}")
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Op {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
    Ne,
    And,
    Or,
}

#[derive(Clone, Debug)]
pub enum Expr {
    Num(f64),
    Time,
    /// Parameter / measurement name.
    Var(String),
    /// Simulation signal, canonical lowercase: `v(out)`, `i(r1)`, `ic(q1)`.
    Sig(String),
    /// Resolved unknown index (behavioral sources).
    X(usize),
    Neg(Box<Expr>),
    Not(Box<Expr>),
    Bin(Op, Box<Expr>, Box<Expr>),
    Call(String, Vec<Expr>),
    Cond(Box<Expr>, Box<Expr>, Box<Expr>),
}

/// User function from `.func name(args) {body}`.
#[derive(Clone, Debug)]
pub struct Func {
    pub args: Vec<String>,
    pub body: Expr,
}

pub type Funcs = std::collections::HashMap<String, Func>;

/// Parse `name(a, b) {body}` (the text after `.func`; `=` before the body is optional).
pub fn parse_func(s: &str) -> Result<(String, Func), String> {
    let (name, r) = s.split_once('(').ok_or_else(|| format!(".func '{s}': expected name(args)"))?;
    let (args, body) = r.split_once(')').ok_or_else(|| format!(".func '{s}': missing ')'"))?;
    let args = args.split(',').map(|a| a.trim().to_ascii_lowercase()).filter(|a| !a.is_empty()).collect();
    let body = body.trim().trim_start_matches('=').trim().trim_matches('\'');
    Ok((name.trim().to_ascii_lowercase(), Func { args, body: Expr::parse(body)? }))
}

/// Values an expression may look up while evaluating.
pub trait Env {
    fn x(&self, _i: usize) -> f64 {
        0.0
    }
    fn sig(&self, _name: &str) -> Option<f64> {
        None
    }
    fn var(&self, _name: &str) -> Option<f64> {
        None
    }
    fn time(&self) -> f64 {
        0.0
    }
}

/// Env with only named variables.
pub struct VarEnv<'a>(pub &'a dyn Fn(&str) -> Option<f64>);
impl Env for VarEnv<'_> {
    fn var(&self, n: &str) -> Option<f64> {
        (self.0)(n)
    }
}

impl Expr {
    pub fn parse(s: &str) -> Result<Expr, String> {
        let mut p = Parser { s: s.as_bytes(), src: s, i: 0 };
        let e = p.ternary()?;
        p.ws();
        if p.i != p.s.len() {
            return Err(format!("unexpected '{}' in expression '{}'", &s[p.i..], s));
        }
        Ok(e)
    }

    pub fn eval(&self, env: &dyn Env) -> Result<f64, String> {
        Ok(match self {
            Expr::Num(v) => *v,
            Expr::Time => env.time(),
            Expr::X(i) => env.x(*i),
            Expr::Var(n) => env.var(n).ok_or_else(|| format!("unknown parameter '{n}'"))?,
            Expr::Sig(n) => env.sig(n).ok_or_else(|| format!("unknown signal '{n}'"))?,
            Expr::Neg(a) => -a.eval(env)?,
            Expr::Not(a) => b2f(a.eval(env)? == 0.0),
            Expr::Bin(op, a, b) => {
                let x = a.eval(env)?;
                let y = b.eval(env)?;
                match op {
                    Op::Add => x + y,
                    Op::Sub => x - y,
                    Op::Mul => x * y,
                    Op::Div => x / y,
                    Op::Pow => x.powf(y),
                    Op::Lt => b2f(x < y),
                    Op::Gt => b2f(x > y),
                    Op::Le => b2f(x <= y),
                    Op::Ge => b2f(x >= y),
                    Op::Eq => b2f(x == y),
                    Op::Ne => b2f(x != y),
                    Op::And => b2f(x != 0.0 && y != 0.0),
                    Op::Or => b2f(x != 0.0 || y != 0.0),
                }
            }
            Expr::Cond(c, a, b) => {
                if c.eval(env)? != 0.0 {
                    a.eval(env)?
                } else {
                    b.eval(env)?
                }
            }
            Expr::Call(f, args) => {
                if f == "if" && args.len() == 3 {
                    return if args[0].eval(env)? != 0.0 { args[1].eval(env) } else { args[2].eval(env) };
                }
                let v: Vec<f64> = args.iter().map(|a| a.eval(env)).collect::<Result<_, _>>()?;
                call(f, &v)?
            }
        })
    }

    /// Replace parameter names with constants and `time` stays symbolic.
    /// Signals are passed to `sig` for renaming (subcircuit node mapping).
    pub fn resolve(&self, var: &dyn Fn(&str) -> Option<f64>, sig: &dyn Fn(&str) -> String) -> Expr {
        let r = |e: &Expr| Box::new(e.resolve(var, sig));
        match self {
            Expr::Var(n) => match var(n) {
                Some(v) => Expr::Num(v),
                None => self.clone(),
            },
            Expr::Sig(n) => Expr::Sig(sig(n)),
            Expr::Neg(a) => Expr::Neg(r(a)),
            Expr::Not(a) => Expr::Not(r(a)),
            Expr::Bin(o, a, b) => Expr::Bin(*o, r(a), r(b)),
            Expr::Cond(c, a, b) => Expr::Cond(r(c), r(a), r(b)),
            Expr::Call(f, a) => Expr::Call(f.clone(), a.iter().map(|e| e.resolve(var, sig)).collect()),
            _ => self.clone(),
        }
    }

    /// Rebuild with `f` applied to each direct child.
    fn map(&self, f: &mut dyn FnMut(&Expr) -> Result<Expr, String>) -> Result<Expr, String> {
        Ok(match self {
            Expr::Neg(a) => Expr::Neg(Box::new(f(a)?)),
            Expr::Not(a) => Expr::Not(Box::new(f(a)?)),
            Expr::Bin(o, a, b) => Expr::Bin(*o, Box::new(f(a)?), Box::new(f(b)?)),
            Expr::Cond(c, a, b) => Expr::Cond(Box::new(f(c)?), Box::new(f(a)?), Box::new(f(b)?)),
            Expr::Call(n, a) => Expr::Call(n.clone(), a.iter().map(&mut *f).collect::<Result<_, _>>()?),
            _ => self.clone(),
        })
    }

    /// Expand calls of user `.func`s; their arguments shadow parameters.
    pub fn inline(&self, funcs: &Funcs) -> Result<Expr, String> {
        if funcs.is_empty() {
            return Ok(self.clone());
        }
        self.inline_d(funcs, 0)
    }

    fn inline_d(&self, funcs: &Funcs, depth: usize) -> Result<Expr, String> {
        if let Expr::Call(n, a) = self
            && let Some(f) = funcs.get(n)
        {
            if depth > 64 {
                return Err(format!("function '{n}' is recursive"));
            }
            if a.len() != f.args.len() {
                return Err(format!("{n}() takes {} argument(s)", f.args.len()));
            }
            let a: Vec<Expr> = a.iter().map(|e| e.inline_d(funcs, depth)).collect::<Result<_, _>>()?;
            return Ok(f.body.inline_d(funcs, depth + 1)?.subst(&f.args, &a));
        }
        self.map(&mut |e| e.inline_d(funcs, depth))
    }

    fn subst(&self, names: &[String], vals: &[Expr]) -> Expr {
        match self {
            Expr::Var(n) => names.iter().position(|a| a == n).map_or_else(|| self.clone(), |i| vals[i].clone()),
            _ => self.map(&mut |e| Ok(e.subst(names, vals))).expect("subst cannot fail"),
        }
    }

    /// Visit every signal name.
    pub fn signals(&self, out: &mut Vec<String>) {
        match self {
            Expr::Sig(n) => out.push(n.clone()),
            Expr::Neg(a) | Expr::Not(a) => a.signals(out),
            Expr::Bin(_, a, b) => {
                a.signals(out);
                b.signals(out);
            }
            Expr::Cond(c, a, b) => {
                c.signals(out);
                a.signals(out);
                b.signals(out);
            }
            Expr::Call(_, a) => a.iter().for_each(|e| e.signals(out)),
            _ => {}
        }
    }

    /// Map signals to unknown indices.
    pub fn bind(&self, f: &dyn Fn(&str) -> Result<usize, String>) -> Result<Expr, String> {
        Ok(match self {
            Expr::Sig(n) => Expr::X(f(n)?),
            Expr::Neg(a) => Expr::Neg(Box::new(a.bind(f)?)),
            Expr::Not(a) => Expr::Not(Box::new(a.bind(f)?)),
            Expr::Bin(o, a, b) => Expr::Bin(*o, Box::new(a.bind(f)?), Box::new(b.bind(f)?)),
            Expr::Cond(c, a, b) => Expr::Cond(Box::new(c.bind(f)?), Box::new(a.bind(f)?), Box::new(b.bind(f)?)),
            Expr::Call(n, a) => Expr::Call(n.clone(), a.iter().map(|e| e.bind(f)).collect::<Result<_, _>>()?),
            _ => self.clone(),
        })
    }

    pub fn uses_time(&self) -> bool {
        match self {
            Expr::Time => true,
            Expr::Neg(a) | Expr::Not(a) => a.uses_time(),
            Expr::Bin(_, a, b) => a.uses_time() || b.uses_time(),
            Expr::Cond(c, a, b) => c.uses_time() || a.uses_time() || b.uses_time(),
            Expr::Call(_, a) => a.iter().any(|e| e.uses_time()),
            _ => false,
        }
    }
}

fn b2f(b: bool) -> f64 {
    if b { 1.0 } else { 0.0 }
}

fn call(f: &str, a: &[f64]) -> Result<f64, String> {
    let n = a.len();
    let need = |k: usize| -> Result<(), String> {
        if n == k { Ok(()) } else { Err(format!("{f}() takes {k} argument(s)")) }
    };
    Ok(match f {
        "abs" => (need(1)?, a[0].abs()).1,
        "sqrt" => (need(1)?, a[0].sqrt()).1,
        "exp" => (need(1)?, a[0].exp()).1,
        "ln" | "log" => (need(1)?, a[0].ln()).1,
        "log10" => (need(1)?, a[0].log10()).1,
        "sin" => (need(1)?, a[0].sin()).1,
        "cos" => (need(1)?, a[0].cos()).1,
        "tan" => (need(1)?, a[0].tan()).1,
        "asin" | "arcsin" => (need(1)?, a[0].asin()).1,
        "acos" | "arccos" => (need(1)?, a[0].acos()).1,
        "atan" | "arctan" => (need(1)?, a[0].atan()).1,
        "atan2" => (need(2)?, a[0].atan2(a[1])).1,
        "sinh" => (need(1)?, a[0].sinh()).1,
        "cosh" => (need(1)?, a[0].cosh()).1,
        "tanh" => (need(1)?, a[0].tanh()).1,
        "floor" => (need(1)?, a[0].floor()).1,
        "ceil" => (need(1)?, a[0].ceil()).1,
        "round" => (need(1)?, a[0].round()).1,
        "int" => (need(1)?, a[0].trunc()).1,
        "sgn" | "sign" => (need(1)?, if a[0] > 0.0 { 1.0 } else if a[0] < 0.0 { -1.0 } else { 0.0 }).1,
        "u" | "step" => (need(1)?, b2f(a[0] > 0.0)).1,
        "uramp" => (need(1)?, a[0].max(0.0)).1,
        "db" => (need(1)?, 20.0 * a[0].abs().log10()).1,
        "pow" | "pwr" => (need(2)?, if f == "pwr" { a[0].abs().powf(a[1]) } else { a[0].powf(a[1]) }).1,
        "pwrs" => (need(2)?, a[0].signum() * a[0].abs().powf(a[1])).1,
        "min" => (need(2)?, a[0].min(a[1])).1,
        "max" => (need(2)?, a[0].max(a[1])).1,
        "limit" => (need(3)?, a[0].clamp(a[1].min(a[2]), a[1].max(a[2]))).1,
        "if" => (need(3)?, if a[0] != 0.0 { a[1] } else { a[2] }).1,
        // table(x, x1,y1, x2,y2, …): linear interpolation, held flat outside; x ascending
        "table" => {
            if n < 3 || n.is_multiple_of(2) {
                return Err("table() takes x and (x, y) pairs".into());
            }
            let p: Vec<(f64, f64)> = a[1..].chunks(2).map(|c| (c[0], c[1])).collect();
            let i = p.partition_point(|q| q.0 <= a[0]);
            if i == 0 {
                p[0].1
            } else if i == p.len() {
                p[i - 1].1
            } else {
                let ((x0, y0), (x1, y1)) = (p[i - 1], p[i]);
                y0 + (y1 - y0) * (a[0] - x0) / (x1 - x0)
            }
        }
        // Monte Carlo (LTSpice): new random draw on every evaluation, i.e. every .step run
        "rand" => (need(1)?, rnd()).1,
        "flat" => (need(1)?, a[0] * (2.0 * rnd() - 1.0)).1,
        "mc" => (need(2)?, a[0] * (1.0 + a[1] * (2.0 * rnd() - 1.0))).1,
        "gauss" => (need(1)?, a[0] * gauss()).1,
        _ => return Err(format!("unknown function '{f}'")),
    })
}

/// Uniform [0,1) from a per-thread xorshift generator.
fn rnd() -> f64 {
    use std::cell::Cell;
    thread_local!(static S: Cell<u64> = Cell::new(
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0x9E37_79B9, |d| d.as_nanos() as u64) | 1
    ));
    S.with(|s| {
        let mut x = s.get();
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        s.set(x);
        (x >> 11) as f64 / (1u64 << 53) as f64
    })
}

/// Standard normal (Box-Muller).
fn gauss() -> f64 {
    let u = rnd().max(1e-300);
    (-2.0 * u.ln()).sqrt() * (2.0 * std::f64::consts::PI * rnd()).cos()
}

struct Parser<'a> {
    s: &'a [u8],
    src: &'a str,
    i: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }
    fn eat(&mut self, t: &str) -> bool {
        self.ws();
        if self.src[self.i..].starts_with(t) {
            self.i += t.len();
            true
        } else {
            false
        }
    }
    fn ternary(&mut self) -> Result<Expr, String> {
        let c = self.or()?;
        if self.eat("?") {
            let a = self.ternary()?;
            if !self.eat(":") {
                return Err("expected ':' in ?: expression".into());
            }
            let b = self.ternary()?;
            return Ok(Expr::Cond(Box::new(c), Box::new(a), Box::new(b)));
        }
        Ok(c)
    }
    fn or(&mut self) -> Result<Expr, String> {
        let mut l = self.and()?;
        while self.eat("||") {
            l = Expr::Bin(Op::Or, Box::new(l), Box::new(self.and()?));
        }
        Ok(l)
    }
    fn and(&mut self) -> Result<Expr, String> {
        let mut l = self.eq()?;
        while self.eat("&&") {
            l = Expr::Bin(Op::And, Box::new(l), Box::new(self.eq()?));
        }
        Ok(l)
    }
    fn eq(&mut self) -> Result<Expr, String> {
        let mut l = self.cmp()?;
        loop {
            let op = if self.eat("==") {
                Op::Eq
            } else if self.eat("!=") {
                Op::Ne
            } else {
                return Ok(l);
            };
            l = Expr::Bin(op, Box::new(l), Box::new(self.cmp()?));
        }
    }
    fn cmp(&mut self) -> Result<Expr, String> {
        let mut l = self.add()?;
        loop {
            let op = if self.eat("<=") {
                Op::Le
            } else if self.eat(">=") {
                Op::Ge
            } else if self.eat("<") {
                Op::Lt
            } else if self.eat(">") {
                Op::Gt
            } else {
                return Ok(l);
            };
            l = Expr::Bin(op, Box::new(l), Box::new(self.add()?));
        }
    }
    fn add(&mut self) -> Result<Expr, String> {
        let mut l = self.mul()?;
        loop {
            let op = if self.eat("+") {
                Op::Add
            } else if self.eat("-") {
                Op::Sub
            } else {
                return Ok(l);
            };
            l = Expr::Bin(op, Box::new(l), Box::new(self.mul()?));
        }
    }
    fn mul(&mut self) -> Result<Expr, String> {
        let mut l = self.unary()?;
        loop {
            self.ws();
            if self.src[self.i..].starts_with("**") {
                return Ok(l); // handled in pow
            }
            let op = if self.eat("*") {
                Op::Mul
            } else if self.eat("/") {
                Op::Div
            } else {
                return Ok(l);
            };
            l = Expr::Bin(op, Box::new(l), Box::new(self.unary()?));
        }
    }
    fn unary(&mut self) -> Result<Expr, String> {
        if self.eat("-") {
            return Ok(Expr::Neg(Box::new(self.unary()?)));
        }
        if self.eat("+") {
            return self.unary();
        }
        if self.eat("!") {
            return Ok(Expr::Not(Box::new(self.unary()?)));
        }
        self.pow()
    }
    fn pow(&mut self) -> Result<Expr, String> {
        let b = self.atom()?;
        if self.eat("**") || self.eat("^") {
            let e = self.unary()?; // right associative
            return Ok(Expr::Bin(Op::Pow, Box::new(b), Box::new(e)));
        }
        Ok(b)
    }
    fn atom(&mut self) -> Result<Expr, String> {
        self.ws();
        if self.i >= self.s.len() {
            return Err(format!("unexpected end of expression '{}'", self.src));
        }
        let c = self.s[self.i];
        if c == b'(' || c == b'{' {
            self.i += 1;
            let e = self.ternary()?;
            if !(self.eat(")") || self.eat("}")) {
                return Err(format!("missing ')' in '{}'", self.src));
            }
            return Ok(e);
        }
        if c.is_ascii_digit() || c == b'.' {
            let (v, n) = parse_number_prefix(&self.src[self.i..])
                .ok_or_else(|| format!("bad number in '{}'", self.src))?;
            self.i += n;
            return Ok(Expr::Num(v));
        }
        if c.is_ascii_alphabetic() || c == b'_' {
            let st = self.i;
            while self.i < self.s.len() && (self.s[self.i].is_ascii_alphanumeric() || self.s[self.i] == b'_') {
                self.i += 1;
            }
            let name = self.src[st..self.i].to_ascii_lowercase();
            self.ws();
            let call = self.i < self.s.len() && self.s[self.i] == b'(';
            if call && is_signal_fn(&name) {
                // raw node / device names: may be numeric or contain '.' ':'
                let st = self.i + 1;
                let end = self.src[st..].find(')').ok_or("missing ')'")? + st;
                let inner = fold(&self.src[st..end]);
                self.i = end + 1;
                let parts: Vec<&str> = inner.split(',').map(|p| p.trim()).collect();
                if name == "v" {
                    // ground is 0 V by definition and is never a stored signal
                    let node = |n: &str| if matches!(n, "0" | "gnd") { Expr::Num(0.0) } else { Expr::Sig(format!("v({n})")) };
                    let a = node(parts[0]);
                    return Ok(match parts.get(1) {
                        Some(b) if !b.is_empty() => Expr::Bin(Op::Sub, Box::new(a), Box::new(node(b))),
                        _ => a,
                    });
                }
                return Ok(Expr::Sig(format!("{name}({})", parts[0])));
            }
            if call {
                self.i += 1;
                let mut args = Vec::new();
                if !self.eat(")") {
                    loop {
                        args.push(self.ternary()?);
                        if self.eat(")") {
                            break;
                        }
                        if !self.eat(",") {
                            return Err(format!("expected ',' or ')' in '{}'", self.src));
                        }
                    }
                }
                return Ok(Expr::Call(name, args));
            }
            return Ok(match name.as_str() {
                "time" => Expr::Time,
                "pi" => Expr::Num(std::f64::consts::PI),
                "true" => Expr::Num(1.0),
                "false" => Expr::Num(0.0),
                _ => Expr::Var(name),
            });
        }
        Err(format!("unexpected '{}' in '{}'", c as char, self.src))
    }
}

fn is_signal_fn(n: &str) -> bool {
    matches!(n, "v" | "i" | "ic" | "ib" | "ie" | "id" | "ig" | "is" | "ix")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        let t = |s: &str, v: f64| {
            let p = parse_number(s).unwrap();
            assert!((p - v).abs() <= 1e-12 * v.abs(), "{s}: {p} != {v}");
        };
        t("1k", 1e3);
        t("4.7u", 4.7e-6);
        t("1meg", 1e6);
        t("10uF", 10e-6);
        t("4k7", 4700.0);
        t("1e-3", 1e-3);
        t("2.5E3", 2500.0);
        t("100", 100.0);
        t(".5", 0.5);
        t("-3m", -3e-3);
        t("5V", 5.0);
        assert!(parse_number("abc").is_none());
    }

    #[test]
    fn expressions() {
        let env = VarEnv(&|n| if n == "r" { Some(2e3) } else { None });
        let e = |s: &str| Expr::parse(s).unwrap().eval(&env).unwrap();
        assert_eq!(e("1+2*3"), 7.0);
        assert_eq!(e("2**3**2"), 512.0);
        assert_eq!(e("-2^2"), -4.0);
        assert_eq!(e("r/2k"), 1.0);
        assert_eq!(e("V(0) + V(gnd, 0) + 1"), 1.0);
        assert_eq!(e("max(1, 3) > 2 ? 10 : 20"), 10.0);
        assert_eq!(e("if(1<0, 5, 6)"), 6.0);
        for _ in 0..200 {
            let v = e("mc(100, 0.05)");
            assert!((95.0..=105.0).contains(&v), "{v}");
            assert!(e("flat(2)").abs() <= 2.0);
        }
        let s = Expr::parse("V(a,b)*I(R1)").unwrap();
        let mut v = vec![];
        s.signals(&mut v);
        assert_eq!(v, vec!["v(a)", "v(b)", "i(r1)"]);
    }
}
