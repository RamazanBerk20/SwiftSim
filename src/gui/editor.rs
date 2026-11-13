//! Schematic canvas: tools, selection, editing, probing, drawing.

use super::schematic::*;
use eframe::egui;
use egui::{
    Align2, Color32, CornerRadius, FontId, Key, Modifiers, PointerButton, Pos2, Rect, Sense, Shape,
    Stroke, StrokeKind, Vec2,
};
use std::collections::{HashMap, HashSet};

/// The canvas's interaction mode, with the state that only exists in that mode.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Tool {
    Select,
    /// `from`: the start of the wire being drawn.
    Wire {
        from: Option<P>,
    },
    /// The part follows the pointer, turned and mirrored as it will be placed.
    Place {
        kind: Kind,
        rot: u8,
        mirror: bool,
    },
    Text,
    Probe,
}

impl Tool {
    pub const WIRE: Tool = Tool::Wire { from: None };

    pub fn place(kind: Kind) -> Tool {
        Tool::Place {
            kind,
            rot: 0,
            mirror: false,
        }
    }

    /// The same tool, whatever its in-progress state (for toolbar highlighting).
    pub fn same(self, o: Tool) -> bool {
        match (self, o) {
            (Tool::Place { kind: a, .. }, Tool::Place { kind: b, .. }) => a == b,
            _ => std::mem::discriminant(&self) == std::mem::discriminant(&o),
        }
    }
}

#[derive(Default, Clone)]
pub struct Sel {
    pub comps: HashSet<usize>,
    pub wires: HashSet<usize>,
    pub texts: HashSet<usize>,
}

