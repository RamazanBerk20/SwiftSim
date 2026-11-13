//! Right-to-left text on top of egui.
//!
//! egui shapes each run of one font (Arabic joins, Hebrew reads the right way), but it has no
//! paragraph direction: runs are placed left to right, and its glyph lists and text cursor assume
//! left-to-right text. This module applies the Unicode Bidirectional Algorithm (UAX #9).
//! `display` reorders text for labels. `line` and `code` keep egui's editing engine, which works on
//! logical character indices, and do the layout, mouse hits, caret and selection in bidi order.

use eframe::egui::{
    self, Align, Color32, Event, FontId, FontSelection, Galley, Key, Pos2, Response, Stroke,
    TextEdit, TextStyle, Ui, Vec2,
};
use egui::TextBuffer;
use egui::epaint::text::Glyph;
use egui::text::{CCursor, CCursorRange, LayoutJob};
use std::ops::Range;
use std::sync::Arc;
use unicode_bidi::{BidiInfo, Level, ParagraphInfo};
use unicode_segmentation::UnicodeSegmentation;

/// Paragraph direction.
#[derive(Clone, Copy, PartialEq)]
pub enum Base {
    /// From the first strong letter (UAX #9 P2): user text such as net labels.
    Auto,
    /// Always left to right: netlists and expressions, with any right-to-left words embedded.
    Ltr,
    /// Always right to left: the interface in a right-to-left language.
    Rtl,
}

fn level(base: Base) -> Option<Level> {
    match base {
        Base::Auto => None,
        Base::Ltr => Some(Level::ltr()),
        Base::Rtl => Some(Level::rtl()),
    }
}

/// Characters of the right-to-left scripts.
pub fn is_rtl(c: char) -> bool {
    matches!(c, '\u{590}'..='\u{8FF}' | '\u{FB1D}'..='\u{FDFF}' | '\u{FE70}'..='\u{FEFF}' | '\u{10800}'..='\u{10FFF}' | '\u{1E800}'..='\u{1EFFF}')
}

/// Characters the shaper keeps in the same run as right-to-left letters, and reverses with them.
fn shaped_with_letters(c: char) -> bool {
    is_rtl(c) || matches!(c, '\u{200C}' | '\u{200D}' | '\u{300}'..='\u{36F}')
}

/// UAX #9 rule L4, for the characters that occur in names and interface text.
fn mirror(c: char) -> char {
    match c {
        '(' => ')',
        ')' => '(',
        '[' => ']',
        ']' => '[',
        '{' => '}',
        '}' => '{',
        '<' => '>',
        '>' => '<',
        '«' => '»',
        '»' => '«',
        '‹' => '›',
        '›' => '‹',
        '≤' => '≥',
        '≥' => '≤',
        c => c,
    }
}

/// One line in display order: the text to give egui, and for each character on screen from left
/// to right its byte offset in `text` and whether it reads right to left.
fn visual_line(
    text: &str,
    info: &BidiInfo,
    para: &ParagraphInfo,
    line: Range<usize>,
) -> (String, Vec<(usize, bool)>) {
    let (mut shown, mut cells) = (String::new(), vec![]);
    if line.is_empty() {
        return (shown, cells);
    }
    let (levels, runs) = info.visual_runs(para, line);
    for run in runs {
        let part = &text[run.clone()];
        if !levels[run.start].is_rtl() {
            cells.extend(part.char_indices().map(|(i, _)| (run.start + i, false)));
            shown.push_str(part);
            continue;
        }
        cells.extend(
            part.char_indices()
                .rev()
                .map(|(i, _)| (run.start + i, true)),
        );
        // The shaper reverses each stretch of letters itself; the rest is ours to place, by whole
        // grapheme clusters so that a multi-part emoji stays in one piece.
        let mut chunks: Vec<(bool, String)> = vec![];
        for g in part.graphemes(true) {
            let letters = g.chars().next().is_some_and(shaped_with_letters);
            match chunks.last_mut() {
                Some((l, s)) if *l == letters => s.push_str(g),
                _ => chunks.push((letters, g.to_string())),
            }
        }
        for (letters, chunk) in chunks.iter().rev() {
            if *letters {
                shown.push_str(chunk);
                continue;
            }
            for g in chunk.graphemes(true).rev() {
                let mut chars = g.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => shown.push(mirror(c)),
                    _ => shown.push_str(g),
                }
            }
        }
    }
    (shown, cells)
}

