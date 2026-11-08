//! Fonts of last resort, so every script displays even on a system without fonts for it.
//! Asked after the system fonts, so an installed font (and its regional glyph forms) always wins.
//! Emoji always use the bundled color font, so they look the same on every machine.
//! `assets/fonts/fallback.bin` is built by `assets/fonts/build.py`.

use eframe::egui::{FontData, FontFamily};
use eframe::egui::epaint::text::{FallbackRequest, FontInsert, FontPriority, FontProvider, InsertFontFamily};
use egui_system_font_provider::SystemFontProvider;
use serde::Deserialize;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::{LazyLock, Mutex};

static PACK: &[u8] = include_bytes!("../../assets/fonts/fallback.bin");

#[derive(Deserialize)]
struct Entry {
    name: String,
    at: usize,
    len: usize,
    /// Sorted, inclusive code point ranges.
    cover: Vec<(u32, u32)>,
}

impl Entry {
    fn covers(&self, c: u32) -> bool {
        let side = |r: &(u32, u32)| if c < r.0 { Ordering::Greater } else if c > r.1 { Ordering::Less } else { Ordering::Equal };
        self.cover.binary_search_by(side).is_ok()
    }
}

fn gunzip(data: &[u8]) -> Vec<u8> {
    let mut out = vec![];
    flate2::read::GzDecoder::new(data).read_to_end(&mut out).expect("bundled fonts are valid gzip");
    out
}

/// (start of the font data in `PACK`, the fonts)
static INDEX: LazyLock<(usize, Vec<Entry>)> = LazyLock::new(|| {
    let len = u32::from_le_bytes([PACK[0], PACK[1], PACK[2], PACK[3]]) as usize;
    (4 + len, serde_json::from_slice(&gunzip(&PACK[4..4 + len])).expect("bundled font index is valid JSON"))
});

/// Unpacks a font the first time it's needed. Kept for the life of the program: each font at most
/// once, and egui keeps using it anyway.
fn load(i: usize) -> &'static [u8] {
    static LOADED: LazyLock<Mutex<HashMap<usize, &'static [u8]>>> = LazyLock::new(Default::default);
    let mut loaded = LOADED.lock().unwrap_or_else(|e| e.into_inner());
    loaded
        .entry(i)
        .or_insert_with(|| {
            let (start, entries) = &*INDEX;
            let e = &entries[i];
            let stored = &PACK[start + e.at..start + e.at + e.len];
            if stored.starts_with(&[0x1f, 0x8b]) { Vec::leak(gunzip(stored)) } else { stored }
        })
}

/// Must be in the fallback chain before any font found later: egui takes the first font with a
/// glyph, and many plain system fonts have black-and-white versions of the emoji.
fn color_emoji(family: &FontFamily) -> Option<FontInsert> {
    let i = INDEX.1.iter().position(|e| e.name.contains("Emoji"))?;
    Some(FontInsert {
        name: "bundled:emoji".to_string(),
        data: FontData::from_static(load(i)),
        families: vec![InsertFontFamily { family: family.clone(), priority: FontPriority::Lowest }],
    })
}

/// The system's fonts first, then the bundled ones.
pub struct Fallback {
    system: SystemFontProvider,
    /// Set once the system lookup has panicked.
    no_system_fonts: AtomicBool,
}

impl Fallback {
    pub fn new() -> Fallback {
        Fallback { system: SystemFontProvider::new(), no_system_fonts: AtomicBool::new(false) }
    }

    /// The font library panics on a machine with no fonts at all, which is exactly when the
    /// bundled ones are needed.
    fn system_font(&self, request: &FallbackRequest<'_>) -> Option<FontInsert> {
        if self.no_system_fonts.load(AtomicOrdering::Relaxed) {
            return None;
        }
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.system.font_for(request))).unwrap_or_else(|_| {
            self.no_system_fonts.store(true, AtomicOrdering::Relaxed);
            None
        })
    }
}

impl FontProvider for Fallback {
    fn fonts_for_family(&self, family: &FontFamily) -> Vec<FontInsert> {
        color_emoji(family).into_iter().collect()
    }

