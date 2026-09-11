//! wl-pick shows a live grid of every window and display as a layer-shell
//! overlay and reports which one you picked. That is all it does: acting on the
//! choice belongs to whatever called it.
//!
//! The interesting constraint is opening fast, because a picker that lags is a
//! picker you stop using. Two things follow from it. The compositor spends ~55ms
//! copying window pixels back for us, and that time is otherwise spent blocked,
//! so the labels are shaped on a worker thread inside it. And the pixels never
//! pass through this process at all: each capture buffer is handed straight to a
//! subsurface with wp_viewporter naming the rectangle to scale it into, so there
//! is no thumbnail encoding, no scaler, and no full-resolution image in our
//! address space.
//!
//! - `cli` — flags and help
//! - `sway` — the window list, over sway's IPC socket
//! - `target` — what a tile stands for, and how a pick is reported
//! - `app` — the Wayland client state everything dispatches into
//! - `capture` — capture sessions and their buffers
//! - `overlay` — the layer surface, the drawing, the keyboard
//! - `theme`, `text`, `shm` — look, labels, and shared memory

// `slice::as_chunks` and friends, which clippy suggests in place of
// `chunks_exact`, are newer than the toolchain this crate says it supports.
#![allow(clippy::chunks_exact_to_as_chunks)]

mod app;
mod capture;
mod cli;
mod config;
mod overlay;
mod shm;
mod sway;
mod target;
mod text;
mod theme;

use std::error::Error;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec};
use wayland_client::globals::registry_queue_init;
use wayland_client::{Connection, EventQueue};

use app::{App, Ending};
use config::Config;
use target::Target;
use theme::Layout;

/// How long any one bounded wait before the overlay is interactive may take.
/// Capture measures ~90ms for fourteen windows, so this is a wide margin around
/// anything healthy, and only a stall reaches it.
const STARTUP_BUDGET: Duration = Duration::from_secs(2);

