//! wlgrid shows a thumbnail grid of every open window as a layer-shell overlay
//! and focuses the one you pick. It replaces a wlthumbs + rofi pipeline, so it
//! keeps that pipeline's contract: sway owns the window list and the focusing,
//! and the look comes straight from the rofi theme (see theme.rs).
//!
//! The pixels never pass through this process. Each window is captured into an
//! shm buffer handed straight to a subsurface, with wp_viewporter telling the
//! compositor which rectangle to scale it into — so there is no thumbnail
//! encoding, no scaler, and no full-resolution image in our address space.

mod shm;
mod sway;
mod theme;

use std::error::Error;
use std::os::fd::AsFd;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use wayland_client::globals::{GlobalList, GlobalListContents, registry_queue_init};
use wayland_client::protocol::{
    wl_buffer::WlBuffer,
    wl_compositor::WlCompositor,
    wl_keyboard::{self, WlKeyboard},
    wl_output,
    wl_registry::WlRegistry,
    wl_seat::{self, WlSeat},
    wl_shm::{self, WlShm},
    wl_shm_pool::WlShmPool,
    wl_subcompositor::WlSubcompositor,
    wl_subsurface::WlSubsurface,
    wl_surface::WlSurface,
};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, delegate_noop, event_created_child,
};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1::{self, ExtForeignToplevelHandleV1},
    ext_foreign_toplevel_list_v1::{self, ExtForeignToplevelListV1},
};
use wayland_protocols::ext::image_capture_source::v1::client::{
    ext_foreign_toplevel_image_capture_source_manager_v1::ExtForeignToplevelImageCaptureSourceManagerV1,
    ext_image_capture_source_v1::ExtImageCaptureSourceV1,
};
use wayland_protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1},
    ext_image_copy_capture_manager_v1::{self, ExtImageCopyCaptureManagerV1},
    ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
};
use wayland_protocols::wp::viewporter::client::{
    wp_viewport::WpViewport, wp_viewporter::WpViewporter,
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};

use theme::{Layout, Rect, Theme, fit_centred};

// evdev keycodes: physical positions, so navigation works on any keyboard layout
// without an xkb keymap. Typing (and therefore xkb) arrives with filtering.
const KEY_ESC: u32 = 1;
const KEY_TAB: u32 = 15;
const KEY_Q: u32 = 16;
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

/// One window: its sway identity, its capture plumbing, and its subsurface.
#[allow(dead_code)] // `handle` is held to keep the toplevel alive
struct Tile {
    win: sway::Win,
    handle: Option<ExtForeignToplevelHandleV1>,

    session: Option<ExtImageCopyCaptureSessionV1>,
    frame: Option<ExtImageCopyCaptureFrameV1>,
    buffer: Option<WlBuffer>,
    formats: Vec<wl_shm::Format>,
    format: Option<wl_shm::Format>,
    /// Buffer size the session requires: the window's full resolution.
    size: (u32, u32),
    transform: wl_output::Transform,
    offset: usize,
    session_done: bool,
    ready: bool,
    failed: bool,
    settled: bool,

    surface: Option<WlSurface>,
    subsurface: Option<WlSubsurface>,
    viewport: Option<WpViewport>,
}

impl Tile {
    fn new(win: sway::Win) -> Self {
        Self {
            win,
            handle: None,
            session: None,
            frame: None,
            buffer: None,
            formats: Vec::new(),
            format: None,
            size: (0, 0),
            transform: wl_output::Transform::Normal,
            offset: 0,
            session_done: false,
            ready: false,
            failed: false,
            settled: false,
            surface: None,
            subsurface: None,
            viewport: None,
        }
    }

    fn bytes(&self) -> usize {
        self.size.0 as usize * 4 * self.size.1 as usize
    }

    /// Whether the buffer's contents are turned on their side relative to the
    /// window, which flips the aspect ratio we have to fit.
    fn rotated(&self) -> bool {
        use wl_output::Transform;
        matches!(
            self.transform,
            Transform::_90 | Transform::_270 | Transform::Flipped90 | Transform::Flipped270
        )
    }
}

struct App {
    compositor: WlCompositor,
    subcompositor: WlSubcompositor,
    shm: WlShm,
    viewporter: WpViewporter,
    layer_shell: ZwlrLayerShellV1,
    copy_mgr: ExtImageCopyCaptureManagerV1,
    src_mgr: ExtForeignToplevelImageCaptureSourceManagerV1,

    /// Toplevel handles as the compositor announces them, paired with the
    /// identifier that joins them to sway's tree.
    toplevels: Vec<(ExtForeignToplevelHandleV1, String)>,
    tiles: Vec<Tile>,

