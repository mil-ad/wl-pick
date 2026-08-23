//! Look and layout, ported from the rofi setup this replaces (mytheme.rasi +
//! the -theme-str rofigrid builds): gruvbox dark, a yellow selection that fills
//! the element padding, and a window that hugs the grid.

/// 0xAARRGGBB, premultiplied (everything here is opaque).
pub type Argb = u32;

pub struct Theme {
    pub bg: Argb,
    /// Label colours; used once tiles are labelled.
    #[allow(dead_code)]
    pub fg: Argb,
    pub sel_bg: Argb,
    #[allow(dead_code)]
    pub sel_fg: Argb,
    pub border: Argb,
    /// Window border, logical px (rasi `border: 0.18em` at 12pt ~ 2px).
    pub border_px: i32,
    /// Thumbnail cell, logical px. 16:9 so wide windows fill it instead of
    /// letterboxing in a square box.
    pub tile_w: i32,
    pub tile_h: i32,
    /// Padding inside one element, i.e. around its thumbnail (rasi `element`).
    pub pad: i32,
    /// Space between elements (rasi `listview { spacing }`).
    pub gap: i32,
    /// Margin between the grid and the window edge.
    pub margin: i32,
    pub max_cols: i32,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            bg: 0xff282828,     // gruvbox-dark-bg0
            fg: 0xffebdbb2,     // gruvbox-dark-fg1
            sel_bg: 0xffd79921, // gruvbox-dark-yellow-dark
            sel_fg: 0xff282828,
            border: 0xffd79921,
            border_px: 2,
            tile_w: 220,
            tile_h: 220 * 9 / 16,
            pad: 12,
            gap: 15,
            margin: 12,
            max_cols: 4,
        }
    }
}

/// Where every element and thumbnail goes, in logical px.
pub struct Layout {
    pub cols: i32,
    pub rows: i32,
    pub width: i32,
    pub height: i32,
    elem_w: i32,
    elem_h: i32,
    margin: i32,
    gap: i32,
    pad: i32,
}

impl Layout {
    /// A balanced grid: ceil(sqrt(n)) columns, capped, so the last row isn't
    /// ragged (6 windows -> 3x2, not 4x2 with two holes). Same rule rofigrid uses.
    pub fn new(t: &Theme, n: i32) -> Self {
        let mut cols = (n as f64).sqrt() as i32;
        if cols * cols < n {
            cols += 1;
        }
        cols = cols.clamp(1, t.max_cols);
        let rows = (n + cols - 1) / cols;
        let (elem_w, elem_h) = (t.tile_w + 2 * t.pad, t.tile_h + 2 * t.pad);
        Self {
            cols,
            rows,
            width: cols * elem_w + (cols - 1) * t.gap + 2 * t.margin,
            height: rows * elem_h + (rows - 1) * t.gap + 2 * t.margin,
            elem_w,
            elem_h,
            margin: t.margin,
            gap: t.gap,
            pad: t.pad,
        }
    }

    /// The element box for index i — what the selection highlight fills.
    pub fn elem(&self, i: i32) -> Rect {
        let (col, row) = (i % self.cols, i / self.cols);
        Rect {
            x: self.margin + col * (self.elem_w + self.gap),
            y: self.margin + row * (self.elem_h + self.gap),
            w: self.elem_w,
            h: self.elem_h,
        }
    }

    /// The thumbnail box for index i, i.e. the element box minus its padding.
    pub fn tile(&self, i: i32) -> Rect {
        let e = self.elem(i);
        Rect {
            x: e.x + self.pad,
            y: e.y + self.pad,
            w: e.w - 2 * self.pad,
            h: e.h - 2 * self.pad,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// Scale (w, h) to fit inside (bw, bh), keeping the aspect ratio, and centre it.
/// Windows are usually portrait-ish next to a 16:9 cell, so this letterboxes the
/// same way rofi's `element-icon { size: W H }` does.
pub fn fit_centred(w: i32, h: i32, box_: Rect) -> Rect {
    if w <= 0 || h <= 0 {
        return box_;
    }
    let (mut dw, mut dh) = (box_.w, box_.w * h / w);
    if dh > box_.h {
        dh = box_.h;
        dw = box_.h * w / h;
    }
    let (dw, dh) = (dw.max(1), dh.max(1));
    Rect {
        x: box_.x + (box_.w - dw) / 2,
        y: box_.y + (box_.h - dh) / 2,
        w: dw,
        h: dh,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The grid maths must match rofigrid's, or the window stops hugging the grid.
    #[test]
    fn grid_matches_rofigrid() {
        let t = Theme::default();
        // (n, cols, rows) from rofigrid: cols = min(ceil(sqrt(n)), 4)
        for (n, cols, rows) in [
            (1, 1, 1),
            (2, 2, 1),
            (4, 2, 2),
            (6, 3, 2),
            (12, 4, 3),
            (17, 4, 5),
        ] {
            let l = Layout::new(&t, n);
            assert_eq!((l.cols, l.rows), (cols, rows), "n = {n}");
            // rofigrid: win_w = cols*(ICON+24) + (cols-1)*15 + 24
            assert_eq!(
                l.width,
                cols * (t.tile_w + 24) + (cols - 1) * 15 + 24,
                "width n = {n}"
            );
        }
    }

    #[test]
    fn elements_stay_inside_the_window() {
        let t = Theme::default();
        for n in 1..=20 {
            let l = Layout::new(&t, n);
            for i in 0..n {
                let e = l.elem(i);
                assert!(e.x >= 0 && e.x + e.w <= l.width, "n = {n}, i = {i}");
                assert!(e.y >= 0 && e.y + e.h <= l.height, "n = {n}, i = {i}");
                let tile = l.tile(i);
                assert!(tile.w == t.tile_w && tile.h == t.tile_h);
            }
        }
    }

    #[test]
    fn fit_preserves_aspect_and_centres() {
        let box_ = Rect {
            x: 10,
            y: 20,
            w: 220,
            h: 123,
        };
        // A portrait window letterboxes: height-bound, centred horizontally.
        let r = fit_centred(1000, 2000, box_);
        assert_eq!((r.w, r.h), (61, 123));
        assert_eq!(r.x, 10 + (220 - 61) / 2);
        assert_eq!(r.y, 20);
        // A wide window is width-bound.
        let r = fit_centred(4000, 1000, box_);
        assert_eq!((r.w, r.h), (220, 55));
        assert_eq!(r.y, 20 + (123 - 55) / 2);
    }
}
