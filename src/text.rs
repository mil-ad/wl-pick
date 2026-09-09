//! Labels.
//!
//! Building a font system and rasterising the first glyphs costs ~55ms, which is
//! almost exactly the window the compositor spends copying window pixels back
//! for us. So all of it happens on a worker thread started before the captures
//! and joined after them: by the time anything is drawn, every label is shaped
//! and its glyphs are already in the cache, and painting one costs ~0.1ms.
//!
//! Sizes here are physical pixels — the caller scales logical units first,
//! because the chrome buffer it paints into is physical too.

use std::thread::{self, JoinHandle};

use cosmic_text::{
    Align, Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, Stretch, SwashCache, Weight,
    Wrap, fontdb,
};

use crate::shm::Painter;
use crate::theme::{Argb, Rect};

pub struct Labels {
    fs: FontSystem,
    cache: SwashCache,
    lines: Vec<Buffer>,
    family: String,
}

/// Shape `texts` into one centred single line each, at most `box_w` wide.
pub fn spawn(
    texts: Vec<String>,
    family: String,
    font_px: f32,
    line_h: f32,
    box_w: f32,
) -> JoinHandle<Labels> {
    thread::spawn(move || build(texts, family, font_px, line_h, box_w))
}

/// The default family: whatever this system calls its monospace font.
pub const SYSTEM_MONO: &str = "monospace";

/// Families to try when fontconfig cannot be asked, roughly in order of how
/// likely a distribution is to ship one as its default monospace.
const MONO_CANDIDATES: &[&str] = &[
    "Noto Sans Mono",
    "DejaVu Sans Mono",
    "Liberation Mono",
    "Adwaita Mono",
    "Source Code Pro",
    "Hack",
    "Fira Mono",
    "Courier New",
];

/// Load the smallest font database that can render `family`.
///
/// `FontSystem::new()` scans every system font, which costs ~37ms — most of the
/// startup budget. A user's own font directories are tiny by comparison, so try
/// those first and only pay for the full scan when the family really isn't there
/// (which is also what makes an unknown family fall back gracefully). The
/// generic default lives among the system fonts, so it skips that shortcut.
fn font_db(family: &str) -> FontSystem {
    let mut db = fontdb::Database::new();
    if !is_generic(family) {
        if let Ok(home) = std::env::var("HOME") {
            db.load_fonts_dir(format!("{home}/.fonts"));
            db.load_fonts_dir(format!("{home}/.local/share/fonts"));
        }
        if interpret(&db, family).is_some() {
            // The locale only orders CJK fallbacks; labels are ids and titles.
            return FontSystem::new_with_locale_and_db("en-US".to_string(), db);
        }
    }
    db.load_system_fonts();
    FontSystem::new_with_locale_and_db("en-US".to_string(), db)
}

fn is_generic(family: &str) -> bool {
    family.eq_ignore_ascii_case(SYSTEM_MONO)
}

/// The database's own spelling of `want`, matched the way fontconfig matches:
/// without caring about case.
fn family_named(db: &fontdb::Database, want: &str) -> Option<String> {
    db.faces()
        .flat_map(|face| face.families.iter())
        .find(|(name, _)| name.eq_ignore_ascii_case(want))
        .map(|(name, _)| name.clone())
}

/// A family name, and the face within it to ask for.
///
/// Fonts are commonly known by their full display name -- "Berkeley Mono Medium
/// SemiCondensed" is what `fc-match` prints and what a font menu shows -- but
/// only "Berkeley Mono" is the family; the rest names a face inside it. Asking
/// fontdb for the whole string matches nothing, so a request is split into the
/// longest part that is a real family and a style read off the remainder.
#[derive(Debug, PartialEq)]
struct Choice {
    family: String,
    weight: Weight,
    stretch: Stretch,
}

impl Choice {
    fn plain(family: &str) -> Self {
        Self {
            family: family.to_string(),
            weight: Weight::NORMAL,
            stretch: Stretch::Normal,
        }
    }