/// Text reordered for a label. Keep it to one line, or use `wrapped`.
pub fn display(text: &str, base: Base) -> String {
    // nothing to reorder, and Latin-only names like `.Text` keep their punctuation where it is
    if !text.chars().any(is_rtl) {
        return text.to_string();
    }
    let line = |l: &str| {
        let info = BidiInfo::new(l, level(base));
        info.paragraphs
            .iter()
            .map(|p| visual_line(l, &info, p, p.range.clone()).0)
            .collect::<String>()
    };
    let mut shown = text.split('\n').map(line).collect::<Vec<_>>().join("\n");
    shown.retain(|c| !matches!(c, FSI | PDI));
    shown
}

/// Marks that keep a value such as a path in its own direction inside a sentence (UAX #9 isolates).
const FSI: char = '\u{2068}';
const PDI: char = '\u{2069}';

/// `value`, safe to insert into right-to-left text: `display` removes the marks again.
pub fn isolate(value: &str) -> String {
    format!("{FSI}{value}{PDI}")
}

/// Line ranges of `range` for `width`, breaking at spaces. Done on the logical text because UAX #9
/// reorders each line after breaking; breaking after reordering would put the last words on top.
fn break_lines(
    ui: &Ui,
    text: &str,
    range: Range<usize>,
    font: &FontId,
    width: f32,
) -> Vec<Range<usize>> {
    if !width.is_finite() {
        return vec![range];
    }
    let space = ui.fonts_mut(|f| f.glyph_width(font, ' '));
    let (mut lines, mut start, mut at, mut used) = (vec![], range.start, range.start, 0.0);
    for word in text[range.clone()].split_inclusive(' ') {
        let w = ui.fonts_mut(|f| {
            f.layout_no_wrap(word.trim_end().to_string(), font.clone(), Color32::WHITE)
                .size()
                .x
        });
        if used + w > width && at > start {
            lines.push(start..at);
            start = at;
            used = 0.0;
        }
        used += w + space;
        at += word.len();
    }
    lines.push(start..range.end);
    lines
}

/// `display` for a paragraph that has to wrap at `width`.
pub fn wrapped(ui: &Ui, text: &str, base: Base, font: &FontId, width: f32) -> String {
    let mut out = vec![];
    for l in text.split('\n') {
        let info = BidiInfo::new(l, level(base));
        for p in &info.paragraphs {
            out.extend(
                break_lines(ui, l, p.range.clone(), font, width)
                    .into_iter()
                    .map(|r| visual_line(l, &info, p, r).0),
            );
        }
    }
    out.join("\n")
}

struct Cell {
    /// Logical character offset within the row.
    col: usize,
    rtl: bool,
}

struct Row {
    /// Logical index of the row's first character in the whole text.
    start: usize,
    /// One per character, left to right on screen.
    cells: Vec<Cell>,
}

/// egui's rows promise one glyph per character, in logical order, but a right-to-left run gets
/// zero-width extras and loses merged ligatures. This gives one glyph per character of `chars`
/// (left to right on screen), which egui's cursor arithmetic relies on.
fn one_glyph_per_char(glyphs: &[Glyph], chars: &[char]) -> Vec<Glyph> {
    let real: Vec<&Glyph> = glyphs.iter().filter(|g| g.advance_width != 0.0).collect();
    let (mut next, mut out): (usize, Vec<Glyph>) = (0, Vec::with_capacity(chars.len()));
    for (i, &c) in chars.iter().enumerate() {
        let matches = real.get(next).is_some_and(|g| g.chr == c);
        if matches || real.len().saturating_sub(next) >= chars.len() - i {
            if let Some(g) = real.get(next) {
                out.push((*g).clone());
                next += 1;
                continue;
            }
        }
        // a combining mark or the tail of a ligature takes no room of its own
        let Some(mut g) = out
            .last()
            .or(real.get(next).copied())
            .or(glyphs.first())
            .cloned()
        else {
            continue;
        };
        if !out.is_empty() {
            g.pos.x += g.advance_width;
        }
        g.chr = c;
        g.advance_width = 0.0;
        out.push(g);
    }
    out
}

