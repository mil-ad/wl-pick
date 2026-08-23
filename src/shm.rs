//! Shared memory: an anonymous memfd handed to wl_shm, plus a tiny ARGB
//! painter for the parts we draw ourselves (background, border, selection).
//!
//! Capture buffers deliberately never get mapped into this process. The
//! compositor writes the window pixels and then samples them again for display,
//! so we only need the fd — mapping them would fault ~7 MB per window into our
//! address space for nothing.

use std::fs::File;
use std::io;

use memmap2::MmapMut;
use rustix::fs::{MemfdFlags, memfd_create};

use crate::theme::{Argb, Rect};

/// An anonymous in-memory file of the given size, for wl_shm.create_pool.
pub fn memfd(name: &str, len: usize) -> io::Result<File> {
    let fd = memfd_create(name, MemfdFlags::CLOEXEC)?;
    let file = File::from(fd);
    file.set_len(len as u64)?;
    Ok(file)
}

/// A mapped memfd we paint into. Holds two slots so we can draw the next frame
/// without touching the one the compositor is currently reading.
pub struct Chrome {
    map: MmapMut,
    pub w: i32,
    pub h: i32,
    slot: usize,
}

impl Chrome {
    pub const SLOTS: usize = 2;

    pub fn new(file: &File, w: i32, h: i32) -> io::Result<Self> {
        let map = unsafe { MmapMut::map_mut(file)? };
        Ok(Self { map, w, h, slot: 0 })
    }

    pub fn stride(w: i32) -> i32 {
        w * 4
    }

    pub fn slot_len(w: i32, h: i32) -> usize {
        (Self::stride(w) * h) as usize
    }

    /// Flip to the other slot and return its byte offset in the pool, so the
    /// caller can attach the matching wl_buffer.
    pub fn next_slot(&mut self) -> usize {
        self.slot = (self.slot + 1) % Self::SLOTS;
        self.slot
    }

    pub fn painter(&mut self) -> Painter<'_> {
        let (w, h) = (self.w, self.h);
        let len = Self::slot_len(w, h);
        let off = self.slot * len;
        Painter {
            px: &mut self.map[off..off + len],
            w,
            h,
        }
    }
}

/// Flat ARGB8888 painter. Everything in this UI is axis-aligned solid fills, so
/// there is no need for a rasteriser.
pub struct Painter<'a> {
    px: &'a mut [u8],
    w: i32,
    h: i32,
}

impl<'a> Painter<'a> {
    /// Wrap a raw ARGB8888 span, so the text path can be exercised without a
    /// compositor. `Chrome::painter` is the normal way in.
    #[cfg(test)]
    pub fn new(px: &'a mut [u8], w: i32, h: i32) -> Self {
        Self { px, w, h }
    }

    pub fn fill(&mut self, c: Argb) {
        for p in self.px.chunks_exact_mut(4) {
            p.copy_from_slice(&c.to_le_bytes());
        }
    }

    pub fn rect(&mut self, r: Rect, c: Argb) {
        let bytes = c.to_le_bytes();
        let (x0, y0) = (r.x.max(0), r.y.max(0));
        let (x1, y1) = ((r.x + r.w).min(self.w), (r.y + r.h).min(self.h));
        for y in y0..y1 {
            let row = (y * self.w * 4) as usize;
            for x in x0..x1 {
                let o = row + (x * 4) as usize;
                self.px[o..o + 4].copy_from_slice(&bytes);
            }
        }
    }

    /// Blend a solid span at `a/255` coverage, clipped to `clip`. Glyph spans
    /// arrive this way: colour plus a coverage alpha.
    pub fn blend(&mut self, r: Rect, (sr, sg, sb, sa): (u8, u8, u8, u8), clip: Rect) {
        if sa == 0 {
            return;
        }
        let a = sa as u32;
        let x0 = r.x.max(clip.x).max(0);
        let y0 = r.y.max(clip.y).max(0);
        let x1 = (r.x + r.w).min(clip.x + clip.w).min(self.w);
        let y1 = (r.y + r.h).min(clip.y + clip.h).min(self.h);
        for y in y0..y1 {
            let row = (y * self.w * 4) as usize;
            for x in x0..x1 {
                let o = row + (x * 4) as usize;
                // Argb8888 little-endian: B, G, R, A.
                for (i, src) in [(0usize, sb), (1, sg), (2, sr)] {
                    let dst = self.px[o + i] as u32;
                    self.px[o + i] = ((src as u32 * a + dst * (255 - a)) / 255) as u8;
                }
                self.px[o + 3] = 255;
            }
        }
    }

    /// A `width`-thick frame just inside the surface edge.
    pub fn frame(&mut self, width: i32, c: Argb) {
        let (w, h) = (self.w, self.h);
        self.rect(
            Rect {
                x: 0,
                y: 0,
                w,
                h: width,
            },
            c,
        );
        self.rect(
            Rect {
                x: 0,
                y: h - width,
                w,
                h: width,
            },
            c,
        );
        self.rect(
            Rect {
                x: 0,
                y: 0,
                w: width,
                h,
            },
            c,
        );
        self.rect(
            Rect {
                x: w - width,
                y: 0,
                w: width,
                h,
            },
            c,
        );
    }
}
