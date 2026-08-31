//! The overlay itself: a layer surface for the chrome, one subsurface per tile,
//! and the keyboard.
//!
//! Scaling is the compositor's job. A tile attaches its capture buffer directly
//! and wp_viewporter names the rectangle to fit it into, so nothing here touches
//! a pixel of window content — only the background, selection and labels.

use std::error::Error;
use std::os::fd::AsFd;

use wayland_client::protocol::{
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

/// evdev button code, as wl_pointer reports it.
const BTN_LEFT: u32 = 0x110;

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
                (),
            ));
        }
        pool.destroy();
        self.chrome = Some(shm::Chrome::new(&file, pw, ph)?);
        self.surface = Some(surface);
        Ok(())
    }

    /// Attach each captured buffer to its own subsurface and let the compositor
    /// scale it into the tile rectangle.
    pub fn place_tiles(&mut self, qh: &QueueHandle<Self>) {
        let parent = self.surface.clone().expect("show() runs first");
        for i in 0..self.tiles.len() {
            if !self.tiles[i].ready {
                continue;
            }
            let (bw, bh) = self.tiles[i].size;
            let (fit_w, fit_h) = if self.tiles[i].rotated() {
                (bh as i32, bw as i32)
            } else {
                (bw as i32, bh as i32)
            };
            let dst = fit_centred(fit_w, fit_h, self.layout.tile(i as i32));
            let surface = self.compositor.create_surface(qh, ());
            let subsurface = self.subcompositor.get_subsurface(&surface, &parent, qh, ());
            let viewport = self.viewporter.get_viewport(&surface, qh, ());
            subsurface.set_position(dst.x, dst.y);
            // Tiles change independently of the chrome — a live frame arrives
            // whenever its window does — so they must not wait on a parent
            // commit.
            subsurface.set_desync();
            // The capture protocol reports the transform the compositor already
            // applied to the buffer, which is exactly what this request means,
            // so it passes straight through and the compositor un-rotates it.
            surface.set_buffer_transform(self.tiles[i].transform);
            viewport.set_destination(dst.w, dst.h);
            let slot = self.tiles[i].showing.expect("a ready tile has a slot");
            surface.attach(Some(&self.tiles[i].slots[slot].buffer), 0, 0);
            surface.damage_buffer(0, 0, bw as i32, bh as i32);
            surface.commit();
            let t = &mut self.tiles[i];
            t.surface = Some(surface);
            t.subsurface = Some(subsurface);
            t.viewport = Some(viewport);
        }
        // Subsurface placement is *parent* state: it only takes effect when the
        // parent commits, desynced children included.
        parent.commit();
    }

    /// Repaint background, selection highlight, labels and border.
    pub fn paint(&mut self) {
        let (scale, sel) = (self.scale, self.sel);
        let elem = self.layout.elem(sel as i32).scaled(scale);
        // Gather geometry before borrowing the chrome and the labels together.
        let label_boxes: Vec<(usize, Rect)> = (0..self.tiles.len())
            .filter_map(|i| self.layout.label(i as i32).map(|r| (i, r.scaled(scale))))
            .collect();
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
        let slot = chrome.next_slot();
        let (cw, ch) = (chrome.w, chrome.h);
        let mut p = chrome.painter();
        p.fill(bg);
        // The selection fills the whole element box, padding included — the same
        // thing rofi's element background does.
        p.rect(elem, sel_bg);
        if let Some(labels) = labels {
            for (i, at) in label_boxes {
                labels.draw(&mut p, i, at, if i == sel { sel_fg } else { fg });
            }
        }
        p.frame(border_px, border);

        let surface = self.surface.clone().expect("show() runs first");
        surface.attach(self.chrome_buffers.get(slot), 0, 0);
        surface.damage_buffer(0, 0, cw, ch);
        surface.commit();
    }

    fn move_sel(&mut self, delta: i32) {
        let n = self.tiles.len() as i32;
        if n == 0 {
            return;
        }
        self.sel = (self.sel as i32 + delta).rem_euclid(n) as usize;
        self.paint();
    }

    fn move_row(&mut self, rows: i32) {
        let n = self.tiles.len() as i32;
        let target = self.sel as i32 + rows * self.layout.cols;
        if target >= 0 && target < n {
            self.sel = target as usize;
            self.paint();
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
                .then(|| self.layout.hit(hover.x as i32, hover.y as i32))
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

    fn key(&mut self, code: u32) {
        match code {
            KEY_LEFTSHIFT | KEY_RIGHTSHIFT => self.shift = true,
            KEY_ESC | KEY_Q => self.ending = Ending::Cancelled,
            KEY_ENTER | KEY_KPENTER => {
                self.picked = self.tiles.get(self.sel).map(|t| t.target.clone());
                self.ending = Ending::Picked;
            }
            KEY_TAB if self.shift => self.move_sel(-1),
            KEY_TAB | KEY_RIGHT | KEY_L => self.move_sel(1),
            KEY_LEFT | KEY_H => self.move_sel(-1),
            KEY_DOWN | KEY_J => self.move_row(1),
            KEY_UP | KEY_K => self.move_row(-1),
            KEY_HOME => {
                self.sel = 0;
                self.paint();
            }
            KEY_END => {
                self.sel = self.tiles.len().saturating_sub(1);
                self.paint();
            }
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
        _: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Key { key, state, .. } = event {
            match state {
                WEnum::Value(wl_keyboard::KeyState::Pressed) => app.key(key),
                WEnum::Value(wl_keyboard::KeyState::Released)
                    if key == KEY_LEFTSHIFT || key == KEY_RIGHTSHIFT =>
                {
                    app.shift = false
                }
                _ => {}
            }
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
        _: &QueueHandle<Self>,
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
            wl_pointer::Event::Axis { value, .. } => app.move_sel(if value > 0.0 { 1 } else { -1 }),
            _ => {}
        }
    }
}
