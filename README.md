# wlgrid

A window switcher for wlroots compositors: a thumbnail grid overlay that looks
like a rofi theme, and focuses the window you pick.

It replaces a `wlthumbs | rofi` pipeline. The difference is that no thumbnails
exist: each window is captured straight into a `wl_shm` buffer that is handed to
its own `wl_subsurface`, and `wp_viewporter` tells the compositor which rectangle
to scale it into. There is no image encoding, no scaler, and no full-resolution
bitmap in this process — which is also why it holds ~9 MB of RSS and appears in
about 60 ms.

```
sway-tree       0.6ms     window list + con_ids over sway IPC
toplevels       0.2ms     ext-foreign-toplevel-list handles
constraints     1.5ms     every capture session's buffer size, in one roundtrip
capture        55.0ms     8 windows, all frames in flight at once
mapped          4.2ms     layer surface + subsurfaces on screen
```

The capture phase is the compositor reading full-resolution window pixels out of
the GPU. It is bandwidth-bound (~1.1 GB/s here) and unaffected by how large the
thumbnails are.

## Status

Working, and usable as a switcher today: a static grid with keyboard navigation.
Labels, filtering and live previews are next — see the roadmap.

## Usage

```
wlgrid [--print] [--verbose] [--timeout SECS]
```

- `--print` writes the selected sway `con_id` to stdout instead of focusing it
- `--verbose` prints phase timings and how many windows were captured
- `--timeout SECS` exits after a deadline (an escape hatch: the overlay takes an
  exclusive keyboard grab)

Bind it in sway:

```
bindsym $mod+Tab exec wlgrid
```

| key | |
|---|---|
| `→` `←` / `Tab` `Shift+Tab` | next / previous window |
| `↑` `↓` | move a row |
| `Home` `End` | first / last |
| `Enter` | focus the selection |
| `Escape` / `q` | cancel, leaving focus alone |

Navigation reads raw evdev keycodes, so it is layout-independent — but it also
means virtual-keyboard clients such as `wtype` (which invent their own keymap)
cannot drive it. That goes away with xkb support, which filtering needs anyway.

## Look

Colours, font metrics and grid geometry come from the rofi theme this replaces
(gruvbox dark, a yellow selection filling the element padding, `ceil(sqrt(n))`
columns capped at 4, 16:9 tiles) and live in `src/theme.rs`. They will move to a
config file so they can't drift from the `.rasi`.

## Requirements

A wlroots compositor advertising `ext-image-copy-capture-v1`,
`ext-image-capture-source-v1` (with the foreign-toplevel source manager),
`ext-foreign-toplevel-list-v1`, `wlr-layer-shell-unstable-v1` and
`wp_viewporter` — sway 1.11+, and in principle Hyprland, labwc and jay, though
only sway is tested. sway is also the source of truth for the window list and
for focusing, over its IPC socket.

Known upstream issue: holding per-toplevel capture sessions open makes windows
blurry on **fractionally scaled** outputs
([sway#9113](https://github.com/swaywm/sway/issues/9113)). Integer scales are
unaffected. It matters more once previews are live.

## Roadmap

- **M2** labels: real text via cosmic-text, `--hide-labels` for an icon-only grid
- **M3** type-to-filter with fzf-quality fuzzy matching (and xkb keyboard input)
- **M4** live previews: keep the capture sessions open and re-capture on a rate
  limit, `--live all|current|none`
- **M5** dmabuf capture, so the pixels never leave the GPU at all

## Building

```
cargo build --release
cargo test          # grid geometry
```
