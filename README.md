# wl-pick

A window switcher for wlroots compositors: a grid overlay of **live** window
previews that looks like a rofi theme, and tells you which one you picked. It
doubles as a screencast source picker for the desktop portal.

No thumbnails exist. Each window is captured straight into a `wl_shm` buffer
handed to its own `wl_subsurface`, and `wp_viewporter` tells the compositor which
rectangle to scale it into — no image encoding, no scaler, no full-resolution
bitmap in this process. Hence ~60 ms to appear and ~6 MB resident (peaking near
18 MB) however many windows are open.

```
sway-tree       0.6ms     window list + con_ids over sway IPC
toplevels       0.2ms     ext-foreign-toplevel-list handles
constraints     1.5ms     every capture session's buffer size, in one roundtrip
capture        52.5ms     8 windows, all frames in flight at once
labels          0.0ms     shaped on a worker thread while the captures ran
mapped          4.9ms     layer surface + subsurfaces on screen
```

Capture is the compositor reading full-resolution pixels out of the GPU: it is
bandwidth-bound (~1.1 GB/s here), unaffected by how large the thumbnails are,
and therefore a free window to work in — the ~20 ms of font loading and glyph
rasterising happens on a worker thread inside it.

Keyboard navigation, mouse and live previews all work. Type-to-filter is the one
thing the rofi version had that this doesn't — see the roadmap.

## Usage

wl-pick is a chooser: the pick goes to stdout, nothing does if you cancel, and
the exit status is 0 for a pick and 1 for anything else (2 if `--timeout`
fires). It never acts on the choice — it has no idea what you want to do with
it. Focusing on sway looks like this:

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

### Options

| flag | |
|---|---|
| `--format tsv\|json\|portal` | how to report the pick (default `tsv`) |
| `--live all\|current\|none` | which tiles keep updating (default `all`; displays are always a single snapshot) |
| `--fps N` | cap on live updates per tile per second (default 12) |
| `--outputs` / `--no-outputs` | whether whole displays are tiles too (default on) |
| `--labels` / `--no-labels` | whether a label is drawn under each thumbnail (default on) |
| `--font NAME` | label font: a family, optionally with a style, as in `"Iosevka Medium Condensed"` |
| `--font-size PX` | label size in logical px |
| `--config PATH` | config file (default `~/.config/wl-pick/config`) |
| `--timeout SECS` | exit after a deadline, in case the keyboard grab ever traps you |
| `--verbose` | phase timings, the tile list, and capture stats |

Both directions of each boolean exist so either can override the config file.
`--hide-labels` is the old spelling of `--no-labels`.

### Output formats

The identifiers different consumers need differ, so there are three:

| `--format` | output |
|---|---|
| `tsv` (default) | `TYPE⇥ID⇥TOPLEVEL_ID⇥APP⇥TITLE` — `ID` is the sway `con_id`, or the output name for a display; `TOPLEVEL_ID` is the ext-foreign-toplevel-list-v1 identifier that `grim -T` and the portal capture by |
| `json` | the same record with every key always present, for `jq` |
| `portal` | `Monitor: NAME` or `Window: TOPLEVEL_ID` |

`portal` is what xdg-desktop-portal-wlr's `simple` chooser reads, so wl-pick can
be the picker for `getDisplayMedia` and friends — with live previews of both
windows and displays:

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

Hovering deliberately does not move the selection — the keyboard keeps it, and a
click acts on whatever is under the cursor. Clicking the margin, a gap, or an
empty cell of a ragged last row does nothing. Navigation reads raw evdev
keycodes, so it is layout-independent, but virtual-keyboard clients such as
`wtype` cannot drive it; that goes away with the xkb support filtering needs
anyway.

**Starting a second wl-pick replaces the first.** The new overlay takes the
keyboard grab and the one that loses it exits without printing. sway answers a
capture request for a toplevel another client is already capturing with silence,
though, so the replacement's thumbnails stay mostly blank until the first
instance has gone. Every wait on a capture is capped at two seconds for that
reason: a tile that never arrives is drawn as a bare label, and the grid still
works.

## Live previews

Capture sessions stay open so tiles can refresh, and three things keep that
cheap. It is damage-driven, so the compositor produces nothing for an idle window
after the first frame. The overlay's own `wl_surface.frame` callbacks are the
clock, so refreshes stop when it isn't being presented, with `--fps` throttling
per tile on top. And tiles scrolled out of sight are unmapped, so they are
skipped too. Measured over 4 s with one animating window in ten:
`52,52,1,1,1,1,1,1,13,1` frames.

The cost is memory and bandwidth — two full-resolution buffers per window (138 MB
of shm for eight windows and a display here, against 83 MB with `--live none`)
and a readback per refreshed frame. `--live current` refreshes only the selected
tile, which is much cheaper and still reads as alive.

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

Sizes take sway's units: `600px` is absolute, `90ppt` a percentage — resolved
against **the display the grid actually appears on**, every time it runs, so one
file gives 90% of a 1280-wide laptop panel and 90% of a 3840-wide monitor. The
overlay maps explicitly on that display, at its scale, so mixed-DPI renders
crisply either way.

All four sizing settings are caps: `max-width`/`max-height` bound the overlay,
`max-columns`/`max-rows` bound the grid inside it. A thumbnail is that box
divided by those caps, which means **its size never depends on how many windows
are open** — one window gets the same thumbnail as thirty, in a smaller overlay,
because the overlay hugs whatever is actually there. Rows past `max-rows`
scroll, with a scrollbar in the right margin and the selection always kept in
view. Turning labels off gives that row back to the thumbnails rather than
shrinking the window.

## Look

The defaults come from the rofi theme this replaces: gruvbox dark, a yellow
selection filling the element padding, `ceil(sqrt(n))` columns capped at 4,
`title · app` centred underneath. Padding, gaps and margins are fixed, in
`src/theme.rs`. Long titles are ellipsised to the cell.

Labels default to the system monospace — whatever `fc-match monospace` answers,
which is what the rest of the desktop uses. `--font` names another, by family or
by full display name: `Berkeley Mono Medium SemiCondensed` works as well as
`Berkeley Mono`, because the name is split into the longest leading part that is
a real family and a style read off the remainder. Weights and widths are
understood joined or spaced, in any case, and a family whose own name ends in a
style word — `Fira Code Light` — still wins over reading that word as a style. A
name that matches nothing says so on stderr and falls back to the system
monospace rather than shaping in an arbitrary face; `--verbose` reports the font
actually used.

## Requirements

A wlroots compositor advertising `ext-image-copy-capture-v1`,
`ext-image-capture-source-v1` (with the foreign-toplevel source manager),
`ext-foreign-toplevel-list-v1`, `wlr-layer-shell-unstable-v1` and
`wp_viewporter` — sway 1.11+, and in principle Hyprland, labwc and jay, though
only sway is tested. sway is also the source of truth for the window list, over
its IPC socket, which is the one thing that would need replacing to run
elsewhere (`ext-foreign-toplevel-list-v1` already reports app id and title).

That socket comes from `SWAYSOCK`/`I3SOCK` when those point at something that
exists, and otherwise from the running sway's socket in `$XDG_RUNTIME_DIR` —
inheriting a stale path is easy, and a picker on a keybinding should not be the
thing that notices.

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
cargo test          # grid geometry, navigation and hit-testing, ellipsising,
                    # output formats, glyph output
```
