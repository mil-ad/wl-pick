//! Command line: flags, defaults, and the help text that documents them.

use std::path::PathBuf;
use std::time::Duration;

use crate::app::Settings;
use crate::capture::Live;
use crate::config::{Config, Length};
use crate::sway::Display;
use crate::target::Format;
use crate::theme::Theme;

const HELP: &str = "\
wl-pick — a live grid of window and display previews, for picking one

usage: wl-pick [options]

  --config PATH             config file [~/.config/wl-pick/config]
  --format tsv|json|portal  how to report the pick [tsv]
  --live all|current|none   which tiles keep updating live [all]
                            (displays are always a single snapshot)
  --fps N                   cap on live updates per tile per second [12]
  --outputs, --no-outputs   include whole displays as tiles [yes]
  --labels, --no-labels     a label under each thumbnail [yes]
  --font NAME               label font: a family, optionally followed by a
                            style, as in 'Iosevka Medium Condensed'
                            [the system monospace font]
  --font-size PX            label size in logical px [13.3]
  --timeout SECS            exit anyway after SECS, in case the keyboard
                            grab ever traps you [off]
  -v, --verbose             phase timings, tile list and capture stats
  -h, --help                this

keys:  arrows, hjkl or Tab/Shift+Tab move; PgUp/PgDn and Home/End jump;
       Enter picks; Escape or q cancels
mouse: click a tile to pick it, scroll to move. Hovering does not move the
       selection, and a click outside a tile does nothing.

The pick goes to stdout and nothing does if you cancel, so exit status is
0 for a pick and 1 for a cancel. Acting on it is the caller's job.

config:

  Flat `key = value` lines, `#` comments, everything optional; a flag beats
  the file. Sizes take sway's units — `600px` is absolute, `70ppt` is a
  percentage of the display the grid appears on, so one file suits monitors
  of different sizes.

    background     = #282828      # the grid's backdrop
    foreground     = #ebdbb2      # label text
    selection      = #d79921      # the highlighted tile
    selection-text = #282828      # its label
    border         = #d79921
    border-width   = 2px

    max-width      = 90ppt        # the box the grid may fill
    max-height     = 90ppt
    max-columns    = 4            # thumbnails are the box divided by these,
    max-rows       = 4            # so their size never depends on how many
                                  # windows are open; further rows scroll

    font           = monospace    # a family, optionally with a style
    font-size      = 13.3
    labels         = yes
    outputs        = yes          # include whole displays as tiles
    live           = all
    fps            = 12
    format         = tsv
    timeout        = 0            # seconds; 0 means none

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

/// What the command line asked for. Every setting is optional so the config file
/// can fill the gaps: a flag beats the file, the file beats the default.
#[derive(Default)]
pub struct Args {
    pub(crate) config: Option<PathBuf>,
    pub(crate) verbose: bool,
    pub(crate) format: Option<Format>,
    pub(crate) outputs: Option<bool>,
    pub(crate) labels: Option<bool>,
    pub(crate) font: Option<String>,
    pub(crate) font_size: Option<f32>,
    pub(crate) live: Option<Live>,
    pub(crate) fps: Option<u32>,
    pub(crate) timeout: Option<Duration>,
}

/// Every setting resolved, with sizes turned into pixels for the display the
/// overlay is about to appear on.
pub struct Options {
    pub verbose: bool,
    pub format: Format,
    pub outputs: bool,
    pub timeout: Option<Duration>,
    /// The logical size of the display the grid will be laid out for.
    pub display: (i32, i32),
    pub settings: Settings,
}

/// An exclusive keyboard grab makes a hung overlay unusable, so keep an escape
/// hatch that cannot itself deadlock: a thread that only exits.
pub fn arm_timeout(timeout: Option<Duration>) {
    if let Some(d) = timeout {
        std::thread::spawn(move || {
            std::thread::sleep(d);
            eprintln!("wl-pick: timeout");
            std::process::exit(2);
        });
    }
}

