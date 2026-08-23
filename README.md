# wl-pick

A window switcher for wlroots compositors: a grid overlay of **live** window
previews that looks like a rofi theme, and tells you which one you picked.

It replaces a `wlthumbs | rofi` pipeline, and doubles as a screencast source
picker for the desktop portal. The difference is that no thumbnails
exist: each window is captured straight into a `wl_shm` buffer that is handed to
its own `wl_subsurface`, and `wp_viewporter` tells the compositor which rectangle
to scale it into. There is no image encoding, no scaler, and no full-resolution
bitmap in this process — which is also why it appears in about 60 ms and holds
~18 MB of RSS however many windows are open.

```
sway-tree       0.6ms     window list + con_ids over sway IPC
toplevels       0.2ms     ext-foreign-toplevel-list handles
constraints     1.5ms     every capture session's buffer size, in one roundtrip
capture        52.5ms     8 windows, all frames in flight at once
labels          0.0ms     shaped on a worker thread while the captures ran
mapped          4.9ms     layer surface + subsurfaces on screen
```

The capture phase is the compositor reading full-resolution window pixels out of
the GPU. It is bandwidth-bound (~1.1 GB/s here) and unaffected by how large the
thumbnails are — which also makes it a free window to do other work in. Loading
a font and rasterising its first glyphs costs ~20ms, so labels are shaped on a
worker thread started before the captures and joined after them, and cost
nothing in wall clock.

## Status

Working: a labelled grid of live previews with keyboard navigation. Type-to-filter
is the one thing the rofi version had that this doesn't — see the roadmap.

## Usage

```
wl-pick [--format tsv|json|portal] [--live all|current|none] [--fps N]
        [--no-outputs] [--hide-labels] [--font FAMILY] [--font-size PX]
        [--timeout SECS] [--verbose]
```

- `--format tsv|json|portal` how to report the pick (default `tsv`)
- `--live all|current|none` which tiles keep updating (default `all`; displays
  are always a single snapshot)
- `--fps N` cap on live updates per tile per second (default 12)
- `--no-outputs` windows only; displays are included as tiles by default
- `--hide-labels` draws an icon-only grid
- `--font FAMILY` label font family (default `Berkeley Mono`)
- `--font-size PX` label size in logical px
- `--timeout SECS` exits after a deadline, in case the keyboard grab ever traps
  you
- `--verbose` phase timings, the tile list, and capture stats

wl-pick is a chooser: the pick goes to stdout, nothing does if you cancel, and
the exit status is 0 for a pick and 1 for a cancel. It never acts on the choice —
it has no idea what you want to do with it. Focusing on sway looks like this:

```sh
#!/usr/bin/env bash
# ~/.local/bin/winmenu, bound to $mod+Tab
IFS=$'\t' read -r type id toplevel app title < <(wl-pick) || exit 0
case $type in
    window) swaymsg "[con_id=$id] focus" ;;
    output) swaymsg "focus output $id" ;;
esac
```

Windows only, as a one-liner:

```sh
swaymsg "[con_id=$(wl-pick --no-outputs | cut -f2)] focus"
```

Three formats, because the identifiers different consumers need differ:

| `--format` | output |
|---|---|
| `tsv` (default) | `TYPE⇥ID⇥TOPLEVEL_ID⇥APP⇥TITLE` — `ID` is the sway `con_id`, or the output name for a display; `TOPLEVEL_ID` is the ext-foreign-toplevel-list-v1 identifier that `grim -T` and the portal capture by |
| `json` | the same record with every key always present, for `jq` |
| `portal` | `Monitor: NAME` or `Window: TOPLEVEL_ID` |

`portal` is exactly what xdg-desktop-portal-wlr's `simple` chooser reads, so
wl-pick can be the picker for `getDisplayMedia` and friends — with live previews
of both windows and displays:

```ini
[screencast]
chooser_type=simple
chooser_cmd=wl-pick --format portal
```

| key | |
|---|---|
| `→` `←` / `l` `h` / `Tab` `Shift+Tab` | next / previous tile |
| `↓` `↑` / `j` `k` | move a row |
| `Home` `End` | first / last |
| `Enter` | pick the selection |
| `Escape` / `q` | cancel |
| click | pick that tile |
| scroll | next / previous tile |

