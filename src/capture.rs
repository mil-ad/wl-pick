//! Capturing windows and displays.
//!
//! One session per tile, all opened before a single roundtrip so their buffer
//! constraints arrive together, and every first frame put in flight at once: the
//! compositor is bandwidth-bound reading pixels back, so serialising the
//! captures only adds latency.
//!
//! The pixels are never mapped into this process. A capture buffer goes straight
//! to a subsurface for display, so the compositor writes those pages and samples
//! them again itself. Nothing here interprets them, which leaves one thing that
//! must still be right: the stride declared with each buffer, since how wide a
//! row is depends on the format the session picked.

use std::error::Error;
use std::os::fd::AsFd;
use std::time::{Duration, Instant};

use wayland_client::protocol::{
    wl_buffer::{self, WlBuffer},
    wl_callback, wl_output, wl_shm,
    wl_subsurface::WlSubsurface,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols::ext::image_capture_source::v1::client::ext_image_capture_source_v1::ExtImageCaptureSourceV1;
use wayland_protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1},
    ext_image_copy_capture_manager_v1,
    ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
};
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;

use crate::app::App;
use crate::shm;
use crate::target::{Kind, Target};

/// Which tiles keep updating after the first frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Live {
    /// Every tile.
    All,
    /// Only the selected tile: much cheaper, and still reads as alive.
    Current,
    /// Nothing: one snapshot each, a picker rather than an expose.
    None,
}

impl Live {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim() {
            "all" => Ok(Live::All),
            "current" => Ok(Live::Current),
            "none" => Ok(Live::None),
            other => Err(format!("{other:?} is not all, current or none")),
        }
    }
}

/// One capture buffer. `busy` means the compositor still holds it — either it is
/// on screen or a capture is writing into it — so we must not scribble over it.
pub struct Slot {
    pub(crate) buffer: WlBuffer,
    pub(crate) busy: bool,
}

/// Pick a buffer format from what the capture session `offered`, and say how
/// many bytes one of its pixels takes.
///
/// Byte order is the compositor's business on both ends -- we never read these
/// pixels -- but the size of a pixel is ours, because the stride declared with
/// the buffer has to match the format. XRGB8888 and ARGB8888 are the two every
/// wl_shm supports, so they come first; otherwise take the first offer we can
/// both measure and hand back for display, since what a session can capture
/// into is not always what wl_shm can show.
fn choose_format(
    offered: &[wl_shm::Format],
    supported: &[wl_shm::Format],
) -> Option<(wl_shm::Format, i32)> {
    let sized = |format: &wl_shm::Format| Some((*format, bytes_per_pixel(*format)?));
    offered
        .iter()
        .filter(|f| matches!(f, wl_shm::Format::Xrgb8888 | wl_shm::Format::Argb8888))
        .find_map(sized)
        .or_else(|| {
            offered
                .iter()
                .filter(|f| supported.contains(f))
                .find_map(sized)
        })
}

/// How many bytes one pixel takes, for the formats a row of which is simply the
/// width times that.
///
/// These are the ones wlroots can hand us (its `pixel_formats` table), and a
/// capture session offers whatever it can capture into -- not only the two
/// wl_shm guarantees, and not all four bytes wide. Declaring a four-byte stride
/// for an eight-byte format is a protocol error that takes the connection with
/// it, so a format missing from here is one to decline rather than guess at.
///
/// Subsampled formats are absent deliberately. YUYV and friends do have a
/// constant block size, but a block covers two pixels, so their rows are
/// narrower than width times it and the arithmetic here would not hold.
fn bytes_per_pixel(format: wl_shm::Format) -> Option<i32> {
    use wl_shm::Format;
    Some(match format {
        Format::Rgb565 | Format::Bgr565 => 2,
        Format::Xrgb1555 | Format::Argb1555 => 2,
        Format::Rgbx4444 | Format::Rgba4444 | Format::Bgrx4444 | Format::Bgra4444 => 2,
        Format::Rgbx5551 | Format::Rgba5551 | Format::Bgrx5551 | Format::Bgra5551 => 2,
        Format::Rgb888 | Format::Bgr888 => 3,
        Format::Xrgb8888 | Format::Argb8888 | Format::Xbgr8888 | Format::Abgr8888 => 4,
        Format::Rgbx8888 | Format::Rgba8888 | Format::Bgrx8888 | Format::Bgra8888 => 4,
        Format::Xrgb2101010 | Format::Argb2101010 => 4,
        Format::Xbgr2101010 | Format::Abgr2101010 => 4,
        Format::Xbgr16161616 | Format::Abgr16161616 => 8,
        Format::Xbgr16161616f | Format::Abgr16161616f => 8,
        _ => return None,
    })
}