    theme: Theme,
    layout: Layout,
    scale: i32,
    sel: usize,
    shift: bool,

    surface: Option<WlSurface>,
    chrome: Option<shm::Chrome>,
    chrome_buffers: Vec<WlBuffer>,
    configured: bool,

    quit: bool,
    activate: Option<i64>,
}

impl App {
    fn new(
        globals: &GlobalList,
        qh: &QueueHandle<Self>,
        wins: Vec<sway::Win>,
        theme: Theme,
        scale: i32,
    ) -> Result<Self, Box<dyn Error>> {
        let layout = Layout::new(&theme, wins.len() as i32);
        // Bind everything up front so a compositor missing a protocol fails
        // here, with a name, rather than halfway through a capture.
        let app = Self {
            compositor: globals.bind(qh, 1..=6, ())?,
            subcompositor: globals.bind(qh, 1..=1, ())?,
            shm: globals.bind(qh, 1..=1, ())?,
            viewporter: globals.bind(qh, 1..=1, ())?,
            layer_shell: globals.bind(qh, 1..=5, ())?,
            copy_mgr: globals.bind(qh, 1..=1, ())?,
            src_mgr: globals.bind(qh, 1..=1, ())?,
            toplevels: Vec::new(),
            tiles: wins.into_iter().map(Tile::new).collect(),
            theme,
            layout,
            scale,
            sel: 0,
            shift: false,
            surface: None,
            chrome: None,
            chrome_buffers: Vec::new(),
            configured: false,
            quit: false,
            activate: None,
        };
        let _: ExtForeignToplevelListV1 = globals.bind(qh, 1..=1, ())?;
        let _: WlSeat = globals.bind(qh, 1..=7, ())?;
        Ok(app)
    }

    /// Open one capture session per window whose toplevel we recognise. They are
    /// all opened before a single roundtrip, so every session's buffer
    /// constraints arrive together instead of costing a round trip each.
    fn open_sessions(&mut self, qh: &QueueHandle<Self>) {
        for (i, tile) in self.tiles.iter_mut().enumerate() {
            let Some(handle) = self
                .toplevels
                .iter()
                .find(|(_, id)| !id.is_empty() && *id == tile.win.ft_id)
                .map(|(h, _)| h.clone())
            else {
                // No identifier match: the tile stays label-only, and must not
                // be waited on.
                tile.settled = true;
                continue;
            };
            let source: ExtImageCaptureSourceV1 = self.src_mgr.create_source(&handle, qh, ());
            tile.handle = Some(handle);
            tile.session = Some(self.copy_mgr.create_session(
                &source,
                ext_image_copy_capture_manager_v1::Options::empty(),
                qh,
                i,
            ));
            source.destroy();
        }
    }

    /// Allocate one pool for every capture buffer and put all the frames in
    /// flight at once: the compositor is bandwidth-bound reading pixels back, so
    /// serialising the captures only adds latency.
    fn start_captures(&mut self, qh: &QueueHandle<Self>) -> Result<(), Box<dyn Error>> {
        const PAGE: usize = 4096;
        let mut total = 0usize;
        for tile in &mut self.tiles {
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
            tile.offset = total;
            total += tile.bytes().div_ceil(PAGE) * PAGE;
        }
        if total == 0 {
            return Ok(());
        }

        // Note: no mmap. The compositor writes these pages and samples them
        // again for display; mapping them here would only cost us the faults.
        let file = shm::memfd("wlgrid-capture", total)?;
        let pool = self.shm.create_pool(file.as_fd(), total as i32, qh, ());
        for i in 0..self.tiles.len() {
            let (w, h, format, offset) = {
                let t = &self.tiles[i];
                if t.settled || t.session.is_none() || t.format.is_none() {
                    continue;
                }
                (
                    t.size.0 as i32,
                    t.size.1 as i32,
                    t.format.unwrap(),
                    t.offset as i32,
                )
            };
            let buffer = pool.create_buffer(offset, w, h, w * 4, format, qh, ());
            let session = self.tiles[i].session.clone().unwrap();
            let frame = session.create_frame(qh, i);
            frame.attach_buffer(&buffer);
            frame.damage_buffer(0, 0, w, h);
            frame.capture();
            let t = &mut self.tiles[i];
            t.buffer = Some(buffer);
            t.frame = Some(frame);
        }
        pool.destroy(); // the buffers keep the mapping alive
        Ok(())
    }

    fn captures_settled(&self) -> bool {
        self.tiles.iter().all(|t| t.settled)
    }

