//! LTspice `.asc` import. LTspice uses 16 units per grid step and its own pin
//! layout per `.asy` symbol, so each SwiftSim part is placed inside the LTspice
//! footprint and joined to the LTspice pin positions with short stub wires.
//! Other symbols are read from their `.asy` (next to the schematic, else in
//! LTspice's `lib/sym`) and become subcircuit parts with the `.asy`'s pins.

use super::schematic::{Component, Kind, P, Schematic, Text, Wire};
use std::path::{Path, PathBuf};

/// Stock LTspice symbol → (kind, `.asy` PIN positions / 16 in SPICE order,
/// SwiftSim part origin in the same local units, flip the part vertically).
fn symbol(name: &str) -> Option<(Kind, &'static [P], P, bool)> {
    use Kind::*;
    Some(match name {
        "res" => (Resistor, &[[1, 1], [1, 6]], [1, 3], false),
        "ind" | "ind2" => (Inductor, &[[1, 1], [1, 6]], [1, 3], false),
        "cap" | "polcap" => (Capacitor, &[[1, 0], [1, 4]], [1, 2], false),
        "diode" | "zener" | "schottky" | "led" => (Diode, &[[1, 0], [1, 4]], [1, 2], false),
        "voltage" => (Voltage, &[[0, 1], [0, 6]], [0, 3], false),
        "bv" => (Behavioral, &[[0, 1], [0, 6]], [0, 3], false),
        "current" => (Current, &[[0, 0], [0, 5]], [0, 2], false),
        "npn" => (Npn, &[[4, 0], [0, 3], [4, 6]], [3, 3], false),
        "pnp" => (Pnp, &[[4, 0], [0, 3], [4, 6]], [3, 3], false),
        "nmos" => (Nmos, &[[3, 0], [0, 5], [3, 6]], [2, 4], false),
        "pmos" => (Pmos, &[[3, 0], [0, 5], [3, 6]], [2, 4], false),
        "nmos4" => (Nmos4, &[[3, 0], [0, 5], [3, 6], [3, 3]], [2, 3], false),
        "pmos4" => (Pmos4, &[[3, 0], [0, 5], [3, 6], [3, 3]], [2, 3], false),
        "njf" => (Njf, &[[3, 0], [0, 4], [3, 6]], [2, 4], false),
        "pjf" => (Pjf, &[[3, 0], [0, 4], [3, 6]], [2, 4], false),
        "e" => (Vcvs, &[[0, 1], [0, 6], [-3, 2], [-3, 5]], [0, 3], false),
        // g's output pins run bottom-to-top; unflipped, the stubs would short them
        "g" => (Vccs, &[[0, 6], [0, 1], [-3, 2], [-3, 5]], [0, 3], true),
        "sw" => (Switch, &[[0, 1], [0, 6], [-3, 5], [-3, 2]], [0, 3], false),
        // LTspice's opamp has − on top; SwiftSim's has + on top, so flip it onto LTspice's pins
        "opamp" => (OpAmp, &[[-2, 5], [-2, 3], [2, 4]], [0, 4], true),
        _ => return None,
    })
}

/// Clockwise quarter turns of an LTspice orientation (`R0`…`R270`, `M0`…`M270`).
fn quarters(o: &str) -> i32 {
    o.get(1..).and_then(|s| s.parse::<i32>().ok()).unwrap_or(0).div_euclid(90).rem_euclid(4)
}

/// LTspice orientation: rotate clockwise on screen, then for `M` mirror x.
fn orient(o: &str, [mut x, mut y]: P) -> P {
    for _ in 0..quarters(o) {
        (x, y) = (-y, x);
    }
    if o.starts_with('M') {
        x = -x;
    }
    [x, y]
}

fn grid(v: i32) -> i32 {
    (v + 8).div_euclid(16)
}

/// The text after the first `n` whitespace-separated words, spacing kept.
fn rest(line: &str, n: usize) -> &str {
    let mut r = line.trim_start();
    for _ in 0..n {
        r = r.find(char::is_whitespace).map_or("", |i| &r[i..]).trim_start();
    }
    r
}

fn join<'a>(v: impl IntoIterator<Item = &'a String>) -> String {
    v.into_iter().filter(|v| !v.is_empty()).cloned().collect::<Vec<_>>().join(" ")
}

