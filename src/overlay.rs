//! The overlay itself: a layer surface for the chrome, one subsurface per tile,
//! and the keyboard.
//!
//! Scaling is the compositor's job. A tile attaches its capture buffer directly
//! and wp_viewporter names the rectangle to fit it into, so nothing here touches
//! a pixel of window content — only the background, selection and labels.

use std::error::Error;
use std::os::fd::AsFd;

use wayland_client::protocol::{
    wl_buffer::{self, WlBuffer},
    wl_keyboard::{self, WlKeyboard},
    wl_pointer::{self, WlPointer},
    wl_seat::{self, WlSeat},
    wl_shm,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::Layer,
    zwlr_layer_surface_v1::{self, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};

use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::Shape;

use crate::app::{App, Ending};
use crate::shm;
use crate::theme::{Rect, fit_centred};

// evdev keycodes: physical positions, so navigation works on any keyboard
// layout without an xkb keymap. Reading typed characters would need one.
const KEY_ESC: u32 = 1;
const KEY_TAB: u32 = 15;
const KEY_Q: u32 = 16;
// hjkl, by physical position: the same keys as vim on a qwerty layout.
const KEY_H: u32 = 35;
const KEY_J: u32 = 36;
const KEY_K: u32 = 37;
const KEY_L: u32 = 38;
const KEY_ENTER: u32 = 28;
const KEY_LEFTSHIFT: u32 = 42;
const KEY_RIGHTSHIFT: u32 = 54;
const KEY_KPENTER: u32 = 96;
const KEY_HOME: u32 = 102;
const KEY_UP: u32 = 103;
const KEY_LEFT: u32 = 105;
const KEY_RIGHT: u32 = 106;
const KEY_END: u32 = 107;
const KEY_DOWN: u32 = 108;
const KEY_PGUP: u32 = 104;
const KEY_PGDN: u32 = 109;

/// evdev button code, as wl_pointer reports it.
const BTN_LEFT: u32 = 0x110;

/// How much continuous scroll makes one move. A wheel notch is one move
/// outright; a touchpad reports a flick as a stream of small values, and one
/// move per value would cross the whole grid in a single gesture.
const FINGER_STEP: f64 = 15.0;

/// Where the pointer is: the surface it entered, and the position within it.
/// Tiles are subsurfaces, so the surface alone usually names a tile; the
/// position is only needed over the chrome around them.
pub struct Hover {
    pub surface: WlSurface,
    pub x: f64,
    pub y: f64,
}

impl App {
    /// Map the overlay: a layer surface sized to hug the grid, plus the shm the
    /// chrome is painted into.
    pub fn show(&mut self, qh: &QueueHandle<Self>) -> Result<(), Box<dyn Error>> {
        let (lw, lh) = (self.layout.width, self.layout.height);
        let surface = self.compositor.create_surface(qh, ());
        // Map on the display the layout was sized against, not wherever the
        // compositor would otherwise put it.
        let output = self
            .outputs
            .iter()
            .find(|(_, name)| *name == self.output)
            .map(|(output, _)| output);
        let layer = self.layer_shell.get_layer_surface(
            &surface,
            output,
            Layer::Overlay,
            "wl-pick".to_string(),
            qh,
            (),
        );
        layer.set_size(lw as u32, lh as u32);
        // The grid is sized against the whole display, so it must not be laid
        // out inside what a bar has reserved: with a panel on screen the
        // compositor would otherwise grant less than was asked for.
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        surface.set_buffer_scale(self.scale);
        surface.commit();

        let (pw, ph) = (lw * self.scale, lh * self.scale);
        let len = shm::Chrome::slot_len(pw, ph) * shm::Chrome::SLOTS;
        let file = shm::memfd("wl-pick-chrome", len)?;
        let pool = self.shm.create_pool(file.as_fd(), len as i32, qh, ());
        for slot in 0..shm::Chrome::SLOTS {
            self.chrome_buffers.push(pool.create_buffer(
                (slot * shm::Chrome::slot_len(pw, ph)) as i32,
                pw,
                ph,
                shm::Chrome::stride(pw),
                wl_shm::Format::Argb8888,
                qh,
                slot,
            ));
        }
        pool.destroy();
        self.chrome = Some(shm::Chrome::new(&file, pw, ph)?);
        self.surface = Some(surface);
        Ok(())
    }

    /// Put every visible tile where the viewport says, and unmap the rest.
    ///
    /// Runs again after each scroll, so a tile scrolled off screen gets a null
    /// buffer — the way to hide a subsurface — rather than being left behind.
    /// Scaling stays the compositor's job: the capture buffer is attached as it
    /// is, and wp_viewporter names the rectangle to fit it into.
    pub fn sync_tiles(&mut self, qh: &QueueHandle<Self>) {
        // A capture that lands before the overlay is mapped has nothing to be
        // placed on yet; the pass after show() picks it up from `showing`.
        let Some(parent) = self.surface.clone() else {
            return;
        };
        let scroll = self.scroll;
        for i in 0..self.tiles.len() {
            let Some(box_) = self.layout.tile(i as i32, scroll) else {
                self.hide_tile(i);
                continue;
            };
            if !self.tiles[i].ready {
                continue;
            }
            let (bw, bh) = self.tiles[i].size;
            let (fit_w, fit_h) = if self.tiles[i].rotated() {
                (bh as i32, bw as i32)
            } else {
                (bw as i32, bh as i32)
            };
            let dst = fit_centred(fit_w, fit_h, box_);
            if self.tiles[i].surface.is_none() {
                let surface = self.compositor.create_surface(qh, ());
                let subsurface = self.subcompositor.get_subsurface(&surface, &parent, qh, ());
                let viewport = self.viewporter.get_viewport(&surface, qh, ());
                // Tiles change independently of the chrome — a live frame
                // arrives whenever its window does — so they must not wait on a
                // parent commit.
                subsurface.set_desync();
                // The capture protocol reports the transform the compositor
                // already applied to the buffer, which is exactly what this
                // request means, so it passes straight through.
                surface.set_buffer_transform(self.tiles[i].transform);
                let t = &mut self.tiles[i];
                t.surface = Some(surface);
                t.subsurface = Some(subsurface);
                t.viewport = Some(viewport);
            }
            let t = &self.tiles[i];
            let (surface, subsurface, viewport) = (
                t.surface.clone().expect("just created"),
                t.subsurface.clone().expect("just created"),
                t.viewport.clone().expect("just created"),
            );
            let slot = t.showing.expect("a ready tile has a slot");
            subsurface.set_position(dst.x, dst.y);
            viewport.set_destination(dst.w, dst.h);
            surface.attach(Some(&t.slots[slot].buffer), 0, 0);
            surface.damage_buffer(0, 0, bw as i32, bh as i32);
            surface.commit();
        }
        // Subsurface placement is *parent* state: it only takes effect when the
        // parent commits, desynced children included.
        parent.commit();
    }

    /// Unmap a tile's subsurface by attaching nothing to it.
    fn hide_tile(&mut self, i: usize) {
        if let Some(surface) = self.tiles[i].surface.clone() {
            surface.attach(None, 0, 0);
            surface.commit();
        }
    }

    /// Repaint background, selection highlight, labels and border.
    pub fn paint(&mut self) {
        // Never paint into a buffer the compositor is still reading. Both slots
        // outstanding means this repaint waits for a release rather than
        // tearing the one on screen; the release handler takes it then.
        let Some(slot) = self.chrome_busy.iter().position(|busy| !busy) else {
            self.repaint_due = true;
            return;
        };
        let (scale, sel, scroll) = (self.scale, self.sel, self.scroll);
        // Gather geometry before borrowing the chrome and the labels together.
        let elem = self
            .layout
            .elem(sel as i32, scroll)
            .map(|r| r.scaled(scale));
        let label_boxes: Vec<(usize, Rect)> = (0..self.tiles.len())
            .filter_map(|i| {
                self.layout
                    .label(i as i32, scroll)
                    .map(|r| (i, r.scaled(scale)))
            })
            .collect();
        let bar = self
            .layout
            .scrollbar(scroll, (self.theme.margin / 3).max(2))
            .map(|(track, thumb)| (track.scaled(scale), thumb.scaled(scale)));
        let t = &self.theme;
        let (bg, sel_bg, fg, sel_fg, border, border_px) = (
            t.bg,
            t.sel_bg,
            t.fg,
            t.sel_fg,
            t.border,
            t.border_px * scale,
        );
        let labels = self.labels.as_mut();
        let Some(chrome) = self.chrome.as_mut() else {
            return;
        };
        let (cw, ch) = (chrome.w, chrome.h);
        let mut p = chrome.painter(slot);
        p.fill(bg);
        // The selection fills the whole element box, padding included — the same
        // thing rofi's element background does. It can be scrolled out of sight.
        if let Some(elem) = elem {
            p.rect(elem, sel_bg);
        }
        if let Some(labels) = labels {
            for (i, at) in label_boxes {
                labels.draw(&mut p, i, at, if i == sel { sel_fg } else { fg });
            }
        }
        // A scrollbar only appears when there is something to scroll, which
        // makes it a hint rather than furniture.
        if let Some((track, thumb)) = bar {
            p.rect(track, bg);
            p.rect(thumb, border);
        }
        p.frame(border_px, border);

        let surface = self.surface.clone().expect("show() runs first");
        surface.attach(self.chrome_buffers.get(slot), 0, 0);
        surface.damage_buffer(0, 0, cw, ch);
        surface.commit();
        self.chrome_busy[slot] = true;
        self.repaint_due = false;
    }

    fn move_sel(&mut self, delta: i32, qh: &QueueHandle<Self>) {
        let n = self.tiles.len() as i32;
        if n == 0 {
            return;
        }
        self.select((self.sel as i32 + delta).rem_euclid(n) as usize, qh);
    }

    fn move_row(&mut self, rows: i32, qh: &QueueHandle<Self>) {
        if self.tiles.is_empty() {
            return;
        }
        self.select(self.layout.step_row(self.sel, rows), qh);
    }

    /// Move the selection, scrolling the least that keeps it on screen. Every
    /// move goes through here, so the selection is never off-view and the
    /// subsurfaces always match the viewport.
    fn select(&mut self, i: usize, qh: &QueueHandle<Self>) {
        self.sel = i;
        let scroll = self.layout.reveal(i, self.scroll);
        if scroll != self.scroll {
            self.scroll = scroll;
            self.sync_tiles(qh);
        }
        self.paint();
    }

    /// One notch of a wheel moves the selection; a touchpad's flick is summed
    /// so that a gesture moves by about as much as it looks like it should.
    fn scroll_by(&mut self, value: f64, qh: &QueueHandle<Self>) {
        let step = if value > 0.0 { 1 } else { -1 };
        if !self.scroll_finger {
            self.move_sel(step, qh);
            return;
        }
        // Turning round starts again, so a flick back does not have to undo
        // what is left over from the last one.
        if self.scroll_acc * value < 0.0 {
            self.scroll_acc = 0.0;
        }
        self.scroll_acc += value;
        while self.scroll_acc.abs() >= FINGER_STEP {
            self.scroll_acc -= FINGER_STEP.copysign(self.scroll_acc);
            self.move_sel(step, qh);
        }
    }

    /// The tile under the pointer, if it is over one. A tile's own subsurface
    /// answers directly; over the parent surface — padding, labels, gaps — the
    /// layout is asked instead.
    fn tile_at_pointer(&self) -> Option<usize> {
        let hover = self.hover.as_ref()?;
        let on_tile = self
            .tiles
            .iter()
            .position(|t| t.surface.as_ref() == Some(&hover.surface));
        on_tile.or_else(|| {
            (Some(&hover.surface) == self.surface.as_ref())
                .then(|| self.layout.hit(hover.x as i32, hover.y as i32, self.scroll))
                .flatten()
        })
    }

    /// Press and release on the same tile picks it. Anywhere else — the margin,
    /// a gap, an empty cell of the last row — does nothing at all.
    fn click(&mut self, pressed: bool) {
        if pressed {
            self.pressed = self.tile_at_pointer();
            return;
        }
        let released = self.tile_at_pointer();
        if let Some(i) = self.pressed.take().filter(|i| Some(*i) == released) {
            self.picked = self.tiles.get(i).map(|t| t.target.clone());
            self.ending = Ending::Picked;
        }
    }

    fn key(&mut self, code: u32, qh: &QueueHandle<Self>) {
        match code {
            KEY_LEFTSHIFT | KEY_RIGHTSHIFT => self.shift = true,
            KEY_ESC | KEY_Q => self.ending = Ending::Cancelled,
            KEY_ENTER | KEY_KPENTER => {
                self.picked = self.tiles.get(self.sel).map(|t| t.target.clone());
                self.ending = Ending::Picked;
            }
            KEY_TAB if self.shift => self.move_sel(-1, qh),
            KEY_TAB | KEY_RIGHT | KEY_L => self.move_sel(1, qh),
            KEY_LEFT | KEY_H => self.move_sel(-1, qh),
            KEY_DOWN | KEY_J => self.move_row(1, qh),
            KEY_UP | KEY_K => self.move_row(-1, qh),
            KEY_HOME => self.select(0, qh),
            KEY_END => self.select(self.tiles.len().saturating_sub(1), qh),
            KEY_PGUP => self.move_row(-self.layout.visible_rows, qh),
            KEY_PGDN => self.move_row(self.layout.visible_rows, qh),
            _ => {}
        }
    }
}

// --- event plumbing -------------------------------------------------------

impl Dispatch<ZwlrLayerSurfaceV1, ()> for App {
    fn event(
        app: &mut Self,
        layer: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure { serial, .. } => {
                layer.ack_configure(serial);
                app.configured = true;
            }
            zwlr_layer_surface_v1::Event::Closed => app.ending = Ending::Closed,
            _ => {}
        }
    }
}