pub struct Tile {
    pub(crate) target: Target,

    pub(crate) session: Option<ExtImageCopyCaptureSessionV1>,
    /// A capture in flight, and which slot it is filling.
    pub(crate) frame: Option<ExtImageCopyCaptureFrameV1>,
    pub(crate) filling: Option<usize>,
    pub(crate) slots: Vec<Slot>,
    /// The slot currently attached to the subsurface.
    pub(crate) showing: Option<usize>,
    pub(crate) formats: Vec<wl_shm::Format>,
    pub(crate) format: Option<wl_shm::Format>,
    /// Bytes per row of the capture buffer, which depends on the format: a
    /// stride that does not match is a protocol error, not a wrong picture.
    pub(crate) stride: i32,
    /// Buffer size the session requires: the window's full resolution.
    pub(crate) size: (u32, u32),
    pub(crate) transform: wl_output::Transform,
    pub(crate) session_done: bool,
    pub(crate) ready: bool,
    pub(crate) settled: bool,
    /// When the last capture was asked for, for rate limiting, and how many
    /// frames this tile has produced.
    pub(crate) asked: Option<Instant>,
    pub(crate) frames: u32,

    pub(crate) surface: Option<WlSurface>,
    pub(crate) subsurface: Option<WlSubsurface>,
    pub(crate) viewport: Option<WpViewport>,
}

impl Tile {
    pub fn new(target: Target) -> Self {
        Self {
            target,
            session: None,
            frame: None,
            filling: None,
            slots: Vec::new(),
            showing: None,
            formats: Vec::new(),
            format: None,
            stride: 0,
            size: (0, 0),
            transform: wl_output::Transform::Normal,
            session_done: false,
            ready: false,
            settled: false,
            asked: None,
            frames: 0,
            surface: None,
            subsurface: None,
            viewport: None,
        }
    }

    pub fn bytes(&self) -> usize {
        self.stride as usize * self.size.1 as usize
    }

    /// Whether the buffer's contents are turned on their side relative to the
    /// window, which flips the aspect ratio we have to fit.
    pub fn rotated(&self) -> bool {
        use wl_output::Transform;
        matches!(
            self.transform,
            Transform::_90 | Transform::_270 | Transform::Flipped90 | Transform::Flipped270
        )
    }
}

impl App {
    /// Open one capture session per window whose toplevel we recognise. They are
    /// all opened before a single roundtrip, so every session's buffer
    /// constraints arrive together instead of costing a round trip each.
    pub fn open_sessions(&mut self, qh: &QueueHandle<Self>) {
        for (i, tile) in self.tiles.iter_mut().enumerate() {
            // A window's source comes from its toplevel handle, a display's from
            // its wl_output; everything after that is identical.
            let source: Option<ExtImageCaptureSourceV1> = match tile.target.kind {
                Kind::Window => self
                    .toplevels
                    .iter()
                    .find(|(_, id)| !id.is_empty() && *id == tile.target.ft_id)
                    .map(|(handle, _)| self.src_mgr.create_source(handle, qh, ())),
                Kind::Output => self
                    .outputs
                    .iter()
                    .find(|(_, n)| *n == tile.target.id)
                    .and_then(|(output, _)| {
                        self.output_src_mgr
                            .as_ref()
                            .map(|mgr| mgr.create_source(output, qh, ()))
                    }),
            };
            let Some(source) = source else {
                // Nothing to capture from: the tile stays label-only, and must
                // not be waited on.
                tile.settled = true;
                continue;
            };
            tile.session = Some(self.copy_mgr.create_session(
                &source,
                ext_image_copy_capture_manager_v1::Options::empty(),
                qh,
                i,
            ));
            source.destroy();
        }
    }