    fn attrs(&self) -> Attrs<'_> {
        Attrs::new()
            .family(Family::Name(&self.family))
            .weight(self.weight)
            .stretch(self.stretch)
    }

    /// How to describe what was used, in the shape the request was written in.
    fn describe(&self) -> String {
        let mut out = self.family.clone();
        if self.weight != Weight::NORMAL {
            out.push(' ');
            out.push_str(weight_name(self.weight));
        }
        if self.stretch != Stretch::Normal {
            out.push(' ');
            out.push_str(stretch_name(self.stretch));
        }
        out
    }
}

/// What `request` names, as a family the database has plus a style.
fn interpret(db: &fontdb::Database, request: &str) -> Option<Choice> {
    split_request(request, |name| family_named(db, name))
}

/// Split `request` into the longest leading part that names a family and a
/// style read off the words after it. `lookup` answers with the database's own
/// spelling of a family, or `None` if it has no such family.
///
/// Longest first, so a family whose own name ends in a style word -- "Fira Code
/// Light" is a family in its own right -- wins over reading that word as a
/// style. A trailing word that is neither a weight nor a width means this
/// reading of the name is wrong, so the search keeps shortening rather than
/// quietly ignoring it.
fn split_request(request: &str, lookup: impl Fn(&str) -> Option<String>) -> Option<Choice> {
    let words: Vec<&str> = request.split_whitespace().collect();
    for split in (1..=words.len()).rev() {
        let Some(family) = lookup(&words[..split].join(" ")) else {
            continue;
        };
        let mut choice = Choice::plain(&family);
        if read_style(&words[split..], &mut choice) {
            return Some(choice);
        }
    }
    None
}

/// Apply the style `words` to `choice`, or report that one of them is not a
/// style at all.
fn read_style(words: &[&str], choice: &mut Choice) -> bool {
    let mut i = 0;
    while i < words.len() {
        // Fonts spell a two-part style either way round -- "ExtraLight" and
        // "Extra Light" are the same face -- so each pair of words gets a look
        // before either is read on its own.
        let pair = words.get(i..i + 2).map(|two| two.concat());
        if pair
            .as_deref()
            .is_some_and(|pair| apply_style(pair, choice))
        {
            i += 2;
        } else if apply_style(words[i], choice) {
            i += 1;
        } else {
            return false;
        }
    }
    true
}

/// Read one word as a weight or a width, and apply it.
fn apply_style(word: &str, choice: &mut Choice) -> bool {
    if let Some(weight) = weight_from(word) {
        choice.weight = weight;
        true
    } else if let Some(stretch) = stretch_from(word) {
        choice.stretch = stretch;
        true
    } else {
        false
    }
}

/// Style words as fonts spell them, joined or spaced, in any case.
fn normalise(word: &str) -> String {
    word.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn weight_from(word: &str) -> Option<Weight> {
    Some(match normalise(word).as_str() {
        "thin" | "hairline" => Weight::THIN,
        "extralight" | "ultralight" => Weight::EXTRA_LIGHT,
        "light" => Weight::LIGHT,
        "regular" | "normal" | "book" => Weight::NORMAL,
        "medium" => Weight::MEDIUM,
        "semibold" | "demibold" => Weight::SEMIBOLD,
        "bold" => Weight::BOLD,
        "extrabold" | "ultrabold" => Weight::EXTRA_BOLD,
        "black" | "heavy" => Weight::BLACK,
        _ => return None,
    })
}

fn weight_name(weight: Weight) -> &'static str {
    match weight {
        Weight::THIN => "Thin",
        Weight::EXTRA_LIGHT => "ExtraLight",
        Weight::LIGHT => "Light",
        Weight::MEDIUM => "Medium",
        Weight::SEMIBOLD => "SemiBold",
        Weight::BOLD => "Bold",
        Weight::EXTRA_BOLD => "ExtraBold",
        Weight::BLACK => "Black",
        _ => "Regular",
    }
}

