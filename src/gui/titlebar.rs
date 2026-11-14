//! The window's own title bar (the system one is turned off): the app's trace mark, the menus,
//! the document name with a run lamp, and the window buttons. Also the window's resize edges,
//! which an undecorated window has to provide itself.

use super::i18n::{self, tr};
use super::state::Sim;
use eframe::egui::text::{LayoutJob, TextWrapping};
use eframe::egui::{
    self, Color32, CursorIcon, Event, FontId, Layout, Pos2, Rect, ResizeDirection, Response, Sense,
    Stroke, StrokeKind, Ui, UiBuilder, ViewportCommand, WidgetInfo, pos2, vec2,
};

pub const HEIGHT: f32 = 32.0;
const BUTTON_W: f32 = 44.0;
const MARK_W: f32 = 34.0;
/// Width of the resize band along the window's edges, and how far a corner reaches along them.
const EDGE: f32 = 5.0;
const CORNER: f32 = 16.0;
/// Space kept between the document name and the menus or buttons.
const GAP: f32 = 16.0;

/// The trace mark's colours, from the app icon.
const AMBER: Color32 = Color32::from_rgb(255, 170, 51);
const CYAN: Color32 = Color32::from_rgb(56, 200, 232);

/// What the lamp beside the document name shows; nothing while the simulator is at rest.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Lamp {
    Running,
    Stopping,
    Failed,
}

impl Lamp {
    pub fn of<J>(sim: &Sim<J>) -> Option<Lamp> {
        match sim {
            Sim::Running { .. } => Some(Lamp::Running),
            Sim::Cancelling { .. } => Some(Lamp::Stopping),
            Sim::Failed { .. } => Some(Lamp::Failed),
            Sim::Idle | Sim::Finished { .. } | Sim::Cancelled { .. } => None,
        }
    }

    fn color(self) -> Color32 {
        match self {
            Lamp::Running => CYAN,
            Lamp::Stopping => AMBER,
            Lamp::Failed => Color32::from_rgb(235, 82, 82),
        }
    }
}

/// The bar's background: a step darker than the panels below it.
pub fn fill(dark: bool) -> Color32 {
    if dark {
        Color32::from_gray(16)
    } else {
        Color32::from_gray(226)
    }
}

