//! Command line: flags, defaults, and the help text that documents them.

use std::time::Duration;

use crate::app::Settings;
use crate::capture::Live;
use crate::target::Format;
use crate::theme::Theme;

const HELP: &str = "\
wl-pick — a live grid of window and display previews, for picking one

usage: wl-pick [options]

  --format tsv|json|portal  how to report the pick [tsv]
  --live all|current|none   which tiles keep updating live [all]
                            (displays are always a single snapshot)
  --fps N                   cap on live updates per tile per second [12]
  --no-outputs              windows only; displays are included by default
  --hide-labels             draw an icon-only grid
  --font FAMILY             label font family [the system monospace font]
  --font-size PX            label size in logical px [13.3]
  --timeout SECS            exit anyway after SECS, in case the keyboard
                            grab ever traps you [off]
  -v, --verbose             phase timings, tile list and capture stats
  -h, --help                this

keys:  arrows, hjkl or Tab/Shift+Tab move; Home/End jump; Enter picks;
       Escape or q cancels
mouse: click a tile to pick it, scroll to move. Hovering does not move the
       selection, and a click outside a tile does nothing.

The pick goes to stdout and nothing does if you cancel, so exit status is
0 for a pick and 1 for a cancel. Acting on it is the caller's job.

formats:

  tsv     TYPE<TAB>ID<TAB>TOPLEVEL_ID<TAB>APP<TAB>TITLE, e.g.

            window	1234	f0e1d2c3b4a59687	firefox	Wikipedia
            output	HDMI-A-1		display	HDMI-A-1

          ID is the thing to act on: a sway con_id, or the display name.
          TOPLEVEL_ID is the ext-foreign-toplevel-list-v1 identifier that
          capture tools address a window by (grim -T, the desktop portal),
          empty for a display.

  json    the same fields as one object, every key always present, for jq

  portal  \"Window: TOPLEVEL_ID\" or \"Monitor: NAME\", what
          xdg-desktop-portal-wlr's simple chooser reads:

            [screencast]
            chooser_type=simple
            chooser_cmd=wl-pick --format portal

focusing on sway:

  IFS=$'\\t' read -r type id toplevel app title < <(wl-pick) &&
    case $type in
      window) swaymsg \"[con_id=$id] focus\" ;;
      output) swaymsg \"focus output $id\" ;;
    esac
";

pub struct Args {
    pub(crate) format: Format,
    pub(crate) outputs: bool,
    pub(crate) verbose: bool,
    pub(crate) hide_labels: bool,
    pub(crate) font: Option<String>,
    pub(crate) font_size: Option<f32>,
    pub(crate) live: Live,
    pub(crate) fps: u32,
    pub(crate) timeout: Option<Duration>,
}

impl Args {
    /// An exclusive keyboard grab makes a hung overlay unusable, so keep an
    /// escape hatch that cannot itself deadlock: a thread that only exits.
    pub fn arm_timeout(&self) {
        if let Some(d) = self.timeout {
            std::thread::spawn(move || {
                std::thread::sleep(d);
                eprintln!("wl-pick: timeout");
                std::process::exit(2);
            });
        }
    }

    /// Everything the overlay needs to know up front. `scale` comes from the
    /// compositor, not the command line, so it is passed in.
    pub fn settings(&self, scale: i32) -> Settings {
        Settings {
            theme: self.theme(),
            live: self.live,
            fps: self.fps,
            scale,
        }
    }

    /// The look, with any overrides applied. Line height follows the font size
    /// unless the size came from the theme, where it is already tuned.
    fn theme(&self) -> Theme {
        let base = Theme::default();
        let font_px = self.font_size.unwrap_or(base.font_px);
        Theme {
            labels: !self.hide_labels,
            font: self.font.clone().unwrap_or_else(|| base.font.clone()),
            line_h: match self.font_size {
                Some(_) => (font_px * 1.3).ceil() as i32,
                None => base.line_h,
            },
            font_px,
            ..base
        }
    }
}

pub fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        format: Format::Tsv,
        outputs: true,
        verbose: false,
        hide_labels: false,
        font: None,
        font_size: None,
        live: Live::All,
        fps: 12,
        timeout: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--format" => {
                args.format = match it.next().ok_or("--format needs tsv|json|portal")?.as_str() {
                    "tsv" => Format::Tsv,
                    "json" => Format::Json,
                    "portal" => Format::Portal,
                    other => return Err(format!("bad --format: {other}")),
                }
            }
            "--outputs" => args.outputs = true,
            "--no-outputs" => args.outputs = false,
            "-v" | "--verbose" => args.verbose = true,
            "--hide-labels" => args.hide_labels = true,
            "--live" => {
                args.live = match it.next().ok_or("--live needs all|current|none")?.as_str() {
                    "all" => Live::All,
                    "current" => Live::Current,
                    "none" => Live::None,
                    other => return Err(format!("bad --live: {other}")),
                }
            }
            "--fps" => {
                let v = it.next().ok_or("--fps needs a number")?;
                args.fps = v.parse().map_err(|_| format!("bad --fps: {v}"))?;
            }
            "--font" => args.font = Some(it.next().ok_or("--font needs a family name")?),
            "--font-size" => {
                let v = it.next().ok_or("--font-size needs px")?;
                args.font_size = Some(v.parse().map_err(|_| format!("bad --font-size: {v}"))?);
            }
            "--timeout" => {
                let v = it.next().ok_or("--timeout needs seconds")?;
                let secs: f64 = v.parse().map_err(|_| format!("bad --timeout: {v}"))?;
                args.timeout = Some(Duration::from_secs_f64(secs));
            }
            "-h" | "--help" => {
                print!("{HELP}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(args)
}
