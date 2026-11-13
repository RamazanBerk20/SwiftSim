//! Schematic document: components, wires, directives; symbol geometry;
//! connectivity and netlist generation.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

pub type P = [i32; 2];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Kind {
    Resistor,
    Capacitor,
    Inductor,
    Diode,
    Voltage,
    Current,
    Npn,
    Pnp,
    Nmos,
    Pmos,
    Nmos4,
    Pmos4,
    Njf,
    Pjf,
    OpAmp,
    Vcvs,
    Vccs,
    Behavioral,
    Switch,
    /// A non-stock LTspice symbol (`X` line); its pins live in `Component::pins`.
    Subckt,
    Ground,
    Label,
}

impl Kind {
    pub const ALL: [Kind; 21] = [
        Kind::Resistor,
        Kind::Capacitor,
        Kind::Inductor,
        Kind::Diode,
        Kind::Voltage,
        Kind::Current,
        Kind::Npn,
        Kind::Pnp,
        Kind::Nmos,
        Kind::Pmos,
        Kind::Nmos4,
        Kind::Pmos4,
        Kind::Njf,
        Kind::Pjf,
        Kind::OpAmp,
        Kind::Vcvs,
        Kind::Vccs,
        Kind::Behavioral,
        Kind::Switch,
        Kind::Ground,
        Kind::Label,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Kind::Resistor => "Resistor",
            Kind::Capacitor => "Capacitor",
            Kind::Inductor => "Inductor",
            Kind::Diode => "Diode",
            Kind::Voltage => "Voltage source",
            Kind::Current => "Current source",
            Kind::Npn => "NPN transistor",
            Kind::Pnp => "PNP transistor",
            Kind::Nmos => "N-MOSFET",
            Kind::Pmos => "P-MOSFET",
            Kind::Nmos4 => "N-MOSFET (4-pin)",
            Kind::Pmos4 => "P-MOSFET (4-pin)",
            Kind::Njf => "N-JFET",
            Kind::Pjf => "P-JFET",
            Kind::OpAmp => "Op-amp",
            Kind::Vcvs => "VCVS (E)",
            Kind::Vccs => "VCCS (G)",
            Kind::Behavioral => "Behavioral source (B)",
            Kind::Switch => "Voltage switch (S)",
            Kind::Subckt => "Subcircuit (X)",
            Kind::Ground => "Ground",
            Kind::Label => "Net label",
        }
    }

    pub fn category(self) -> &'static str {
        match self {
            Kind::Resistor | Kind::Capacitor | Kind::Inductor => "Passive",
            Kind::Diode
            | Kind::Npn
            | Kind::Pnp
            | Kind::Nmos
            | Kind::Pmos
            | Kind::Nmos4
            | Kind::Pmos4
            | Kind::Njf
            | Kind::Pjf => "Semiconductor",
            Kind::Voltage | Kind::Current | Kind::Vcvs | Kind::Vccs | Kind::Behavioral => "Sources",
            Kind::OpAmp | Kind::Switch | Kind::Subckt => "Blocks",
            Kind::Ground | Kind::Label => "Wiring",
        }
    }

    pub fn shortcut(self) -> Option<&'static str> {
        Some(match self {
            Kind::Resistor => "R",
            Kind::Capacitor => "C",
            Kind::Inductor => "L",
            Kind::Diode => "D",
            Kind::Voltage => "V",
            Kind::Current => "I",
            Kind::Npn => "Q",
            Kind::Nmos => "M",
            Kind::OpAmp => "U",
            Kind::Ground => "G",
            Kind::Label => "N",
            _ => return None,
        })
    }

    /// Reference prefix for auto naming (empty for ground/labels).
    pub fn prefix(self) -> &'static str {
        match self {
            Kind::Resistor => "R",
            Kind::Capacitor => "C",
            Kind::Inductor => "L",
            Kind::Diode => "D",
            Kind::Voltage => "V",
            Kind::Current => "I",
            Kind::Npn | Kind::Pnp => "Q",
            Kind::Nmos | Kind::Pmos | Kind::Nmos4 | Kind::Pmos4 => "M",
            Kind::Njf | Kind::Pjf => "J",
            Kind::OpAmp => "U",
            Kind::Vcvs => "E",
            Kind::Vccs => "G",
            Kind::Behavioral => "B",
            Kind::Switch => "S",
            Kind::Subckt => "X",
            Kind::Ground | Kind::Label => "",
        }
    }

    pub fn default_value(self) -> &'static str {
        match self {
            Kind::Resistor => "1k",
            Kind::Capacitor => "1u",
            Kind::Inductor => "1m",
            Kind::Diode => "1N4148",
            Kind::Voltage => "5",
            Kind::Current => "1m",
            Kind::Npn => "2N3904",
            Kind::Pnp => "2N3906",
            Kind::Nmos => "2N7000",
            Kind::Pmos => "BSS84",
            Kind::Nmos4 => "NMOS_GENERIC W=10u L=1u",
            Kind::Pmos4 => "PMOS_GENERIC W=20u L=1u",
            Kind::Njf => "NJF",
            Kind::Pjf => "PJF",
            Kind::OpAmp => "Aol=100k GBW=10Meg",
            Kind::Vcvs | Kind::Vccs => "1",
            Kind::Behavioral => "V=V(in)*2",
            Kind::Switch => "SW",
            Kind::Subckt | Kind::Ground => "",
            Kind::Label => "out",
        }
    }

    /// Model kinds used to fill the model picker.
    pub fn model_kinds(self) -> &'static [&'static str] {
        match self {
            Kind::Diode => &["d"],
            Kind::Npn => &["npn"],
            Kind::Pnp => &["pnp"],
            Kind::Nmos => &["nmos", "vdmos"],
            Kind::Pmos => &["pmos", "vdmos"],
            Kind::Nmos4 => &["nmos"],
            Kind::Pmos4 => &["pmos"],
            Kind::Njf => &["njf"],
            Kind::Pjf => &["pjf"],
            Kind::Switch => &["sw"],
            _ => &[],
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Component {
    pub kind: Kind,
    pub name: String,
    pub value: String,
    pub pos: P,
    pub rot: u8,
    pub mirror: bool,
    /// Subcircuit pins in local grid units and SPICE order; other kinds use `pins(kind)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pins: Vec<P>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Wire {
    pub a: P,
    pub b: P,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Text {
    pub pos: P,
    pub text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Schematic {
    /// File format version; files saved before versioning load as 0 (see `upgrade`).
    #[serde(default)]
    pub format: u32,
    pub comps: Vec<Component>,
    pub wires: Vec<Wire>,
    pub texts: Vec<Text>,
}

impl Default for Schematic {
    fn default() -> Self {
        Schematic {
            format: Schematic::FORMAT,
            comps: vec![],
            wires: vec![],
            texts: vec![],
        }
    }
}

/// Drawing primitive in local grid units.
pub enum Prim {
    Line(Vec<[f32; 2]>),
    Circle([f32; 2], f32),
    Poly(Vec<[f32; 2]>),
    Text([f32; 2], &'static str),
}

/// Pins in local grid units, in netlist order.
pub fn pins(k: Kind) -> &'static [P] {
    match k {
        Kind::Resistor
        | Kind::Capacitor
        | Kind::Inductor
        | Kind::Diode
        | Kind::Voltage
        | Kind::Current
        | Kind::Behavioral => &[[0, -2], [0, 2]],
        Kind::Npn | Kind::Pnp | Kind::Nmos | Kind::Pmos | Kind::Njf | Kind::Pjf => {
            &[[1, -2], [-2, 0], [1, 2]]
        }
        Kind::Nmos4 | Kind::Pmos4 => &[[1, -2], [-2, 0], [1, 2], [1, 0]],
        // inp (+) on top, inn (−) below (format 1; format 0 had them the other way round)
        Kind::OpAmp => &[[-2, -1], [-2, 1], [2, 0]],
        Kind::Vcvs | Kind::Vccs | Kind::Switch => &[[0, -2], [0, 2], [-2, -1], [-2, 1]],
        Kind::Ground | Kind::Label => &[[0, 0]],
        Kind::Subckt => &[],
    }
}

fn arc(cx: f32, cy: f32, r: f32, a0: f32, a1: f32) -> Vec<[f32; 2]> {
    (0..=12)
        .map(|i| {
            let a = a0 + (a1 - a0) * i as f32 / 12.0;
            [cx + r * a.cos(), cy + r * a.sin()]
        })
        .collect()
}

pub fn symbol(k: Kind) -> Vec<Prim> {
    use Prim::*;
    let pi = std::f32::consts::PI;
    match k {
        Kind::Resistor => vec![Line(vec![
            [0., -2.],
            [0., -1.2],
            [0.4, -1.0],
            [-0.4, -0.6],
            [0.4, -0.2],
            [-0.4, 0.2],
            [0.4, 0.6],
            [-0.4, 1.0],
            [0., 1.2],
            [0., 2.],
        ])],
        Kind::Capacitor => vec![
            Line(vec![[0., -2.], [0., -0.25]]),
            Line(vec![[-0.8, -0.25], [0.8, -0.25]]),
            Line(vec![[-0.8, 0.25], [0.8, 0.25]]),
            Line(vec![[0., 0.25], [0., 2.]]),
        ],
        Kind::Inductor => {
            let mut v = vec![
                Line(vec![[0., -2.], [0., -1.4]]),
                Line(vec![[0., 1.4], [0., 2.]]),
            ];
            for i in 0..4 {
                let cy = -1.05 + 0.7 * i as f32;
                v.push(Line(arc(0.0, cy, 0.35, -pi / 2.0, pi / 2.0)));
            }
            v
        }
        Kind::Diode => vec![
            Line(vec![[0., -2.], [0., -0.5]]),
            Poly(vec![[-0.6, -0.5], [0.6, -0.5], [0., 0.5]]),
            Line(vec![[-0.6, 0.5], [0.6, 0.5]]),
            Line(vec![[0., 0.5], [0., 2.]]),
        ],
        Kind::Voltage => vec![
            Line(vec![[0., -2.], [0., -1.]]),
            Circle([0., 0.], 1.0),
            Line(vec![[0., 1.], [0., 2.]]),
            Line(vec![[-0.25, -0.5], [0.25, -0.5]]),
            Line(vec![[0., -0.75], [0., -0.25]]),
            Line(vec![[-0.25, 0.5], [0.25, 0.5]]),
        ],
        Kind::Current => vec![
            Line(vec![[0., -2.], [0., -1.]]),
            Circle([0., 0.], 1.0),
            Line(vec![[0., 1.], [0., 2.]]),
            Line(vec![[0., -0.6], [0., 0.5]]),
            Poly(vec![[-0.25, 0.2], [0.25, 0.2], [0., 0.6]]),
        ],
        Kind::Npn | Kind::Pnp => {
            let mut v = vec![
                Line(vec![[-2., 0.], [-0.5, 0.]]),
                Line(vec![[-0.5, -0.9], [-0.5, 0.9]]),
                Line(vec![[-0.5, -0.4], [1., -1.1], [1., -2.]]),
                Line(vec![[-0.5, 0.4], [1., 1.1], [1., 2.]]),
                Circle([0.2, 0.], 1.35),
            ];
            v.push(if k == Kind::Npn {
                Poly(vec![[0.95, 1.08], [0.45, 1.1], [0.68, 0.7]])
            } else {
                Poly(vec![[-0.45, 0.43], [0.0, 0.42], [-0.18, 0.82]])
            });
            v
        }
        Kind::Nmos | Kind::Pmos | Kind::Nmos4 | Kind::Pmos4 => {
            let four = matches!(k, Kind::Nmos4 | Kind::Pmos4);
            let mut v = vec![
                Line(vec![[-2., 0.], [-0.8, 0.]]),
                Line(vec![[-0.8, -0.8], [-0.8, 0.8]]),
                Line(vec![[-0.5, -1.0], [-0.5, -0.45]]),
                Line(vec![[-0.5, -0.25], [-0.5, 0.25]]),
                Line(vec![[-0.5, 0.45], [-0.5, 1.0]]),
                Line(vec![[-0.5, -0.7], [1., -0.7], [1., -2.]]),
                Line(vec![[-0.5, 0.7], [1., 0.7], [1., 2.]]),
                // 3-pin parts tie the body to the source; 4-pin parts bring it out
                Line(if four {
                    vec![[-0.5, 0.], [1., 0.]]
                } else {
                    vec![[-0.5, 0.], [1., 0.], [1., 0.7]]
                }),
            ];
            v.push(if matches!(k, Kind::Nmos | Kind::Nmos4) {
                Poly(vec![[-0.45, 0.], [-0.05, -0.22], [-0.05, 0.22]])
            } else {
                Poly(vec![[0.45, 0.], [0.05, -0.22], [0.05, 0.22]])
            });
            v
        }
        Kind::Njf | Kind::Pjf => vec![
            Line(vec![[-2., 0.], [-0.5, 0.]]),
            Line(vec![[-0.5, -1.0], [-0.5, 1.0]]),
            Line(vec![[-0.5, -0.7], [1., -0.7], [1., -2.]]),
            Line(vec![[-0.5, 0.7], [1., 0.7], [1., 2.]]),
            if k == Kind::Njf {
                Poly(vec![[-0.55, 0.], [-1.0, -0.22], [-1.0, 0.22]])
            } else {
                Poly(vec![[-1.0, 0.], [-0.55, -0.22], [-0.55, 0.22]])
            },
        ],
        // outline, not filled: the + (top, non-inverting) and − (bottom) marks sit inside it
        Kind::OpAmp => vec![
            Line(vec![[-1.5, -1.8], [-1.5, 1.8], [1.6, 0.], [-1.5, -1.8]]),
            Line(vec![[-2., -1.], [-1.5, -1.]]),
            Line(vec![[-2., 1.], [-1.5, 1.]]),
            Line(vec![[1.6, 0.], [2., 0.]]),
            Line(vec![[-1.35, -1.], [-0.85, -1.]]),
            Line(vec![[-1.1, -1.25], [-1.1, -0.75]]),
            Line(vec![[-1.35, 1.], [-0.85, 1.]]),
        ],
        Kind::Vcvs | Kind::Vccs | Kind::Behavioral => {
            let mut v = vec![
                Line(vec![[0., -2.], [0., -1.]]),
                Line(vec![[0., -1.], [1., 0.], [0., 1.], [-1., 0.], [0., -1.]]),
                Line(vec![[0., 1.], [0., 2.]]),
            ];
            if k == Kind::Vccs {
                v.push(Line(vec![[0., -0.55], [0., 0.45]]));
                v.push(Poly(vec![[-0.2, 0.15], [0.2, 0.15], [0., 0.5]]));
            } else {
                v.push(Line(vec![[-0.2, -0.45], [0.2, -0.45]]));
                v.push(Line(vec![[0., -0.65], [0., -0.25]]));
                v.push(Line(vec![[-0.2, 0.45], [0.2, 0.45]]));
            }
            if k != Kind::Behavioral {
                v.push(Line(vec![[-2., -1.], [-1.4, -1.]]));
                v.push(Line(vec![[-2., 1.], [-1.4, 1.]]));
                v.push(Text([-1.5, -1.5], "+"));
                v.push(Text([-1.5, 1.5], "−"));
            }
            v
        }
        Kind::Switch => vec![
            Line(vec![[0., -2.], [0., -0.8]]),
            Line(vec![[0., 0.8], [0., 2.]]),
            Line(vec![[0., 0.8], [0.6, -0.7]]),
            Circle([0., -0.8], 0.1),
            Circle([0., 0.8], 0.1),
            Line(vec![[-2., -1.], [-1.2, -1.]]),
            Line(vec![[-2., 1.], [-1.2, 1.]]),
            Line(vec![[-1.2, -1.], [-1.2, 1.]]),
            Text([-1.6, -1.5], "+"),
            Text([-1.6, 1.5], "−"),
        ],
        Kind::Ground => vec![
            Line(vec![[0., 0.], [0., 0.6]]),
            Line(vec![[-0.8, 0.6], [0.8, 0.6]]),
            Line(vec![[-0.5, 0.9], [0.5, 0.9]]),
            Line(vec![[-0.2, 1.2], [0.2, 1.2]]),
        ],
        Kind::Label | Kind::Subckt => vec![],
    }
}

/// Local bounding box (min, max) of a symbol.
pub fn bbox(k: Kind) -> ([f32; 2], [f32; 2]) {
    match k {
        Kind::Resistor | Kind::Inductor | Kind::Diode | Kind::Capacitor => {
            ([-0.8, -2.0], [0.8, 2.0])
        }
        Kind::Voltage | Kind::Current | Kind::Behavioral => ([-1.0, -2.0], [1.0, 2.0]),
        Kind::Npn
        | Kind::Pnp
        | Kind::Nmos
        | Kind::Pmos
        | Kind::Nmos4
        | Kind::Pmos4
        | Kind::Njf
        | Kind::Pjf => ([-2.0, -2.0], [1.6, 2.0]),
        Kind::OpAmp => ([-2.0, -1.8], [2.0, 1.8]),
        Kind::Vcvs | Kind::Vccs | Kind::Switch => ([-2.0, -2.0], [1.0, 2.0]),
        Kind::Ground => ([-0.8, 0.0], [0.8, 1.3]),
        Kind::Label => ([-0.3, -0.6], [3.0, 0.3]),
        Kind::Subckt => ([-1.0, -1.0], [1.0, 1.0]),
    }
}

impl Component {
    pub fn tf(&self, p: [f32; 2]) -> [f32; 2] {
        let (mut x, mut y) = (p[0], p[1]);
        if self.mirror {
            x = -x;
        }
        // counter-clockwise on screen: pin 1 of a once-rotated part ends up on the left
        for _ in 0..(self.rot % 4) {
            (x, y) = (y, -x);
        }
        [x + self.pos[0] as f32, y + self.pos[1] as f32]
    }
    pub fn local_pins(&self) -> &[P] {
        if self.kind == Kind::Subckt {
            &self.pins
        } else {
            pins(self.kind)
        }
    }
    /// A subcircuit is drawn as the box through its pins (at least 2 units each way).
    pub fn local_bbox(&self) -> ([f32; 2], [f32; 2]) {
        if self.kind != Kind::Subckt || self.pins.is_empty() {
            return bbox(self.kind);
        }
        let (mut a, mut b) = ([f32::MAX; 2], [f32::MIN; 2]);
        for p in &self.pins {
            for i in 0..2 {
                (a[i], b[i]) = (a[i].min(p[i] as f32), b[i].max(p[i] as f32));
            }
        }
        for i in 0..2 {
            if b[i] - a[i] < 2.0 {
                (a[i], b[i]) = (a[i] - 1.0, b[i] + 1.0);
            }
        }
        (a, b)
    }
    pub fn prims(&self) -> Vec<Prim> {
        if self.kind != Kind::Subckt {
            return symbol(self.kind);
        }
        let (a, b) = self.local_bbox();
        vec![Prim::Line(vec![a, [b[0], a[1]], b, [a[0], b[1]], a])]
    }
    pub fn pin_pos(&self) -> Vec<P> {
        self.local_pins()
            .iter()
            .map(|p| {
                let q = self.tf([p[0] as f32, p[1] as f32]);
                [q[0].round() as i32, q[1].round() as i32]
            })
            .collect()
    }
    pub fn world_bbox(&self) -> ([f32; 2], [f32; 2]) {
        let (a, b) = self.local_bbox();
        let c = [
            self.tf(a),
            self.tf(b),
            self.tf([a[0], b[1]]),
            self.tf([b[0], a[1]]),
        ];
        let min = [
            c.iter().map(|p| p[0]).fold(f32::MAX, f32::min),
            c.iter().map(|p| p[1]).fold(f32::MAX, f32::min),
        ];
        let max = [
            c.iter().map(|p| p[0]).fold(f32::MIN, f32::max),
            c.iter().map(|p| p[1]).fold(f32::MIN, f32::max),
        ];
        (min, max)
    }
    /// Signal to probe when the component body is clicked.
    pub fn current_signal(&self) -> Option<String> {
        let n = self.spice_name();
        Some(match self.kind {
            Kind::Ground | Kind::Label | Kind::OpAmp | Kind::Subckt => return None,
            Kind::Npn | Kind::Pnp => format!("Ic({n})"),
            Kind::Nmos | Kind::Pmos | Kind::Nmos4 | Kind::Pmos4 | Kind::Njf | Kind::Pjf => {
                format!("Id({n})")
            }
            _ => format!("I({n})"),
        })
    }
    pub fn spice_name(&self) -> String {
        let p = self.kind.prefix();
        if self.kind == Kind::OpAmp {
            return format!("X{}", self.name);
        }
        if p.is_empty() || self.name.to_ascii_uppercase().starts_with(p) {
            self.name.clone()
        } else {
            format!("{p}{}", self.name)
        }
    }
}

pub fn on_segment(p: P, w: &Wire) -> bool {
    let (ax, ay, bx, by) = (w.a[0] as i64, w.a[1] as i64, w.b[0] as i64, w.b[1] as i64);
    let (px, py) = (p[0] as i64, p[1] as i64);
    let cross = (bx - ax) * (py - ay) - (by - ay) * (px - ax);
    cross == 0 && px >= ax.min(bx) && px <= ax.max(bx) && py >= ay.min(by) && py <= ay.max(by)
}

struct Dsu(Vec<usize>);
impl Dsu {
    fn find(&mut self, i: usize) -> usize {
        let mut r = i;
        while self.0[r] != r {
            r = self.0[r];
        }
        let mut i = i;
        while self.0[i] != r {
            let n = self.0[i];
            self.0[i] = r;
            i = n;
        }
        r
    }
    fn union(&mut self, a: usize, b: usize) {
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            self.0[a] = b;
        }
    }
}

pub const OPAMP_SUBCKT: &str = "\
.subckt swiftsim_opamp inp inn out params: aol=100k gbw=10meg rout=10
Rin inp inn 10meg
G1 0 n1 inp inn 1m
R1 n1 0 {aol/1m}
C1 n1 0 {1m/(2*pi*gbw)}
E1 o 0 n1 0 1
Rout o out {rout}
.ends";

pub struct Netlist {
    pub text: String,
    /// Net name of every connection point.
    pub nets: HashMap<P, String>,
    /// Number of connections at each point (for junction dots).
    pub degree: HashMap<P, usize>,
    /// Pin positions of parts (not grounds or labels), for open-pin marks.
    pub pins: HashSet<P>,
}

/// Points by column and by row, so a wire is tested only against the points on its line.
struct Lines<'a> {
    pts: &'a [P],
    by_x: HashMap<i32, Vec<usize>>,
    by_y: HashMap<i32, Vec<usize>>,
}

impl<'a> Lines<'a> {
    fn new(pts: &'a [P]) -> Lines<'a> {
        let (mut by_x, mut by_y) = (
            HashMap::<i32, Vec<usize>>::new(),
            HashMap::<i32, Vec<usize>>::new(),
        );
        for (i, p) in pts.iter().enumerate() {
            by_x.entry(p[0]).or_default().push(i);
            by_y.entry(p[1]).or_default().push(i);
        }
        Lines { pts, by_x, by_y }
    }

    /// Indices of the points on wire `w` (diagonal wires fall back to testing every point).
    fn on(&self, w: &Wire) -> Vec<usize> {
        let cand: Vec<usize> = if w.a[0] == w.b[0] {
            self.by_x.get(&w.a[0]).cloned().unwrap_or_default()
        } else if w.a[1] == w.b[1] {
            self.by_y.get(&w.a[1]).cloned().unwrap_or_default()
        } else {
            (0..self.pts.len()).collect()
        };
        cand.into_iter()
            .filter(|&i| on_segment(self.pts[i], w))
            .collect()
    }
}

/// A net label as a SPICE node name: whitespace would split the card.
fn node_name(label: &str) -> String {
    label.split_whitespace().collect::<Vec<_>>().join("_")
}

impl Schematic {
    pub fn connectivity(&self) -> (Vec<P>, Vec<usize>, HashMap<P, usize>) {
        let mut pts: Vec<P> = vec![];
        let mut idx: HashMap<P, usize> = HashMap::new();
        let mut add = |p: P, pts: &mut Vec<P>| {
            *idx.entry(p).or_insert_with(|| {
                pts.push(p);
                pts.len() - 1
            })
        };
        for c in &self.comps {
            for p in c.pin_pos() {
                add(p, &mut pts);
            }
        }
        for w in &self.wires {
            add(w.a, &mut pts);
            add(w.b, &mut pts);
        }
        let mut d = Dsu((0..pts.len()).collect());
        let lines = Lines::new(&pts);
        for w in &self.wires {
            let ia = idx[&w.a];
            for i in lines.on(w) {
                d.union(ia, i);
            }
        }
        // labels with the same name are one net; all grounds are one net
        let mut by_name: HashMap<String, usize> = HashMap::new();
        for c in &self.comps {
            let key = match c.kind {
                Kind::Ground => "0".to_string(),
                Kind::Label if !c.value.trim().is_empty() => {
                    swiftsim::expr::fold(&node_name(&c.value))
                }
                _ => continue,
            };
            let Some(p) = c.pin_pos().first().copied() else {
                continue;
            };
            let i = idx[&p];
            match by_name.get(&key) {
                Some(&j) => d.union(i, j),
                None => {
                    by_name.insert(key, i);
                }
            }
        }
        let roots: Vec<usize> = (0..pts.len()).map(|i| d.find(i)).collect();
        (pts, roots, idx)
    }

    /// Why this schematic can't be loaded, if it can't: a subcircuit part needs pins.
    pub fn check(&self) -> Result<(), String> {
        match self
            .comps
            .iter()
            .find(|c| c.kind == Kind::Subckt && c.pins.is_empty())
        {
            Some(c) => Err(format!(
                "part {} (subcircuit {}) has no pins",
                c.spice_name(),
                c.value.trim()
            )),
            None => Ok(()),
        }
    }

    /// Warnings for net labels that are different words but one net, such as `kır` and `kir`,
    /// and for labels with spaces (netlisted with `_`).
    pub fn name_conflicts(&self) -> Vec<String> {
        let mut first: HashMap<String, &str> = HashMap::new();
        let mut out = vec![];
        for c in self.comps.iter().filter(|c| c.kind == Kind::Label) {
            let v = c.value.trim();
            if v.contains(char::is_whitespace) {
                let msg = format!(
                    "warning: net label '{v}' contains spaces; it is the net '{}'",
                    node_name(v)
                );
                if !out.contains(&msg) {
                    out.push(msg);
                }
            }
            match first.get(&swiftsim::expr::fold(v)) {
                None => {
                    first.insert(swiftsim::expr::fold(v), v);
                }
                Some(f) if swiftsim::expr::distinct_spellings(f, v) => {
                    let msg = format!(
                        "warning: net labels '{f}' and '{v}' are one net (names ignore case, and ı and İ count as i)"
                    );
                    if !out.contains(&msg) {
                        out.push(msg);
                    }
                }
                _ => {}
            }
        }
        out
    }

    pub fn netlist(&self) -> Netlist {
        let (pts, roots, idx) = self.connectivity();
        let mut names: HashMap<usize, String> = HashMap::new();
        for c in &self.comps {
            // only grounds and labels name nets (both have one pin)
            let root = || c.pin_pos().first().map(|p| roots[idx[p]]);
            match c.kind {
                Kind::Ground => {
                    if let Some(r) = root() {
                        names.insert(r, "0".into());
                    }
                }
                Kind::Label if !c.value.trim().is_empty() => {
                    if let Some(r) = root() {
                        names.entry(r).or_insert_with(|| node_name(&c.value));
                    }
                }
                _ => {}
            }
        }
        let mut k = 0;
        for &r in &roots {
            names.entry(r).or_insert_with(|| {
                k += 1;
                format!("N{k:03}")
            });
        }
        let nets: HashMap<P, String> = pts
            .iter()
            .enumerate()
            .map(|(i, p)| (*p, names[&roots[i]].clone()))
            .collect();
        let mut degree: HashMap<P, usize> = HashMap::new();
        let mut pins = HashSet::new();
        for c in &self.comps {
            for p in c.pin_pos() {
                *degree.entry(p).or_default() += 1;
                if !matches!(c.kind, Kind::Ground | Kind::Label) {
                    pins.insert(p);
                }
            }
        }
        let lines = Lines::new(&pts);
        for w in &self.wires {
            for i in lines.on(w) {
                let p = pts[i];
                *degree.entry(p).or_default() += if p == w.a || p == w.b { 1 } else { 2 };
            }
        }

        let mut t = String::from("* SwiftSim schematic\n");
        let mut opamp = false;
        for c in &self.comps {
            let n: Vec<&str> = c.pin_pos().iter().map(|p| nets[p].as_str()).collect();
            // one card per part: a line break in a value (possible in a .sws) would start another
            let v = c.value.trim().replace(['\n', '\r'], " ");
            let v = v.as_str();
            let name = node_name(&c.spice_name());
            let line = match c.kind {
                Kind::Ground | Kind::Label => continue,
                Kind::Resistor
                | Kind::Capacitor
                | Kind::Inductor
                | Kind::Voltage
                | Kind::Current
                | Kind::Behavioral => {
                    format!("{name} {} {} {v}", n[0], n[1])
                }
                Kind::Diode => format!(
                    "{name} {} {} {}",
                    n[0],
                    n[1],
                    if v.is_empty() { "D" } else { v }
                ),
                Kind::Npn
                | Kind::Pnp
                | Kind::Nmos
                | Kind::Pmos
                | Kind::Nmos4
                | Kind::Pmos4
                | Kind::Njf
                | Kind::Pjf => {
                    let def = match c.kind {
                        Kind::Npn => "NPN",
                        Kind::Pnp => "PNP",
                        Kind::Nmos | Kind::Nmos4 => "NMOS",
                        Kind::Njf => "NJF",
                        Kind::Pjf => "PJF",
                        _ => "PMOS",
                    };
                    format!(
                        "{name} {} {}",
                        n.join(" "),
                        if v.is_empty() { def } else { v }
                    )
                }
                Kind::OpAmp => {
                    opamp = true;
                    format!("{name} {} {} {} swiftsim_opamp {v}", n[0], n[1], n[2])
                }
                Kind::Vcvs | Kind::Vccs | Kind::Switch => {
                    format!("{name} {} {} {} {} {v}", n[0], n[1], n[2], n[3])
                }
                Kind::Subckt => format!("{name} {} {v}", n.join(" ")),
            };
            t.push_str(&line);
            t.push('\n');
        }
        for d in &self.texts {
            for l in d.text.lines() {
                t.push_str(l.trim());
                t.push('\n');
            }
        }
        if opamp {
            t.push_str(OPAMP_SUBCKT);
            t.push('\n');
        }
        t.push_str(".end\n");
        Netlist {
            text: t,
            nets,
            degree,
            pins,
        }
    }

    /// Current file format. 1: op-amp + input moved to the top.
    pub const FORMAT: u32 = 1;

    /// Brings a schematic read from an older file to the current format. Every pin keeps its
    /// position, so connections and simulation results are unchanged.
    pub fn upgrade(&mut self) {
        if self.format < 1 {
            // format 0 op-amps had + at the bottom: flip vertically (mirror + half turn)
            for c in self.comps.iter_mut().filter(|c| c.kind == Kind::OpAmp) {
                c.rot = (c.rot + 2) % 4;
                c.mirror = !c.mirror;
            }
        }
        self.format = Self::FORMAT;
    }

    /// Nets of the net labels, in placement order, for auto-plotting. A label on a grounded
    /// net is skipped: that net is `0` (always 0 V), so `V(label)` would not exist.
    pub fn label_nets(&self) -> Vec<String> {
        let n = self.netlist();
        let mut v: Vec<String> = vec![];
        for c in self.comps.iter().filter(|c| c.kind == Kind::Label) {
            let Some(net) = c.pin_pos().first().map(|p| &n.nets[p]) else {
                continue;
            };
            if net != "0" && !v.contains(net) {
                v.push(net.clone());
            }
        }
        v
    }

    pub fn next_name(&self, k: Kind) -> String {
        let p = k.prefix();
        if p.is_empty() {
            return String::new();
        }
        let used: Vec<u32> = self
            .comps
            .iter()
            .filter(|c| c.kind.prefix() == p)
            .filter_map(|c| {
                c.name
                    .to_ascii_uppercase()
                    .strip_prefix(p)
                    .and_then(|s| s.parse().ok())
            })
            .collect();
        let mut i = 1;
        while used.contains(&i) {
            i += 1;
        }
        format!("{p}{i}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comp(kind: Kind, name: &str, value: &str, pos: P) -> Component {
        Component {
            kind,
            name: name.into(),
            value: value.into(),
            pos,
            rot: 0,
            mirror: false,
            pins: vec![],
        }
    }

    #[test]
    fn divider_netlist_runs() {
        // V1 at (0,0): pins (0,-2) (0,2). R1 at (4,0) pins (4,-2) (4,2); R2 at (4,4) pins (4,2) (4,6)
        let s = Schematic {
            comps: vec![
                comp(Kind::Voltage, "V1", "10", [0, 0]),
                comp(Kind::Resistor, "R1", "1k", [4, 0]),
                comp(Kind::Resistor, "R2", "1k", [4, 4]),
                comp(Kind::Ground, "", "", [0, 6]),
                comp(Kind::Ground, "", "", [4, 6]),
                comp(Kind::Label, "", "out", [6, 2]),
            ],
            wires: vec![
                Wire {
                    a: [0, -2],
                    b: [4, -2],
                },
                Wire {
                    a: [0, 2],
                    b: [0, 6],
                },
                Wire {
                    a: [4, 2],
                    b: [6, 2],
                },
            ],
            texts: vec![Text {
                pos: [0, 10],
                text: ".op".into(),
            }],
            ..Default::default()
        };
        let n = s.netlist();
        assert!(n.text.contains("R2 out 0 1k"), "{}", n.text);
        let r = swiftsim::run_netlist(&n.text, None, &swiftsim::Control::default()).unwrap();
        let p = &r.plots[0];
        let v = p.re[p.index("v(out)").unwrap()][0];
        assert!((v - 5.0).abs() < 1e-6);
    }

    #[test]
    fn old_files_keep_op_amp_pins_when_upgraded() {
        // a format-0 file (no "format" field): op-amp pins were inp (-2,1), inn (-2,-1), out (2,0)
        for rot in 0..4u8 {
            for mirror in [false, true] {
                let c = Component {
                    rot,
                    mirror,
                    ..comp(Kind::OpAmp, "U1", "", [5, 7])
                };
                let old: Vec<P> = [[-2.0, 1.0], [-2.0, -1.0], [2.0, 0.0]]
                    .iter()
                    .map(|&p| c.tf(p))
                    .map(|q| [q[0].round() as i32, q[1].round() as i32])
                    .collect();
                let json = format!(
                    r#"{{"comps":[{}],"wires":[],"texts":[]}}"#,
                    serde_json::to_string(&c).unwrap()
                );
                let mut s: Schematic = serde_json::from_str(&json).unwrap();
                assert_eq!(s.format, 0);
                s.upgrade();
                assert_eq!(
                    (s.format, s.comps[0].pin_pos()),
                    (Schematic::FORMAT, old.clone()),
                    "rot {rot} mirror {mirror}"
                );
                s.upgrade(); // idempotent
                assert_eq!(s.comps[0].pin_pos(), old);
            }
        }
        // new files say format 1 and are left alone
        let json = serde_json::to_string(&Schematic {
            comps: vec![comp(Kind::OpAmp, "U1", "", [0, 0])],
            ..Default::default()
        })
        .unwrap();
        let mut s: Schematic = serde_json::from_str(&json).unwrap();
        s.upgrade();
        assert_eq!((s.comps[0].rot, s.comps[0].mirror), (0, false));
    }

    #[test]
    fn labels_on_ground_are_not_plotted() {
        // lab Fig. 4: B is wired straight to ground, A and C are real nets
        let s = Schematic {
            comps: vec![
                comp(Kind::Resistor, "R2", "1k", [0, 0]),
                comp(Kind::Ground, "", "", [0, 2]),
                comp(Kind::Label, "", "A", [0, -2]),
                comp(Kind::Label, "", "B", [4, 0]),
                comp(Kind::Ground, "", "", [4, 2]),
                comp(Kind::Label, "", "C", [8, 0]),
                comp(Kind::Label, "", "A", [0, -2]),
            ],
            wires: vec![Wire {
                a: [4, 0],
                b: [4, 2],
            }],
            texts: vec![],
            ..Default::default()
        };
        assert_eq!(s.label_nets(), ["A", "C"]);
    }

    #[test]
    fn t_junction_mid_wire_connects() {
        let s = Schematic {
            comps: vec![],
            wires: vec![
                Wire {
                    a: [0, 0],
                    b: [10, 0],
                },
                Wire {
                    a: [5, 0],
                    b: [5, 5],
                },
            ],
            texts: vec![],
            ..Default::default()
        };
        let n = s.netlist();
        assert_eq!(n.nets[&[0, 0]], n.nets[&[5, 5]]);
        assert_eq!(n.degree[&[5, 0]], 3);
    }

    /// A subcircuit part without pins (odd `.asy`, hand-made `.sws`) used to panic the netlister
    /// on every frame; it is a load error now and netlists without a panic.
    #[test]
    fn subcircuit_without_pins() {
        let s: Schematic =
            serde_json::from_str(include_str!("../../tests/fixtures/audit/nopin.sws")).unwrap();
        assert!(
            s.check()
                .is_err_and(|e| e.contains("X1") && e.contains("no pins"))
        );
        assert!(s.netlist().text.contains("X1  foo"));
        assert!(s.label_nets().is_empty() && s.name_conflicts().is_empty());
        assert!(Schematic::default().check().is_ok());
    }

    #[test]
    fn labels_and_values_cannot_break_cards() {
        let s = Schematic {
            comps: vec![
                comp(Kind::Resistor, "R1", "1k\nV9 x 0 5", [0, 0]),
                comp(Kind::Label, "", "my  net", [0, -2]),
                comp(Kind::Ground, "", "", [0, 2]),
            ],
            ..Default::default()
        };
        let n = s.netlist();
        assert!(n.text.contains("R1 my_net 0 1k V9 x 0 5\n"), "{}", n.text);
        assert!(!n.text.lines().any(|l| l.starts_with("V9")));
        assert_eq!(s.label_nets(), ["my_net"]);
        assert!(
            s.name_conflicts()
                .iter()
                .any(|w| w.contains("my  net") && w.contains("my_net"))
        );
    }

    #[test]
    fn diagonal_wires_still_connect() {
        let s = Schematic {
            wires: vec![
                Wire {
                    a: [0, 0],
                    b: [4, 4],
                },
                Wire {
                    a: [2, 2],
                    b: [2, 9],
                },
            ],
            ..Default::default()
        };
        let n = s.netlist();
        assert_eq!(n.nets[&[0, 0]], n.nets[&[2, 9]]);
        assert_eq!(n.degree[&[2, 2]], 3);
    }
}