#[derive(Default)]
struct Attrs {
    /// Value, Value2, SpiceLine, SpiceLine2 — joined in this order like LTspice's netlister.
    vals: [String; 4],
    model: String,
    file: String,
}

impl Attrs {
    fn set(&mut self, key: &str, v: String) {
        match key {
            "Value" => self.vals[0] = v,
            "Value2" => self.vals[1] = v,
            "SpiceLine" => self.vals[2] = v,
            "SpiceLine2" => self.vals[3] = v,
            "SpiceModel" => self.model = v,
            "ModelFile" => self.file = v,
            _ => {}
        }
    }

    /// LTspice's netlisting of an `X` symbol: (library to `.lib`, words after the nodes).
    fn subckt(&self) -> (&str, String) {
        let [v, v2, ..] = &self.vals;
        if self.file.is_empty() && !v.is_empty() && !v2.is_empty() {
            // "auto-load" symbol: SpiceModel names the library, Value2 the subcircuit, Value is only shown
            return (&self.model, v2.clone());
        }
        (&self.file, join([&self.model].into_iter().chain(&self.vals)))
    }
}

/// A non-stock symbol: pin positions (LTspice units, SPICE order) and default attributes.
/// Only subcircuit symbols (`Prefix X`) are taken.
fn read_asy(text: &str) -> Option<(Vec<P>, Attrs)> {
    let (mut pins, mut a, mut prefix) = (vec![], Attrs::default(), String::new());
    for line in text.lines() {
        let w: Vec<&str> = line.split_whitespace().collect();
        let num = |i: usize| w.get(i).and_then(|v| v.parse::<i32>().ok()).unwrap_or(0);
        match (w.first().copied(), w.get(1).copied()) {
            // pins without a SpiceOrder keep their file order, after the numbered ones
            (Some("PIN"), _) => pins.push((i32::MAX, [num(1), num(2)])),
            (Some("PINATTR"), Some("SpiceOrder")) => {
                if let Some(p) = pins.last_mut() {
                    p.0 = num(2);
                }
            }
            (Some("SYMATTR"), Some("Prefix")) => prefix = rest(line, 2).to_string(),
            (Some("SYMATTR"), Some(k)) => a.set(k, rest(line, 2).to_string()),
            _ => {}
        }
    }
    pins.sort_by_key(|p| p.0);
    prefix.eq_ignore_ascii_case("x").then(|| (pins.into_iter().map(|p| p.1).collect(), a))
}

/// `p`, or the existing path that differs from it only in letter case
/// (LTspice files come from case-insensitive Windows file systems).
fn existing(p: &Path) -> Option<PathBuf> {
    if p.exists() {
        return Some(p.to_path_buf());
    }
    let (dir, name) = (existing(p.parent()?)?, p.file_name()?);
    std::fs::read_dir(&dir).ok()?.flatten().map(|e| e.path()).find(|e| e.file_name().is_some_and(|n| n.eq_ignore_ascii_case(name)))
}

/// The `.asy` of `name` as written in the `.asc` (`\` separates folders): next to
/// the schematic by its file name, else under LTspice's `lib/sym`.
fn find_asy(name: &str, dir: Option<&Path>) -> Option<String> {
    let parts: Vec<&str> = name.split('\\').filter(|s| !s.is_empty()).collect();
    let (file, folders) = parts.split_last()?;
    let file = format!("{file}.asy");
    let lib = swiftsim::netlist::ltspice_lib().map(|l| folders.iter().fold(l.join("sym"), |p, f| p.join(f)).join(&file));
    dir.map(|d| d.join(&file)).into_iter().chain(lib).find_map(|p| std::fs::read(existing(&p)?).ok()).map(|b| decode(&b))
}

struct Part {
    kind: Kind,
    /// LTspice pin positions in SPICE order (LTspice units, unrotated).
    lt: Vec<P>,
    /// SwiftSim part origin in the symbol's grid units; flip the part vertically.
    origin: P,
    flip: bool,
    at: P,
    orient: String,
    inst: String,
    attrs: Attrs,
}