/// How long a keyboard leave is given to turn out to be a focus refresh rather
/// than a real loss. sway's pair arrives microseconds apart; this is only long
/// enough to be sure, and short enough that a real handover looks instant.
const REFOCUS_GRACE: Duration = Duration::from_millis(150);

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("wl-pick: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode, Box<dyn Error>> {
    let args = cli::parse_args().map_err(Box::<dyn Error>::from)?;
    let config = Config::load(args.config.as_deref()).map_err(Box::<dyn Error>::from)?;

    let start = Instant::now();
    let mut phases = Phases::new(args.verbose);
    // One IPC conversation: the window list, and the displays the grid sizes
    // itself against. It is closed again before the overlay maps.
    let (targets, opts) = {
        let mut sway = sway::connect()?;
        // The displays come first: the grid is sized against the one it will
        // appear on, so every percentage in the config resolves per monitor.
        let displays = sway::displays(&mut sway)?;
        let display = sway::focused(&displays).ok_or("sway reports no active display")?;
        let opts = args.resolve(&config, display);
        let mut targets = sway::windows(&mut sway)?;
        if opts.outputs {
            // Displays go last, after the windows, so window positions are
            // stable as windows come and go.
            targets.extend(displays.iter().map(|d| Target::output(d.name.clone())));
        }
        (targets, opts)
    };
    if targets.is_empty() {
        // Nothing was picked, so this exits the way a cancel does: the
        // documented contract is 0 for a pick and 1 for anything else.
        return Ok(ExitCode::FAILURE);
    }
    phases.mark("sway-tree");

    cli::arm_timeout(opts.timeout);
    let (display, settings) = (opts.display, opts.settings);
    let theme = &settings.theme;
    let scale = settings.scale;
    // The grid is measured once here: the label shaping below and the overlay
    // itself must agree about how wide a label may be.
    let layout = Layout::new(theme, targets.len() as i32, display);
    // Start shaping labels now: it costs ~55ms of font loading and glyph
    // rasterising, and the captures below are ~55ms of waiting on the
    // compositor, so the two overlap almost exactly.
    let labels = layout.label(0, 0).map(|label| {
        text::spawn(
            targets.iter().map(Target::label).collect(),
            theme.font.clone(),
            theme.font_px * scale as f32,
            (theme.line_h * scale) as f32,
            (label.w * scale) as f32,
        )
    });

    let conn = Connection::connect_to_env()?;
    let (globals, mut queue) = registry_queue_init::<App>(&conn)?;
    let qh = queue.handle();
    let mut app = App::new(&globals, &qh, targets, settings, layout)?;

    // Two roundtrips: one for the toplevel list, one for each handle's state.
    queue.roundtrip(&mut app)?;
    queue.roundtrip(&mut app)?;
    phases.mark("toplevels");

    app.open_sessions(&qh);
    queue.roundtrip(&mut app)?; // every session's constraints at once
    phases.mark("constraints");

    app.start_captures(&qh)?;
    // Tiles that never delivered are shown as labels without a thumbnail,
    // exactly as an outright capture failure is. Better a grid you can use
    // than a process you have to hunt down.
    if !pump_for(
        &conn,
        &mut queue,
        &mut app,
        |a| a.captures_settled(),
        STARTUP_BUDGET,
    )? {
        app.report_unsettled();
    }
    phases.mark("capture");

    if let Some(job) = labels {
        app.labels = Some(job.join().map_err(|_| "label thread panicked")?);
    }
    phases.mark("labels");
    if opts.verbose {
        app.describe();
    }

    app.show(&qh)?;
    if !pump_for(
        &conn,
        &mut queue,
        &mut app,
        |a| a.configured,
        STARTUP_BUDGET,
    )? {
        return Err("the compositor never configured the overlay".into());
    }
    app.paint();
    app.sync_tiles(&qh);
    app.arm_frame_callback(&qh);
    conn.flush()?;
    phases.mark("mapped");

    pump_interactive(&conn, &mut queue, &mut app)?;
    if opts.verbose {
        app.report(start.elapsed());
    }

    let Some(target) = app.picked() else {
        return Ok(ExitCode::FAILURE); // cancelled: nothing on stdout
    };
    match target.render(opts.format) {
        Some(line) => println!("{line}"),
        // Only the portal format can fail to name something: it identifies a
        // window by its foreign-toplevel identifier, and this one has none.
        None => {
            eprintln!("wl-pick: {:?} has no toplevel identifier", target.title);
            return Ok(ExitCode::FAILURE);
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Run the overlay until the user picks or cancels, or the keyboard goes away.
///
/// The grab is what makes the overlay usable, so losing it for good ends the
/// run: that is how a second wl-pick, started from the same keybinding,
/// replaces the first instead of leaving it stranded on screen. For *good*,
/// because sway also cycles focus off and straight back on in a single batch as
/// the pointer crosses the surface, so a leave is believed only once it has
/// failed to come back.
fn pump_interactive(
    conn: &Connection,
    queue: &mut EventQueue<App>,
    app: &mut App,
) -> Result<(), Box<dyn Error>> {
    while !app.finished() {
        queue.blocking_dispatch(app)?;
        if app.finished() || app.focused {
            continue;
        }
        if !pump_for(
            conn,
            queue,
            app,
            |a| a.focused || a.finished(),
            REFOCUS_GRACE,
        )? {
            app.ending = Ending::Unfocused;
        }
    }
    Ok(())
}

/// Run the event loop until `done`, or until `limit` has passed. Returns
/// whether `done` came true in time.
///
/// Every wait on a capture is bounded, because a compositor is entitled to
/// simply never answer. sway does exactly that for a capture request on a
/// toplevel another client is already capturing: no frame, no `failed`, no
/// `stopped`, just silence — and an unbounded wait on that is a picker with no
/// window that has to be killed from another terminal.
///
/// The roundtrips above are not bounded this way, so a compositor that stalls
/// on the toplevel list or on a session's constraints still hangs us;
/// `--timeout` is the only backstop there.
fn pump_for(
    conn: &Connection,
    queue: &mut EventQueue<App>,
    app: &mut App,
    done: impl Fn(&App) -> bool,
    limit: Duration,
) -> Result<bool, Box<dyn Error>> {
    let deadline = Instant::now() + limit;
    loop {
        queue.dispatch_pending(app)?;
        if done(app) {
            return Ok(true);
        }
        conn.flush()?;
        // No guard means events arrived while we were asking; go read them.
        let Some(guard) = conn.prepare_read() else {
            continue;
        };
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            return Ok(false);
        };
        let fd = guard.connection_fd();
        let mut fds = [PollFd::new(&fd, PollFlags::IN)];
        let timeout = Timespec {
            tv_sec: left.as_secs() as _,
            tv_nsec: left.subsec_nanos() as _,
        };
        match rustix::event::poll(&mut fds, Some(&timeout)) {
            Ok(0) => return Ok(false),
            // An interrupted poll has simply not waited its full time yet.
            Ok(_) | Err(rustix::io::Errno::INTR) => {}
            Err(e) => return Err(Box::new(e)),
        }
        guard.read()?;
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
