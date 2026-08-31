//! The client: everything the compositor talks to us through.
//!
//! `App` is the one state the Wayland event queue dispatches into, so it holds
//! the bound globals, the tiles, and the pieces of the overlay. The work itself
//! lives next door: capture.rs drives the sessions, overlay.rs draws and reads
//! the keyboard.

use std::error::Error;

use wayland_client::globals::{GlobalList, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer,
    wl_compositor::WlCompositor,
    wl_output::{self, WlOutput},
    wl_registry::WlRegistry,
    wl_seat::WlSeat,
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    wl_subcompositor::WlSubcompositor,
    wl_subsurface::WlSubsurface,
    wl_surface::WlSurface,
};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, delegate_noop, event_created_child,
};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1::{self, ExtForeignToplevelHandleV1},
    ext_foreign_toplevel_list_v1::{self, ExtForeignToplevelListV1},
};
use wayland_protocols::ext::image_capture_source::v1::client::{
    ext_foreign_toplevel_image_capture_source_manager_v1::ExtForeignToplevelImageCaptureSourceManagerV1,
    ext_image_capture_source_v1::ExtImageCaptureSourceV1,
    ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1,
};
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1;
use wayland_protocols::wp::cursor_shape::v1::client::{
    wp_cursor_shape_device_v1::WpCursorShapeDeviceV1,
    wp_cursor_shape_manager_v1::WpCursorShapeManagerV1,
};
use wayland_protocols::wp::viewporter::client::{
    wp_viewport::WpViewport, wp_viewporter::WpViewporter,
};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::ZwlrLayerShellV1;

use crate::capture::{Live, Tile};
use crate::overlay;
use crate::shm;
use crate::target::Target;
use crate::text;
use crate::theme::{Layout, Theme};

/// What the caller decided before any of this started: the look, and how much
/// live capture to do. Passed as one value because `theme`, `live`, `fps` and
/// `scale` all arrive together and two of them are bare integers.
pub struct Settings {
    pub theme: Theme,
    pub live: Live,
    pub fps: u32,
    /// Integer scale of the display the overlay renders on.
    pub scale: i32,
    /// That display's logical size, which the grid is fitted into.
    pub display: (i32, i32),
    /// And its name, so the overlay maps there rather than wherever the
    /// compositor would have put it.
    pub output: String,
}

pub struct App {
    pub(crate) compositor: WlCompositor,
    pub(crate) subcompositor: WlSubcompositor,
    pub(crate) shm: WlShm,
    pub(crate) viewporter: WpViewporter,
    pub(crate) layer_shell: ZwlrLayerShellV1,
    pub(crate) copy_mgr: ExtImageCopyCaptureManagerV1,
    pub(crate) src_mgr: ExtForeignToplevelImageCaptureSourceManagerV1,

    /// Toplevel handles as the compositor announces them, paired with the
    /// identifier that joins them to sway's tree.
    pub(crate) toplevels: Vec<(ExtForeignToplevelHandleV1, String)>,
    /// Displays, paired with the name the compositor gives them (wl_output v4).
    pub(crate) outputs: Vec<(WlOutput, String)>,
    pub(crate) output_src_mgr: Option<ExtOutputImageCaptureSourceManagerV1>,
    pub(crate) tiles: Vec<Tile>,

    pub(crate) theme: Theme,
    pub(crate) layout: Layout,
    pub(crate) live: Live,
    pub(crate) fps: u32,
    pub(crate) scale: i32,
    pub(crate) sel: usize,
    /// First row of the grid on screen. The rest scroll.
    pub(crate) scroll: i32,
    /// Set when the viewport moved and the subsurfaces need re-placing.
    pub(crate) needs_tiles: bool,
    pub(crate) shift: bool,

    /// Where the pointer is, and which tile it pressed. Hovering deliberately
    /// does not move the keyboard selection; a click acts on what is under the
    /// cursor instead.
    pub(crate) hover: Option<overlay::Hover>,
    pub(crate) pressed: Option<usize>,
    /// Set the cursor shape without shipping a cursor theme. Optional: without
    /// it the pointer keeps whatever shape it had over the window below.
    pub(crate) cursor_shape: Option<WpCursorShapeManagerV1>,
    pub(crate) cursor_device: Option<WpCursorShapeDeviceV1>,

    pub(crate) labels: Option<text::Labels>,
    pub(crate) surface: Option<WlSurface>,
    pub(crate) chrome: Option<shm::Chrome>,
    pub(crate) chrome_buffers: Vec<WlBuffer>,
    pub(crate) configured: bool,

    /// The display the overlay maps on, by name.
    pub(crate) output: String,

    pub(crate) ending: Ending,
    pub(crate) picked: Option<Target>,
    pub(crate) stats: Stats,
}

/// Counters worth reporting with --verbose. Live capture is easy to get subtly
/// wrong — a starved buffer pool or a clock that never ticks both look like
/// "nothing updates" — so the numbers that distinguish those stay available.
#[derive(Default)]
pub struct Stats {
    /// Frame callbacks received, i.e. how often the live clock fired.
    pub(crate) ticks: u32,
    /// Re-captures skipped because every buffer was still held.
    pub(crate) starved: u32,
    /// Bytes of shm handed to the compositor for capture buffers.
    pub(crate) pool_bytes: usize,
}

/// Why the overlay stopped, for --verbose. An exit status of 1 cannot tell a
/// cancel from a surface the compositor took away.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    Running,
    Picked,
    Cancelled,
    Closed,
}

