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
    Align, Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache, Wrap, fontdb,
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
        if has_family(&db, family) {
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

fn has_family(db: &fontdb::Database, family: &str) -> bool {
    db.faces()
        .any(|f| f.families.iter().any(|(name, _)| name == family))
}

/// Turn the generic default into a real family name.
///
/// cosmic-text's own generic resolves through fontdb's built-in preference
/// ("FreeMono"), which is usually absent and then lands on an arbitrary face — so
/// ask fontconfig instead, since that is what the rest of the desktop uses. A
/// named family passes through untouched; if it turns out to be missing,
/// cosmic-text falls back on its own.
fn resolve_family(db: &fontdb::Database, family: &str) -> String {
    if !is_generic(family) {
        return family.to_string();
    }
    fc_match_mono()
        .filter(|name| has_family(db, name))
        .or_else(|| {
            MONO_CANDIDATES
                .iter()
                .find(|name| has_family(db, name))
                .map(|name| name.to_string())
        })
        .unwrap_or_else(|| family.to_string())
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
    let family = resolve_family(fs.db(), &family);
    let attrs = Attrs::new().family(Family::Name(&family));
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
        family,
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
        let resolved = resolve_family(fs.db(), SYSTEM_MONO);
        assert_ne!(resolved, SYSTEM_MONO, "should have named a real family");
        assert!(
            has_family(fs.db(), &resolved),
            "{resolved:?} is not in the database"
        );
        // A named family passes through, present or not.
        assert_eq!(resolve_family(fs.db(), "Some Font"), "Some Font");
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