fn place(s: &mut Schematic, p: Part) {
    let (kind, flip) = (p.kind, p.flip);
    let (m, n) = (p.orient.starts_with('M'), quarters(&p.orient));
    // orient() ∘ flip, rewritten as SwiftSim's mirror-then-counter-clockwise rotation
    let rot = ((if m { n } else { -n }) + if flip { 2 } else { 0 }).rem_euclid(4) as u8;
    let o = orient(&p.orient, p.origin);
    let (lib, value) = match kind {
        Kind::Subckt => p.attrs.subckt(),
        // the ideal opamp's Value names LTspice's own subcircuit; SwiftSim only takes the parameters
        Kind::OpAmp => ("", join(&p.attrs.vals[2..])),
        _ => ("", join(&p.attrs.vals)),
    };
    // a subcircuit part's pins are the .asy's own; stub wires still cover off-grid ones
    let pins = if kind == Kind::Subckt { p.lt.iter().map(|q| [grid(q[0]), grid(q[1])]).collect() } else { vec![] };
    let name = if p.inst.is_empty() { s.next_name(kind) } else { p.inst };
    let c = Component { kind, name, value, pos: [grid(p.at[0]) + o[0], grid(p.at[1]) + o[1]], rot, mirror: m != flip, pins };
    for (a, q) in c.pin_pos().into_iter().zip(&p.lt) {
        let l = orient(&p.orient, *q);
        let b = [grid(p.at[0] + l[0]), grid(p.at[1] + l[1])];
        if a != b {
            s.wires.push(Wire { a, b });
        }
    }
    let text = if lib.contains(char::is_whitespace) { format!(".lib \"{lib}\"") } else { format!(".lib {lib}") };
    if !lib.is_empty() && !s.texts.iter().any(|t| t.text == text) {
        s.texts.push(Text { pos: [c.pos[0], c.world_bbox().1[1].ceil() as i32 + 2], text });
    }
    s.comps.push(c);
}

pub use swiftsim::netlist::decode;