/// Draws the bar into `ui` (the bar's panel). `menus` adds the menu bar and returns the rect its
/// menus cover. Returns where the compositor's window menu was asked for (right click on the bar).
pub fn show(
    ui: &mut Ui,
    title: &str,
    lamp: Option<(Lamp, String)>,
    menus: impl FnOnce(&mut Ui) -> Rect,
) -> Option<Pos2> {
    let bar = ui.max_rect();
    let rtl = i18n::is_rtl_ui();
    let ctx = ui.ctx().clone();
    let (maximized, fullscreen) = ui.input(|i| {
        let v = i.viewport();
        (v.maximized.unwrap_or(false), v.fullscreen.unwrap_or(false))
    });
    // window buttons on the far side, the mark and the menus on the reading side
    let w = 3.0 * BUTTON_W;
    let (buttons, mark) = if rtl {
        (
            Rect::from_min_size(bar.min, vec2(w, HEIGHT)),
            Rect::from_min_max(pos2(bar.right() - MARK_W, bar.top()), bar.max),
        )
    } else {
        (
            Rect::from_min_max(pos2(bar.right() - w, bar.top()), bar.max),
            Rect::from_min_size(bar.min, vec2(MARK_W, HEIGHT)),
        )
    };
    draw_mark(ui, mark.center());
    let rest = Rect::from_x_y_ranges(
        if rtl {
            buttons.right()..=mark.left()
        } else {
            mark.right()..=buttons.left()
        },
        bar.y_range(),
    );
    let menu_rect = menus(
        &mut ui.new_child(
            UiBuilder::new()
                .max_rect(rest)
                .layout(Layout::left_to_right(egui::Align::Center)),
        ),
    );

    // the document name, centred on the window while there is room, and its lamp
    let free = if rtl {
        (buttons.right(), menu_rect.left())
    } else {
        (menu_rect.right(), buttons.left())
    };
    let font = FontId::proportional(13.5);
    let color = ui.visuals().text_color();
    let lamp_w = if lamp.is_some() { 16.0 } else { 0.0 };
    let full = ui
        .painter()
        .layout_no_wrap(title.to_string(), font.clone(), color);
    let (x0, x1) = title_span((bar.left(), bar.right()), free, full.size().x + lamp_w);
    let text_w = (x1 - x0 - lamp_w).max(0.0);
    let galley = if full.size().x <= text_w {
        full
    } else {
        let mut job = LayoutJob::simple_singleline(title.to_string(), font, color);
        job.wrap = TextWrapping::truncate_at_width(text_w);
        ui.painter().layout_job(job)
    };
    let text_x = if rtl { x1 - galley.size().x } else { x0 };
    let text_pos = pos2(text_x, bar.center().y - galley.size().y / 2.0);
    let text_rect = Rect::from_min_size(text_pos, galley.size());
    let tip = if galley.elided {
        Some((text_rect, title.to_string()))
    } else {
        None
    };
    ui.painter().galley(text_pos, galley, color);
    let lamp_tip = lamp.map(|(lamp, tip)| {
        let x = if rtl {
            text_rect.left() - 10.0
        } else {
            text_rect.right() + 10.0
        };
        let r = Rect::from_center_size(pos2(x, bar.center().y), vec2(8.0, 8.0));
        ui.painter().rect_filled(r, 2.0, lamp.color());
        (r.expand(4.0), tip)
    });

    // The buttons and the empty bar space (the name and the mark included) sit in layers above
    // the dialogs, so the window can be moved and closed while one is open, as with the system
    // bar. Each layer covers only its own rect: the menus below stay reachable.
    above_dialogs(&ctx, "titlebar-buttons", buttons, |ui| {
        let mut b = ui.new_child(UiBuilder::new().max_rect(buttons).layout(if rtl {
            Layout::left_to_right(egui::Align::Center)
        } else {
            Layout::right_to_left(egui::Align::Center)
        }));
        b.spacing_mut().item_spacing.x = 0.0;
        if button(&mut b, Button::Close, false).clicked() {
            // the same path as the system close button: unsaved-changes dialog, then `on_exit`
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
        if button(&mut b, Button::Maximize, maximized || fullscreen).clicked() {
            ctx.send_viewport_cmd(if fullscreen {
                ViewportCommand::Fullscreen(false)
            } else {
                ViewportCommand::Maximized(!maximized)
            });
        }
        if button(&mut b, Button::Minimize, false).clicked() {
            ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
        }
    });
    let free = Rect::from_x_y_ranges(free.0..=free.1, bar.y_range());
    let mut window_menu = None;
    for (name, rect) in [("titlebar-free", free), ("titlebar-mark", mark)] {
        above_dialogs(&ctx, name, rect, |ui| {
            let mut r = ui.interact(rect, ui.scope_id().with(name), Sense::click_and_drag());
            let at = r.hover_pos();
            for (area, text) in tip.iter().chain(lamp_tip.iter()) {
                if at.is_some_and(|p| area.contains(p)) {
                    r = r.on_hover_text(text);
                }
            }
            if r.drag_started_by(egui::PointerButton::Primary) {
                ctx.send_viewport_cmd(ViewportCommand::StartDrag);
                // the compositor keeps the button release on Wayland: `input` supplies one
                if let Some(p) = ui.input(|i| i.pointer.latest_pos()) {
                    ctx.data_mut(|d| d.insert_temp(moved_id(), p));
                }
            }
            if r.double_clicked() {
                ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
            }
            if r.secondary_clicked() {
                window_menu = r.interact_pointer_pos();
            }
        });
    }
    window_menu
}

/// `add` in a layer of its own over `rect`, above egui's modal dialogs.
fn above_dialogs(ctx: &egui::Context, name: &str, rect: Rect, add: impl FnOnce(&mut Ui)) {
    egui::Area::new(egui::Id::unique(name))
        .order(egui::Order::Tooltip)
        .fixed_pos(rect.min)
        .constrain(false)
        .show(ctx, |ui| {
            ui.set_min_size(rect.size());
            ui.set_max_size(rect.size());
            add(ui);
        });
}

/// Left and right end of the document name (with its lamp) `width` wide: centred on the bar
/// while that stays inside the free space between the menus and the buttons, otherwise pushed
/// aside, and cut to the free space when it is wider.
pub fn title_span(bar: (f32, f32), free: (f32, f32), width: f32) -> (f32, f32) {
    let (lo, hi) = (free.0 + GAP, free.1 - GAP);
    if hi <= lo {
        return (lo, lo);
    }
    let w = width.min(hi - lo);
    let x0 = ((bar.0 + bar.1 - w) / 2.0).min(hi - w).max(lo);
    (x0, x0 + w)
}

/// The edge or corner of the window `rect` that `p` is on, if any.
pub fn resize_dir(p: Pos2, rect: Rect) -> Option<ResizeDirection> {
    if !rect.contains(p) {
        return None;
    }
    let (l, r, t, b) = (
        p.x - rect.left(),
        rect.right() - p.x,
        p.y - rect.top(),
        rect.bottom() - p.y,
    );
    // near a corner the band reaches further along the edge
    let reach_x = if t < EDGE || b < EDGE { CORNER } else { EDGE };
    let reach_y = if l < EDGE || r < EDGE { CORNER } else { EDGE };
    let x = if l < reach_x {
        -1
    } else if r < reach_x {
        1
    } else {
        0
    };
    let y = if t < reach_y {
        -1
    } else if b < reach_y {
        1
    } else {
        0
    };
    use ResizeDirection::*;
    Some(match (x, y) {
        (-1, -1) => NorthWest,
        (1, -1) => NorthEast,
        (-1, 1) => SouthWest,
        (1, 1) => SouthEast,
        (-1, _) => West,
        (1, _) => East,
        (_, -1) => North,
        (_, 1) => South,
        _ => return None,
    })
}

fn moved_id() -> egui::Id {
    egui::Id::unique("titlebar-moved")
}

/// For `App::raw_input_hook`. Releases the button after a window move (otherwise egui would
/// think the bar is still being dragged), and takes a left press on a resize edge out of `raw`,
/// so the widget under it never sees it, returning the resize it starts. No resizing while the
/// window is maximized or full screen.
pub fn input(ctx: &egui::Context, raw: &mut egui::RawInput) -> Option<ResizeDirection> {
    if let Some(pos) = ctx.data_mut(|d| d.remove_temp::<Pos2>(moved_id())) {
        raw.events.insert(
            0,
            Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        );
    }
    let v = raw.viewport();
    if v.maximized == Some(true) || v.fullscreen == Some(true) {
        return None;
    }
    let rect = raw.screen_rect?;
    let mut dir = None;
    raw.events.retain(|e| match e {
        Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            ..
        } if dir.is_none() => {
            dir = resize_dir(*pos, rect);
            dir.is_none()
        }
        _ => true,
    });
    dir
}

