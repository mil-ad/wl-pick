//! The config file: `~/.config/wl-pick/config`.
//!
//! Flat `key = value` lines with `#` comments — no sections, no nesting, so a
//! TOML parser would be a dependency bought for nothing. Every setting is
//! optional; anything absent keeps its default, and a command-line flag beats
//! the file.
//!
//! Sizes take sway's syntax: `600px` is absolute, `70ppt` is 70 percent of the
//! display the grid appears on. That matters on a multi-monitor setup, where a
//! pixel size that suits one screen is wrong on the next — percentages are
//! resolved against whichever display the overlay actually maps on, each time
//! it runs.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::capture::Live;
use crate::target::Format;
use crate::theme::Argb;

/// A size, either absolute or relative to the display it will be shown on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Length {
    Px(i32),
    /// Percentage points, as sway spells it.
    Ppt(f32),
}

impl Length {
    /// `600px`, `70ppt`, `70%`, or a bare number meaning pixels.
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        let (number, unit) = match s.find(|c: char| c.is_alphabetic() || c == '%') {
            Some(i) => (&s[..i], s[i..].trim()),
            None => (s, ""),
        };
        let n: f32 = number
            .trim()
            .parse()
            .map_err(|_| format!("{s:?} is not a number followed by px or ppt"))?;
        match unit {
            "" | "px" => Ok(Length::Px(n.round() as i32)),
            "ppt" | "%" => Ok(Length::Ppt(n)),
            other => Err(format!("unknown unit {other:?}, expected px or ppt")),
        }
    }

    /// Turn into pixels. `basis` is the display's size along the same axis.
    pub fn resolve(self, basis: i32) -> i32 {
        match self {
            Length::Px(px) => px,
            Length::Ppt(pct) => (basis as f32 * pct / 100.0).round() as i32,
        }
    }
}

/// `#rrggbb` or `#aarrggbb`, to the premultiplied-alpha-free 0xAARRGGBB the
/// painter uses. Opaque when no alpha is given.
pub fn colour(s: &str) -> Result<Argb, String> {
    let hex = s.trim().strip_prefix('#').unwrap_or(s.trim());
    let value = u32::from_str_radix(hex, 16).map_err(|_| format!("{s:?} is not a colour"))?;
    match hex.len() {
        6 => Ok(0xff00_0000 | value),
        8 => Ok(value),
        _ => Err(format!("{s:?} should be #rrggbb or #aarrggbb")),
    }
}

fn boolean(s: &str) -> Result<bool, String> {
    match s.trim() {
        "true" | "yes" | "on" | "1" => Ok(true),
        "false" | "no" | "off" | "0" => Ok(false),
        other => Err(format!("{other:?} is not true or false")),
    }
}

/// Everything the file can set. `None` means "keep the default".
#[derive(Debug, Default)]
pub struct Config {
    pub background: Option<Argb>,
    pub foreground: Option<Argb>,
    pub selection: Option<Argb>,
    pub selection_text: Option<Argb>,
    pub border: Option<Argb>,
    pub border_width: Option<Length>,
    /// Largest a thumbnail may be. Height defaults to the display's aspect, so
    /// a tile is shaped like the windows it shows.
    pub tile_width: Option<Length>,
    pub tile_height: Option<Length>,
    pub max_columns: Option<i32>,
    pub font: Option<String>,
    pub font_size: Option<f32>,
    pub labels: Option<bool>,
    pub outputs: Option<bool>,
    pub live: Option<Live>,
    pub fps: Option<u32>,
    pub format: Option<Format>,
    pub timeout: Option<Duration>,
}