    /// Allocate the capture buffers in one pool and put every first frame in
    /// flight at once: the compositor is bandwidth-bound reading pixels back, so
    /// serialising the captures only adds latency.
    ///
    /// Live mode gets two buffers per window. A capture may not write into the
    /// buffer the compositor is currently displaying, so the two alternate:
    /// fill B while A is on screen, swap, and wait for A's release before
    /// touching it again.
    pub fn start_captures(&mut self, qh: &QueueHandle<Self>) -> Result<(), Box<dyn Error>> {
        const PAGE: usize = 4096;
        // Cloned because the loop below borrows the tiles mutably; the list is
        // short and fixed once wl_shm has announced it.
        let shm_formats = self.shm_formats.clone();
        let mut total = 0usize;
        let mut offsets: Vec<Vec<usize>> = Vec::with_capacity(self.tiles.len());
        for tile in &mut self.tiles {
            offsets.push(Vec::new());
            if tile.session.is_none() {
                continue;
            }
            if !tile.session_done || tile.size.0 == 0 || tile.size.1 == 0 {
                tile.settled = true;
                continue;
            }
            let Some((format, bytes)) = choose_format(&tile.formats, &shm_formats) else {
                eprintln!(
                    "wl-pick: no usable buffer format for {:?} (offered {:?})",
                    tile.target.title, tile.formats
                );
                tile.settled = true;
                continue;
            };
            tile.format = Some(format);
            tile.stride = tile.size.0 as i32 * bytes;
            // Only a tile that will be re-captured needs a second buffer, and a
            // display's is the size of the whole screen.
            let slots = if self.live == Live::None || tile.target.kind == Kind::Output {
                1
            } else {
                2
            };
            let last = offsets.last_mut().expect("just pushed");
            for _ in 0..slots {
                last.push(total);
                total += tile.bytes().div_ceil(PAGE) * PAGE;
            }
        }
        if total == 0 {
            return Ok(());
        }

        self.stats.pool_bytes = total;
        // Note: no mmap. The compositor writes these pages and samples them
        // again for display; mapping them here would only cost us the faults.
        let file = shm::memfd("wl-pick-capture", total)?;
        let pool = self.shm.create_pool(file.as_fd(), total as i32, qh, ());
        for (i, slot_offsets) in offsets.iter().enumerate() {
            let (w, h, stride, format) = {
                let t = &self.tiles[i];
                if t.settled || t.session.is_none() || t.format.is_none() {
                    continue;
                }
                (
                    t.size.0 as i32,
                    t.size.1 as i32,
                    t.stride,
                    t.format.unwrap(),
                )
            };
            for &offset in slot_offsets {
                let slot = self.tiles[i].slots.len();
                let buffer = pool.create_buffer(offset as i32, w, h, stride, format, qh, (i, slot));
                self.tiles[i].slots.push(Slot {
                    buffer,
                    busy: false,
                });
            }
            self.request_capture(i, qh);
        }
        pool.destroy(); // the buffers keep the mapping alive
        Ok(())
    }

    /// Ask the compositor for one frame of window `i`, into a free slot.
    ///
    /// After a session's first frame the compositor only answers once the window
    /// content changes, so a request left outstanding on an idle window costs
    /// nothing: this is damage-driven, and the rate limit only bites on windows
    /// that really are animating.
    fn request_capture(&mut self, i: usize, qh: &QueueHandle<Self>) {
        let t = &mut self.tiles[i];
        if t.frame.is_some() || t.session.is_none() {
            return; // already waiting on one
        }
        let Some(slot) = t.slots.iter().position(|s| !s.busy) else {
            self.stats.starved += 1;
            return; // both buffers still held by the compositor
        };
        let (w, h) = (t.size.0 as i32, t.size.1 as i32);
        let frame = t
            .session
            .as_ref()
            .expect("checked above")
            .create_frame(qh, i);
        frame.attach_buffer(&t.slots[slot].buffer);
        frame.damage_buffer(0, 0, w, h);
        frame.capture();
        t.frame = Some(frame);
        t.filling = Some(slot);
        t.asked = Some(Instant::now());
    }

    /// A capture landed: show it, and let go of the slot it replaced. Says
    /// whether the tile is still waiting for a subsurface, which only the
    /// overlay can give it.
    fn frame_ready(&mut self, i: usize) -> bool {
        // A tile scrolled out of sight was unmapped with a null buffer and
        // still carries the position it had when it was last visible, so
        // attaching here would put a stale thumbnail over whatever occupies
        // that cell now. sync_tiles hands it back when it scrolls into view.
        let visible = self.layout.tile(i as i32, self.scroll).is_some();
        let t = &mut self.tiles[i];
        let Some(slot) = t.filling.take() else {
            return false;
        };
        t.frames += 1;
        t.ready = true;
        t.settled = true;
        t.slots[slot].busy = true; // the compositor reads it until it releases it
        let previous = t.showing.replace(slot);
        match t.surface.clone() {
            Some(surface) if visible => {
                let (w, h) = (t.size.0 as i32, t.size.1 as i32);
                surface.attach(Some(&t.slots[slot].buffer), 0, 0);
                surface.damage_buffer(0, 0, w, h);
                surface.commit();
            }
            // Nothing was attached, so the old slot was never actually read.
            _ => {
                if let Some(prev) = previous {
                    t.slots[prev].busy = false;
                }
            }
        }
        // Before the overlay is mapped there is nothing to attach to yet, and
        // sync_tiles picks up `showing` instead. A first frame landing after
        // that has to ask for a subsurface, or it is captured and never seen.
        t.surface.is_none()
    }

