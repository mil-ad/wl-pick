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
    /// The box the grid may not exceed, in logical px. Thumbnails are sized to
    /// divide it by the column and row caps below, so a thumbnail is the same
    /// size whether one window is open or thirty — only the window around them
    /// shrinks to hug what is there.
    pub max_w: i32,
    pub max_h: i32,
    /// Padding inside one element, i.e. around its thumbnail (rasi `element`).
    pub pad: i32,
    /// Space between elements (rasi `listview { spacing }`).
    pub gap: i32,
    /// Margin between the grid and the window edge.
    pub margin: i32,
    /// How many tiles the grid may show at once. Rows beyond `max_rows` scroll.
    pub max_cols: i32,
    pub max_rows: i32,
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
            // Placeholders: the command line resolves these against the
            // display the grid will appear on.
            max_w: 1152,
            max_h: 1296,
            pad: 12,
            gap: 15,
            margin: 12,
            // Equal caps make a cell shaped like the display, since max_w and
            // max_h are the same fraction of it.
            max_cols: 4,
            max_rows: 4,
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

impl Layout {
    /// A balanced grid: ceil(sqrt(n)) columns, capped, so the last row isn't
    /// ragged (6 windows -> 3x2, not 4x2 with two holes). Same rule rofigrid uses.
    ///
    /// A thumbnail is the size that divides the configured box by the column and
    /// row caps, so it does not change with how many windows are open: one
    /// window gets a normal thumbnail in a small overlay, thirty get the same
    /// thumbnail and scroll. The overlay then hugs whatever is actually there.
    pub fn new(t: &Theme, n: i32, display: (i32, i32)) -> Self {
        let n = n.max(0);
        let (cap_cols, cap_rows) = (t.max_cols.max(1), t.max_rows.max(1));
        // The box may never exceed the display, whatever the config says.
        let box_w = t.max_w.clamp(1, display.0.max(1));
        let box_h = t.max_h.clamp(1, display.1.max(1));
        let label_row = if t.labels { t.spacing + t.line_h } else { 0 };

        // Divide the box by the caps: what is left after the furniture is one
        // thumbnail.
        let per_col = 2 * t.pad + t.gap;
        let per_row = 2 * t.pad + label_row + t.gap;
        let tile_w = ((box_w - 2 * t.margin + t.gap) / cap_cols - per_col).max(1);
        let tile_h = ((box_h - 2 * t.margin + t.gap) / cap_rows - per_row).max(1);
        let (elem_w, elem_h) = (tile_w + 2 * t.pad, tile_h + label_row + 2 * t.pad);

        // Columns: the balanced rule, so a handful of windows makes a tidy grid
        // rather than one long row, capped by the config.
        let mut cols = (n as f64).sqrt() as i32;
        if cols * cols < n {
            cols += 1;
        }
        cols = cols.clamp(1, cap_cols);
        let rows = (n + cols - 1) / cols;
        let visible_rows = cap_rows.clamp(1, rows.max(1));

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

    /// A display large enough that the caps, not the screen, decide everything.
    const ROOMY: (i32, i32) = (10_000, 10_000);

    /// The caps and the box are what the config sets; a test theme states them
    /// outright rather than relying on placeholders.
    fn theme(max_w: i32, max_h: i32, cols: i32, rows: i32) -> Theme {
        Theme {
            max_w,
            max_h,
            max_cols: cols,
            max_rows: rows,
            ..Theme::default()
        }
    }

    #[test]
    fn a_thumbnail_is_the_box_divided_by_the_caps() {
        let t = theme(1000, 900, 4, 3);
        let l = Layout::new(&t, 12, ROOMY);
        let tile = l.tile(0, 0).expect("visible");
        // Four columns of (tile + padding) plus three gaps plus two margins fill
        // the box, give or take integer division.
        let used = 4 * (tile.w + 2 * t.pad) + 3 * t.gap + 2 * t.margin;
        assert!((1000 - used).abs() <= 4, "width {used} should fill 1000");
        let label_row = t.spacing + t.line_h;
        let used = 3 * (tile.h + label_row + 2 * t.pad) + 2 * t.gap + 2 * t.margin;
        assert!((900 - used).abs() <= 4, "height {used} should fill 900");
    }

    #[test]
    fn one_window_gets_the_same_thumbnail_as_thirty() {
        let t = theme(1000, 900, 4, 3);
        let one = Layout::new(&t, 1, ROOMY);
        let many = Layout::new(&t, 30, ROOMY);
        assert_eq!(
            one.tile(0, 0).expect("visible").w,
            many.tile(0, 0).expect("visible").w,
            "thumbnail size must not depend on how many windows are open"
        );
        // The overlay hugs what is there: one tile is a small window.
        assert_eq!((one.cols, one.rows), (1, 1));
        assert!(
            one.width < many.width && one.height < many.height,
            "{one:?}"
        );
        assert!(!one.scrollable() && many.scrollable());
    }

    #[test]
    fn grids_stay_balanced_and_within_the_caps() {
        let t = theme(1000, 900, 4, 3);
        // (n, cols, rows): ceil(sqrt(n)) columns, capped at four.
        for (n, cols, rows) in [
            (1, 1, 1),
            (2, 2, 1),
            (4, 2, 2),
            (6, 3, 2),
            (12, 4, 3),
            (30, 4, 8),
        ] {
            let l = Layout::new(&t, n, ROOMY);
            assert_eq!((l.cols, l.rows), (cols, rows), "n = {n}");
            assert!(l.visible_rows <= t.max_rows, "n = {n}");
        }
    }

    #[test]
    fn elements_stay_inside_the_window() {
        let t = theme(1000, 900, 4, 3);
        for n in 1..=12 {
            let l = Layout::new(&t, n, ROOMY);
            for i in 0..n {
                let e = l.elem(i, 0).expect("visible");
                assert!(e.x >= 0 && e.x + e.w <= l.width, "n = {n}, i = {i}");
                assert!(e.y >= 0 && e.y + e.h <= l.height, "n = {n}, i = {i}");
            }
        }
    }

    #[test]
    fn labels_take_their_room_from_the_thumbnail() {
        let mut t = theme(1000, 900, 4, 3);
        let with = Layout::new(&t, 12, ROOMY);
        t.labels = false;
        let without = Layout::new(&t, 12, ROOMY);
        // The box is fixed, so dropping labels makes thumbnails taller rather
        // than the window shorter.
        assert!(
            without.tile(0, 0).expect("visible").h > with.tile(0, 0).expect("visible").h,
            "thumbnails should grow into the freed row"
        );
        assert!(with.label(0, 0).is_some() && without.label(0, 0).is_none());

        let t = theme(1000, 900, 4, 3);
        let l = Layout::new(&t, 4, ROOMY);
        for i in 0..4 {
            let (tile, label, elem) = (
                l.tile(i, 0).expect("visible"),
                l.label(i, 0).unwrap(),
                l.elem(i, 0).expect("visible"),
            );
            assert_eq!(label.y, tile.y + tile.h + t.spacing);
            assert_eq!(label.w, tile.w);
            assert!(label.y + label.h + t.pad <= elem.y + elem.h);
        }
    }

    #[test]
    fn the_box_never_exceeds_the_display() {
        // A config asking for more than the screen has, on a small screen.
        let t = theme(4000, 3000, 4, 3);
        let l = Layout::new(&t, 30, (640, 480));
        assert!(l.width <= 640 && l.height <= 480, "{l:?}");
        assert!(l.tile(0, 0).expect("visible").w >= 1);
        assert!(l.scrollable());
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
        let t = theme(1000, 900, 4, 3);
        // Thirty tiles need more rows than the cap allows, so they scroll.
        let l = Layout::new(&t, 30, ROOMY);
        assert!(l.scrollable(), "{l:?} should scroll");
        assert!(l.visible_rows < l.rows);
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
    fn max_rows_keeps_the_grid_compact() {
        let mut t = theme(1000, 900, 4, 3);
        let full = Layout::new(&t, 30, ROOMY);
        t.max_rows = 2;
        let capped = Layout::new(&t, 30, ROOMY);
        assert!(
            capped.visible_rows == 2 && full.visible_rows > 2,
            "{capped:?}"
        );
        assert!(capped.height < full.height, "a shorter overlay");
        assert!(capped.scrollable());
        // The cap cannot invent rows: four tiles make a 2x2 grid, and a cap of
        // five leaves it alone.
        t.max_rows = 5;
        let few = Layout::new(&t, 4, ROOMY);
        assert_eq!((few.cols, few.rows, few.visible_rows), (2, 2, 2), "{few:?}");
        assert!(!few.scrollable());
    }

    #[test]
    fn revealing_moves_the_viewport_as_little_as_possible() {
        let t = theme(1000, 900, 4, 3);
        let l = Layout::new(&t, 30, ROOMY);
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
        let t = theme(1000, 900, 4, 3);
        let l = Layout::new(&t, 30, ROOMY);
        let first = l.elem(0, 0).expect("visible");
        let probe = (first.x + first.w / 2, first.y + first.h / 2);
        assert_eq!(l.hit(probe.0, probe.1, 0), Some(0));
        // The same pixel is a different tile once the grid has scrolled.
        assert_eq!(l.hit(probe.0, probe.1, 1), Some(l.cols as usize));
    }

    #[test]
    fn a_scrollbar_appears_only_when_there_is_more_to_see() {
        let t = theme(1000, 900, 4, 3);
        assert!(Layout::new(&t, 4, ROOMY).scrollbar(0, 4).is_none());
        let l = Layout::new(&t, 30, ROOMY);
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