/// The resize cursor over the window's edges. Call after everything else is drawn.
pub fn edge_cursor(ctx: &egui::Context) {
    let (pos, rect, fixed) = ctx.input(|i| {
        let v = i.viewport();
        (
            i.pointer.hover_pos(),
            i.content_rect(),
            v.maximized == Some(true) || v.fullscreen == Some(true),
        )
    });
    // a drag inside the window (a panel edge, a part) keeps its own cursor
    if fixed || ctx.dragged_id().is_some() {
        return;
    }
    if let Some(d) = pos.and_then(|p| resize_dir(p, rect)) {
        use ResizeDirection::*;
        ctx.set_cursor_icon(match d {
            North => CursorIcon::ResizeNorth,
            South => CursorIcon::ResizeSouth,
            East => CursorIcon::ResizeEast,
            West => CursorIcon::ResizeWest,
            NorthEast => CursorIcon::ResizeNorthEast,
            NorthWest => CursorIcon::ResizeNorthWest,
            SouthEast => CursorIcon::ResizeSouthEast,
            SouthWest => CursorIcon::ResizeSouthWest,
        });
    }
}

#[derive(PartialEq, Clone, Copy)]
enum Button {
    Minimize,
    Maximize,
    Close,
}

/// One window button: a flat field that lights on hover, close in red.
fn button(ui: &mut Ui, kind: Button, restore: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(BUTTON_W, HEIGHT), Sense::click());
    let label = match kind {
        Button::Minimize => tr("Minimize"),
        Button::Maximize if restore => tr("Restore"),
        Button::Maximize => tr("Maximize"),
        Button::Close => tr("Close"),
    };
    resp.widget_info(|| WidgetInfo::labeled(egui::Role::Button, true, label));
    let v = ui.visuals();
    let hot = resp.hovered() || resp.is_pointer_button_down_on();
    let close = kind == Button::Close;
    let p = ui.painter();
    if hot {
        let fill = if close {
            Color32::from_rgb(196, 43, 28)
        } else {
            // a wash of the text colour reads on the bar in either theme
            let pressed = resp.is_pointer_button_down_on();
            v.text_color()
                .gamma_multiply(if pressed { 0.2 } else { 0.12 })
        };
        p.rect_filled(rect, 0.0, fill);
    }
    if resp.has_focus() {
        p.rect_stroke(
            rect.shrink(3.0),
            2.0,
            v.selection.stroke,
            StrokeKind::Inside,
        );
    }
    let ink = if close && hot {
        Color32::WHITE
    } else {
        v.text_color()
    };
    let s = Stroke::new(1.2, ink);
    // a 10 px glyph on whole pixels
    let c = rect.center().round();
    let at = |x: f32, y: f32| c + vec2(x, y);
    match kind {
        Button::Minimize => {
            p.rect_filled(
                Rect::from_min_size(at(-5.0, 0.0), vec2(10.0, 1.2)),
                0.0,
                ink,
            );
        }
        Button::Maximize if restore => {
            p.rect_stroke(
                Rect::from_min_max(at(-5.0, -3.0), at(3.0, 5.0)),
                0.0,
                s,
                StrokeKind::Middle,
            );
            p.line_segment([at(-3.0, -3.0), at(-3.0, -5.0)], s);
            p.line_segment([at(-3.0, -5.0), at(5.0, -5.0)], s);
            p.line_segment([at(5.0, -5.0), at(5.0, 3.0)], s);
            p.line_segment([at(5.0, 3.0), at(3.0, 3.0)], s);
        }
        Button::Maximize => {
            p.rect_stroke(
                Rect::from_min_max(at(-5.0, -5.0), at(5.0, 5.0)),
                0.0,
                s,
                StrokeKind::Middle,
            );
        }
        Button::Close => {
            p.line_segment([at(-5.0, -5.0), at(5.0, 5.0)], s);
            p.line_segment([at(-5.0, 5.0), at(5.0, -5.0)], s);
        }
    }
    resp.on_hover_text(label)
}