fn layout(
    ui: &Ui,
    text: &str,
    font: &FontId,
    color: Color32,
    wrap: f32,
    base: Base,
    halign: Align,
) -> (Arc<Galley>, Vec<Row>) {
    let (mut shown, mut rows, mut expected, mut hard) = (vec![], vec![], vec![], vec![]);
    let paragraphs: Vec<&str> = text.split('\n').collect();
    let mut start = 0;
    for (n, l) in paragraphs.iter().enumerate() {
        let info = BidiInfo::new(l, level(base));
        let lines: Vec<(&ParagraphInfo, Range<usize>)> = info
            .paragraphs
            .iter()
            .flat_map(|p| {
                break_lines(ui, l, p.range.clone(), font, wrap)
                    .into_iter()
                    .map(move |r| (p, r))
            })
            .collect();
        if lines.is_empty() {
            shown.push(String::new());
            rows.push(Row {
                start,
                cells: vec![],
            });
            expected.push(vec![]);
            hard.push(n + 1 < paragraphs.len());
        }
        let count = lines.len();
        for (k, (p, line)) in lines.into_iter().enumerate() {
            let offsets: Vec<usize> = l[line.clone()]
                .char_indices()
                .map(|(i, _)| line.start + i)
                .collect();
            let (s, cells) = visual_line(l, &info, p, line.clone());
            let chars = cells
                .iter()
                .map(|&(b, rtl)| {
                    let c = l[b..].chars().next().unwrap_or(' ');
                    if rtl && !shaped_with_letters(c) {
                        mirror(c)
                    } else {
                        c
                    }
                })
                .collect();
            let cells = cells
                .iter()
                .map(|&(b, rtl)| Cell {
                    col: offsets.binary_search(&b).unwrap_or(0),
                    rtl,
                })
                .collect();
            shown.push(s);
            rows.push(Row {
                start: start + l[..line.start].chars().count(),
                cells,
            });
            expected.push(chars);
            hard.push(k + 1 == count && n + 1 < paragraphs.len());
        }
        start += l.chars().count() + 1;
    }
    let mut job = LayoutJob::simple(shown.join("\n"), font.clone(), color, f32::INFINITY);
    job.halign = halign;
    job.keep_trailing_whitespace = true;
    let height = ui.fonts_mut(|f| f.row_height(font));
    job.sections
        .iter_mut()
        .for_each(|s| s.format.line_height = Some(height));
    let mut galley = ui.fonts_mut(|f| f.layout_job(job));
    for (i, placed) in Arc::make_mut(&mut galley).rows.iter_mut().enumerate() {
        placed.ends_with_newline = hard.get(i).copied().unwrap_or(false);
        if let Some(chars) = expected.get(i) {
            let row = Arc::make_mut(&mut placed.row);
            row.glyphs = one_glyph_per_char(&row.glyphs, chars);
            // egui recolours selected text by glyph index, which is meaningless in display order
            let end = row.visuals.glyph_vertex_range.end as u32;
            row.glyphs.iter_mut().for_each(|g| g.first_vertex = end);
        }
    }
    (galley, rows)
}