fn stretch_from(word: &str) -> Option<Stretch> {
    Some(match normalise(word).as_str() {
        "ultracondensed" => Stretch::UltraCondensed,
        "extracondensed" => Stretch::ExtraCondensed,
        "condensed" => Stretch::Condensed,
        "semicondensed" => Stretch::SemiCondensed,
        "semiexpanded" => Stretch::SemiExpanded,
        "expanded" => Stretch::Expanded,
        "extraexpanded" => Stretch::ExtraExpanded,
        "ultraexpanded" => Stretch::UltraExpanded,
        _ => return None,
    })
}

fn stretch_name(stretch: Stretch) -> &'static str {
    match stretch {
        Stretch::UltraCondensed => "UltraCondensed",
        Stretch::ExtraCondensed => "ExtraCondensed",
        Stretch::Condensed => "Condensed",
        Stretch::SemiCondensed => "SemiCondensed",
        Stretch::Normal => "Normal",
        Stretch::SemiExpanded => "SemiExpanded",
        Stretch::Expanded => "Expanded",
        Stretch::ExtraExpanded => "ExtraExpanded",
        Stretch::UltraExpanded => "UltraExpanded",
    }
}

/// What to shape the labels with, given what was asked for.
///
/// A request that names nothing at all is worth complaining about: silently
/// drawing in some other font looks like the setting was ignored, which is
/// exactly how it reads from the outside.
fn choose(db: &fontdb::Database, request: &str) -> Choice {
    if !is_generic(request) {
        if let Some(choice) = interpret(db, request) {
            return choice;
        }
        let fallback = system_mono(db);
        eprintln!(
            "wl-pick: no font matching {request:?}, using {:?}; \
             `fc-match -f '%{{family}}\\n' {request:?}` names the family",
            fallback.family
        );
        return fallback;
    }
    system_mono(db)
}

/// The generic default, resolved to a real family. cosmic-text's own generic
/// goes through fontdb's built-in preference ("FreeMono"), which is usually
/// absent and then lands on an arbitrary face — so ask fontconfig instead,
/// since that is what the rest of the desktop uses.
fn system_mono(db: &fontdb::Database) -> Choice {
    fc_match_mono()
        .filter(|name| family_named(db, name).is_some())
        .or_else(|| {
            MONO_CANDIDATES
                .iter()
                .find(|name| family_named(db, name).is_some())
                .map(|name| name.to_string())
        })
        .map_or_else(|| Choice::plain(SYSTEM_MONO), |name| Choice::plain(&name))
}

/// What fontconfig says "monospace" means here. A system without the fontconfig
/// tools is not an error: the candidate list covers the common defaults.
fn fc_match_mono() -> Option<String> {
    let out = std::process::Command::new("fc-match")
        .args(["-f", "%{family}", SYSTEM_MONO])
        .output()
        .ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    // fc-match can answer with several comma-separated aliases for one face.
    let first = text.split(',').next()?.trim().to_string();
    (!first.is_empty()).then_some(first)
}

fn build(texts: Vec<String>, family: String, font_px: f32, line_h: f32, box_w: f32) -> Labels {
    let mut fs = font_db(&family);
    let mut cache = SwashCache::new();
    let choice = choose(fs.db(), &family);
    let attrs = choice.attrs();
    let metrics = Metrics::new(font_px, line_h);

    let mut lines = Vec::with_capacity(texts.len());
    for text in &texts {
        let fitted = ellipsize(&mut fs, &attrs, metrics, text, box_w);
        let mut buf = Buffer::new(&mut fs, metrics);
        buf.set_wrap(Wrap::None);
        buf.set_size(Some(box_w), Some(line_h));
        buf.set_text(&fitted, &attrs, Shaping::Advanced, Some(Align::Center));
        // Warm the glyph cache here instead of on the first paint.
        buf.draw(&mut fs, &mut cache, Color::rgb(0, 0, 0), |_, _, _, _, _| {});
        lines.push(buf);
    }
    Labels {
        fs,
        cache,
        lines,
        // What was actually used, not what was asked for.
        family: choice.describe(),
    }
}