/// Converts an LTspice schematic; `dir` is its folder (for `.asy` files next to it).
/// Also returns the names of symbols that are neither stock nor a subcircuit
/// `.asy` that could be found (sorted, unique); those parts are left out.
pub fn import(text: &str, dir: Option<&Path>) -> (Schematic, Vec<String>) {
    let mut s = Schematic::default();
    let mut skipped: Vec<String> = vec![];
    let mut cur: Option<Part> = None;
    for line in text.lines() {
        let w: Vec<&str> = line.split_whitespace().collect();
        let num = |i: usize| w.get(i).and_then(|v| v.parse::<i32>().ok()).unwrap_or(0);
        let at = [grid(num(1)), grid(num(2))];
        match w.first().copied() {
            Some("WIRE") => s.wires.push(Wire { a: at, b: [grid(num(3)), grid(num(4))] }),
            Some("FLAG") => {
                let net = rest(line, 3);
                let kind = if net == "0" { Kind::Ground } else { Kind::Label };
                let value = if net == "0" { String::new() } else { net.to_string() };
                s.comps.push(Component { kind, name: String::new(), value, pos: at, rot: 0, mirror: false, pins: vec![] });
            }
            Some("SYMBOL") => {
                if let Some(p) = cur.take() {
                    place(&mut s, p);
                }
                let name = w.get(1).copied().unwrap_or("");
                let sym = match symbol(&name.rsplit('\\').next().unwrap_or("").to_ascii_lowercase()) {
                    Some((kind, pins, origin, flip)) => Some((kind, pins.iter().map(|q| [q[0] * 16, q[1] * 16]).collect(), origin, flip, Attrs::default())),
                    None => find_asy(name, dir).and_then(|t| read_asy(&t)).map(|(lt, attrs)| (Kind::Subckt, lt, [0, 0], false, attrs)),
                };
                match sym {
                    Some((kind, lt, origin, flip, attrs)) => {
                        let orient = w.get(4).copied().unwrap_or("R0").to_string();
                        cur = Some(Part { kind, lt, origin, flip, at: [num(2), num(3)], orient, inst: String::new(), attrs });
                    }
                    None => skipped.push(name.to_string()),
                }
            }
            Some("SYMATTR") => {
                if let Some(p) = cur.as_mut() {
                    let v = rest(line, 2).to_string();
                    match w.get(1).copied() {
                        Some("InstName") => p.inst = v,
                        Some(k) => p.attrs.set(k, v),
                        None => {}
                    }
                }
            }
            Some("TEXT") => {
                let t = rest(line, 5);
                let text = if let Some(d) = t.strip_prefix('!') {
                    d.replace("\\n", "\n")
                } else if let Some(c) = t.strip_prefix(';') {
                    c.replace("\\n", "\n").lines().map(|l| format!("* {l}")).collect::<Vec<_>>().join("\n")
                } else {
                    continue;
                };
                s.texts.push(Text { pos: at, text });
            }
            _ => {}
        }
    }
    if let Some(p) = cur.take() {
        place(&mut s, p);
    }
    skipped.sort();
    skipped.dedup();
    (s, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(asc: &str) -> Vec<String> {
        let (s, skipped) = import(asc, None);
        assert!(skipped.is_empty(), "{skipped:?}");
        s.netlist().text.lines().map(String::from).collect()
    }

    fn op(asc: &str, nodes: &[&str]) -> Vec<f64> {
        let n = import(asc, None).0.netlist();
        let r = swiftsim::run_netlist(&n.text, None, &swiftsim::Control::default()).unwrap_or_else(|e| panic!("{e}\n{}", n.text));
        let p = &r.plots[0];
        nodes.iter().map(|v| p.re[p.index(&format!("v({v})")).unwrap_or_else(|| panic!("{v}\n{}", n.text))][0]).collect()
    }

    const RC: &str = "Version 4
SHEET 1 880 680
WIRE 96 96 32 96
WIRE 240 96 176 96
FLAG 32 176 0
FLAG 240 160 0
FLAG 32 96 in
FLAG 240 96 out
SYMBOL voltage 32 80 R0
WINDOW 123 0 0 Left 0
SYMATTR InstName V1
SYMATTR Value 5
SYMATTR Value2 AC 1
SYMBOL res 192 80 R90
WINDOW 0 0 56 VBottom 2
WINDOW 3 32 56 VTop 2
SYMATTR InstName R1
SYMATTR Value 1k
SYMBOL cap 224 96 R0
SYMATTR InstName C1
SYMATTR Value 100n
TEXT 32 232 Left 2 !.op
TEXT 32 264 Left 2 ;RC low-pass\\nfc = 1.6 kHz
";

    #[test]
    fn rc_filter() {
        let l = lines(RC);
        for want in ["V1 in 0 5 AC 1", "R1 out in 1k", "C1 out 0 100n", ".op", "* RC low-pass", "* fc = 1.6 kHz"] {
            assert!(l.iter().any(|x| x == want), "missing {want:?} in {l:#?}");
        }
        assert!((op(RC, &["out"])[0] - 5.0).abs() < 1e-6);
    }

    // common-emitter stage with a divider bias, laid out like an LTspice-saved file
    const BJT: &str = "Version 4
SHEET 1 880 680
WIRE 192 16 192 48
WIRE 16 96 128 96
WIRE 80 96 80 112
FLAG -208 144 0
FLAG -208 64 vcc
FLAG 192 -64 vcc
FLAG 192 48 c
FLAG 128 96 b
FLAG 192 144 e
FLAG 192 224 0
FLAG -64 96 vcc
FLAG 80 192 0
SYMBOL voltage -208 48 R0
SYMATTR InstName V1
SYMATTR Value 12
SYMBOL res 176 -80 R0
SYMATTR InstName RC
SYMATTR Value 4.7k
SYMBOL npn 128 48 R0
SYMATTR InstName Q1
SYMATTR Value 2N3904
SYMBOL res 176 128 R0
SYMATTR InstName RE
SYMATTR Value 1k
SYMBOL res 32 80 R90
SYMATTR InstName RB1
SYMATTR Value 47k
SYMBOL res 64 96 R0
SYMATTR InstName RB2
SYMATTR Value 10k
TEXT -208 280 Left 2 !.op
";

    #[test]
    fn bjt_stage() {
        let l = lines(BJT);
        for want in ["V1 vcc 0 12", "RC vcc c 4.7k", "Q1 c b e 2N3904", "RE e 0 1k", "RB1 b vcc 47k", "RB2 b 0 10k"] {
            assert!(l.iter().any(|x| x == want), "missing {want:?} in {l:#?}");
        }
        let v = op(BJT, &["c", "b", "e"]);
        assert!(12.0 > v[0] && v[0] > v[1] && v[1] > v[2] && v[2] > 0.5, "not forward-active: {v:?}");
    }

    // every pin labelled at the position LTspice puts it (computed by hand from the .asy PINs);
    // pnp R180/M180 match real LTspice-saved files
    const ROTATED: &str = "Version 4
SYMBOL pnp 208 336 R180
SYMATTR InstName Q2
SYMATTR Value 2N3906
FLAG 144 336 q2c
FLAG 208 288 q2b
FLAG 144 240 q2e
SYMBOL pnp 400 112 M180
SYMATTR InstName Q3
SYMATTR Value 2N3906
FLAG 464 112 q3c
FLAG 400 64 q3b
FLAG 464 16 q3e
SYMBOL npn 0 400 M0
SYMATTR InstName Q4
SYMATTR Value 2N3904
FLAG -64 400 q4c
FLAG 0 448 q4b
FLAG -64 496 q4e
SYMBOL res 600 400 R270
SYMATTR InstName R1
SYMATTR Value 1k
FLAG 616 384 r1a
FLAG 696 384 r1b
SYMBOL cap 800 400 M90
SYMATTR InstName C1
SYMATTR Value 1u
FLAG 800 416 c1a
FLAG 864 416 c1b
SYMBOL nmos 1000 400 M270
SYMATTR InstName M1
SYMATTR Value 2N7000
FLAG 1000 352 m1d
FLAG 920 400 m1g
FLAG 904 352 m1s
SYMBOL g 1200 400 R90
SYMATTR InstName G1
SYMATTR Value 2m
FLAG 1104 400 g1p
FLAG 1184 400 g1n
FLAG 1168 352 g1cp
FLAG 1120 352 g1cn
SYMBOL sw 1400 400 R0
SYMATTR InstName S1
SYMATTR Value SW
FLAG 1400 416 s1p
FLAG 1400 496 s1n
FLAG 1352 480 s1cp
FLAG 1352 432 s1cn
SYMBOL Opamps\\\\opamp 1600 400 M0
SYMATTR InstName U1
SYMATTR SpiceLine Aol=1Meg
FLAG 1632 480 uinp
FLAG 1632 448 uinn
FLAG 1568 464 uout
SYMBOL e 1800 400 R180
SYMATTR InstName E1
SYMATTR Value 10
FLAG 1800 384 e1p
FLAG 1800 304 e1n
FLAG 1848 368 e1cp
FLAG 1848 320 e1cn
SYMBOL current 2000 400 R90
SYMATTR InstName I1
SYMATTR Value 1m
FLAG 2000 400 i1p
FLAG 1920 400 i1n
";

    #[test]
    fn rotated_and_mirrored() {
        let l = lines(ROTATED);
        for want in [
            "Q2 q2c q2b q2e 2N3906",
            "Q3 q3c q3b q3e 2N3906",
            "Q4 q4c q4b q4e 2N3904",
            "R1 r1a r1b 1k",
            "C1 c1a c1b 1u",
            "M1 m1d m1g m1s 2N7000",
            "G1 g1p g1n g1cp g1cn 2m",
            "S1 s1p s1n s1cp s1cn SW",
            "XU1 uinp uinn uout swiftsim_opamp Aol=1Meg",
            "E1 e1p e1n e1cp e1cn 10",
            "I1 i1p i1n 1m",
        ] {
            assert!(l.iter().any(|x| x == want), "missing {want:?} in {l:#?}");
        }
    }

    #[test]
    fn every_symbol_and_orientation_keeps_pins_apart() {
        for name in ["res", "ind", "cap", "diode", "voltage", "bv", "current", "npn", "pnp", "nmos", "pmos", "nmos4", "pmos4", "njf", "pjf", "e", "g", "sw", "opamp"] {
            let pins = symbol(name).unwrap().1;
            for o in ["R0", "R90", "R180", "R270", "M0", "M90", "M180", "M270"] {
                let mut asc = format!("SYMBOL {name} 32 -48 {o}\n");
                for (i, p) in pins.iter().enumerate() {
                    let q = orient(o, [p[0] * 16, p[1] * 16]);
                    asc += &format!("FLAG {} {} p{}\n", 32 + q[0], -48 + q[1], i + 1);
                }
                let net = import(&asc, None).0.netlist().text;
                let got: Vec<&str> = net.lines().nth(1).unwrap().split_whitespace().skip(1).take(pins.len()).collect();
                let want: Vec<String> = (1..=pins.len()).map(|i| format!("p{i}")).collect();
                assert_eq!(got, want, "{name} {o}\n{net}");
            }
        }
    }

    // pins flagged at the stock .asy positions: nmos4 B at (48,48), njf G at (0,64)
    const FOUR_PIN: &str = "Version 4
SYMBOL nmos4 0 0 R0
SYMATTR InstName M1
SYMATTR Value NMOS_GENERIC
SYMATTR SpiceLine W=10u L=10u
FLAG 48 0 d
FLAG 0 80 g
FLAG 48 96 0
FLAG 48 48 b
SYMBOL njf 200 0 R0
SYMATTR InstName J1
SYMATTR Value NJF
FLAG 248 0 d
FLAG 200 64 0
FLAG 248 96 s
TEXT 0 200 Left 2 !Vd d 0 5\\nVg g 0 2\\nVb b 0 -1\\nRs s 0 1k\\n.op
";

    #[test]
    fn four_pin_mos_and_jfet() {
        let l = lines(FOUR_PIN);
        for want in ["M1 d g 0 b NMOS_GENERIC W=10u L=10u", "J1 d 0 s NJF"] {
            assert!(l.iter().any(|x| x == want), "missing {want:?} in {l:#?}");
        }
        let n = import(FOUR_PIN, None).0.netlist();
        let r = swiftsim::run_netlist(&n.text, None, &swiftsim::Control::default()).unwrap();
        let p = &r.plots[0];
        let v = |s: &str| p.re[p.index(s).unwrap()][0];
        // body effect through the bulk pin: Vth = 0.7 + 0.4(√1.7 - √0.7), Id = β/2·Vov²·(1 + 0.04·5)
        let vov = 2.0 - (0.7 + 0.4 * (1.7f64.sqrt() - 0.7f64.sqrt()));
        assert!((v("id(m1)") - 55e-6 * vov * vov * 1.2).abs() < 1e-9, "{}", v("id(m1)"));
        // self-biased NJF (Vto=-2, β=100µ): Id·1k = u with u² - 14u + 4 = 0
        assert!((v("v(s)") - (14.0 - 180f64.sqrt()) / 2.0).abs() < 1e-6, "{}", v("v(s)"));
    }

    #[test]
    fn unknown_symbols_are_reported_not_merged() {
        let (s, skipped) = import("SYMBOL res 0 0 R0\nSYMATTR InstName R1\nSYMATTR Value 1k\nSYMBOL Opamps\\\\NoSuchAmp 100 0 R0\nSYMATTR InstName U1\nSYMATTR Value NoSuchAmp\nSYMBOL csw 300 0 R0\nSYMBOL csw 400 0 R0\n", None);
        assert_eq!(skipped, ["Opamps\\\\NoSuchAmp", "csw"]);
        assert_eq!(s.comps.len(), 1);
        assert_eq!((s.comps[0].name.as_str(), s.comps[0].value.as_str()), ("R1", "1k"));
    }

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("swiftsim-asy-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    // a made-up 6-pin symbol: SpiceOrder differs from file order, pin 6 is off the 16-unit grid
    const MYAMP_ASY: &str = "Version 4
SymbolType CELL
LINE Normal -32 32 32 64
PIN -32 80 NONE 8
PINATTR PinName In-
PINATTR SpiceOrder 2
PIN -32 48 NONE 8
PINATTR PinName In+
PINATTR SpiceOrder 1
PIN 0 32 NONE 8
PINATTR SpiceOrder 3
PIN 0 96 NONE 8
PINATTR SpiceOrder 4
PIN 32 64 NONE 8
PINATTR SpiceOrder 5
PIN 24 88 NONE 8
PINATTR SpiceOrder 6
SYMATTR Prefix X
SYMATTR SpiceModel MYAMP
SYMATTR ModelFile myamp.lib
";
    /// MYAMP_ASY's pins in SPICE order.
    const LT_PINS: [P; 6] = [[-32, 48], [-32, 80], [0, 32], [0, 96], [32, 64], [24, 88]];

    #[test]
    fn custom_asy_next_to_schematic_every_orientation() {
        let dir = temp("orient");
        std::fs::write(dir.join("myamp.asy"), MYAMP_ASY).unwrap();
        for o in ["R0", "R90", "R180", "R270", "M0", "M90", "M180", "M270"] {
            let mut asc = format!("SYMBOL myamp 160 -48 {o}\nSYMATTR InstName U1\n");
            for (i, q) in LT_PINS.iter().enumerate() {
                let q = orient(o, *q);
                asc += &format!("FLAG {} {} p{}\n", 160 + q[0], -48 + q[1], i + 1);
            }
            let (s, skipped) = import(&asc, Some(&dir));
            let net = s.netlist().text;
            assert!(skipped.is_empty() && net.lines().any(|l| l == "XU1 p1 p2 p3 p4 p5 p6 MYAMP"), "{o} {skipped:?}\n{net}");
            assert_eq!(net.matches(".lib myamp.lib").count(), 1, "{net}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    // a stand-in LTspice lib folder written by the test: the .asy in lib/sym (other letter case), the model in lib/sub
    #[test]
    fn custom_asy_from_ltspice_lib_simulates_and_round_trips() {
        let lib = temp("lib");
        std::fs::create_dir_all(lib.join("sym/Custom")).unwrap();
        std::fs::create_dir_all(lib.join("sub")).unwrap();
        std::fs::write(lib.join("sym/Custom/MyAmp.asy"), MYAMP_ASY).unwrap();
        std::fs::write(lib.join("sub/myamp.lib"), ".subckt MYAMP inp inn vp vn out ref params: gain=1k\nE1 out ref inp inn {gain}\nRs vp vn 1meg\n.ends MYAMP\n").unwrap();
        // non-inverting buffer: In+ = in, In- = out
        let flag = |i: usize, net: &str| {
            let q = orient("M90", LT_PINS[i]);
            format!("FLAG {} {} {net}\n", 320 + q[0], 160 + q[1])
        };
        let asc = ["SYMBOL voltage 0 0 R0\nSYMATTR InstName V1\nSYMATTR Value 1\nFLAG 0 16 in\nFLAG 0 96 0\n", "SYMBOL voltage 96 0 R0\nSYMATTR InstName V2\nSYMATTR Value 5\nFLAG 96 16 vcc\nFLAG 96 96 0\n", "SYMBOL Custom\\\\myamp 320 160 M90\nSYMATTR InstName U1\nSYMATTR SpiceLine gain=1e5\n", &flag(0, "in"), &flag(1, "out"), &flag(2, "vcc"), &flag(3, "0"), &flag(4, "out"), &flag(5, "0"), "TEXT 0 300 Left 2 !.op\n"].concat();

        swiftsim::netlist::set_ltspice_lib(Some(lib.clone()));
        let (s, skipped) = import(&asc, None);
        let n = s.netlist();
        let r = swiftsim::run_netlist(&n.text, None, &swiftsim::Control::default());
        swiftsim::netlist::set_ltspice_lib(None);
        std::fs::remove_dir_all(&lib).ok();

        assert!(skipped.is_empty(), "{skipped:?}");
        assert!(n.text.lines().any(|l| l == "XU1 in out vcc 0 out 0 MYAMP gain=1e5"), "{}", n.text);
        let r = r.unwrap_or_else(|e| panic!("{e}\n{}", n.text));
        let p = &r.plots[0];
        let out = p.re[p.index("v(out)").unwrap()][0];
        assert!((out - 1e5 / (1e5 + 1.0)).abs() < 1e-9, "{out}");

        let json = serde_json::to_string(&s).unwrap();
        let back: Schematic = serde_json::from_str(&json).unwrap();
        assert_eq!(back.netlist().text, n.text);
        assert_eq!(back.comps.iter().find(|c| c.kind == Kind::Subckt).map(|c| c.pins.len()), Some(6));
        // other parts save exactly as before
        assert_eq!(json.matches("\"pins\"").count(), 1, "{json}");
    }

    #[test]
    fn auto_load_asy_netlists_value2_and_only_x_is_taken() {
        let (pins, a) = read_asy("SYMATTR Value LT9999\nSYMATTR Prefix X\nSYMATTR SpiceModel VENDOR.lib\nSYMATTR Value2 LT9999\nPIN 0 0 NONE 8\n").unwrap();
        assert_eq!((pins, a.subckt()), (vec![[0, 0]], ("VENDOR.lib", "LT9999".to_string())));
        assert!(read_asy("SYMATTR Prefix D\nPIN 0 0 NONE 8\n").is_none());
    }

    #[test]
    fn decodes_utf16_and_latin1() {
        let text = "Version 4\nSYMATTR Value 1µ\n";
        let utf16: Vec<u8> = [0xFF, 0xFE].into_iter().chain(text.encode_utf16().flat_map(|u| u.to_le_bytes())).collect();
        assert_eq!(decode(&utf16), text);
        assert_eq!(decode(b"Value 1\xb5"), "Value 1µ");
        assert_eq!(decode(text.as_bytes()), text);
    }
}