    fn font_for(&self, request: &FallbackRequest<'_>) -> Option<FontInsert> {
        self.system_font(request).or_else(|| Bundled.font_for(request))
    }
}

struct Bundled;

impl FontProvider for Bundled {
    fn font_for(&self, request: &FallbackRequest<'_>) -> Option<FontInsert> {
        let c = request.cluster.chars().next()? as u32;
        let i = INDEX.1.iter().position(|e| e.covers(c))?;
        Some(FontInsert {
            name: format!("bundled:{}", INDEX.1[i].name),
            data: FontData::from_static(load(i)),
            families: vec![InsertFontFamily { family: request.family.clone(), priority: FontPriority::Lowest }],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{Color32, FontDefinitions, FontId};
    use eframe::egui::epaint::text::{Fonts, TextOptions};

    fn ask(c: char) -> Option<FontInsert> {
        Bundled.font_for(&FallbackRequest { cluster: &c.to_string(), family: &FontFamily::Proportional })
    }

    #[test]
    fn every_ui_language_and_major_script_has_a_font() {
        for c in "ıİşğçÇ ЖжЯ خجلا ה 出力ノ 输节 출력 แ आ অ த ಕ മ ሀ ა Ա ဗ ក ⵣ ꦗ ệ Ω".chars().filter(|c| *c != ' ') {
            let font = ask(c).unwrap_or_else(|| panic!("no bundled font for {c}"));
            assert!(font.name.starts_with("bundled:"));
        }
    }

    #[test]
    fn all_letters_of_a_script_come_from_one_font() {
        // two fonts inside one word would break Arabic joining and the shaping of Indic scripts
        for script in ["ابتثجحخدذرزسشصضطظعغفقكلمنهويءئؤأإآةى", "אבגדהוזחטיכלמנסעפצקרשת", "अआइउएकखगटतपमरलवसह", "กขคงจฉชซญดตถทนบป"] {
            let names: std::collections::HashSet<String> = script.chars().map(|c| ask(c).unwrap().name).collect();
            assert_eq!(names.len(), 1, "{script}: {names:?}");
            // and no other font can take a letter once it's in egui's fallback chain
            for c in script.chars() {
                assert_eq!(INDEX.1.iter().filter(|e| e.covers(c as u32)).count(), 1, "{c}");
            }
        }
    }

    #[test]
    fn emoji_are_in_color_and_plain_symbols_are_not() {
        let providers: Vec<std::sync::Arc<dyn FontProvider>> = vec![std::sync::Arc::new(Emoji), std::sync::Arc::new(Bundled)];
        let mut fonts = Fonts::new(TextOptions::default(), FontDefinitions::default()).with_font_providers(providers);
        let mut colors = |t: &str| -> Vec<bool> {
            let galley = fonts.with_pixels_per_point(2.0).layout_no_wrap(t.to_string(), FontId::proportional(14.0), Color32::WHITE);
            galley.rows[0].row.glyphs.iter().filter(|g| g.advance_width > 0.0).map(|g| g.is_color).collect()
        };
        for e in ["😀", "🦀", "❤️", "🇹🇷", "👍🏽", "👨\u{200D}👩\u{200D}👧"] {
            assert_eq!(colors(e), [true], "{e}: one color glyph");
        }
        for s in ["▶", "■", "⟲", "A", "1"] {
            assert_eq!(colors(s), [false], "{s}: stays a plain symbol");
        }
    }

    /// `Fallback` without the system lookup, so the test sees only what's compiled in.
    struct Emoji;
    impl FontProvider for Emoji {
        fn fonts_for_family(&self, family: &FontFamily) -> Vec<FontInsert> {
            color_emoji(family).into_iter().collect()
        }
    }

    #[test]
    fn unassigned_code_points_are_left_alone() {
        assert!(ask('\u{378}').is_none());
    }

    #[test]
    fn fonts_unpack_to_font_files_and_only_once() {
        let (a, b) = (load(1), load(1));
        assert!(std::ptr::eq(a, b));
        assert!(matches!(&a[..4], b"OTTO" | [0, 1, 0, 0]), "not a font: {:?}", &a[..4]);
    }
}
