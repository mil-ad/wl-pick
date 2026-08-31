//! Look and layout, ported from the rofi setup this replaces (mytheme.rasi +
//! the -theme-str rofigrid builds): gruvbox dark, a yellow selection that fills
//! the element padding, and a window that hugs the grid.

/// 0xAARRGGBB, premultiplied (everything here is opaque).
pub type Argb = u32;

pub struct Theme {
    pub bg: Argb,
    /// Label colours.
    pub fg: Argb,
    pub sel_bg: Argb,
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
    /// Gap between a thumbnail and its label (rasi `element { spacing }`).
    pub spacing: i32,
    /// Label font family, resolved against the system's fonts. The default is
    /// the generic "monospace", which becomes whatever fontconfig says that is
    /// here. Size and line height are logical px, matching the rofi theme the
    /// look came from (12pt at pango size="small").
    pub font: String,
    pub font_px: f32,
    pub line_h: i32,
    /// Draw labels at all (rofigrid's --hide-labels drew an icon-only grid).
    pub labels: bool,
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
            spacing: 10,
            font: crate::text::SYSTEM_MONO.to_string(),
            font_px: 13.3,
            line_h: 17,
            labels: true,
        }
    }
}

/// Where every element and thumbnail goes, in logical px.
#[derive(Debug)]
pub struct Layout {
    pub cols: i32,
    pub rows: i32,
    /// How many tiles there are, which the last row may not fill.
    n: i32,
    pub width: i32,
    pub height: i32,
    elem_w: i32,
    elem_h: i32,
    margin: i32,
    gap: i32,
    pad: i32,
    tile_h: i32,
    spacing: i32,
    line_h: i32,
    labels: bool,
}

/// How much of the display the grid may occupy before tiles start shrinking.
const FILL: i32 = 90;