/// Logical column in a row for a position `x` along it.
fn column_at(row: &Row, glyphs: &[Glyph], x: f32) -> usize {
    let side = |j: usize, before: bool| row.cells[j].col + usize::from(before == row.cells[j].rtl);
    for (j, g) in glyphs
        .iter()
        .enumerate()
        .filter(|(_, g)| g.advance_width > 0.0)
    {
        if x < g.pos.x + g.advance_width {
            return side(j, x < g.pos.x + g.advance_width / 2.0);
        }
    }
    glyphs
        .iter()
        .rposition(|g| g.advance_width > 0.0)
        .map_or(0, |j| side(j, false))
}

/// Position along a row of the caret before logical column `col`.
fn caret_x(row: &Row, glyphs: &[Glyph], col: usize) -> f32 {
    let at = |j: usize, leading: bool| {
        glyphs.get(j).map_or(0.0, |g| {
            if leading == row.cells[j].rtl {
                g.pos.x + g.advance_width
            } else {
                g.pos.x
            }
        })
    };
    if let Some(j) = row.cells.iter().position(|c| c.col == col) {
        return at(j, true);
    }
    row.cells
        .iter()
        .position(|c| c.col + 1 == col)
        .map_or(0.0, |j| at(j, false))
}

/// Character index for a screen position, given the rows of a laid-out text.
fn hit(rows: &[Row], galley: &Galley, origin: Pos2, p: Pos2) -> usize {
    let rel = p - origin;
    let last = rows.len().saturating_sub(1);
    let i = galley
        .rows
        .iter()
        .position(|r| rel.y < r.pos.y + r.row.size.y)
        .unwrap_or(last)
        .min(last);
    let (Some(row), Some(placed)) = (rows.get(i), galley.rows.get(i)) else {
        return 0;
    };
    row.start + column_at(row, &placed.row.glyphs, rel.x - placed.pos.x)
}

fn selection_of_word(text: &str, at: usize) -> (usize, usize) {
    let chars: Vec<char> = text.chars().collect();
    let word = |i: usize| {
        chars
            .get(i)
            .is_some_and(|c| c.is_alphanumeric() || shaped_with_letters(*c))
    };
    let (mut a, mut b) = (at.min(chars.len()), at.min(chars.len()));
    while a > 0 && word(a - 1) {
        a -= 1;
    }
    while word(b) {
        b += 1;
    }
    (a, b)
}

/// Left and Right move visually, so inside right-to-left text they mean the opposite logically.
fn swap_arrows_in_rtl_text(ui: &mut Ui, id: egui::Id, text: &str) {
    if !ui.memory(|m| m.has_focus(id)) {
        return;
    }
    let Some(range) = TextEdit::load_state(ui.ctx(), id).and_then(|s| s.cursor.char_range()) else {
        return;
    };
    let at = range.primary.index.0;
    let c = text
        .chars()
        .nth(at)
        .or_else(|| at.checked_sub(1).and_then(|i| text.chars().nth(i)));
    if c.is_some_and(is_rtl) {
        ui.input_mut(|i| {
            for e in &mut i.events {
                if let Event::Key { key, .. } = e {
                    *key = match *key {
                        Key::ArrowLeft => Key::ArrowRight,
                        Key::ArrowRight => Key::ArrowLeft,
                        k => k,
                    };
                }
            }
        });
    }
}

/// The character index where each grapheme cluster starts, and the end of the text.
fn cluster_starts(text: &str) -> Vec<usize> {
    let mut at = 0;
    let mut starts: Vec<usize> = text
        .graphemes(true)
        .map(|g| {
            let start = at;
            at += g.chars().count();
            start
        })
        .collect();
    starts.push(at);
    starts
}

/// A caret position inside a cluster moves out of it, on the side it was travelling towards.
fn snap_to_cluster(starts: &[usize], at: usize, from: usize) -> usize {
    match starts.binary_search(&at) {
        Ok(_) => at,
        Err(i) if at < from => starts[i - 1],
        Err(i) => starts.get(i).copied().unwrap_or(at),
    }
}