impl Dispatch<WlSeat, ()> for App {
    fn event(
        app: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(caps),
        } = event
        else {
            return;
        };
        if caps.contains(wl_seat::Capability::Keyboard) {
            seat.get_keyboard(qh, ());
        }
        if caps.contains(wl_seat::Capability::Pointer) {
            let pointer = seat.get_pointer(qh, ());
            app.cursor_device = app
                .cursor_shape
                .as_ref()
                .map(|mgr| mgr.get_pointer(&pointer, qh, ()));
        }
    }
}

impl Dispatch<WlKeyboard, ()> for App {
    fn event(
        app: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Key { key, state, .. } => match state {
                WEnum::Value(wl_keyboard::KeyState::Pressed) => app.key(key, qh),
                WEnum::Value(wl_keyboard::KeyState::Released)
                    if key == KEY_LEFTSHIFT || key == KEY_RIGHTSHIFT =>
                {
                    app.shift = false
                }
                _ => {}
            },
            // Focus is only tracked here. sway sends leave immediately
            // followed by enter on the same surface when the pointer crosses
            // it, so whether the grab is really gone is decided by the main
            // loop, once the event batch has been dispatched.
            //
            // The grab arrives with the keys already down, which is the only
            // place they can be read: a keybinding with Shift in it is still
            // held when the overlay opens, and taking Shift from later events
            // alone would make Shift+Tab move forwards.
            wl_keyboard::Event::Enter { keys, .. } => {
                app.focused = true;
                app.shift = keys
                    .chunks_exact(4)
                    .filter_map(|k| k.try_into().ok())
                    .map(u32::from_ne_bytes)
                    .any(|k| k == KEY_LEFTSHIFT || k == KEY_RIGHTSHIFT);
            }
            wl_keyboard::Event::Leave { .. } => app.focused = false,
            _ => {}
        }
    }
}