impl Args {
    /// Resolve against the file and the display. Percentages become pixels here,
    /// against this display, which is what lets one config suit monitors of
    /// different sizes.
    pub fn resolve(&self, cfg: &Config, display: &Display) -> Options {
        let base = Theme::default();
        let font_px = self.font_size.or(cfg.font_size).unwrap_or(base.font_px);
        // The box the grid may fill. Left alone it is most of the display, and
        // being a percentage it travels between monitors.
        let max_w = cfg
            .max_width
            .unwrap_or(Length::Ppt(90.0))
            .resolve(display.width);
        let max_h = cfg
            .max_height
            .unwrap_or(Length::Ppt(90.0))
            .resolve(display.height);
        let theme = Theme {
            bg: cfg.background.unwrap_or(base.bg),
            fg: cfg.foreground.unwrap_or(base.fg),
            sel_bg: cfg.selection.unwrap_or(base.sel_bg),
            sel_fg: cfg.selection_text.unwrap_or(base.sel_fg),
            border: cfg.border.unwrap_or(base.border),
            border_px: cfg
                .border_width
                .map_or(base.border_px, |l| l.resolve(display.width)),
            max_w: max_w.max(1),
            max_h: max_h.max(1),
            max_cols: cfg.max_columns.unwrap_or(base.max_cols).max(1),
            max_rows: cfg.max_rows.unwrap_or(base.max_rows).max(1),
            labels: self.labels.or(cfg.labels).unwrap_or(base.labels),
            font: self
                .font
                .clone()
                .or_else(|| cfg.font.clone())
                .unwrap_or_else(|| base.font.clone()),
            // Line height follows an explicit size; the default is already tuned.
            line_h: match self.font_size.or(cfg.font_size) {
                Some(_) => (font_px * 1.3).ceil() as i32,
                None => base.line_h,
            },
            font_px,
            ..base
        };
        Options {
            verbose: self.verbose,
            format: self.format.or(cfg.format).unwrap_or(Format::Tsv),
            outputs: self.outputs.or(cfg.outputs).unwrap_or(true),
            timeout: self.timeout.or(cfg.timeout),
            display: (display.width, display.height),
            settings: Settings {
                theme,
                live: self.live.or(cfg.live).unwrap_or(Live::All),
                fps: self.fps.or(cfg.fps).unwrap_or(12),
                scale: display.scale,
                output: display.name.clone(),
            },
        }
    }
}

pub fn parse_args() -> Result<Args, String> {
    parse(std::env::args().skip(1))
}

fn parse(it: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut args = Args::default();
    let mut it = it;
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--format" => {
                let v = it.next().ok_or("--format needs tsv|json|portal")?;
                args.format = Some(Format::parse(&v)?);
            }
            "--outputs" => args.outputs = Some(true),
            "--no-outputs" => args.outputs = Some(false),
            "--config" => {
                args.config = Some(PathBuf::from(it.next().ok_or("--config needs a path")?))
            }
            "-v" | "--verbose" => args.verbose = true,
            "--labels" => args.labels = Some(true),
            // --hide-labels was the only spelling before --labels existed, and
            // is still accepted for whatever it is wired into.
            "--no-labels" | "--hide-labels" => args.labels = Some(false),
            "--live" => {
                let v = it.next().ok_or("--live needs all|current|none")?;
                args.live = Some(Live::parse(&v)?);
            }
            "--fps" => {
                let v = it.next().ok_or("--fps needs a number")?;
                args.fps = Some(v.parse().map_err(|_| format!("bad --fps: {v}"))?);
            }
            "--font" => args.font = Some(it.next().ok_or("--font needs a font name")?),
            "--font-size" => {
                let v = it.next().ok_or("--font-size needs px")?;
                args.font_size = Some(v.parse().map_err(|_| format!("bad --font-size: {v}"))?);
            }
            "--timeout" => {
                let v = it.next().ok_or("--timeout needs seconds")?;
                let secs: f64 = v.parse().map_err(|_| format!("bad --timeout: {v}"))?;
                args.timeout = (secs > 0.0).then(|| Duration::from_secs_f64(secs));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn args(flags: &[&str]) -> Args {
        parse(flags.iter().map(|s| s.to_string())).expect("should parse")
    }

    /// A config file that sets both booleans the same way.
    fn file(on: bool) -> Config {
        Config {
            outputs: Some(on),
            labels: Some(on),
            ..Config::default()
        }
    }

    fn display() -> Display {
        Display {
            name: "DP-1".into(),
            width: 2560,
            height: 1440,
            scale: 2,
            focused: true,
        }
    }

    #[test]
    fn every_boolean_can_be_set_both_ways() {
        assert_eq!(args(&["--outputs"]).outputs, Some(true));
        assert_eq!(args(&["--no-outputs"]).outputs, Some(false));
        assert_eq!(args(&["--labels"]).labels, Some(true));
        assert_eq!(args(&["--no-labels"]).labels, Some(false));
        assert_eq!(args(&["--hide-labels"]).labels, Some(false), "old spelling");
        // Unset is what lets the file have its say.
        assert_eq!(args(&[]).outputs, None);
        assert_eq!(args(&[]).labels, None);
    }

    #[test]
    fn a_flag_beats_the_file_in_both_directions() {
        // Turning something back on is the case that used to be unsayable:
        // there was a --no-outputs but no --outputs, so a config saying no
        // could not be overridden from the command line at all.
        let on = args(&["--outputs", "--labels"]).resolve(&file(false), &display());
        assert!(on.outputs);
        assert!(on.settings.theme.labels);

        let off = args(&["--no-outputs", "--no-labels"]).resolve(&file(true), &display());
        assert!(!off.outputs);
        assert!(!off.settings.theme.labels);

        // With no flag the file decides, and with no file either, the default.
        assert!(!args(&[]).resolve(&file(false), &display()).outputs);
        assert!(args(&[]).resolve(&Config::default(), &display()).outputs);
    }
}