    /// Ask for the next frame callback. A commit is needed for the compositor to
    /// schedule one, and an empty commit is enough.
    pub fn arm_frame_callback(&mut self, qh: &QueueHandle<Self>) {
        if self.live == Live::None {
            return;
        }
        if let Some(surface) = self.surface.clone() {
            surface.frame(qh, ());
            surface.commit();
        }
    }

    /// Re-capture whatever is due. Driven by frame callbacks, so it stops when
    /// the overlay is not being presented.
    pub fn tick(&mut self, qh: &QueueHandle<Self>) {
        self.stats.ticks += 1;
        if self.live == Live::None {
            return;
        }
        let interval = Duration::from_secs_f64(1.0 / self.fps.max(1) as f64);
        let now = Instant::now();
        for i in 0..self.tiles.len() {
            if self.live == Live::Current && i != self.sel {
                continue;
            }
            // A display tile shows this overlay, which shows the display tile:
            // refreshing it never settles and costs a whole screen per frame.
            if self.tiles[i].target.kind == Kind::Output {
                continue;
            }
            // Nor is there any point refreshing a tile that is scrolled out of
            // sight — that is a readback for pixels nobody sees.
            if self.layout.tile(i as i32, self.scroll).is_none() {
                continue;
            }
            let t = &self.tiles[i];
            if t.slots.is_empty() || t.frame.is_some() {
                continue;
            }
            if t.asked.is_some_and(|a| now.duration_since(a) < interval) {
                continue;
            }
            self.request_capture(i, qh);
        }
    }
}

// --- event plumbing -------------------------------------------------------