impl Sel {
    pub fn is_empty(&self) -> bool {
        self.comps.is_empty() && self.wires.is_empty() && self.texts.is_empty()
    }
    pub fn clear(&mut self) {
        *self = Sel::default();
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Hit {
    None,
    Comp(usize),
    Wire(usize),
    Text(usize),
}

enum Drag {
    None,
    Move {
        last: P,
        moved: bool,
        ends: Vec<(usize, bool)>,
    },
    Boxing {
        start: Pos2,
    },
}

struct Pal {
    bg: Color32,
    grid: Color32,
    wire: Color32,
    comp: Color32,
    sel: Color32,
    text: Color32,
    label: Color32,
    dir: Color32,
    pin: Color32,
    probe: Color32,
}

fn pal(dark: bool) -> Pal {
    if dark {
        Pal {
            bg: Color32::from_rgb(24, 26, 30),
            grid: Color32::from_rgb(52, 56, 64),
            wire: Color32::from_rgb(64, 196, 255),
            comp: Color32::from_rgb(222, 226, 232),
            sel: Color32::from_rgb(255, 176, 64),
            text: Color32::from_rgb(170, 176, 186),
            label: Color32::from_rgb(206, 147, 216),
            dir: Color32::from_rgb(140, 210, 110),
            pin: Color32::from_rgb(255, 99, 99),
            probe: Color32::from_rgb(255, 235, 59),
        }
    } else {
        Pal {
            bg: Color32::from_rgb(250, 250, 248),
            grid: Color32::from_rgb(205, 208, 214),
            wire: Color32::from_rgb(0, 102, 204),
            comp: Color32::from_rgb(30, 32, 36),
            sel: Color32::from_rgb(230, 120, 0),
            text: Color32::from_rgb(80, 84, 92),
            label: Color32::from_rgb(140, 40, 160),
            dir: Color32::from_rgb(40, 130, 40),
            pin: Color32::from_rgb(220, 40, 40),
            probe: Color32::from_rgb(200, 150, 0),
        }
    }
}

pub struct Editor {
    pub sch: Schematic,
    pub tool: Tool,
    pub sel: Sel,
    offset: Vec2,
    zoom: f32,
    drag: Drag,
    undo: Vec<Schematic>,
    redo: Vec<Schematic>,
    clip: Option<Schematic>,
    /// Bumped by every edit; the app's document compares it with the saved version.
    version: u64,
    cache: Option<(u64, Netlist)>,
    text_rects: Vec<Rect>,
    /// Signal picked with the probe tool, taken by the app.
    pub probe: Option<String>,
    /// Ask the properties panel to focus the value field.
    pub focus_value: bool,
    /// Operating-point voltages by lowercase net name.
    pub op_values: HashMap<String, f64>,
    pub show_op: bool,
    fit_next: bool,
}

impl Default for Editor {
    fn default() -> Self {
        Editor {
            sch: Schematic::default(),
            tool: Tool::Select,
            sel: Sel::default(),
            offset: Vec2::new(300.0, 200.0),
            zoom: 16.0,
            drag: Drag::None,
            undo: vec![],
            redo: vec![],
            clip: None,
            version: 0,
            cache: None,
            text_rects: vec![],
            probe: None,
            focus_value: false,
            op_values: HashMap::new(),
            show_op: true,
            fit_next: false,
        }
    }
}

fn dist_seg(p: [f32; 2], a: P, b: P) -> f32 {
    let (ax, ay, bx, by) = (a[0] as f32, a[1] as f32, b[0] as f32, b[1] as f32);
    let (dx, dy) = (bx - ax, by - ay);
    let l2 = dx * dx + dy * dy;
    let t = if l2 == 0.0 {
        0.0
    } else {
        (((p[0] - ax) * dx + (p[1] - ay) * dy) / l2).clamp(0.0, 1.0)
    };
    ((p[0] - ax - t * dx).powi(2) + (p[1] - ay - t * dy).powi(2)).sqrt()
}

impl Editor {
    pub fn load(&mut self, sch: Schematic) {
        *self = Editor {
            sch,
            fit_next: true,
            show_op: self.show_op,
            ..Default::default()
        };
    }

    pub fn changed(&mut self) {
        self.version += 1;
        self.op_values.clear();
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn checkpoint(&mut self) {
        self.undo.push(self.sch.clone());
        if self.undo.len() > 200 {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    pub fn undo(&mut self) {
        if let Some(s) = self.undo.pop() {
            self.redo.push(std::mem::replace(&mut self.sch, s));
            self.sel.clear();
            self.changed();
        }
    }

    pub fn redo(&mut self) {
        if let Some(s) = self.redo.pop() {
            self.undo.push(std::mem::replace(&mut self.sch, s));
            self.sel.clear();
            self.changed();
        }
    }

    pub fn netlist(&mut self) -> &Netlist {
        if self.cache.as_ref().is_none_or(|c| c.0 != self.version) {
            self.cache = Some((self.version, self.sch.netlist()));
        }
        &self.cache.as_ref().unwrap().1
    }

    pub fn delete_selected(&mut self) {
        if self.sel.is_empty() {
            return;
        }
        self.checkpoint();
        let s = std::mem::take(&mut self.sel);
        let keep = |set: &HashSet<usize>, i: usize| !set.contains(&i);
        let mut i = 0;
        self.sch.comps.retain(|_| (keep(&s.comps, i), i += 1).0);
        i = 0;
        self.sch.wires.retain(|_| (keep(&s.wires, i), i += 1).0);
        i = 0;
        self.sch.texts.retain(|_| (keep(&s.texts, i), i += 1).0);
        self.changed();
    }

    pub fn rotate(&mut self) {
        if let Tool::Place { rot, .. } = &mut self.tool {
            *rot = (*rot + 1) % 4;
        } else if !self.sel.comps.is_empty() {
            self.checkpoint();
            for &i in &self.sel.comps {
                self.sch.comps[i].rot = (self.sch.comps[i].rot + 1) % 4;
            }
            self.changed();
        }
    }

    pub fn mirror(&mut self) {
        if let Tool::Place { mirror, .. } = &mut self.tool {
            *mirror = !*mirror;
        } else if !self.sel.comps.is_empty() {
            self.checkpoint();
            for &i in &self.sel.comps {
                self.sch.comps[i].mirror = !self.sch.comps[i].mirror;
            }
            self.changed();
        }
    }

    pub fn select_all(&mut self) {
        self.sel.comps = (0..self.sch.comps.len()).collect();
        self.sel.wires = (0..self.sch.wires.len()).collect();
        self.sel.texts = (0..self.sch.texts.len()).collect();
    }

    pub fn copy(&mut self) {
        if self.sel.is_empty() {
            return;
        }
        let s = &self.sch;
        self.clip = Some(Schematic {
            comps: self.sel.comps.iter().map(|&i| s.comps[i].clone()).collect(),
            wires: self.sel.wires.iter().map(|&i| s.wires[i]).collect(),
            texts: self.sel.texts.iter().map(|&i| s.texts[i].clone()).collect(),
            ..Default::default()
        });
    }

    pub fn paste(&mut self) {
        let Some(c) = self.clip.clone() else { return };
        self.checkpoint();
        self.sel.clear();
        let d = [4, 4];
        let add = |p: P| [p[0] + d[0], p[1] + d[1]];
        for mut comp in c.comps {
            comp.pos = add(comp.pos);
            if !comp.kind.prefix().is_empty() {
                comp.name = self.sch.next_name(comp.kind);
            }
            self.sel.comps.insert(self.sch.comps.len());
            self.sch.comps.push(comp);
        }
        for w in c.wires {
            self.sel.wires.insert(self.sch.wires.len());
            self.sch.wires.push(Wire {
                a: add(w.a),
                b: add(w.b),
            });
        }
        for mut t in c.texts {
            t.pos = add(t.pos);
            self.sel.texts.insert(self.sch.texts.len());
            self.sch.texts.push(t);
        }
        // shift the clipboard so repeated pastes cascade
        if let Some(cl) = &mut self.clip {
            cl.comps.iter_mut().for_each(|c| c.pos = add(c.pos));
            cl.wires.iter_mut().for_each(|w| {
                *w = Wire {
                    a: add(w.a),
                    b: add(w.b),
                }
            });
            cl.texts.iter_mut().for_each(|t| t.pos = add(t.pos));
        }
        self.changed();
    }

    pub fn fit(&mut self) {
        self.fit_next = true;
    }

    /// Switches mode; whatever was in progress (a wire, a drag) ends.
    pub fn set_tool(&mut self, t: Tool) {
        // rotation sticks while placing more of the same part, resets on a different one
        if !(matches!(t, Tool::Place { .. }) && t.same(self.tool)) {
            self.tool = t;
        }
        if let Tool::Wire { from } = &mut self.tool {
            *from = None;
        }
        self.drag = Drag::None;
    }

    fn to_screen(&self, r: Rect, p: [f32; 2]) -> Pos2 {
        r.min + self.offset + Vec2::new(p[0], p[1]) * self.zoom
    }
    fn to_world(&self, r: Rect, s: Pos2) -> [f32; 2] {
        let v = (s - r.min - self.offset) / self.zoom;
        [v.x, v.y]
    }
    fn snap(w: [f32; 2]) -> P {
        [w[0].round() as i32, w[1].round() as i32]
    }

    fn hit(&self, w: [f32; 2], r: Rect) -> Hit {
        let sp = self.to_screen(r, w);
        if let Some(i) = self
            .text_rects
            .iter()
            .position(|t| t.expand(2.0).contains(sp))
        {
            return Hit::Text(i);
        }
        for (i, c) in self.sch.comps.iter().enumerate().rev() {
            let (a, b) = c.world_bbox();
            if w[0] >= a[0] - 0.2 && w[0] <= b[0] + 0.2 && w[1] >= a[1] - 0.2 && w[1] <= b[1] + 0.2
            {
                return Hit::Comp(i);
            }
        }
        let tol = (6.0 / self.zoom).max(0.25);
        self.sch
            .wires
            .iter()
            .enumerate()
            .filter(|(_, wi)| dist_seg(w, wi.a, wi.b) < tol)
            .map(|(i, _)| Hit::Wire(i))
            .next()
            .unwrap_or(Hit::None)
    }

    fn is_connection(&self, p: P) -> bool {
        self.sch.comps.iter().any(|c| c.pin_pos().contains(&p))
            || self.sch.wires.iter().any(|w| on_segment(p, w))
    }

    /// Net under a world point (wire or pin), if any.
    fn net_at(&mut self, w: [f32; 2]) -> Option<String> {
        let g = Self::snap(w);
        let tol = (6.0 / self.zoom).max(0.25);
        let wire = self
            .sch
            .wires
            .iter()
            .find(|wi| dist_seg(w, wi.a, wi.b) < tol)
            .map(|wi| wi.a);
        let pin = self
            .sch
            .comps
            .iter()
            .flat_map(|c| c.pin_pos())
            .find(|p| *p == g && (w[0] - g[0] as f32).hypot(w[1] - g[1] as f32) < 0.4);
        let p = wire.or(pin)?;
        self.netlist().nets.get(&p).cloned()
    }

    fn add_wire(&mut self, a: P, b: P) {
        if a == b {
            return;
        }
        if a[0] == b[0] || a[1] == b[1] {
            self.sch.wires.push(Wire { a, b });
        } else {
            let m = [b[0], a[1]];
            self.sch.wires.push(Wire { a, b: m });
            self.sch.wires.push(Wire { a: m, b });
        }
    }

    pub fn place_text(&mut self, text: &str) {
        self.checkpoint();
        let y = self
            .sch
            .comps
            .iter()
            .map(|c| c.world_bbox().1[1] as i32)
            .chain(self.sch.wires.iter().map(|w| w.a[1].max(w.b[1])))
            .chain(self.sch.texts.iter().map(|t| t.pos[1] + 2))
            .max()
            .unwrap_or(0)
            + 3;
        let x = self.sch.comps.iter().map(|c| c.pos[0]).min().unwrap_or(0);
        self.sch.texts.push(Text {
            pos: [x, y],
            text: text.into(),
        });
        self.changed();
    }

    fn handle_keys(&mut self, ui: &egui::Ui) {
        if ui.ctx().egui_wants_keyboard_input() {
            return;
        }
        let cmd = |k: Key| ui.input_mut(|i| i.consume_key(Modifiers::COMMAND, k));
        let cmd_shift =
            |k: Key| ui.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, k));
        if cmd_shift(Key::Z) || cmd(Key::Y) {
            self.redo();
        }
        if cmd(Key::Z) {
            self.undo();
        }
        if cmd(Key::R) {
            self.rotate();
        }
        if cmd(Key::E) {
            self.mirror();
        }
        if cmd(Key::A) {
            self.select_all();
        }
        if cmd(Key::C) {
            self.copy();
        }
        if cmd(Key::X) {
            self.copy();
            self.delete_selected();
        }
        if cmd(Key::V) {
            self.paste();
        }
        if cmd(Key::D) {
            self.copy();
            self.paste();
        }
        let plain = |k: Key| ui.input_mut(|i| i.consume_key(Modifiers::NONE, k));
        if plain(Key::Escape) {
            if let Tool::Wire {
                from: from @ Some(_),
            } = &mut self.tool
            {
                *from = None;
            } else if self.tool != Tool::Select {
                self.set_tool(Tool::Select);
            } else {
                self.sel.clear();
            }
        }
        if plain(Key::Delete) || plain(Key::Backspace) {
            self.delete_selected();
        }
        if plain(Key::Space) {
            self.fit();
        }
        for k in Kind::ALL {
            if let Some(s) = k.shortcut() {
                if let Some(key) = Key::from_name(s) {
                    if plain(key) {
                        self.set_tool(Tool::place(k));
                    }
                }
            }
        }
        if plain(Key::W) || plain(Key::F3) {
            self.set_tool(Tool::WIRE);
        }
        if plain(Key::F4) {
            self.set_tool(Tool::place(Kind::Label));
        }
        if plain(Key::T) || plain(Key::S) || plain(Key::Period) {
            self.set_tool(Tool::Text);
        }
        if plain(Key::P) {
            self.set_tool(Tool::Probe);
        }
    }

    fn do_fit(&mut self, r: Rect) {
        let mut pts: Vec<[f32; 2]> = vec![];
        for c in &self.sch.comps {
            let (a, b) = c.world_bbox();
            pts.push(a);
            pts.push(b);
        }
        for w in &self.sch.wires {
            pts.push([w.a[0] as f32, w.a[1] as f32]);
            pts.push([w.b[0] as f32, w.b[1] as f32]);
        }
        for t in &self.sch.texts {
            let lines = t.text.lines().count().max(1) as f32;
            let width = t.text.lines().map(|l| l.len()).max().unwrap_or(4) as f32 * 0.5;
            pts.push([t.pos[0] as f32, t.pos[1] as f32]);
            pts.push([t.pos[0] as f32 + width, t.pos[1] as f32 + lines]);
        }
        if pts.is_empty() {
            self.zoom = 16.0;
            self.offset = r.size() / 2.0;
            return;
        }
        let (mut a, mut b) = ([f32::MAX; 2], [f32::MIN; 2]);
        for p in pts {
            a = [a[0].min(p[0]), a[1].min(p[1])];
            b = [b[0].max(p[0]), b[1].max(p[1])];
        }
        let (w, h) = ((b[0] - a[0]).max(4.0) + 4.0, (b[1] - a[1]).max(4.0) + 4.0);
        self.zoom = (r.width() / w).min(r.height() / h).clamp(3.0, 40.0);
        let c = Vec2::new((a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0);
        self.offset = r.size() / 2.0 - c * self.zoom;
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let rect = ui.available_rect_before_wrap();
        let resp = ui.allocate_rect(rect, Sense::click_and_drag());
        if self.fit_next {
            self.fit_next = false;
            self.do_fit(rect);
        }
        if resp.hovered() || resp.has_focus() {
            self.handle_keys(ui);
        }
        let hover = resp.hover_pos();
        let hw = hover.map(|p| self.to_world(rect, p));
        let hg = hw.map(Self::snap);

        // zoom & pan
        if resp.hovered() {
            let (scroll, zoomd) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta()));
            let mut f = zoomd;
            if scroll.y != 0.0 {
                f *= (scroll.y * 0.0025).exp();
            }
            if f != 1.0 {
                if let Some(p) = hover {
                    let w = self.to_world(rect, p);
                    self.zoom = (self.zoom * f).clamp(3.0, 80.0);
                    self.offset = p - rect.min - Vec2::new(w[0], w[1]) * self.zoom;
                }
            }
        }
        if resp.dragged_by(PointerButton::Middle) || resp.dragged_by(PointerButton::Secondary) {
            self.offset += resp.drag_delta();
        }

        let shift = ui.input(|i| i.modifiers.shift);
        match self.tool {
            Tool::Select => self.select_input(&resp, rect, hw, hg, shift),
            Tool::Wire { from } => {
                let mut next = from;
                if let Some(g) = hg {
                    if resp.double_clicked() {
                        next = None;
                    } else if resp.clicked() {
                        match from {
                            None => next = Some(g),
                            Some(f) if f != g => {
                                let end = self.is_connection(g);
                                self.checkpoint();
                                self.add_wire(f, g);
                                self.changed();
                                next = if end { None } else { Some(g) };
                            }
                            _ => {}
                        }
                    }
                }
                if resp.secondary_clicked() {
                    next = None;
                }
                self.tool = Tool::Wire { from: next };
            }
            Tool::Place {
                kind: k,
                rot,
                mirror,
            } => {
                if resp.clicked() {
                    if let Some(g) = hg {
                        self.checkpoint();
                        let name = self.sch.next_name(k);
                        self.sch.comps.push(Component {
                            kind: k,
                            name,
                            value: k.default_value().into(),
                            pos: g,
                            rot,
                            mirror,
                            pins: vec![],
                        });
                        self.changed();
                    }
                }
                if resp.secondary_clicked() {
                    self.set_tool(Tool::Select);
                }
            }
            Tool::Text => {
                if resp.clicked() {
                    if let Some(g) = hg {
                        self.checkpoint();
                        self.sch.texts.push(Text {
                            pos: g,
                            text: ".tran 10m".into(),
                        });
                        self.sel.clear();
                        self.sel.texts.insert(self.sch.texts.len() - 1);
                        self.changed();
                        self.set_tool(Tool::Select);
                        self.focus_value = true;
                    }
                }
            }
            Tool::Probe => {
                if resp.clicked() {
                    if let Some(w) = hw {
                        self.probe = match self.net_at(w) {
                            Some(n) if n != "0" => Some(format!("V({n})")),
                            Some(_) => None,
                            None => match self.hit(w, rect) {
                                Hit::Comp(i) => self.sch.comps[i].current_signal(),
                                _ => None,
                            },
                        };
                    }
                }
                if resp.secondary_clicked() {
                    self.set_tool(Tool::Select);
                }
            }
        }

        let cursor = match self.tool {
            Tool::Select => egui::CursorIcon::Default,
            Tool::Probe => egui::CursorIcon::PointingHand,
            _ => egui::CursorIcon::Crosshair,
        };
        if resp.hovered() {
            ui.ctx().set_cursor_icon(cursor);
        }
        self.draw(ui, rect, hw, hg);
    }

    fn select_input(
        &mut self,
        resp: &egui::Response,
        rect: Rect,
        hw: Option<[f32; 2]>,
        hg: Option<P>,
        shift: bool,
    ) {
        if resp.drag_started_by(PointerButton::Primary) {
            let press = resp.interact_pointer_pos().unwrap_or_default();
            let w = self.to_world(rect, press);
            let h = self.hit(w, rect);
            let selected = match h {
                Hit::Comp(i) => self.sel.comps.contains(&i),
                Hit::Wire(i) => self.sel.wires.contains(&i),
                Hit::Text(i) => self.sel.texts.contains(&i),
                Hit::None => false,
            };
            if h == Hit::None {
                if !shift {
                    self.sel.clear();
                }
                self.drag = Drag::Boxing { start: press };
            } else {
                if !selected {
                    if !shift {
                        self.sel.clear();
                    }
                    match h {
                        Hit::Comp(i) => self.sel.comps.insert(i),
                        Hit::Wire(i) => self.sel.wires.insert(i),
                        Hit::Text(i) => self.sel.texts.insert(i),
                        Hit::None => false,
                    };
                }
                // wires attached to moving pins / wire ends follow (rubber band)
                let mut anchors: HashSet<P> = self
                    .sel
                    .comps
                    .iter()
                    .flat_map(|&i| self.sch.comps[i].pin_pos())
                    .collect();
                anchors.extend(
                    self.sel
                        .wires
                        .iter()
                        .flat_map(|&i| [self.sch.wires[i].a, self.sch.wires[i].b]),
                );
                let mut ends = vec![];
                for (i, wi) in self.sch.wires.iter().enumerate() {
                    if self.sel.wires.contains(&i) {
                        continue;
                    }
                    if anchors.contains(&wi.a) {
                        ends.push((i, true));
                    }
                    if anchors.contains(&wi.b) {
                        ends.push((i, false));
                    }
                }
                self.drag = Drag::Move {
                    last: Self::snap(w),
                    moved: false,
                    ends,
                };
            }
        }
        if resp.dragged_by(PointerButton::Primary) {
            if let (Drag::Move { last, moved, ends }, Some(g)) = (&mut self.drag, hg) {
                let d = [g[0] - last[0], g[1] - last[1]];
                if d != [0, 0] {
                    if !*moved {
                        self.undo.push(self.sch.clone());
                        self.redo.clear();
                        *moved = true;
                    }
                    *last = g;
                    let mv = |p: &mut P| *p = [p[0] + d[0], p[1] + d[1]];
                    for &i in &self.sel.comps {
                        mv(&mut self.sch.comps[i].pos);
                    }
                    for &i in &self.sel.texts {
                        mv(&mut self.sch.texts[i].pos);
                    }
                    for &i in &self.sel.wires {
                        mv(&mut self.sch.wires[i].a);
                        mv(&mut self.sch.wires[i].b);
                    }
                    for &(i, a) in ends.iter() {
                        let w = &mut self.sch.wires[i];
                        mv(if a { &mut w.a } else { &mut w.b });
                    }
                    self.changed();
                }
            }
        }
        if resp.drag_stopped() {
            if let Drag::Boxing { start } = self.drag {
                if let Some(end) = resp.interact_pointer_pos().or(resp.hover_pos()) {
                    let (a, b) = (self.to_world(rect, start), self.to_world(rect, end));
                    let (lo, hi) = (
                        [a[0].min(b[0]), a[1].min(b[1])],
                        [a[0].max(b[0]), a[1].max(b[1])],
                    );
                    let inside = |p: [f32; 2]| {
                        p[0] >= lo[0] && p[0] <= hi[0] && p[1] >= lo[1] && p[1] <= hi[1]
                    };
                    for (i, c) in self.sch.comps.iter().enumerate() {
                        let (a, b) = c.world_bbox();
                        if inside(a) && inside(b) {
                            self.sel.comps.insert(i);
                        }
                    }
                    for (i, w) in self.sch.wires.iter().enumerate() {
                        if inside([w.a[0] as f32, w.a[1] as f32])
                            && inside([w.b[0] as f32, w.b[1] as f32])
                        {
                            self.sel.wires.insert(i);
                        }
                    }
                    for (i, t) in self.sch.texts.iter().enumerate() {
                        if inside([t.pos[0] as f32, t.pos[1] as f32]) {
                            self.sel.texts.insert(i);
                        }
                    }
                }
            }
            self.drag = Drag::None;
        }
        if resp.clicked() {
            if let Some(w) = hw {
                let h = self.hit(w, rect);
                if !shift {
                    self.sel.clear();
                }
                match h {
                    Hit::Comp(i) => toggle(&mut self.sel.comps, i),
                    Hit::Wire(i) => toggle(&mut self.sel.wires, i),
                    Hit::Text(i) => toggle(&mut self.sel.texts, i),
                    Hit::None => {}
                }
            }
        }
        if resp.double_clicked() && !self.sel.is_empty() {
            self.focus_value = true;
        }
    }

    fn draw(&mut self, ui: &egui::Ui, rect: Rect, hw: Option<[f32; 2]>, hg: Option<P>) {
        let p = pal(ui.visuals().dark_mode);
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, CornerRadius::ZERO, p.bg);
        let z = self.zoom;
        // grid
        let step = if z < 6.0 {
            4
        } else if z < 10.0 {
            2
        } else {
            1
        };
        let a = self.to_world(rect, rect.min);
        let b = self.to_world(rect, rect.max);
        let (x0, y0) = (
            (a[0] / step as f32).floor() as i32 * step,
            (a[1] / step as f32).floor() as i32 * step,
        );
        let r = if z > 20.0 { 1.4 } else { 1.0 };
        let mut y = y0;
        while (y as f32) <= b[1] {
            let mut x = x0;
            while (x as f32) <= b[0] {
                painter.circle_filled(self.to_screen(rect, [x as f32, y as f32]), r, p.grid);
                x += step;
            }
            y += step;
        }

        let probe_net = match (self.tool, hw) {
            (Tool::Probe, Some(w)) => self.net_at(w),
            _ => None,
        };
        let nl_nets = self.netlist().nets.clone();
        let degree = self.netlist().degree.clone();
        let pins = self.netlist().pins.clone();
        let sw = (z * 0.12).clamp(1.2, 3.0);
        // wires
        for (i, w) in self.sch.wires.iter().enumerate() {
            let hl = probe_net
                .as_ref()
                .is_some_and(|n| nl_nets.get(&w.a) == Some(n));
            let col = if self.sel.wires.contains(&i) {
                p.sel
            } else if hl {
                p.probe
            } else {
                p.wire
            };
            painter.line_segment(
                [
                    self.to_screen(rect, [w.a[0] as f32, w.a[1] as f32]),
                    self.to_screen(rect, [w.b[0] as f32, w.b[1] as f32]),
                ],
                Stroke::new(if hl { sw * 1.8 } else { sw }, col),
            );
        }
        for (pt, d) in &degree {
            let s = self.to_screen(rect, [pt[0] as f32, pt[1] as f32]);
            if *d >= 3 {
                painter.circle_filled(s, (z * 0.22).clamp(2.5, 6.0), p.wire);
            } else if *d == 1 && pins.contains(pt) {
                let h = (z * 0.14).clamp(2.0, 4.0);
                painter.rect_stroke(
                    Rect::from_center_size(s, Vec2::splat(h * 2.0)),
                    CornerRadius::ZERO,
                    Stroke::new(1.0, p.pin),
                    StrokeKind::Middle,
                );
            }
        }
        // components
        let font = FontId::proportional((z * 0.75).clamp(8.0, 22.0));
        let hover_comp = match (self.tool, hw) {
            (Tool::Probe, Some(w)) if probe_net.is_none() => match self.hit(w, rect) {
                Hit::Comp(i) => Some(i),
                _ => None,
            },
            _ => None,
        };
        for (i, c) in self.sch.comps.iter().enumerate() {
            let col = if self.sel.comps.contains(&i) {
                p.sel
            } else if hover_comp == Some(i) {
                p.probe
            } else if c.kind == Kind::Label {
                p.label
            } else {
                p.comp
            };
            self.draw_comp(&painter, rect, c, col, sw, &font, &p);
        }
        // ghost of the part being placed
        if let (
            Tool::Place {
                kind: k,
                rot,
                mirror,
            },
            Some(g),
        ) = (self.tool, hg)
        {
            let ghost = Component {
                kind: k,
                name: String::new(),
                value: k.default_value().into(),
                pos: g,
                rot,
                mirror,
                pins: vec![],
            };
            self.draw_comp(
                &painter,
                rect,
                &ghost,
                p.sel.gamma_multiply(0.6),
                sw,
                &font,
                &p,
            );
        }
        // wire preview
        if let (Tool::Wire { from: Some(f) }, Some(g)) = (self.tool, hg) {
            let m = [g[0], f[1]];
            let pts = vec![
                self.to_screen(rect, [f[0] as f32, f[1] as f32]),
                self.to_screen(rect, [m[0] as f32, m[1] as f32]),
                self.to_screen(rect, [g[0] as f32, g[1] as f32]),
            ];
            painter.add(Shape::line(pts, Stroke::new(sw, p.sel)));
        }
        if let (Tool::Wire { .. } | Tool::Place { .. } | Tool::Text, Some(g)) = (self.tool, hg) {
            let s = self.to_screen(rect, [g[0] as f32, g[1] as f32]);
            painter.circle_stroke(s, 4.0, Stroke::new(1.0, p.sel));
        }
        // directives
        self.text_rects.clear();
        let mono = FontId::monospace((z * 0.75).clamp(8.0, 22.0));
        for (i, t) in self.sch.texts.iter().enumerate() {
            let col = if self.sel.texts.contains(&i) {
                p.sel
            } else {
                p.dir
            };
            let s = self.to_screen(rect, [t.pos[0] as f32, t.pos[1] as f32]);
            let g = painter.layout_no_wrap(
                super::bidi::display(&t.text, super::bidi::Base::Ltr),
                mono.clone(),
                col,
            );
            let r = Rect::from_min_size(s, g.size());
            painter.galley(s, g, col);
            self.text_rects.push(r);
        }
        // operating point annotations
        if self.show_op && !self.op_values.is_empty() {
            let mut shown = HashSet::new();
            let small = FontId::proportional((z * 0.65).clamp(8.0, 16.0));
            for w in &self.sch.wires {
                let Some(n) = nl_nets.get(&w.a) else { continue };
                let key = swiftsim::expr::fold(n);
                if key == "0" || !shown.insert(key.clone()) {
                    continue;
                }
                if let Some(v) = self.op_values.get(&key) {
                    let mid = [
                        (w.a[0] + w.b[0]) as f32 / 2.0,
                        (w.a[1] + w.b[1]) as f32 / 2.0 - 0.6,
                    ];
                    let s = self.to_screen(rect, mid);
                    let txt = format!("{}V", swiftsim::expr::fmt_si(*v));
                    let g = painter.layout_no_wrap(txt, small.clone(), Color32::BLACK);
                    let r = Rect::from_center_size(s, g.size() + Vec2::new(6.0, 2.0));
                    painter.rect_filled(r, CornerRadius::same(3), p.probe);
                    painter.galley(r.min + Vec2::new(3.0, 1.0), g, Color32::BLACK);
                }
            }
        }
        if let Drag::Boxing { start } = self.drag {
            if let Some(h) = ui.ctx().pointer_hover_pos() {
                let r = Rect::from_two_pos(start, h);
                painter.rect_filled(r, CornerRadius::ZERO, p.sel.gamma_multiply(0.08));
                painter.rect_stroke(
                    r,
                    CornerRadius::ZERO,
                    Stroke::new(1.0, p.sel),
                    StrokeKind::Middle,
                );
            }
        }
        if let Some(n) = probe_net.filter(|n| n != "0") {
            if let Some(h) = ui.ctx().pointer_hover_pos() {
                painter.text(
                    h + Vec2::new(14.0, -14.0),
                    Align2::LEFT_BOTTOM,
                    format!("V({n})"),
                    FontId::proportional(13.0),
                    p.probe,
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_comp(
        &self,
        painter: &egui::Painter,
        rect: Rect,
        c: &Component,
        col: Color32,
        sw: f32,
        font: &FontId,
        p: &Pal,
    ) {
        let st = Stroke::new(sw, col);
        let tf = |q: [f32; 2]| self.to_screen(rect, c.tf(q));
        for prim in c.prims() {
            match prim {
                Prim::Line(v) => {
                    painter.add(Shape::line(v.into_iter().map(tf).collect(), st));
                }
                Prim::Poly(v) => {
                    painter.add(Shape::convex_polygon(
                        v.into_iter().map(tf).collect(),
                        col,
                        Stroke::NONE,
                    ));
                }
                Prim::Circle(ctr, r) => {
                    painter.circle_stroke(tf(ctr), r * self.zoom, st);
                }
                Prim::Text(at, s) => {
                    painter.text(tf(at), Align2::CENTER_CENTER, s, font.clone(), col);
                }
            }
        }
        if c.kind == Kind::Label {
            let s = tf([0.0, 0.0]);
            let g = painter.layout_no_wrap(
                super::bidi::display(&c.value, super::bidi::Base::Auto),
                font.clone(),
                col,
            );
            let r = Rect::from_min_size(
                s + Vec2::new(4.0, -g.size().y - 2.0),
                g.size() + Vec2::new(6.0, 2.0),
            );
            painter.rect_stroke(
                r,
                CornerRadius::same(3),
                Stroke::new(1.0, col),
                StrokeKind::Middle,
            );
            painter.galley(r.min + Vec2::new(3.0, 1.0), g, col);
            painter.circle_filled(s, (self.zoom * 0.15).clamp(1.5, 4.0), col);
            return;
        }
        if c.kind == Kind::Ground {
            return;
        }
        let (a, b) = c.world_bbox();
        let vertical = (b[1] - a[1]) >= (b[0] - a[0]);
        let tc = p.text;
        if vertical {
            let x = b[0] + 0.3;
            let cy = (a[1] + b[1]) / 2.0;
            painter.text(
                self.to_screen(rect, [x, cy - 0.45]),
                Align2::LEFT_CENTER,
                &c.name,
                font.clone(),
                col,
            );
            painter.text(
                self.to_screen(rect, [x, cy + 0.45]),
                Align2::LEFT_CENTER,
                &c.value,
                font.clone(),
                tc,
            );
        } else {
            let cx = (a[0] + b[0]) / 2.0;
            painter.text(
                self.to_screen(rect, [cx, a[1] - 0.15]),
                Align2::CENTER_BOTTOM,
                &c.name,
                font.clone(),
                col,
            );
            painter.text(
                self.to_screen(rect, [cx, b[1] + 0.15]),
                Align2::CENTER_TOP,
                &c.value,
                font.clone(),
                tc,
            );
        }
    }
}

fn toggle(s: &mut HashSet<usize>, i: usize) {
    if !s.remove(&i) {
        s.insert(i);
    }
}