Hovering deliberately does not move the selection — the keyboard keeps it, and a
click acts on whatever is under the cursor. Clicking the margin, a gap, or an
empty cell of a ragged last row does nothing. Tiles are subsurfaces, so a click
on a thumbnail identifies its tile by surface; only clicks on the chrome around
them need hit-testing.

Navigation reads raw evdev keycodes, so it is layout-independent — but it also
means virtual-keyboard clients such as `wtype` (which invent their own keymap)
cannot drive it. That goes away with xkb support, which filtering needs anyway.

## Live previews

Capture sessions stay open, so a tile can be refreshed. Three things keep that
from being expensive:

- **It is damage-driven.** After a session's first frame the compositor only
  produces another once the window content changes, so a request left
  outstanding on an idle window costs nothing. Measured over 4s with one
  animating window out of ten: `52,52,1,1,1,1,1,1,13,1` frames — the static
  windows delivered exactly their first frame and nothing more.
- **Frame callbacks are the clock.** Re-captures are driven by the overlay's own
  `wl_surface.frame` callbacks, so they stop when it isn't being presented, and
  `--fps` throttles per tile on top of that (12 fps measured as 12.1).
- **Two buffers per window, alternating.** A capture must not write into a buffer
  the compositor is reading, so each window gets two and `wl_buffer.release`
  decides which is free. Note that release is the entire contract: with `wl_shm`
  the compositor copies the pixels out at commit and hands the buffer straight
  back, so the slot on screen is usually free too — waiting for it to stop being
  displayed instead deadlocks after two frames.

The cost is memory and bandwidth: two full-resolution buffers per window (138 MB
of shm for eight windows and a display here, against 83 MB with `--live none`)
and a readback per refreshed frame. `--live current` refreshes only the selected tile,
which is much cheaper and still reads as alive.

## Look

Colours, font metrics and grid geometry come from the rofi theme this replaces
(gruvbox dark, a yellow selection filling the element padding, `ceil(sqrt(n))`
columns capped at 4, 16:9 tiles, `title · app` centred underneath) and live in
`src/theme.rs`, which is the one place to change them. They are not
configurable at runtime beyond the font flags.

The font is looked up by family name. Your own font directories are scanned
first because they are small; the full system scan (~37ms) happens only if the
family isn't found there, and an unknown family then falls back to whatever
cosmic-text picks rather than failing. Long titles are ellipsised to the cell.

## Requirements

A wlroots compositor advertising `ext-image-copy-capture-v1`,
`ext-image-capture-source-v1` (with the foreign-toplevel source manager),
`ext-foreign-toplevel-list-v1`, `wlr-layer-shell-unstable-v1` and
`wp_viewporter` — sway 1.11+, and in principle Hyprland, labwc and jay, though
only sway is tested. sway is also the source of truth for the window list, over
its IPC socket, which is the one thing that would need replacing to run
elsewhere (`ext-foreign-toplevel-list-v1` already reports app id and title).

Known upstream issue: holding per-toplevel capture sessions open makes windows
blurry on **fractionally scaled** outputs
([sway#9113](https://github.com/swaywm/sway/issues/9113)). Integer scales are
unaffected. It matters more once previews are live.

## Roadmap

- type-to-filter with fzf-quality fuzzy matching (and the xkb keyboard input it
  needs, which would also let virtual-keyboard clients drive the overlay)
- dmabuf capture, so the pixels never leave the GPU at all — and live previews
  stop costing a readback per frame

## Source layout

```
main.rs     orchestration: list, capture, map, report the pick
cli.rs      flags, defaults, and the help text that documents them
sway.rs     the window list and display names, over sway's IPC socket
target.rs   what a tile stands for, and the three output formats
app.rs      the Wayland client state every event dispatches into
capture.rs  capture sessions, their buffers, and the live clock
overlay.rs  the layer surface, the drawing, and the keyboard
theme.rs    colours, grid geometry, aspect fitting
text.rs     label shaping on a worker thread
shm.rs      memfd allocation and the ARGB painter
```

## Building

```
cargo build --release
cargo test          # grid geometry and hit-testing, ellipsising, output
                    # formats, glyph output
```