    /// Map the overlay: a layer surface sized to hug the grid, plus the shm the
    /// chrome is painted into.
    fn show(&mut self, qh: &QueueHandle<Self>) -> Result<(), Box<dyn Error>> {
        let (lw, lh) = (self.layout.width, self.layout.height);
        let surface = self.compositor.create_surface(qh, ());
        let layer = self.layer_shell.get_layer_surface(
            &surface,
            None, // let the compositor place it on the active output
            Layer::Overlay,
            "wlgrid".to_string(),
            qh,
            (),
        );
        layer.set_size(lw as u32, lh as u32);
        layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        surface.set_buffer_scale(self.scale);
        surface.commit();

        let (pw, ph) = (lw * self.scale, lh * self.scale);
        let len = shm::Chrome::slot_len(pw, ph) * shm::Chrome::SLOTS;
        let file = shm::memfd("wlgrid-chrome", len)?;
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
    fn place_tiles(&mut self, qh: &QueueHandle<Self>) {
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
            // Tiles change independently of the chrome (selection moves now,
            // live frames later), so they must not wait on a parent commit.
            subsurface.set_desync();
            // The capture protocol reports the transform the compositor already
            // applied to the buffer, which is exactly what this request means,
            // so it passes straight through and the compositor un-rotates it.
            surface.set_buffer_transform(self.tiles[i].transform);
            viewport.set_destination(dst.w, dst.h);
            surface.attach(self.tiles[i].buffer.as_ref(), 0, 0);
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

    /// Repaint background, selection highlight and border.
    fn paint(&mut self) {
        let (theme, scale, sel) = (&self.theme, self.scale, self.sel);
        let elem = self.layout.elem(sel as i32);
        let Some(chrome) = self.chrome.as_mut() else {
            return;
        };
        let slot = chrome.next_slot();
        let (cw, ch) = (chrome.w, chrome.h);
        let mut p = chrome.painter();
        p.fill(theme.bg);
        // The selection fills the whole element box, padding included — the same
        // thing rofi's element background does.
        p.rect(
            Rect {
                x: elem.x * scale,
                y: elem.y * scale,
                w: elem.w * scale,
                h: elem.h * scale,
            },
            theme.sel_bg,
        );
        p.frame(theme.border_px * scale, theme.border);

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

    fn key(&mut self, code: u32) {
        match code {
            KEY_LEFTSHIFT | KEY_RIGHTSHIFT => self.shift = true,
            KEY_ESC | KEY_Q => self.quit = true,
            KEY_ENTER | KEY_KPENTER => {
                self.activate = self.tiles.get(self.sel).map(|t| t.win.con_id);
                self.quit = true;
            }
            KEY_TAB if self.shift => self.move_sel(-1),
            KEY_TAB | KEY_RIGHT => self.move_sel(1),
            KEY_LEFT => self.move_sel(-1),
            KEY_DOWN => self.move_row(1),
            KEY_UP => self.move_row(-1),
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

/// Phase timings, printed with --verbose. Opening latency is the whole point of
/// this tool, so it stays measurable.
struct Phases {
    on: bool,
    last: Instant,
}

impl Phases {
    fn new(on: bool) -> Self {
        Self {
            on,
            last: Instant::now(),
        }
    }

    fn mark(&mut self, label: &str) {
        if self.on {
            let now = Instant::now();
            eprintln!(
                "{label:<12} {:6.1}ms",
                (now - self.last).as_secs_f64() * 1000.0
            );
            self.last = now;
        }
    }
}

fn pump(
    queue: &mut EventQueue<App>,
    app: &mut App,
    done: impl Fn(&App) -> bool,
) -> Result<(), Box<dyn Error>> {
    while !done(app) {
        queue.blocking_dispatch(app)?;
    }
    Ok(())
}

struct Args {
    print: bool,
    verbose: bool,
    timeout: Option<Duration>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        print: false,
        verbose: false,
        timeout: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--print" => args.print = true,
            "-v" | "--verbose" => args.verbose = true,
            "--timeout" => {
                let v = it.next().ok_or("--timeout needs seconds")?;
                let secs: f64 = v.parse().map_err(|_| format!("bad --timeout: {v}"))?;
                args.timeout = Some(Duration::from_secs_f64(secs));
            }
            "-h" | "--help" => {
                println!("usage: wlgrid [--print] [--verbose] [--timeout SECS]");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(args)
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("wlgrid: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode, Box<dyn Error>> {
    let args = parse_args().map_err(|e| -> Box<dyn Error> { e.into() })?;
    // An exclusive keyboard grab makes a hung overlay unusable, so keep an
    // escape hatch that cannot itself deadlock.
    if let Some(d) = args.timeout {
        std::thread::spawn(move || {
            std::thread::sleep(d);
            eprintln!("wlgrid: timeout");
            std::process::exit(2);
        });
    }

    let mut phases = Phases::new(args.verbose);
    let mut sway_conn = swayipc::Connection::new()?;
    let wins = sway::windows(&mut sway_conn)?;
    if wins.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }
    let scale = sway_conn
        .get_outputs()?
        .iter()
        .filter(|o| o.active)
        .map(|o| o.scale.unwrap_or(1.0).ceil() as i32)
        .max()
        .unwrap_or(1)
        .max(1);

    phases.mark("sway-tree");

    let conn = Connection::connect_to_env()?;
    let (globals, mut queue) = registry_queue_init::<App>(&conn)?;
    let qh = queue.handle();
    let mut app = App::new(&globals, &qh, wins, Theme::default(), scale)?;

    // Two roundtrips: one for the toplevel list, one for each handle's state.
    queue.roundtrip(&mut app)?;
    queue.roundtrip(&mut app)?;

    phases.mark("toplevels");

    app.open_sessions(&qh);
    queue.roundtrip(&mut app)?; // every session's constraints at once
    phases.mark("constraints");
    app.start_captures(&qh)?;
    pump(&mut queue, &mut app, |a| a.captures_settled())?;
    phases.mark("capture");

    if args.verbose {
        let ready = app.tiles.iter().filter(|t| t.ready).count();
        let matched = app.tiles.iter().filter(|t| t.handle.is_some()).count();
        eprintln!(
            "wlgrid: {} window(s), {matched} matched, {ready} captured; \
             grid {}x{}, surface {}x{} logical at scale {}",
            app.tiles.len(),
            app.layout.cols,
            app.layout.rows,
            app.layout.width,
            app.layout.height,
            app.scale,
        );
    }
    app.show(&qh)?;
    pump(&mut queue, &mut app, |a| a.configured)?;
    app.paint();
    app.place_tiles(&qh);
    conn.flush()?;
    phases.mark("mapped");

    pump(&mut queue, &mut app, |a| a.quit)?;

    if let Some(con_id) = app.activate {
        if args.print {
            println!("{con_id}");
        } else {
            sway::focus(&mut sway_conn, con_id)?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

// --- event plumbing -------------------------------------------------------

impl Dispatch<WlRegistry, GlobalListContents> for App {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtForeignToplevelListV1, ()> for App {
    fn event(
        app: &mut Self,
        _: &ExtForeignToplevelListV1,
        event: ext_foreign_toplevel_list_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel } = event {
            app.toplevels.push((toplevel, String::new()));
        }
    }

    event_created_child!(App, ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ExtForeignToplevelHandleV1, ()> for App {
    fn event(
        app: &mut Self,
        handle: &ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_foreign_toplevel_handle_v1::Event::Identifier { identifier } = event
            && let Some(entry) = app.toplevels.iter_mut().find(|(h, _)| h == handle)
        {
            entry.1 = identifier;
        }
    }
}

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
            ext_image_copy_capture_session_v1::Event::Stopped => {
                tile.failed = true;
                tile.settled = true;
            }
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
                tile.ready = true;
                tile.settled = true;
                // The protocol wants the frame destroyed once ready; the buffer
                // stays ours to display.
                if let Some(frame) = tile.frame.take() {
                    frame.destroy();
                }
            }
            ext_image_copy_capture_frame_v1::Event::Failed { reason } => {
                eprintln!(
                    "wlgrid: capture failed for {:?} ({reason:?})",
                    tile.win.title
                );
                tile.failed = true;
                tile.settled = true;
                if let Some(frame) = tile.frame.take() {
                    frame.destroy();
                }
            }
            _ => {}
        }
    }
}

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
            zwlr_layer_surface_v1::Event::Closed => app.quit = true,
            _ => {}
        }
    }
}

impl Dispatch<WlSeat, ()> for App {
    fn event(
        _: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(caps),
        } = event
            && caps.contains(wl_seat::Capability::Keyboard)
        {
            seat.get_keyboard(qh, ());
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

// Interfaces we drive but never listen to.
delegate_noop!(App: WlCompositor);
delegate_noop!(App: WlSubcompositor);
delegate_noop!(App: WlSubsurface);
delegate_noop!(App: ignore WlShm);
delegate_noop!(App: WlShmPool);
delegate_noop!(App: WpViewporter);
delegate_noop!(App: WpViewport);
delegate_noop!(App: ZwlrLayerShellV1);
delegate_noop!(App: ExtImageCopyCaptureManagerV1);
delegate_noop!(App: ExtForeignToplevelImageCaptureSourceManagerV1);
delegate_noop!(App: ExtImageCaptureSourceV1);
delegate_noop!(App: ignore WlSurface);
delegate_noop!(App: ignore WlBuffer);