impl Layout {
    /// A balanced grid: ceil(sqrt(n)) columns, capped, so the last row isn't
    /// ragged (6 windows -> 3x2, not 4x2 with two holes). Same rule rofigrid uses.
    ///
    /// `display` is the logical size of the screen this will appear on. Tiles are
    /// the size the theme asks for until the grid would outgrow the screen, at
    /// which point they shrink together — keeping their aspect — so thirty
    /// windows give small tiles rather than a surface larger than the monitor.
    pub fn new(t: &Theme, n: i32, display: (i32, i32)) -> Self {
        let mut cols = (n as f64).sqrt() as i32;
        if cols * cols < n {
            cols += 1;
        }
        cols = cols.clamp(1, t.max_cols);
        let rows = (n + cols - 1) / cols;
        // An element is the thumbnail, optionally a label under it, and padding.
        let label_row = if t.labels { t.spacing + t.line_h } else { 0 };
        let (tile_w, tile_h) = fit_grid(t, n, cols, rows, label_row, display);
        let (elem_w, elem_h) = (tile_w + 2 * t.pad, tile_h + label_row + 2 * t.pad);
        Self {
            cols,
            rows,
            n,
            // Padding, gaps and label rows can outgrow a small display all by
            // themselves, so the surface is capped: better a clipped last row
            // than asking for a window larger than the screen showing it.
            width: (cols * elem_w + (cols - 1) * t.gap + 2 * t.margin).min(display.0.max(1)),
            height: (rows * elem_h + (rows - 1) * t.gap + 2 * t.margin).min(display.1.max(1)),
            elem_w,
            elem_h,
            margin: t.margin,
            gap: t.gap,
            pad: t.pad,
            tile_h,
            spacing: t.spacing,
            line_h: t.line_h,
            labels: t.labels,
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

    /// The thumbnail box for index i: the top of the element, above the label.
    pub fn tile(&self, i: i32) -> Rect {
        let e = self.elem(i);
        Rect {
            x: e.x + self.pad,
            y: e.y + self.pad,
            w: e.w - 2 * self.pad,
            h: self.tile_h,
        }
    }

    /// The tile at a point in surface-local coordinates, if any. Points in the
    /// gaps between elements and in the window margin belong to nothing, and so
    /// do the empty cells of a ragged last row.
    pub fn hit(&self, x: i32, y: i32) -> Option<usize> {
        let col = self.axis(x, self.margin, self.elem_w, self.cols)?;
        let row = self.axis(y, self.margin, self.elem_h, self.rows)?;
        let i = row * self.cols + col;
        (i < self.n).then_some(i as usize)
    }

    /// Which cell along one axis a coordinate falls in, or None if it landed in
    /// the margin or a gap.
    fn axis(&self, v: i32, margin: i32, elem: i32, count: i32) -> Option<i32> {
        let pitch = elem + self.gap;
        let offset = v - margin;
        if offset < 0 {
            return None;
        }
        let cell = offset / pitch;
        (cell < count && offset % pitch < elem).then_some(cell)
    }

    /// The single line of text under the thumbnail, if labels are drawn.
    pub fn label(&self, i: i32) -> Option<Rect> {
        if !self.labels {
            return None;
        }
        let t = self.tile(i);
        Some(Rect {
            x: t.x,
            y: t.y + t.h + self.spacing,
            w: t.w,
            h: self.line_h,
        })
    }
}

/// The largest tile that keeps a `cols`x`rows` grid inside `FILL`% of the
/// display, never larger than the theme asks for, and at least a pixel. Both
/// axes shrink by the same factor, so a tile keeps its shape.
fn fit_grid(
    t: &Theme,
    n: i32,
    cols: i32,
    rows: i32,
    label_row: i32,
    (dw, dh): (i32, i32),
) -> (i32, i32) {
    let (want_w, want_h) = (t.tile_w.max(1), t.tile_h.max(1));
    if n == 0 || dw <= 0 || dh <= 0 {
        return (want_w, want_h);
    }
    // Everything in the surface that is not thumbnail.
    let chrome_w = cols * 2 * t.pad + (cols - 1) * t.gap + 2 * t.margin;
    let chrome_h = rows * (2 * t.pad + label_row) + (rows - 1) * t.gap + 2 * t.margin;
    let room_w = (dw * FILL / 100 - chrome_w).max(cols);
    let room_h = (dh * FILL / 100 - chrome_h).max(rows);
    let scale = (room_w as f32 / (cols * want_w) as f32)
        .min(room_h as f32 / (rows * want_h) as f32)
        .min(1.0);
    (
        ((want_w as f32 * scale) as i32).max(1),
        ((want_h as f32 * scale) as i32).max(1),
    )
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    /// Logical to physical, for painting into a scaled buffer.
    pub fn scaled(self, scale: i32) -> Self {
        Self {
            x: self.x * scale,
            y: self.y * scale,
            w: self.w * scale,
            h: self.h * scale,
        }
    }
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

    /// A display large enough that nothing is clamped, so the geometry tests
    /// keep testing geometry.
    const ROOMY: (i32, i32) = (10_000, 10_000);

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
            let l = Layout::new(&t, n, ROOMY);
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
            let l = Layout::new(&t, n, ROOMY);
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
    fn labels_add_a_row_under_each_thumbnail() {
        let mut t = Theme::default();
        let with = Layout::new(&t, 4, ROOMY);
        t.labels = false;
        let without = Layout::new(&t, 4, ROOMY);
        let rows = 2;
        assert_eq!(with.height - without.height, rows * (t.spacing + t.line_h));
        assert!(without.label(0).is_none());

        let t = Theme::default();
        let l = Layout::new(&t, 4, ROOMY);
        for i in 0..4 {
            let (tile, label, elem) = (l.tile(i), l.label(i).unwrap(), l.elem(i));
            assert_eq!(tile.h, t.tile_h);
            assert_eq!(label.y, tile.y + tile.h + t.spacing);
            assert_eq!(label.w, tile.w);
            // Everything, padding included, stays inside the element.
            assert!(label.y + label.h + t.pad <= elem.y + elem.h);
        }
    }

    #[test]
    fn the_grid_shrinks_to_fit_a_small_display() {
        let t = Theme::default();
        let big = Layout::new(&t, 12, ROOMY);
        assert_eq!(big.tile(0).w, t.tile_w, "no clamping when there is room");

        // Twenty tiles at full size cannot fit a 1024x768 screen.
        let small = Layout::new(&t, 20, (1024, 768));
        assert!(
            small.width <= 1024 && small.height <= 768,
            "{small:?} overflows"
        );
        assert!(small.tile(0).w < t.tile_w, "tiles should have shrunk");
        // Shrinking keeps the tile's shape.
        let want = t.tile_w as f32 / t.tile_h as f32;
        let got = small.tile(0).w as f32 / small.tile(0).h as f32;
        assert!(
            (want - got).abs() < 0.05,
            "aspect {got} drifted from {want}"
        );
    }

    #[test]
    fn a_tiny_display_never_gets_an_oversized_surface() {
        let t = Theme::default();
        // Thirty windows on a 640x480 screen: the padding and label rows alone
        // do not fit, so tiles bottom out and the surface is capped instead.
        let l = Layout::new(&t, 30, (640, 480));
        assert!(l.tile(0).w >= 1 && l.tile(0).h >= 1, "{l:?}");
        assert!(l.width <= 640 && l.height <= 480, "{l:?}");
    }

    #[test]
    fn hit_testing_is_the_inverse_of_the_layout() {
        let t = Theme::default();
        // 7 tiles over 3 columns: the last row holds one, so two cells are empty.
        let l = Layout::new(&t, 7, ROOMY);
        for i in 0..7 {
            let e = l.elem(i);
            for (x, y, what) in [
                (e.x, e.y, "top left"),
                (e.x + e.w / 2, e.y + e.h / 2, "centre"),
                (e.x + e.w - 1, e.y + e.h - 1, "bottom right"),
            ] {
                assert_eq!(l.hit(x, y), Some(i as usize), "{what} of element {i}");
            }
        }
        // The window margin, the gap between elements, and the empty cells of
        // the last row all belong to no tile.
        assert_eq!(l.hit(0, 0), None, "margin");
        let first = l.elem(0);
        assert_eq!(
            l.hit(first.x + first.w + 1, first.y),
            None,
            "gap between columns"
        );
        assert_eq!(
            l.hit(first.x, first.y + first.h + 1),
            None,
            "gap between rows"
        );
        let empty = l.elem(8); // row 2, column 2: past the seventh tile
        assert_eq!(l.hit(empty.x + 4, empty.y + 4), None, "empty cell");
        assert_eq!(l.hit(-5, -5), None, "outside");
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