impl Config {
    /// Read `path`, or the default location. A missing file is not an error; a
    /// malformed one is, because silently ignoring a typo in a colour is worse
    /// than refusing to start.
    pub fn load(path: Option<&Path>) -> Result<Self, String> {
        let (path, required) = match path {
            Some(p) => (p.to_path_buf(), true),
            None => (default_path(), false),
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(_) if !required => Ok(Self::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    fn parse(text: &str) -> Result<Self, String> {
        let mut cfg = Self::default();
        for (n, line) in text.lines().enumerate() {
            let line = strip_comment(line).trim();
            if line.is_empty() {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                return Err(format!("line {}: expected key = value", n + 1));
            };
            cfg.set(key.trim(), value.trim())
                .map_err(|e| format!("line {}: {e}", n + 1))?;
        }
        Ok(cfg)
    }

    fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        match key {
            "background" => self.background = Some(colour(value)?),
            "foreground" => self.foreground = Some(colour(value)?),
            "selection" => self.selection = Some(colour(value)?),
            "selection-text" => self.selection_text = Some(colour(value)?),
            "border" => self.border = Some(colour(value)?),
            "border-width" => self.border_width = Some(Length::parse(value)?),
            "tile-width" => self.tile_width = Some(Length::parse(value)?),
            "tile-height" => self.tile_height = Some(Length::parse(value)?),
            "max-columns" => {
                self.max_columns = Some(number(value)?);
            }
            "font" => self.font = Some(value.to_string()),
            "font-size" => self.font_size = Some(number(value)?),
            "labels" => self.labels = Some(boolean(value)?),
            "outputs" => self.outputs = Some(boolean(value)?),
            "live" => self.live = Some(Live::parse(value)?),
            "fps" => self.fps = Some(number(value)?),
            "format" => self.format = Some(Format::parse(value)?),
            "timeout" => self.timeout = Some(Duration::from_secs_f64(number(value)?)),
            other => return Err(format!("unknown setting {other:?}")),
        }
        Ok(())
    }
}

/// Cut a trailing comment. `#` starts one only when followed by whitespace or
/// the end of the line, so `#d79921` stays a colour.
fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    for (i, _) in line.char_indices().filter(|(_, c)| *c == '#') {
        match bytes.get(i + 1) {
            None => return &line[..i],
            Some(c) if c.is_ascii_whitespace() => return &line[..i],
            _ => {}
        }
    }
    line
}

fn number<T: std::str::FromStr>(s: &str) -> Result<T, String> {
    s.trim()
        .parse()
        .map_err(|_| format!("{s:?} is not a number"))
}

/// `$XDG_CONFIG_HOME/wl-pick/config`, or `~/.config/wl-pick/config`.
fn default_path() -> PathBuf {
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_default();
    dir.join("wl-pick").join("config")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths_take_sways_units() {
        assert_eq!(Length::parse("600px"), Ok(Length::Px(600)));
        assert_eq!(Length::parse("600"), Ok(Length::Px(600)));
        assert_eq!(Length::parse(" 70 ppt "), Ok(Length::Ppt(70.0)));
        assert_eq!(Length::parse("70ppt"), Ok(Length::Ppt(70.0)));
        assert_eq!(Length::parse("12.5%"), Ok(Length::Ppt(12.5)));
        assert!(Length::parse("wide").is_err());
        assert!(Length::parse("70em").is_err());
    }

    #[test]
    fn percentages_resolve_against_the_display() {
        // The same config gives different pixels on different monitors, which is
        // the whole point of allowing ppt.
        assert_eq!(Length::Ppt(20.0).resolve(1280), 256);
        assert_eq!(Length::Ppt(20.0).resolve(3840), 768);
        assert_eq!(Length::Px(220).resolve(3840), 220);
    }

    #[test]
    fn colours_take_both_lengths() {
        assert_eq!(colour("#282828"), Ok(0xff282828));
        assert_eq!(colour("#80ffffff"), Ok(0x80ffffff));
        assert_eq!(colour("282828"), Ok(0xff282828));
        assert!(colour("#zzz").is_err());
        assert!(colour("#fff").is_err());
    }

    #[test]
    fn a_whole_file_parses() {
        let cfg = Config::parse(
            "\
# looks
background = #282828
selection  = #d79921   # trailing comment
border-width = 2px

tile-width = 18ppt
max-columns = 4

live = current
fps = 30
labels = no
",
        )
        .expect("should parse");
        assert_eq!(cfg.background, Some(0xff282828));
        assert_eq!(cfg.selection, Some(0xffd79921));
        assert_eq!(cfg.border_width, Some(Length::Px(2)));
        assert_eq!(cfg.tile_width, Some(Length::Ppt(18.0)));
        assert_eq!(cfg.max_columns, Some(4));
        assert_eq!(cfg.fps, Some(30));
        assert_eq!(cfg.labels, Some(false));
        assert!(cfg.live.is_some());
        // Untouched settings stay unset, so defaults survive.
        assert_eq!(cfg.foreground, None);
        assert_eq!(cfg.tile_height, None);
    }

    #[test]
    fn comments_do_not_eat_colours() {
        assert_eq!(
            strip_comment("selection = #d79921   # note"),
            "selection = #d79921   "
        );
        assert_eq!(strip_comment("# whole line"), "");
        assert_eq!(strip_comment("border = #fff000"), "border = #fff000");
        assert_eq!(strip_comment("fps = 30 #"), "fps = 30 ");
        let cfg = Config::parse("selection = #d79921 # trailing\n").expect("parses");
        assert_eq!(cfg.selection, Some(0xffd79921));
    }

    #[test]
    fn mistakes_say_which_line() {
        let err = Config::parse("background = #282828\nselection = nope\n").unwrap_err();
        assert!(err.starts_with("line 2:"), "{err}");
        let err = Config::parse("border-width\n").unwrap_err();
        assert!(err.starts_with("line 1:"), "{err}");
        let err = Config::parse("colour = #fff000\n").unwrap_err();
        assert!(err.contains("unknown setting"), "{err}");
    }

    #[test]
    fn a_missing_file_is_not_an_error() {
        let missing = Path::new("/nonexistent/wl-pick/config");
        assert!(
            Config::load(Some(missing)).is_err(),
            "named file must exist"
        );
        // The default location is allowed to be absent.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", "/nonexistent") };
        assert!(Config::load(None).is_ok());
    }
}