/// Shorten `text` until it fits in `box_w`, ending with an ellipsis — window
/// titles are arbitrarily long, and rofi ellipsised them too.
fn ellipsize(
    fs: &mut FontSystem,
    attrs: &Attrs,
    metrics: Metrics,
    text: &str,
    box_w: f32,
) -> String {
    let measure = |fs: &mut FontSystem, s: &str| {
        let mut b = Buffer::new(fs, metrics);
        b.set_wrap(Wrap::None);
        b.set_size(None, Some(metrics.line_height));
        b.set_text(s, attrs, Shaping::Advanced, None);
        b.shape_until_scroll(fs, false);
        b.layout_runs().map(|r| r.line_w).fold(0.0, f32::max)
    };

    let full = measure(fs, text);
    if full <= box_w {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    // Proportional first guess, then shrink geometrically. Bounded, because a
    // pathological title should not cost hundreds of reshapes.
    let mut keep = ((chars.len() as f32) * box_w / full).floor() as usize;
    for _ in 0..12 {
        keep = keep.min(chars.len().saturating_sub(1));
        let mut s: String = chars[..keep].iter().collect();
        s.push('…');
        if keep == 0 || measure(fs, &s) <= box_w {
            return s;
        }
        keep = (keep * 9 / 10).min(keep.saturating_sub(1));
    }
    let mut s: String = chars[..keep.min(chars.len())].iter().collect();
    s.push('…');
    s
}

impl Labels {
    /// The family the labels were actually shaped with, which for the generic
    /// default depends on what this system has installed.
    pub fn family(&self) -> &str {
        &self.family
    }

    /// Draw label `i` inside `at` (physical px), clipped to it.
    pub fn draw(&mut self, p: &mut Painter, i: usize, at: Rect, color: Argb) {
        let Some(buf) = self.lines.get_mut(i) else {
            return;
        };
        let rgb = Color::rgb((color >> 16) as u8, (color >> 8) as u8, color as u8);
        buf.draw(&mut self.fs, &mut self.cache, rgb, |x, y, w, h, c| {
            p.blend(
                Rect {
                    x: at.x + x,
                    y: at.y + y,
                    w: w as i32,
                    h: h as i32,
                },
                (c.r(), c.g(), c.b(), c.a()),
                at,
            );
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of a label is pixels on the surface, so assert some.
    #[test]
    fn draws_visible_glyphs() {
        let (w, h) = (400, 34);
        let mut labels = build(
            vec!["Hello".to_string()],
            "monospace".to_string(),
            26.0,
            h as f32,
            w as f32,
        );
        let mut px = vec![0u8; (w * h * 4) as usize];
        let mut p = Painter::new(&mut px, w, h);
        let at = Rect { x: 0, y: 0, w, h };
        labels.draw(&mut p, 0, at, 0x00ffffff);
        let touched = px
            .chunks_exact(4)
            .filter(|c| c[0] != 0 || c[1] != 0 || c[2] != 0)
            .count();
        assert!(touched > 20, "only {touched} pixels were painted");
    }

    #[test]
    fn the_generic_default_resolves_to_a_real_monospace_family() {
        let fs = font_db(SYSTEM_MONO);
        let choice = choose(fs.db(), SYSTEM_MONO);
        assert_ne!(
            choice.family, SYSTEM_MONO,
            "should have named a real family"
        );
        assert!(
            family_named(fs.db(), &choice.family).is_some(),
            "{:?} is not in the database",
            choice.family
        );
    }

    #[test]
    fn a_family_that_is_not_installed_falls_back_to_the_default() {
        // This used to pass the name straight through to cosmic-text, which
        // shaped in whatever it liked while --verbose reported the name that
        // had been asked for -- so a font setting that did nothing looked
        // exactly like one that worked.
        let fs = font_db(SYSTEM_MONO);
        let asked = choose(fs.db(), "No Such Family At All");
        assert_eq!(asked, choose(fs.db(), SYSTEM_MONO), "should be the default");
        assert_ne!(asked.family, "No Such Family At All");
    }

    /// A database that knows exactly these families.
    fn db(families: &[&'static str]) -> impl Fn(&str) -> Option<String> {
        let families: Vec<&str> = families.to_vec();
        move |want| {
            families
                .iter()
                .find(|name| name.eq_ignore_ascii_case(want))
                .map(|name| name.to_string())
        }
    }

    #[test]
    fn a_full_font_name_splits_into_family_and_style() {
        // What fc-match prints and what a font menu shows: only the first part
        // of it is the family, which is why asking fontdb for the whole string
        // matched nothing.
        let choice = split_request("Berkeley Mono Medium SemiCondensed", db(&["Berkeley Mono"]))
            .expect("should resolve");
        assert_eq!(choice.family, "Berkeley Mono");
        assert_eq!(choice.weight, Weight::MEDIUM);
        assert_eq!(choice.stretch, Stretch::SemiCondensed);
        assert_eq!(choice.describe(), "Berkeley Mono Medium SemiCondensed");
    }

    #[test]
    fn a_plain_family_keeps_its_defaults() {
        let choice = split_request("Berkeley Mono", db(&["Berkeley Mono"])).expect("resolves");
        assert_eq!(choice, Choice::plain("Berkeley Mono"));
        assert_eq!(choice.describe(), "Berkeley Mono");
    }

    #[test]
    fn a_family_may_end_in_a_style_word() {
        // "Fira Code Light" is a family in its own right, so it must win over
        // reading "Light" as the weight of "Fira Code".
        let choice = split_request("Fira Code Light", db(&["Fira Code", "Fira Code Light"]))
            .expect("resolves");
        assert_eq!(choice.family, "Fira Code Light");
        assert_eq!(choice.weight, Weight::NORMAL);
    }

    #[test]
    fn style_words_are_spelled_many_ways() {
        let cases = [
            ("Iosevka demibold", Weight::SEMIBOLD, Stretch::Normal),
            ("Iosevka Extra Light", Weight::EXTRA_LIGHT, Stretch::Normal),
            (
                "Iosevka ULTRACONDENSED",
                Weight::NORMAL,
                Stretch::UltraCondensed,
            ),
            ("Iosevka Bold Condensed", Weight::BOLD, Stretch::Condensed),
        ];
        for (request, weight, stretch) in cases {
            let choice = split_request(request, db(&["Iosevka"]))
                .unwrap_or_else(|| panic!("{request:?} should resolve"));
            assert_eq!(choice.family, "Iosevka", "{request:?}");
            assert_eq!(choice.weight, weight, "{request:?}");
            assert_eq!(choice.stretch, stretch, "{request:?}");
        }
    }

    #[test]
    fn a_name_that_means_nothing_resolves_to_nothing() {
        // The caller warns and falls back. Quietly dropping the word it cannot
        // read would shape in the wrong face and say nothing about it.
        let known = db(&["Berkeley Mono"]);
        assert_eq!(split_request("Berkeley Monospace Bold", &known), None);
        assert_eq!(split_request("Berkeley Mono Nonsense", &known), None);
        assert_eq!(split_request("Comic Sans", &known), None);
        assert_eq!(split_request("", &known), None);
    }

    #[test]
    fn case_does_not_matter_but_the_font_keeps_its_own_spelling() {
        let choice = split_request("berkeley mono bold", db(&["Berkeley Mono"])).expect("resolves");
        assert_eq!(choice.family, "Berkeley Mono", "the database's spelling");
        assert_eq!(choice.weight, Weight::BOLD);
    }

    #[test]
    fn ellipsizes_long_titles() {
        let mut fs = FontSystem::new();
        let attrs = Attrs::new();
        let metrics = Metrics::new(26.0, 34.0);
        let long = "a very long window title that certainly does not fit in one narrow cell";
        let out = ellipsize(&mut fs, &attrs, metrics, long, 200.0);
        assert!(out.ends_with('…'), "got {out:?}");
        assert!(out.chars().count() < long.chars().count());
        // Short text is left alone.
        assert_eq!(ellipsize(&mut fs, &attrs, metrics, "zsh", 200.0), "zsh");
    }
}
