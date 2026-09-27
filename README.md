# wl-pick

A window switcher for wlroots compositors: a grid of **live** window previews
that prints which one you picked. It doubles as a screencast source picker for
the desktop portal.

No thumbnails are ever made. Each window is captured straight into a buffer
handed to its own subsurface and the compositor does the scaling, so wl-pick
appears in about 60 ms and sits around 6 MB resident however many windows are
open. There is a longer write-up of why it works this way in
[wl-pick: a live window picker for Sway](https://mil.ad/blog/2026/wl-pick.html).

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

Arrows, `hjkl` or `Tab`/`Shift+Tab` move, `PgUp`/`PgDn` and `Home`/`End` jump,
`Enter` picks and `Escape` or `q` cancels; clicking a tile picks it.
`wl-pick --help` lists the flags.

### Output formats

Different consumers need different identifiers, so `--format` offers three.
`tsv`, the default, prints `TYPE⇥ID⇥TOPLEVEL_ID⇥APP⇥TITLE`, where `ID` is the
sway `con_id` or an output name; `json` prints the same fields for `jq`; and
`portal` prints `Monitor: NAME` or `Window: TOPLEVEL_ID`.

That last one is exactly what xdg-desktop-portal-wlr's `simple` chooser reads,
so wl-pick can be the picker for `getDisplayMedia` and friends:

```ini
[screencast]
chooser_type=simple
chooser_cmd=wl-pick --format portal
```

## Config

`~/.config/wl-pick/config`, or `--config PATH`. Flat `key = value` lines with
`#` comments, everything optional, and a flag always beats the file. `--help`
lists every key; the ones worth explaining are the sizes.

```ini
max-width      = 90ppt        # the box the grid may fill
max-height     = 90ppt
max-columns    = 4            # thumbnails are that box divided by these
max-rows       = 4
```

`600px` is absolute and `90ppt` a percentage of the display the grid appears
on, worked out afresh each run, so one file suits a 1280-wide laptop panel and a
3840-wide monitor alike. All four are caps: the first two bound the overlay, the
last two bound the grid inside it, and a thumbnail is simply the one divided by
the other. A thumbnail is therefore the same size whether one window is open or
thirty — the overlay just hugs whatever is there. Rows past `max-rows` scroll.

## Requirements

A wlroots compositor advertising `ext-image-copy-capture-v1`,
`ext-image-capture-source-v1`, `ext-foreign-toplevel-list-v1`,
`wlr-layer-shell-unstable-v1` and `wp_viewporter`. In practice that means
sway 1.12+: 1.11 could capture whole outputs, and 1.12 extended that to
individual windows, which is what the previews are. sway's IPC socket is also
the source of truth for the window list. Hyprland, labwc and jay advertise the
protocols but are untested.

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

## Building

```sh
cargo build --release
cargo test
```
