# wl-pick

A window switcher for wlroots compositors: a grid of **live** window previews,
styled like a rofi theme, that prints which one you picked. It doubles as a
screencast source picker for the desktop portal.

No thumbnails are ever made. Each window is captured straight into a buffer
handed to its own subsurface and the compositor does the scaling, so wl-pick
appears in about 60 ms and sits around 6 MB resident however many windows are
open.

## Install

```sh
cargo install wl-pick
```

## Usage

wl-pick is a chooser: the pick goes to stdout, nothing does if you cancel, and
it never acts on the choice itself. Exit status is 0 for a pick, 1 for anything
else, and 2 if `--timeout` fires.

```sh
#!/usr/bin/env bash
# save as a script and bind it to $mod+Tab
IFS=$'\t' read -r type id toplevel app title < <(wl-pick) || exit 0
case $type in
    window) swaymsg "[con_id=$id] focus" ;;
    output) swaymsg "focus output $id" ;;
esac
```

### Options

| flag | |
|---|---|
| `--format tsv\|json\|portal` | how to report the pick (default `tsv`) |
| `--live all\|current\|none` | which tiles keep updating (default `all`; `current` is much cheaper) |
| `--fps N` | live updates per tile per second (default 12) |
| `--outputs` / `--no-outputs` | whether whole displays are tiles too (default on) |
| `--labels` / `--no-labels` | whether a label is drawn under each thumbnail (default on) |
| `--font NAME` | label font: a family, optionally with a style |
| `--font-size PX` | label size in logical px |
| `--config PATH` | config file (default `~/.config/wl-pick/config`) |
| `--timeout SECS` | exit after a deadline, whatever has happened |
| `--verbose` | phase timings, the tile list, and capture stats |

Both directions of each boolean exist so either can override the config file.

### Output formats

Different consumers need different identifiers, so there are three:

| `--format` | output |
|---|---|
| `tsv` (default) | `TYPE⇥ID⇥TOPLEVEL_ID⇥APP⇥TITLE` — `ID` is the sway `con_id`, or the output name for a display |
| `json` | the same fields, for `jq` |
| `portal` | `Monitor: NAME` or `Window: TOPLEVEL_ID` |

`TOPLEVEL_ID` is the ext-foreign-toplevel-list-v1 identifier, which is what
`grim -T` and the desktop portal capture by. `portal` is exactly the format
xdg-desktop-portal-wlr's `simple` chooser reads, so wl-pick can be the picker
for `getDisplayMedia` and friends:

```ini
[screencast]
chooser_type=simple
chooser_cmd=wl-pick --format portal
```

### Keys

| key | |
|---|---|
| `→` `←` / `l` `h` / `Tab` `Shift+Tab` | next / previous tile |
| `↓` `↑` / `j` `k` | move a row |
| `Home` `End` / `PgUp` `PgDn` | first / last, or a screen at a time |
| `Enter` | pick the selection |
| `Escape` / `q` | cancel |
| click | pick that tile |
| scroll | next / previous tile |

Hovering deliberately does not move the selection: the keyboard keeps it, and a
click acts on whatever is under the cursor.

## Config

`~/.config/wl-pick/config`, or `--config PATH`. Flat `key = value` lines with
`#` comments, everything optional, and a flag always beats the file.

```ini
background     = #282828      # the grid's backdrop
foreground     = #ebdbb2      # label text
selection      = #d79921      # the highlighted tile
selection-text = #282828      # its label
border         = #d79921
border-width   = 2px

max-width      = 90ppt        # the box the grid may fill
max-height     = 90ppt
max-columns    = 4            # thumbnails are that box divided by these
max-rows       = 4

font           = monospace
font-size      = 13.3
labels         = yes
outputs        = yes
live           = all
fps            = 12
format         = tsv
timeout        = 0            # seconds; 0 means none
```

Sizes take sway's units: `600px` is absolute, `90ppt` a percentage of the
display the grid appears on, worked out afresh each run — so one file suits a
1280-wide laptop panel and a 3840-wide monitor alike.

The four `max-` settings are caps: the first two bound the overlay, the last two
bound the grid inside it, and a thumbnail is simply the one divided by the
other. A thumbnail is therefore the same size whether one window is open or
thirty — the overlay just hugs whatever is there. Rows past `max-rows` scroll.

The default look is the rofi theme this replaces: gruvbox dark, `ceil(sqrt(n))`
columns capped at 4, `title · app` centred under each thumbnail. Labels default
to the system monospace font; `--font` and the `font` setting take either a
family such as `Iosevka`, or a full name with a style such as `Iosevka Bold`.
A name matching nothing says so on stderr rather than quietly using something
else.

## Requirements

A wlroots compositor advertising `ext-image-copy-capture-v1`,
`ext-image-capture-source-v1`, `ext-foreign-toplevel-list-v1`,
`wlr-layer-shell-unstable-v1` and `wp_viewporter`. In practice that means
sway 1.11+, since sway's IPC socket is also the source of truth for the window
list; Hyprland, labwc and jay advertise the protocols but are untested.

Known upstream issue: holding per-toplevel capture sessions open makes windows
blurry on **fractionally scaled** outputs
([sway#9113](https://github.com/swaywm/sway/issues/9113)). Integer scales are
unaffected, and `--live none` avoids it.

## Notes

- Starting a second wl-pick replaces the first: the new overlay takes the
  keyboard and the old one exits without printing anything.
- Navigation reads raw evdev keycodes, so it is layout-independent — but
  virtual-keyboard clients such as `wtype` cannot drive it.
- The sway socket comes from `SWAYSOCK`/`I3SOCK` when those point at something
  real, and from the running sway otherwise, since inheriting a stale path is
  easy.

## Roadmap

- type-to-filter with fzf-quality fuzzy matching, and the xkb keyboard input it
  needs
- dmabuf capture, so the pixels never leave the GPU and live previews stop
  costing a readback per frame

## Building

```sh
cargo build --release
cargo test
```
