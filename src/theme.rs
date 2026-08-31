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
    /// Rows the whole grid needs, and how many of them fit on screen at once.
    pub rows: i32,
    pub visible_rows: i32,
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

/// How much of the display the grid may occupy.
const FILL: i32 = 90;

impl Layout {
    /// A balanced grid: ceil(sqrt(n)) columns, capped, so the last row isn't
    /// ragged (6 windows -> 3x2, not 4x2 with two holes). Same rule rofigrid uses.
    ///
    /// Tiles are the size the theme asks for — a configured size that quietly
    /// shrank would be a setting ignored — so when the grid needs more rows than
    /// the display can show, the extra rows scroll. Only a tile too large for
    /// even one row or column is shrunk, since then something has to give.
    pub fn new(t: &Theme, n: i32, display: (i32, i32)) -> Self {
        let (dw, dh) = (display.0.max(1), display.1.max(1));
        let room_w = (dw * FILL / 100 - 2 * t.margin).max(1);
        let room_h = (dh * FILL / 100 - 2 * t.margin).max(1);
        let label_row = if t.labels { t.spacing + t.line_h } else { 0 };
        let (tile_w, tile_h) = shrink_to_one(t, label_row, room_w, room_h);
        let (elem_w, elem_h) = (tile_w + 2 * t.pad, tile_h + label_row + 2 * t.pad);

        // Columns: the balanced rule, capped by the config and by what fits.
        let mut cols = (n as f64).sqrt() as i32;
        if cols * cols < n {
            cols += 1;
        }
        let fit_cols = ((room_w + t.gap) / (elem_w + t.gap)).max(1);
        cols = cols.clamp(1, t.max_cols.min(fit_cols)).max(1);
        let rows = (n + cols - 1) / cols;
        let visible_rows = ((room_h + t.gap) / (elem_h + t.gap)).clamp(1, rows.max(1));

        Self {
            cols,
            rows,
            visible_rows,
            n,
            width: cols * elem_w + (cols - 1) * t.gap + 2 * t.margin,
            height: visible_rows * elem_h + (visible_rows - 1) * t.gap + 2 * t.margin,
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

    /// The furthest the viewport can scroll, in rows.
    pub fn max_scroll(&self) -> i32 {
        (self.rows - self.visible_rows).max(0)
    }

    pub fn scrollable(&self) -> bool {
        self.max_scroll() > 0
    }

    pub fn row_of(&self, i: usize) -> i32 {
        i as i32 / self.cols
    }

    /// Where the viewport must sit for tile `i` to be on screen, moving as
    /// little as possible from `scroll`.
    pub fn reveal(&self, i: usize, scroll: i32) -> i32 {
        let row = self.row_of(i);
        let top = row.min(scroll);
        let bottom = (row - self.visible_rows + 1).max(top);
        bottom.clamp(0, self.max_scroll())
    }

    /// The element box for tile `i` with the viewport at `scroll`, or None when
    /// that tile is scrolled out of sight. This is what the selection fills.
    pub fn elem(&self, i: i32, scroll: i32) -> Option<Rect> {
        let (col, row) = (i % self.cols, i / self.cols);
        let visible = row - scroll;
        if i < 0 || i >= self.n || visible < 0 || visible >= self.visible_rows {
            return None;
        }
        Some(Rect {
            x: self.margin + col * (self.elem_w + self.gap),
            y: self.margin + visible * (self.elem_h + self.gap),
            w: self.elem_w,
            h: self.elem_h,
        })
    }

    /// The thumbnail box for tile `i`: the top of the element, above the label.
    pub fn tile(&self, i: i32, scroll: i32) -> Option<Rect> {
        self.elem(i, scroll).map(|e| Rect {
            x: e.x + self.pad,
            y: e.y + self.pad,
            w: e.w - 2 * self.pad,
            h: self.tile_h,
        })
    }

    /// The single line of text under the thumbnail, if labels are drawn.
    pub fn label(&self, i: i32, scroll: i32) -> Option<Rect> {
        if !self.labels {
            return None;
        }
        self.tile(i, scroll).map(|t| Rect {
            x: t.x,
            y: t.y + t.h + self.spacing,
            w: t.w,
            h: self.line_h,
        })
    }

    /// The tile at a point in surface-local coordinates, if any. Points in the
    /// gaps between elements and in the window margin belong to nothing, and so
    /// do the empty cells of a ragged last row.
    pub fn hit(&self, x: i32, y: i32, scroll: i32) -> Option<usize> {
        let col = self.axis(x, self.elem_w, self.cols)?;
        let row = self.axis(y, self.elem_h, self.visible_rows)? + scroll;
        let i = row * self.cols + col;
        (i >= 0 && i < self.n).then_some(i as usize)
    }

    /// Which cell along one axis a coordinate falls in, or None if it landed in
    /// the margin or a gap.
    fn axis(&self, v: i32, elem: i32, count: i32) -> Option<i32> {
        let pitch = elem + self.gap;
        let offset = v - self.margin;
        if offset < 0 {
            return None;
        }
        let cell = offset / pitch;
        (cell < count && offset % pitch < elem).then_some(cell)
    }

    /// Track and thumb for a scrollbar down the right margin, or None when
    /// everything already fits.
    pub fn scrollbar(&self, scroll: i32, width: i32) -> Option<(Rect, Rect)> {
        if !self.scrollable() {
            return None;
        }
        let track = Rect {
            x: self.width - self.margin + (self.margin - width) / 2,
            y: self.margin,
            w: width,
            h: self.height - 2 * self.margin,
        };
        let span = (track.h * self.visible_rows / self.rows).max(width);
        let travel = track.h - span;
        let thumb = Rect {
            y: track.y + travel * scroll / self.max_scroll(),
            h: span,
            ..track
        };
        Some((track, thumb))
    }
}

/// A tile larger than one row or column of the display has to give way, since
/// nothing can be shown otherwise. Both axes shrink together, keeping its shape.
fn shrink_to_one(t: &Theme, label_row: i32, room_w: i32, room_h: i32) -> (i32, i32) {
    let (want_w, want_h) = (t.tile_w.max(1), t.tile_h.max(1));
    let cell_w = want_w + 2 * t.pad;
    let cell_h = want_h + label_row + 2 * t.pad;
    let scale = (room_w as f32 / cell_w as f32)
        .min(room_h as f32 / cell_h as f32)
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
                let e = l.elem(i, 0).expect("visible");
                assert!(e.x >= 0 && e.x + e.w <= l.width, "n = {n}, i = {i}");
                assert!(e.y >= 0 && e.y + e.h <= l.height, "n = {n}, i = {i}");
                let tile = l.tile(i, 0).expect("visible");
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
        assert!(without.label(0, 0).is_none());

        let t = Theme::default();
        let l = Layout::new(&t, 4, ROOMY);
        for i in 0..4 {
            let (tile, label, elem) = (
                l.tile(i, 0).expect("visible"),
                l.label(i, 0).unwrap(),
                l.elem(i, 0).expect("visible"),
            );
            assert_eq!(tile.h, t.tile_h);
            assert_eq!(label.y, tile.y + tile.h + t.spacing);
            assert_eq!(label.w, tile.w);
            // Everything, padding included, stays inside the element.
            assert!(label.y + label.h + t.pad <= elem.y + elem.h);
        }
    }

    #[test]
    fn a_tile_too_big_for_one_cell_is_the_only_thing_that_shrinks() {
        let mut t = Theme::default();
        let roomy = Layout::new(&t, 12, ROOMY);
        assert_eq!(
            roomy.tile(0, 0).expect("visible").w,
            t.tile_w,
            "left alone when there is room"
        );

        // A tile wider and taller than the whole screen has to give way, since
        // otherwise there is nothing to show.
        t.tile_w = 2000;
        t.tile_h = 1500;
        let l = Layout::new(&t, 4, (800, 600));
        let tile = l.tile(0, 0).expect("visible");
        assert!(tile.w < t.tile_w && tile.h < t.tile_h, "should have shrunk");
        assert!(l.width <= 800 && l.height <= 600, "{l:?}");
        assert_eq!(l.cols, 1, "only one column can fit");
        // Shrinking keeps the tile's shape.
        let (want, got) = (
            t.tile_w as f32 / t.tile_h as f32,
            tile.w as f32 / tile.h as f32,
        );
        assert!(
            (want - got).abs() < 0.05,
            "aspect {got} drifted from {want}"
        );
    }

    #[test]
    fn a_tiny_display_never_gets_an_oversized_surface() {
        let t = Theme::default();
        // Thirty windows on a 640x480 screen: only a row or two can be shown,
        // and the rest scroll.
        let l = Layout::new(&t, 30, (640, 480));
        let tile = l.tile(0, 0).expect("visible");
        assert!(tile.w >= 1 && tile.h >= 1, "{l:?}");
        assert!(l.width <= 640 && l.height <= 480, "{l:?}");
    }

    #[test]
    fn hit_testing_is_the_inverse_of_the_layout() {
        let t = Theme::default();
        // 7 tiles over 3 columns: the last row holds one, so two cells are empty.
        let l = Layout::new(&t, 7, ROOMY);
        for i in 0..7 {
            let e = l.elem(i, 0).expect("visible");
            for (x, y, what) in [
                (e.x, e.y, "top left"),
                (e.x + e.w / 2, e.y + e.h / 2, "centre"),
                (e.x + e.w - 1, e.y + e.h - 1, "bottom right"),
            ] {
                assert_eq!(l.hit(x, y, 0), Some(i as usize), "{what} of element {i}");
            }
        }
        // The window margin, the gap between elements, and the empty cells of
        // the last row all belong to no tile.
        assert_eq!(l.hit(0, 0, 0), None, "margin");
        let first = l.elem(0, 0).expect("visible");
        assert_eq!(
            l.hit(first.x + first.w + 1, first.y, 0),
            None,
            "gap between columns"
        );
        assert_eq!(
            l.hit(first.x, first.y + first.h + 1, 0),
            None,
            "gap between rows"
        );
        // Row 2, column 2 is past the seventh tile: take its column from the top
        // row and its row from the first column.
        let col2 = l.elem(2, 0).expect("visible");
        let row2 = l.elem(6, 0).expect("visible");
        assert_eq!(l.hit(col2.x + 4, row2.y + 4, 0), None, "empty cell");
        assert_eq!(l.hit(-5, -5, 0), None, "outside");
    }

    #[test]
    fn rows_beyond_the_display_scroll_instead_of_shrinking() {
        let t = Theme::default();
        // Thirty tiles cannot fit; the configured tile size must survive anyway.
        let l = Layout::new(&t, 30, (1280, 1440));
        assert_eq!(
            l.tile(0, 0).expect("visible").w,
            t.tile_w,
            "tiles kept their size"
        );
        assert!(l.scrollable(), "{l:?} should scroll");
        assert!(l.visible_rows < l.rows);
        assert!(l.height <= 1440 && l.width <= 1280, "{l:?}");

        // The viewport shows a window of rows, and nothing outside it.
        let per_screen = (l.visible_rows * l.cols) as usize;
        assert!(l.elem(0, 0).is_some());
        assert!(
            l.elem(per_screen as i32, 0).is_none(),
            "first row below the fold"
        );
        assert!(
            l.elem(per_screen as i32, 1).is_some(),
            "and visible once scrolled"
        );
    }

    #[test]
    fn revealing_moves_the_viewport_as_little_as_possible() {
        let t = Theme::default();
        let l = Layout::new(&t, 30, (1280, 1440));
        let last_visible = (l.visible_rows * l.cols - 1) as usize;
        assert_eq!(l.reveal(0, 0), 0, "already on screen");
        assert_eq!(l.reveal(last_visible, 0), 0, "still on screen");
        // One row further down scrolls by exactly one row.
        assert_eq!(l.reveal(last_visible + 1, 0), 1);
        // Jumping to the end goes as far as it can, and no further.
        assert_eq!(l.reveal(29, 0), l.max_scroll());
        // Coming back up scrolls the other way.
        assert_eq!(l.reveal(0, l.max_scroll()), 0);
    }

    #[test]
    fn hit_testing_follows_the_scroll() {
        let t = Theme::default();
        let l = Layout::new(&t, 30, (1280, 1440));
        let first = l.elem(0, 0).expect("visible");
        let probe = (first.x + first.w / 2, first.y + first.h / 2);
        assert_eq!(l.hit(probe.0, probe.1, 0), Some(0));
        // The same pixel is a different tile once the grid has scrolled.
        assert_eq!(l.hit(probe.0, probe.1, 1), Some(l.cols as usize));
    }

    #[test]
    fn a_scrollbar_appears_only_when_there_is_more_to_see() {
        let t = Theme::default();
        assert!(Layout::new(&t, 4, ROOMY).scrollbar(0, 4).is_none());
        let l = Layout::new(&t, 30, (1280, 1440));
        let (track, top) = l.scrollbar(0, 4).expect("scrollable");
        assert_eq!(top.y, track.y, "thumb starts at the top");
        assert!(top.h < track.h, "thumb is shorter than its track");
        let (_, bottom) = l.scrollbar(l.max_scroll(), 4).expect("scrollable");
        assert_eq!(
            bottom.y + bottom.h,
            track.y + track.h,
            "and ends at the bottom"
        );
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
