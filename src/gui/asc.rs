//! LTspice `.asc` import. LTspice uses 16 units per grid step and its own pin
//! layout per `.asy` symbol, so each SwiftSim part is placed inside the LTspice
//! footprint and joined to the LTspice pin positions with short stub wires.

use super::schematic::{Component, Kind, P, Schematic, Text, Wire};

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
        "opamp" => (OpAmp, &[[-2, 5], [-2, 3], [2, 4]], [0, 4], false),
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

struct Part {
    sym: (Kind, &'static [P], P, bool),
    at: P,
    orient: String,
    inst: String,
    /// Value, Value2, SpiceLine, SpiceLine2 — joined in this order like LTspice's netlister.
    vals: [String; 4],
}

fn place(s: &mut Schematic, p: Part) {
    let (kind, lt_pins, origin, flip) = p.sym;
    let (m, n) = (p.orient.starts_with('M'), quarters(&p.orient));
    // orient() ∘ flip, rewritten as SwiftSim's mirror-then-counter-clockwise rotation
    let rot = ((if m { n } else { -n }) + if flip { 2 } else { 0 }).rem_euclid(4) as u8;
    let o = orient(&p.orient, origin);
    // the ideal opamp's Value names LTspice's own subcircuit; SwiftSim only takes the parameters
    let vals = if kind == Kind::OpAmp { &p.vals[2..] } else { &p.vals[..] };
    let value = vals.iter().filter(|v| !v.is_empty()).cloned().collect::<Vec<_>>().join(" ");
    let name = if p.inst.is_empty() { s.next_name(kind) } else { p.inst };
    let c = Component { kind, name, value, pos: [grid(p.at[0]) + o[0], grid(p.at[1]) + o[1]], rot, mirror: m != flip };
    for (a, q) in c.pin_pos().into_iter().zip(lt_pins) {
        let l = orient(&p.orient, [q[0] * 16, q[1] * 16]);
        let b = [grid(p.at[0] + l[0]), grid(p.at[1] + l[1])];
        if a != b {
            s.wires.push(Wire { a, b });
        }
    }
    s.comps.push(c);
}

pub use swiftsim::netlist::decode;

/// Converts an LTspice schematic. Also returns the names of symbols that have
/// no SwiftSim equivalent (sorted, unique); those parts are left out.
pub fn import(text: &str) -> (Schematic, Vec<String>) {
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
                s.comps.push(Component { kind, name: String::new(), value, pos: at, rot: 0, mirror: false });
            }
            Some("SYMBOL") => {
                if let Some(p) = cur.take() {
                    place(&mut s, p);
                }
                let name = w.get(1).copied().unwrap_or("");
                match symbol(&name.rsplit('\\').next().unwrap_or("").to_ascii_lowercase()) {
                    Some(sym) => {
                        let orient = w.get(4).copied().unwrap_or("R0").to_string();
                        cur = Some(Part { sym, at: [num(2), num(3)], orient, inst: String::new(), vals: Default::default() });
                    }
                    None => skipped.push(name.to_string()),
                }
            }
            Some("SYMATTR") => {
                if let Some(p) = cur.as_mut() {
                    let v = rest(line, 2).to_string();
                    match w.get(1).copied() {
                        Some("InstName") => p.inst = v,
                        Some("Value") => p.vals[0] = v,
                        Some("Value2") => p.vals[1] = v,
                        Some("SpiceLine") => p.vals[2] = v,
                        Some("SpiceLine2") => p.vals[3] = v,
                        _ => {}
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
        let (s, skipped) = import(asc);
        assert!(skipped.is_empty(), "{skipped:?}");
        s.netlist().text.lines().map(String::from).collect()
    }

    fn op(asc: &str, nodes: &[&str]) -> Vec<f64> {
        let n = import(asc).0.netlist();
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
                let net = import(&asc).0.netlist().text;
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
        let n = import(FOUR_PIN).0.netlist();
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
        let (s, skipped) = import("SYMBOL res 0 0 R0\nSYMATTR InstName R1\nSYMATTR Value 1k\nSYMBOL Opamps\\\\LT1001 100 0 R0\nSYMATTR InstName U1\nSYMATTR Value LT1001\nSYMBOL csw 300 0 R0\nSYMBOL csw 400 0 R0\n");
        assert_eq!(skipped, ["Opamps\\\\LT1001", "csw"]);
        assert_eq!(s.comps.len(), 1);
        assert_eq!((s.comps[0].name.as_str(), s.comps[0].value.as_str()), ("R1", "1k"));
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
