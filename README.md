# niri-screenshare

portal backend for niri. replaces `xdg-desktop-portal-gnome` for screen sharing.

```
paru -S niri-screenshare
```

no manual config needed — `portals.conf` is written on first service start.

## behavior

**dynamic cast (default)** — auto-selects the focused output. change targets anytime via niri keybinds (`set-dynamic-cast-monitor`, `set-dynamic-cast-window`). for more read the wiki https://niri-wm.github.io/niri/Screencasting.html#dynamic-screencast-target

**picker mode (default)** — a GTK4 dialog with Displays / Windows tabs appears when an app requests screen sharing. set `NIRI_SCREENSHARE_NO_PICKER=1` to skip the dialog and auto-select the focused output.

| build | behavior | size |
|-------|----------|------|
| default | GTK4 picker dialog | 1.6 MB |
| `--no-default-features` | dynamic cast, no dialog | 1.6 MB |

Debug the picker UI without starting a cast:

```
cargo run -- --debug-picker
```

## how

app → portal daemon → niri-screenshare → niri's Mutter.ScreenCast → PipeWire