impl Ending {
    pub fn as_str(self) -> &'static str {
        match self {
            Ending::Running => "still running",
            Ending::Picked => "picked",
            Ending::Cancelled => "cancelled",
            Ending::Closed => "the compositor closed the overlay",
        }
    }
}

impl App {
    pub fn new(
        globals: &GlobalList,
        qh: &QueueHandle<Self>,
        targets: Vec<Target>,
        settings: Settings,
    ) -> Result<Self, Box<dyn Error>> {
        let Settings {
            theme,
            live,
            fps,
            scale,
            display,
            output,
        } = settings;
        let layout = Layout::new(&theme, targets.len() as i32, display);
        // Bind everything up front so a compositor missing a protocol fails
        // here, with a name, rather than halfway through a capture.
        let mut app = Self {
            compositor: globals.bind(qh, 1..=6, ())?,
            subcompositor: globals.bind(qh, 1..=1, ())?,
            shm: globals.bind(qh, 1..=1, ())?,
            viewporter: globals.bind(qh, 1..=1, ())?,
            layer_shell: globals.bind(qh, 1..=5, ())?,
            copy_mgr: globals.bind(qh, 1..=1, ())?,
            src_mgr: globals.bind(qh, 1..=1, ())?,
            toplevels: Vec::new(),
            outputs: Vec::new(),
            // Optional: a compositor without it simply gets no display tiles.
            output_src_mgr: globals.bind(qh, 1..=1, ()).ok(),
            tiles: targets.into_iter().map(Tile::new).collect(),
            theme,
            layout,
            live,
            fps,
            scale,
            sel: 0,
            scroll: 0,
            needs_tiles: false,
            shift: false,
            hover: None,
            pressed: None,
            cursor_shape: globals.bind(qh, 1..=2, ()).ok(),
            cursor_device: None,
            labels: None,
            surface: None,
            chrome: None,
            chrome_buffers: Vec::new(),
            configured: false,
            output,
            ending: Ending::Running,
            picked: None,
            stats: Stats::default(),
        };
        let _: ExtForeignToplevelListV1 = globals.bind(qh, 1..=1, ())?;
        // One wl_output per display, bound at v4 so it tells us its name.
        for global in globals.contents().clone_list() {
            if global.interface == WlOutput::interface().name {
                let version = global.version.min(4);
                if version >= 4 {
                    let output: WlOutput = globals.registry().bind(global.name, version, qh, ());
                    app.outputs.push((output, String::new()));
                }
            }
        }
        let _: WlSeat = globals.bind(qh, 1..=7, ())?;
        Ok(app)
    }

    pub fn finished(&self) -> bool {
        self.ending != Ending::Running
    }

    /// The pick, if there was one.
    pub fn picked(&self) -> Option<&Target> {
        self.picked.as_ref()
    }

    /// What the grid is about to show: one line per tile, then the geometry.
    pub fn describe(&self) {
        for (i, t) in self.tiles.iter().enumerate() {
            let mark = if t.ready { "" } else { "  (no thumbnail)" };
            eprintln!("  [{i}] {}{mark}", t.target.tsv());
        }
        eprintln!(
            "wl-pick: {} tile(s), {} captured; grid {}x{}, surface {}x{} logical \
             at scale {}, {} MB of capture buffers, labels in {:?}",
            self.tiles.len(),
            self.tiles.iter().filter(|t| t.ready).count(),
            self.layout.cols,
            self.layout.rows,
            self.layout.width,
            self.layout.height,
            self.scale,
            self.stats.pool_bytes >> 20,
            self.labels.as_ref().map(|l| l.family()).unwrap_or("none"),
        );
        if self.layout.scrollable() {
            eprintln!(
                "wl-pick: {} of {} rows fit; the rest scroll",
                self.layout.visible_rows, self.layout.rows
            );
        }
    }

    /// What live capture actually did, once the overlay is closing.
    pub fn report(&self, open_for: std::time::Duration) {
        let frames: u32 = self.tiles.iter().map(|t| t.frames).sum();
        let secs = open_for.as_secs_f64();
        eprintln!(
            "wl-pick: {}, {frames} frame(s) over {secs:.1}s = {:.1}/s, {} tick(s), \
             {} starved; per tile: {}",
            self.ending.as_str(),
            frames as f64 / secs,
            self.stats.ticks,
            self.stats.starved,
            self.tiles
                .iter()
                .map(|t| t.frames.to_string())
                .collect::<Vec<_>>()
                .join(",")
        );
    }

    pub fn captures_settled(&self) -> bool {
        self.tiles.iter().all(|t| t.settled)
    }
}

// --- enumeration ----------------------------------------------------------

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

/// wl_output tells us its name (v4), which is how a display tile is labelled
/// and how `focus output NAME` finds it again.
impl Dispatch<WlOutput, ()> for App {
    fn event(
        app: &mut Self,
        output: &WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = event
            && let Some(entry) = app.outputs.iter_mut().find(|(o, _)| o == output)
        {
            entry.1 = name;
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
delegate_noop!(App: ExtOutputImageCaptureSourceManagerV1);
delegate_noop!(App: ignore WlSurface);
// The chrome's own buffers: two slots alternating on keypresses, so their
// release timing does not matter.
delegate_noop!(App: WpCursorShapeManagerV1);
delegate_noop!(App: WpCursorShapeDeviceV1);
delegate_noop!(App: ignore WlBuffer);