/// (primary, secondary) of a cursor range, out of any cluster. A plain caret stays a plain caret.
fn snap_range(starts: &[usize], primary: usize, secondary: usize, from: usize) -> (usize, usize) {
    let p = snap_to_cluster(starts, primary, from);
    (
        p,
        if primary == secondary {
            p
        } else {
            snap_to_cluster(starts, secondary, secondary)
        },
    )
}

/// egui deletes one character at a time, which would leave half of a flag or a family emoji.
/// Selecting the whole cluster first makes Backspace and Delete remove all of it.
fn select_cluster_being_deleted(ui: &mut Ui, id: egui::Id, starts: &[usize]) {
    let keys = ui.input(|i| {
        (
            i.key_pressed(Key::Backspace),
            i.key_pressed(Key::Delete),
            i.modifiers.is_none(),
        )
    });
    let Some(mut state) = TextEdit::load_state(ui.ctx(), id) else {
        return;
    };
    let Some(range) = state.cursor.char_range().filter(|r| r.is_empty()) else {
        return;
    };
    let at = range.primary.index.0;
    let Ok(i) = starts.binary_search(&at) else {
        return;
    };
    let span = match keys {
        (true, _, true) if i > 0 => (starts[i - 1], at),
        (_, true, true) if i + 1 < starts.len() => (at, starts[i + 1]),
        _ => return,
    };
    if span.1 - span.0 > 1 {
        state.cursor.set_char_range(Some(CCursorRange::two(
            CCursor::new(span.0),
            CCursor::new(span.1),
        )));
        TextEdit::store_state(ui.ctx(), id, state);
    }
}