/// The app icon's trace in miniature: a lead, a resistor zigzag in amber, a sine in cyan.
fn draw_mark(ui: &Ui, c: Pos2) {
    let o = c + vec2(-11.0, 0.0);
    let amber: Vec<Pos2> = [
        (0.0, 0.0),
        (3.0, 0.0),
        (4.0, -4.0),
        (5.5, 4.0),
        (7.0, -4.0),
        (8.5, 4.0),
        (10.0, 0.0),
    ]
    .iter()
    .map(|&(x, y)| o + vec2(x, y))
    .collect();
    let cyan: Vec<Pos2> = (0..=24)
        .map(|i| {
            let t = i as f32 / 24.0;
            o + vec2(10.0 + 12.0 * t, -5.0 * (t * std::f32::consts::TAU).sin())
        })
        .collect();
    let p = ui.painter();
    p.line(amber, Stroke::new(1.6, AMBER));
    p.line(cyan, Stroke::new(1.6, CYAN));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edges_and_corners_pick_their_resize_direction() {
        let w = Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 800.0));
        use ResizeDirection::*;
        for (p, d) in [
            ((500.0, 400.0), None),
            ((500.0, 2.0), Some(North)),
            ((500.0, 798.0), Some(South)),
            ((1.0, 400.0), Some(West)),
            ((999.0, 400.0), Some(East)),
            ((1.0, 1.0), Some(NorthWest)),
            ((998.0, 799.0), Some(SouthEast)),
            // a corner reaches a little way along each edge
            ((12.0, 2.0), Some(NorthWest)),
            ((998.0, 12.0), Some(NorthEast)),
            ((2.0, 790.0), Some(SouthWest)),
            ((20.0, 2.0), Some(North)),
            ((6.0, 400.0), None),
            ((1001.0, 400.0), None),
        ] {
            assert_eq!(resize_dir(pos2(p.0, p.1), w), d, "{p:?}");
        }
    }

    #[test]
    fn title_is_centred_pushed_aside_or_cut() {
        // room in the middle: centred on the bar
        assert_eq!(
            title_span((0.0, 1000.0), (300.0, 860.0), 100.0),
            (450.0, 550.0)
        );
        // the menus reach past the middle: starts after them
        assert_eq!(
            title_span((0.0, 1000.0), (520.0, 860.0), 100.0),
            (536.0, 636.0)
        );
        // wider than the free space: cut to it
        assert_eq!(
            title_span((0.0, 1000.0), (300.0, 860.0), 900.0),
            (316.0, 844.0)
        );
        // no room at all
        assert_eq!(
            title_span((0.0, 1000.0), (500.0, 520.0), 100.0),
            (516.0, 516.0)
        );
    }

    #[test]
    fn lamp_shows_only_while_a_run_needs_attention() {
        assert_eq!(Lamp::of(&Sim::<()>::Idle), None);
        assert_eq!(
            Lamp::of(&Sim::Running { doc: 1, job: () }),
            Some(Lamp::Running)
        );
        assert_eq!(
            Lamp::of(&Sim::Cancelling { doc: 1, job: () }),
            Some(Lamp::Stopping)
        );
        assert_eq!(Lamp::of(&Sim::<()>::Finished { doc: 1, secs: 0.1 }), None);
        assert_eq!(Lamp::of(&Sim::<()>::Cancelled { doc: 1 }), None);
        let failed = Sim::<()>::Failed {
            doc: 1,
            msg: "singular matrix".into(),
        };
        assert_eq!(Lamp::of(&failed), Some(Lamp::Failed));
    }
}
