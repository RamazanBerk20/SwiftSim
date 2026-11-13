//! UI languages. The English text is the key; `lang/<code>.json` maps it to the translation.

use super::bidi::{self, Base};
use eframe::egui::containers::menu::SubMenuButton;
use eframe::egui::{Align, Atom, Button, FontSelection, Layout, Ui};
use std::collections::HashMap;
use std::fmt::Display;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicUsize, Ordering};

/// (code, name in that language)
pub const LANGS: [(&str, &str); 10] = [
    ("en", "English"),
    ("tr", "Türkçe"),
    ("es", "Español"),
    ("fr", "Français"),
    ("de", "Deutsch"),
    ("ru", "Русский"),
    ("ar", "العربية"),
    ("ja", "日本語"),
    ("zh", "中文"),
    ("ko", "한국어"),
];

const SOURCES: [&str; 10] = [
    "{}",
    include_str!("lang/tr.json"),
    include_str!("lang/es.json"),
    include_str!("lang/fr.json"),
    include_str!("lang/de.json"),
    include_str!("lang/ru.json"),
    include_str!("lang/ar.json"),
    include_str!("lang/ja.json"),
    include_str!("lang/zh.json"),
    include_str!("lang/ko.json"),
];

/// English text → (translation as written, reordered for display)
static TABLES: LazyLock<Vec<HashMap<String, (String, String)>>> = LazyLock::new(|| {
    SOURCES
        .iter()
        .zip(LANGS)
        .map(|(src, (code, _))| {
            let table: HashMap<String, String> =
                serde_json::from_str(src).expect("bundled translation is valid JSON");
            let base = if code == "ar" { Base::Rtl } else { Base::Auto };
            table
                .into_iter()
                .map(|(k, v)| {
                    let shown = bidi::display(&v, base);
                    (k, (v, shown))
                })
                .collect()
        })
        .collect()
});

static CURRENT: AtomicUsize = AtomicUsize::new(0);

/// Selects the UI language; `None` follows the system language (English if SwiftSim doesn't have it).
pub fn set(code: Option<&str>) {
    let system = sys_locale::get_locale();
    let code = code
        .or(system.as_deref().and_then(|l| l.split(['-', '_']).next()))
        .unwrap_or("en");
    CURRENT.store(
        LANGS.iter().position(|l| l.0 == code).unwrap_or(0),
        Ordering::Relaxed,
    );
}

/// The current language's text for `key`, or `key` itself when it has none.
pub fn tr(key: &'static str) -> &'static str {
    entry(key).map_or(key, |e| e.1.as_str())
}

fn entry(key: &str) -> Option<&'static (String, String)> {
    TABLES[CURRENT.load(Ordering::Relaxed)].get(key)
}

/// `tr` for a paragraph that wraps in a panel as wide as `ui`.
pub fn para(ui: &Ui, key: &'static str) -> String {
    para_in(ui, key, ui.available_width())
}

/// `para` for an explicit width, such as a tooltip's.
pub fn para_in(ui: &Ui, key: &'static str, width: f32) -> String {
    match entry(key) {
        Some((text, _)) if is_rtl_ui() => bidi::wrapped(
            ui,
            text,
            Base::Rtl,
            &FontSelection::default().resolve(ui.style()),
            width,
        ),
        _ => tr(key).to_string(),
    }
}

/// Whether the interface is in a right-to-left language.
pub fn is_rtl_ui() -> bool {
    LANGS[CURRENT.load(Ordering::Relaxed)].0 == "ar"
}

/// A label with a symbol in front of it, on the reading side.
pub fn with_symbol(symbol: &str, label: &str) -> String {
    if is_rtl_ui() {
        format!("{label} {symbol}")
    } else {
        format!("{symbol} {label}")
    }
}

/// The contents of a panel or dialog, mirrored in a right-to-left language: egui then runs
/// `ui.horizontal` rows from the right too. Plots, the schematic and code stay left to right.
pub fn mirrored<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    if is_rtl_ui() {
        ui.with_layout(Layout::top_down(Align::Max), add).inner
    } else {
        add(ui)
    }
}

/// A row in a layout egui fixes to left-to-right (the menu bar).
pub fn bar<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    if is_rtl_ui() {
        ui.with_layout(Layout::right_to_left(Align::Center), add)
            .inner
    } else {
        add(ui)
    }
}

/// A menu whose entries sit on the reading side.
pub fn menu(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui)) {
    ui.menu_button(title, |ui| {
        if is_rtl_ui() {
            ui.with_layout(Layout::top_down_justified(Align::Max), add)
                .inner
        } else {
            add(ui)
        }
    });
}

/// A menu inside another menu.
pub fn submenu(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui)) {
    if is_rtl_ui() {
        SubMenuButton::from_button(Button::new(("◂", Atom::grow(), title))).ui(ui, |ui| {
            ui.with_layout(Layout::top_down_justified(Align::Max), add)
        });
    } else {
        ui.menu_button(title, add);
    }
}

/// One row of a two-column form, with the label on the reading side.
pub fn form_row(ui: &mut Ui, label: &str, field: impl FnOnce(&mut Ui)) {
    if is_rtl_ui() {
        field(ui);
        ui.label(label);
    } else {
        ui.label(label);
        field(ui);
    }
    ui.end_row();
}

/// `tr` for texts with `{}` placeholders, filled in order.
pub fn trf(key: &'static str, args: &[&dyn Display]) -> String {
    let mut s = entry(key).map_or(key, |e| e.0.as_str()).to_string();
    for a in args {
        s = s.replacen("{}", &bidi::isolate(&a.to_string()), 1);
    }
    bidi::display(&s, if is_rtl_ui() { Base::Rtl } else { Base::Auto })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_language_covers_every_text() {
        let tables = &*TABLES;
        let reference = &tables[1];
        for ((code, _), table) in LANGS.iter().zip(tables.iter()).skip(1) {
            for key in reference.keys() {
                assert!(table.contains_key(key), "{code}: missing {key:?}");
            }
            for (key, (value, _)) in table {
                assert!(reference.contains_key(key), "{code}: stale {key:?}");
                assert_eq!(
                    key.matches("{}").count(),
                    value.matches("{}").count(),
                    "{code}: placeholders of {key:?}"
                );
                assert!(!value.trim().is_empty(), "{code}: empty {key:?}");
            }
        }
        // every literal passed to tr()/trf() in the sources has a translation
        let sources = [include_str!("mod.rs"), include_str!("waveform.rs")].concat();
        for (i, _) in sources.match_indices("(\"") {
            let function = sources[..i]
                .rsplit(|c: char| !c.is_alphanumeric() && c != '_')
                .next();
            if matches!(function, Some("tr" | "trf" | "para")) {
                let rest = &sources[i + 2..];
                let key = &rest[..rest.find('"').unwrap()];
                assert!(reference.contains_key(key), "no translation of {key:?}");
            }
        }
    }
}