/// Hovering does not move the selection — that belongs to the keyboard — so the
/// pointer only tracks where it is and what it clicked. Scrolling is a
/// deliberate gesture, so that does move the selection.
impl Dispatch<WlPointer, ()> for App {
    fn event(
        app: &mut Self,
        _: &WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter {
                serial,
                surface,
                surface_x,
                surface_y,
            } => {
                // A client owns the cursor over its own surfaces; without this
                // the pointer keeps whatever shape the window below gave it.
                if let Some(device) = &app.cursor_device {
                    device.set_shape(serial, Shape::Default);
                }
                app.hover = Some(Hover {
                    surface,
                    x: surface_x,
                    y: surface_y,
                });
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                if let Some(hover) = app.hover.as_mut() {
                    (hover.x, hover.y) = (surface_x, surface_y);
                }
            }
            wl_pointer::Event::Leave { .. } => {
                app.hover = None;
                app.pressed = None;
            }
            wl_pointer::Event::Button {
                button: BTN_LEFT,
                state: WEnum::Value(state),
                ..
            } => app.click(state == wl_pointer::ButtonState::Pressed),
            // Which device is scrolling decides how a value reads, and
            // sideways scroll is not a selection move at all.
            wl_pointer::Event::AxisSource {
                axis_source: WEnum::Value(source),
            } => {
                let finger = matches!(
                    source,
                    wl_pointer::AxisSource::Finger | wl_pointer::AxisSource::Continuous
                );
                // This arrives once per frame, not once per gesture, so only a
                // change of device starts the sum again: clearing it every time
                // would mean a touchpad never reached a whole step at all.
                if finger != app.scroll_finger {
                    app.scroll_acc = 0.0;
                }
                app.scroll_finger = finger;
            }
            wl_pointer::Event::Axis {
                axis: WEnum::Value(wl_pointer::Axis::VerticalScroll),
                value,
                ..
            } => app.scroll_by(value, qh),
            wl_pointer::Event::AxisStop { .. } => app.scroll_acc = 0.0,
            _ => {}
        }
    }
}

/// The chrome's own buffers. Two slots are enough to always have one free, but
/// only if the free one is the one the compositor has released — a repaint that
/// found both outstanding is taken here instead.
impl Dispatch<WlBuffer, usize> for App {
    fn event(
        app: &mut Self,
        _: &WlBuffer,
        event: wl_buffer::Event,
        &slot: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            if let Some(busy) = app.chrome_busy.get_mut(slot) {
                *busy = false;
            }
            if app.repaint_due {
                app.paint();
            }
        }
    }
}