fn edit<'t>(
    ui: &mut Ui,
    text: &'t mut String,
    multiline: bool,
    base: Base,
    setup: impl FnOnce(TextEdit<'_>) -> TextEdit<'_>,
) -> Response {
    let id = ui.next_auto_id();
    let has_rtl = text.chars().any(is_rtl);
    let starts = cluster_starts(text);
    let has_clusters = starts.len() != text.chars().count() + 1;
    let focused = ui.memory(|m| m.has_focus(id));
    if has_clusters && focused {
        select_cluster_being_deleted(ui, id, &starts);
    }
    let caret_before = TextEdit::load_state(ui.ctx(), id)
        .and_then(|s| s.cursor.char_range())
        .map_or(0, |r| r.primary.index.0);
    let rtl_paragraph = has_rtl
        && BidiInfo::new(text, level(base))
            .paragraphs
            .first()
            .is_some_and(|p| p.level.is_rtl());
    if has_rtl {
        swap_arrows_in_rtl_text(ui, id, text);
    }
    let base_edit = if multiline {
        TextEdit::multiline(text).code_editor()
    } else {
        TextEdit::singleline(text)
    };
    let field = setup(base_edit).id(id);
    if !has_rtl {
        if !has_clusters {
            return ui.add(field);
        }
        let out = field.show(ui);
        let starts = cluster_starts(text);
        keep_caret_out_of_clusters(ui, id, out.state, &starts, caret_before);
        return out.response.response;
    }
    let font = if multiline {
        TextStyle::Monospace.resolve(ui.style())
    } else {
        FontSelection::default().resolve(ui.style())
    };
    let color = ui.visuals().widgets.inactive.text_color();
    let halign = if rtl_paragraph {
        Align::RIGHT
    } else {
        Align::LEFT
    };
    let mut rows = vec![];
    let mut layouter = |ui: &Ui, buf: &dyn TextBuffer, wrap: f32| {
        let (galley, r) = layout(
            ui,
            buf.as_str(),
            &font,
            color,
            if multiline { wrap } else { f32::INFINITY },
            base,
            halign,
        );
        rows = r;
        galley
    };
    // egui's own caret and selection would follow its left-to-right arithmetic
    let saved = ui.visuals().clone();
    let v = ui.visuals_mut();
    v.text_cursor.stroke = Stroke::NONE;
    v.selection.bg_fill = Color32::TRANSPARENT;
    let out = field
        .layouter(&mut layouter)
        .align(if rtl_paragraph {
            egui::Align2::RIGHT_TOP
        } else {
            egui::Align2::LEFT_TOP
        })
        .show(ui);
    *ui.visuals_mut() = saved;

    let response = out.response.response;
    let galley = &out.galley;
    let origin = out.galley_pos - Vec2::new(galley.rect.left(), 0.0);
    let mut state = out.state;
    let pointer = ui.input(|i| (i.pointer.press_origin(), i.pointer.interact_pos()));
    let set = match pointer {
        (_, Some(p)) if response.double_clicked() => {
            Some(selection_of_word(text, hit(&rows, galley, origin, p)))
        }
        (Some(a), Some(p)) if response.is_pointer_button_down_on() => {
            Some((hit(&rows, galley, origin, a), hit(&rows, galley, origin, p)))
        }
        _ => None,
    };
    if let Some((a, b)) = set {
        state
            .cursor
            .set_char_range(Some(CCursorRange::two(CCursor::new(a), CCursor::new(b))));
        TextEdit::store_state(ui.ctx(), id, state.clone());
    }
    let state = keep_caret_out_of_clusters(ui, id, state, &cluster_starts(text), caret_before);
    let Some(range) = state.cursor.char_range() else {
        return response;
    };
    let painter = ui.painter_at(out.text_clip_rect);
    let [lo, hi] = range.sorted_cursors().map(|c| c.index.0);
    let selection = ui.visuals().selection.bg_fill.gamma_multiply(0.55);
    for (row, placed) in rows.iter().zip(&galley.rows) {
        let mut spans: Vec<(f32, f32)> = vec![];
        for (cell, g) in row.cells.iter().zip(&placed.row.glyphs) {
            if (lo..hi).contains(&(row.start + cell.col)) {
                match spans.last_mut() {
                    Some((_, end)) if (*end - g.pos.x).abs() < 0.5 => {
                        *end = g.pos.x + g.advance_width
                    }
                    _ => spans.push((g.pos.x, g.pos.x + g.advance_width)),
                }
            }
        }
        let y = origin.y + placed.pos.y;
        for (a, b) in spans {
            let x = origin.x + placed.pos.x;
            painter.rect_filled(
                egui::Rect::from_min_max(
                    Pos2::new(x + a, y),
                    Pos2::new(x + b, y + placed.row.size.y),
                ),
                0.0,
                selection,
            );
        }
    }
    if response.has_focus() {
        let cursor = ui.visuals().text_cursor.clone();
        let time = ui.input(|i| i.time);
        let on = !cursor.blink
            || (time % f64::from(cursor.on_duration + cursor.off_duration))
                < f64::from(cursor.on_duration);
        let at = range.primary.index.0;
        if let (true, Some((row, placed))) = (
            on,
            rows.iter().zip(&galley.rows).rfind(|(r, _)| r.start <= at),
        ) {
            let x = origin.x + placed.pos.x + caret_x(row, &placed.row.glyphs, at - row.start);
            let y = origin.y + placed.pos.y;
            painter.vline(x, y..=y + placed.row.size.y, cursor.stroke);
        }
    }
    response
}

fn keep_caret_out_of_clusters(
    ui: &Ui,
    id: egui::Id,
    mut state: egui::text_edit::TextEditState,
    starts: &[usize],
    from: usize,
) -> egui::text_edit::TextEditState {
    if let Some(range) = state.cursor.char_range() {
        let (primary, secondary) = (range.primary.index.0, range.secondary.index.0);
        let (p, s) = snap_range(starts, primary, secondary, from);
        if (p, s) != (primary, secondary) {
            state
                .cursor
                .set_char_range(Some(CCursorRange::two(CCursor::new(s), CCursor::new(p))));
            TextEdit::store_state(ui.ctx(), id, state.clone());
        }
    }
    state
}

