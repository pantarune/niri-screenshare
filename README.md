# niri-screenshare

portal backend for niri. replaces `xdg-desktop-portal-gnome` for screen sharing.

```
paru -S niri-screenshare
```

no manual config needed — `portals.conf` is written on first service start.

## behavior

**dynamic cast (default)** — auto-selects the focused output. change targets anytime via niri keybinds (`set-dynamic-cast-monitor`, `set-dynamic-cast-window`). for more read the wiki https://niri-wm.github.io/niri/Screencasting.html#dynamic-screencast-target

**picker mode** — build with `--features picker` and set `NIRI_SCREENSHARE_PICKER=1` to get a GTK4 dialog with Displays / Windows tabs.

| build | behavior | size |
|-------|----------|------|
| default | dynamic cast, no dialog | 2.1 MB |
| `--features picker` | + optional GTK4 picker | 2.2 MB |

Debug the picker UI without starting a cast:

```
cargo run --features picker -- --debug-picker
```

## how

app → portal daemon → niri-screenshare → niri's Mutter.ScreenCast → PipeWire
