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

mod app;
mod capture;
mod cli;
mod overlay;
mod shm;
mod sway;
mod target;
mod text;
mod theme;

use std::error::Error;
use std::process::ExitCode;
use std::time::Instant;

use wayland_client::globals::registry_queue_init;
use wayland_client::{Connection, EventQueue};

use app::App;
use cli::Args;
use target::Target;
use theme::Layout;

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
    let args = cli::parse_args().map_err(|e| -> Box<dyn Error> { e.into() })?;
    args.arm_timeout();

    let start = Instant::now();
    let mut phases = Phases::new(args.verbose);
    let (targets, scale) = list(&args)?;
    if targets.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }
    phases.mark("sway-tree");

    let settings = args.settings(scale);
    let theme = &settings.theme;
    // Start shaping labels now: it costs ~55ms of font loading and glyph
    // rasterising, and the captures below are ~55ms of waiting on the
    // compositor, so the two overlap almost exactly.
    let labels = theme.labels.then(|| {
        let layout = Layout::new(theme, targets.len() as i32);
        text::spawn(
            targets.iter().map(Target::label).collect(),
            theme.font.clone(),
            theme.font_px * scale as f32,
            (theme.line_h * scale) as f32,
            (layout.label(0).map(|r| r.w).unwrap_or(theme.tile_w) * scale) as f32,
        )
    });

    let conn = Connection::connect_to_env()?;
    let (globals, mut queue) = registry_queue_init::<App>(&conn)?;
    let qh = queue.handle();
    let mut app = App::new(&globals, &qh, targets, settings)?;

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

    if let Some(job) = labels {
        app.labels = Some(job.join().map_err(|_| "label thread panicked")?);
    }
    phases.mark("labels");
    if args.verbose {
        app.describe();
    }

    app.show(&qh)?;
    pump(&mut queue, &mut app, |a| a.configured)?;
    app.paint();
    app.place_tiles(&qh);
    app.arm_frame_callback(&qh);
    conn.flush()?;
    phases.mark("mapped");

    pump(&mut queue, &mut app, |a| a.finished())?;
    if args.verbose {
        app.report(start.elapsed());
    }

    let Some(target) = app.picked() else {
        return Ok(ExitCode::FAILURE); // cancelled: nothing on stdout
    };
    match target.render(args.format) {
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

/// Everything the grid can show, windows first so their positions stay stable as
/// displays come and go. The IPC connection is only needed for this, and is
/// closed again before the overlay maps.
fn list(args: &Args) -> Result<(Vec<Target>, i32), Box<dyn Error>> {
    let mut sway = swayipc::Connection::new()?;
    let mut targets = sway::windows(&mut sway)?;
    let displays = sway::displays(&mut sway)?;
    if args.outputs {
        targets.extend(displays.names.iter().cloned().map(Target::output));
    }
    Ok((targets, displays.scale))
}

/// Run the event loop until `done`.
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
