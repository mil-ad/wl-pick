//! Capturing windows and displays.
//!
//! One session per tile, all opened before a single roundtrip so their buffer
//! constraints arrive together, and every first frame put in flight at once: the
//! compositor is bandwidth-bound reading pixels back, so serialising the
//! captures only adds latency.
//!
//! The pixels are never mapped into this process. A capture buffer goes straight
//! to a subsurface for display, so the compositor writes those pages and samples
//! them again itself.

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
        self.size.0 as usize * 4 * self.size.1 as usize
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
            // Any 32-bit format will do: we never read these pixels, we hand the
            // buffer straight back for display, so byte order stays the
            // compositor's business on both ends.
            tile.format = tile
                .formats
                .iter()
                .copied()
                .find(|f| matches!(f, wl_shm::Format::Xrgb8888 | wl_shm::Format::Argb8888))
                .or_else(|| tile.formats.first().copied());
            if tile.format.is_none() {
                tile.settled = true;
                continue;
            }
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
            let (w, h, format) = {
                let t = &self.tiles[i];
                if t.settled || t.session.is_none() || t.format.is_none() {
                    continue;
                }
                (t.size.0 as i32, t.size.1 as i32, t.format.unwrap())
            };
            for &offset in slot_offsets {
                let slot = self.tiles[i].slots.len();
                let buffer = pool.create_buffer(offset as i32, w, h, w * 4, format, qh, (i, slot));
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
    fn request_capture(&mut self, i: usize, qh: &QueueHandle<Self>) -> bool {
        let t = &mut self.tiles[i];
        if t.frame.is_some() || t.session.is_none() {
            return false; // already waiting on one
        }
        let Some(slot) = t.slots.iter().position(|s| !s.busy) else {
            self.stats.starved += 1;
            return false; // both buffers still held by the compositor
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
        true
    }

    /// A capture landed: show it, and let go of the slot it replaced.
    fn frame_ready(&mut self, i: usize) {
        let t = &mut self.tiles[i];
        let Some(slot) = t.filling.take() else { return };
        t.frames += 1;
        t.ready = true;
        t.settled = true;
        t.slots[slot].busy = true; // the compositor reads it until it releases it
        let previous = t.showing.replace(slot);
        // Before the overlay is mapped there is nothing to attach to yet;
        // place_tiles picks up `showing` instead.
        if let Some(surface) = t.surface.clone() {
            let (w, h) = (t.size.0 as i32, t.size.1 as i32);
            surface.attach(Some(&t.slots[slot].buffer), 0, 0);
            surface.damage_buffer(0, 0, w, h);
            surface.commit();
        } else if let Some(prev) = previous {
            // Not on screen yet, so the old slot was never actually read.
            t.slots[prev].busy = false;
        }
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
        _: &QueueHandle<Self>,
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
                app.frame_ready(i);
            }
            ext_image_copy_capture_frame_v1::Event::Failed { reason } => {
                // Live mode just retries on the next tick; only a failure with no
                // frame yet leaves the tile without a thumbnail.
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