impl Dispatch<ExtImageCopyCaptureSessionV1, usize> for App {
    fn event(
        app: &mut Self,
        _: &ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        &i: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(tile) = app.tiles.get_mut(i) else {
            return;
        };
        match event {
            ext_image_copy_capture_session_v1::Event::BufferSize { width, height } => {
                tile.size = (width, height)
            }
            ext_image_copy_capture_session_v1::Event::ShmFormat {
                format: WEnum::Value(f),
            } => tile.formats.push(f),
            ext_image_copy_capture_session_v1::Event::Done => tile.session_done = true,
            ext_image_copy_capture_session_v1::Event::Stopped => tile.settled = true,
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, usize> for App {
    fn event(
        app: &mut Self,
        _: &ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        &i: &usize,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let Some(tile) = app.tiles.get_mut(i) else {
            return;
        };
        match event {
            ext_image_copy_capture_frame_v1::Event::Transform {
                transform: WEnum::Value(t),
            } => tile.transform = t,
            ext_image_copy_capture_frame_v1::Event::Ready => {
                // The protocol wants the frame destroyed once ready; the buffer
                // stays ours to display.
                if let Some(frame) = tile.frame.take() {
                    frame.destroy();
                }
                // A tile whose first frame arrives after the overlay mapped has
                // no subsurface yet, and nothing else would ever give it one.
                if app.frame_ready(i) {
                    app.sync_tiles(qh);
                }
            }
            ext_image_copy_capture_frame_v1::Event::Failed { reason } => {
                // Live mode retries on the next tick; only a failure with no
                // frame yet leaves the tile without a thumbnail.
                if tile.frames == 0 {
                    eprintln!(
                        "wl-pick: capture failed for {:?} ({reason:?})",
                        tile.target.title
                    );
                }
                tile.settled = true;
                if let Some(slot) = tile.filling.take() {
                    tile.slots[slot].busy = false;
                }
                if let Some(frame) = tile.frame.take() {
                    frame.destroy();
                }
            }
            _ => {}
        }
    }
}

/// A released capture buffer is a slot we may capture into again.
///
/// Release is the whole contract: with wl_shm the compositor copies the pixels
/// out at commit and hands the buffer straight back, so the slot currently on
/// screen is usually free too. (Waiting for it to stop being the displayed slot
/// instead would deadlock — that release never comes twice.)
impl Dispatch<WlBuffer, (usize, usize)> for App {
    fn event(
        app: &mut Self,
        _: &WlBuffer,
        event: wl_buffer::Event,
        &(tile, slot): &(usize, usize),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event
            && let Some(t) = app.tiles.get_mut(tile)
        {
            t.slots[slot].busy = false;
        }
    }
}

/// Frame callbacks are the clock for live updates: they arrive as the compositor
/// presents the overlay, so re-captures stop when it is not being shown.
impl Dispatch<wl_callback::WlCallback, ()> for App {
    fn event(
        app: &mut Self,
        _: &wl_callback::WlCallback,
        event: wl_callback::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            app.tick(qh);
            app.arm_frame_callback(qh);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wl_shm::Format;

    /// Everything wl_shm here advertises, plus the two it must.
    fn supported() -> Vec<Format> {
        vec![
            Format::Argb8888,
            Format::Xrgb8888,
            Format::Xbgr8888,
            Format::Bgr888,
            Format::Xrgb2101010,
            Format::Abgr16161616f,
            Format::Xbgr16161616f,
        ]
    }

    #[test]
    fn the_guaranteed_formats_win() {
        // Both are four bytes, and every wl_shm has them, so they are picked
        // over anything else on offer however the session orders them.
        let offered = [Format::Abgr16161616f, Format::Bgr888, Format::Xrgb8888];
        assert_eq!(
            choose_format(&offered, &supported()),
            Some((Format::Xrgb8888, 4))
        );
    }

    /// wlroots' own rule, from pixel_format_info_check_stride: a stride must
    /// be a whole number of pixels and cover the width.
    fn wlroots_accepts(stride: i32, width: i32, bytes: i32) -> bool {
        stride % bytes == 0 && stride >= width * bytes
    }

    #[test]
    fn the_declared_stride_matches_the_format() {
        // The reported bug: a session offering neither XRGB8888 nor ARGB8888
        // fell through to whatever came first, with a stride of four bytes a
        // pixel regardless. A 3830-wide window then declared 15320, which is
        // too small for an eight-byte format and not a whole number of
        // three-byte ones -- "Invalid stride (15320)", and the connection dies.
        let width = 3830;
        for (format, bytes) in [
            (Format::Bgr888, 3),
            (Format::Xbgr8888, 4),
            (Format::Abgr16161616f, 8),
        ] {
            let (chosen, chosen_bytes) =
                choose_format(&[format], &supported()).unwrap_or_else(|| panic!("{format:?}"));
            assert_eq!((chosen, chosen_bytes), (format, bytes));
            assert!(
                wlroots_accepts(width * chosen_bytes, width, bytes),
                "{format:?} stride {} rejected",
                width * chosen_bytes
            );
        }

        // And the old formula is what fails, for the formats that are not four
        // bytes wide.
        assert!(!wlroots_accepts(width * 4, width, 3), "15320 for BGR888");
        assert!(!wlroots_accepts(width * 4, width, 8), "15320 for ABGR16F");
        assert!(wlroots_accepts(width * 4, width, 4), "four bytes was fine");
    }

    #[test]
    fn formats_we_cannot_measure_are_declined() {
        // Multi-planar and subsampled rows are not width times a constant, and
        // a format wl_shm never advertised cannot be shown even if the session
        // can capture into it. Refusing costs one tile its thumbnail; guessing
        // costs the whole connection.
        assert_eq!(choose_format(&[Format::Nv12], &supported()), None);
        assert_eq!(choose_format(&[Format::Yuv420], &supported()), None);
        assert_eq!(choose_format(&[], &supported()), None);
        assert_eq!(
            choose_format(&[Format::Abgr16161616f], &[Format::Xrgb8888]),
            None,
            "offered but not displayable"
        );
    }

    #[test]
    fn pixel_sizes_match_the_names() {
        assert_eq!(bytes_per_pixel(Format::Rgb565), Some(2));
        assert_eq!(bytes_per_pixel(Format::Bgr888), Some(3));
        assert_eq!(bytes_per_pixel(Format::Xrgb2101010), Some(4));
        assert_eq!(bytes_per_pixel(Format::Xbgr16161616f), Some(8));
        // Subsampled: wlroots gives these a four-byte block too, but a block
        // is two pixels wide, so a row is not the width times it.
        assert_eq!(bytes_per_pixel(Format::Yuyv), None);
        // Not something a wlroots renderer offers, so not something to guess.
        assert_eq!(bytes_per_pixel(Format::Rgb332), None);
    }
}