/// A single-line text field that handles right-to-left text and multi-part emoji. Customize it through `setup`.
pub fn line(
    ui: &mut Ui,
    text: &mut String,
    base: Base,
    setup: impl FnOnce(TextEdit<'_>) -> TextEdit<'_>,
) -> Response {
    edit(ui, text, false, base, setup)
}

/// A multi-line monospace field for netlists: always left to right, with right-to-left words inside.
pub fn code(
    ui: &mut Ui,
    text: &mut String,
    setup: impl FnOnce(TextEdit<'_>) -> TextEdit<'_>,
) -> Response {
    edit(ui, text, true, Base::Ltr, setup)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn right_to_left_lines_read_from_the_right() {
        assert_eq!(display("ملف جديد", Base::Auto), "جديد ملف");
        assert_eq!(display("الاسم:", Base::Auto), ":الاسم");
        assert_eq!(display("(خرج)", Base::Auto), "(خرج)");
        // Latin words and numbers keep their own order, as one block
        assert_eq!(
            display("حفظ SwiftSim Pro 12 ملف", Base::Auto),
            "ملف SwiftSim Pro 12 حفظ"
        );
        assert_eq!(
            display("פלט ראשי\nخرج أول", Base::Auto),
            "ראשי פלט\nأول خرج"
        );
    }

    #[test]
    fn left_to_right_lines_stay_as_they_are() {
        for s in ["out", "çıkış ÇIKIŞ", "出力 ノード", "V(خرج) x", "abc خرج"] {
            assert_eq!(display(s, Base::Auto), s);
        }
    }

    #[test]
    fn netlist_comments_keep_their_marker_on_the_left() {
        assert_eq!(display("* خرج أول", Base::Ltr), "* أول خرج");
        assert_eq!(display("* خرج أول", Base::Auto), "أول خرج *");
    }

    #[test]
    fn interface_text_is_right_to_left_even_when_it_starts_in_latin() {
        assert_eq!(display("SwiftSim جاهز", Base::Rtl), "جاهز SwiftSim");
        assert_eq!(display("MOSFET (E)", Base::Rtl), "MOSFET (E)");
        assert_eq!(display(".Text", Base::Rtl), ".Text");
    }

    #[test]
    fn inserted_values_keep_their_own_direction() {
        // a path starts with a neutral '/', which would otherwise jump to the far side
        let sentence = format!("تم فتح {}", isolate("/tmp/a b.sws"));
        assert_eq!(display(&sentence, Base::Rtl), "/tmp/a b.sws فتح تم");
    }

    fn cells(text: &str, base: Base) -> Vec<(usize, bool)> {
        let info = BidiInfo::new(text, level(base));
        let p = &info.paragraphs[0];
        visual_line(text, &info, p, p.range.clone()).1
    }

    #[test]
    fn the_character_order_on_screen_follows_the_algorithm() {
        // bytes: ab = 0,1; space = 2; two Arabic letters of 2 bytes each at 3 and 5
        assert_eq!(
            cells("ab جد", Base::Auto),
            [(0, false), (1, false), (2, false), (5, true), (3, true)]
        );
        // a right-to-left paragraph with a number: digits stay left to right in the middle
        let ar = "ج 12 د"; // ج=0..2 space=2 1=3 2=4 space=5 د=6
        assert_eq!(
            cells(ar, Base::Auto),
            [
                (6, true),
                (5, true),
                (3, false),
                (4, false),
                (2, true),
                (0, true)
            ]
        );
    }

    fn glyph(chr: char, x: f32, advance_width: f32) -> Glyph {
        // egui has no public way to build a glyph, so start from a laid-out one
        let mut fonts = egui::epaint::text::Fonts::new(
            egui::epaint::text::TextOptions::default(),
            egui::FontDefinitions::default(),
        );
        let galley = fonts.with_pixels_per_point(1.0).layout_no_wrap(
            "x".to_string(),
            FontId::proportional(14.0),
            Color32::WHITE,
        );
        let mut g = galley.rows[0].row.glyphs[0].clone();
        g.chr = chr;
        g.pos.x = x;
        g.advance_width = advance_width;
        g
    }

    #[test]
    fn extra_and_merged_glyphs_become_one_per_character() {
        // what egui gives for a right-to-left run of 3 characters: 3 glyphs and 2 zero-width extras
        let v = [
            glyph('c', 0.0, 5.0),
            glyph('b', 5.0, 5.0),
            glyph('a', 10.0, 5.0),
            glyph('b', 15.0, 0.0),
            glyph('c', 15.0, 0.0),
        ];
        let g = one_glyph_per_char(&v, &['c', 'b', 'a']);
        assert_eq!(g.iter().map(|g| g.chr).collect::<String>(), "cba");
        // a lam-alef ligature: two characters, one glyph whose character is the logically first
        let g = one_glyph_per_char(&[glyph('ل', 0.0, 8.0)], &['ا', 'ل']);
        assert_eq!(g.len(), 2);
        assert_eq!(g[1].advance_width, 8.0);
    }

    #[test]
    fn caret_and_mouse_agree_in_right_to_left_text() {
        // on screen, left to right: the logically 3rd, 2nd, 1st characters, 10 wide each
        let row = Row {
            start: 0,
            cells: (0..3).rev().map(|col| Cell { col, rtl: true }).collect(),
        };
        let glyphs: Vec<Glyph> = (0..3).map(|i| glyph('ا', i as f32 * 10.0, 10.0)).collect();
        assert_eq!(
            caret_x(&row, &glyphs, 0),
            30.0,
            "text starts at the right edge"
        );
        assert_eq!(caret_x(&row, &glyphs, 1), 20.0);
        assert_eq!(caret_x(&row, &glyphs, 3), 0.0, "text ends at the left edge");
        assert_eq!(
            column_at(&row, &glyphs, 29.0),
            0,
            "right half of the first letter"
        );
        assert_eq!(
            column_at(&row, &glyphs, 21.0),
            1,
            "left half of the first letter"
        );
        assert_eq!(
            column_at(&row, &glyphs, -5.0),
            3,
            "left of everything is the end"
        );
    }

    #[test]
    fn emoji_sequences_stay_whole_in_right_to_left_text() {
        let family = "👨\u{200D}👩\u{200D}👧";
        assert_eq!(
            display(&format!("خرج {family} أول"), Base::Auto),
            format!("أول {family} خرج")
        );
        assert_eq!(display("خرج 👍🏽 أول", Base::Auto), "أول 👍🏽 خرج");
        assert_eq!(display("خرج 🇹🇷 أول", Base::Auto), "أول 🇹🇷 خرج");
    }

    #[test]
    fn the_caret_never_lands_inside_an_emoji() {
        let family = "👨\u{200D}👩\u{200D}👧"; // 5 characters, one cluster
        let starts = cluster_starts(&format!("a{family}b"));
        assert_eq!(starts, [0, 1, 6, 7]);
        assert_eq!(snap_to_cluster(&starts, 2, 1), 6, "moving right");
        assert_eq!(snap_to_cluster(&starts, 5, 6), 1, "moving left");
        assert_eq!(
            snap_to_cluster(&starts, 6, 1),
            6,
            "already between clusters"
        );
        // moving over a cluster must not select it
        assert_eq!(snap_range(&[0, 2, 3], 1, 1, 2), (0, 0));
        assert_eq!(snap_range(&[0, 2, 3], 1, 1, 0), (2, 2));
        assert_eq!(
            snap_range(&[0, 2, 3], 1, 3, 2),
            (0, 3),
            "a real selection keeps both ends"
        );
        assert_eq!(cluster_starts("🇹🇷"), [0, 2]);
        assert_eq!(cluster_starts(""), [0]);
    }

    #[test]
    fn a_word_is_selected_around_the_click() {
        assert_eq!(selection_of_word("ab خرج cd", 4), (3, 6));
        assert_eq!(
            selection_of_word("ab", 9),
            (0, 2),
            "past the end is the last word"
        );
    }
}
