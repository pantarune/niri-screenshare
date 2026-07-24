# niri-screenshare

portal backend for niri implementing `org.freedesktop.impl.portal.ScreenCast`.
replaces `xdg-desktop-portal-gnome` for screen sharing.

## install

### arch

```
paru -S niri-screenshare
```

### other

requires `gtk4` and `libadwaita` system packages (for the default picker build).
build without them via `cargo build --release --no-default-features`.

```
git clone https://github.com/pantarune/niri-screenshare
cd niri-screenshare
cargo build --release
sudo cp target/release/niri-screenshare /usr/lib/
sudo cp data/niri.portal /usr/share/xdg-desktop-portal/portals/
sudo cp data/org.freedesktop.impl.portal.desktop.niri.service /usr/share/dbus-1/services/
cp data/niri-screenshare.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now niri-screenshare.service
```

no manual config needed — `portals.conf` is written on first service start.

## behavior

**picker mode (default)** — a GTK4 dialog with Displays / Windows tabs
appears when an app requests screen sharing (OBS, Discord, Firefox, etc).
set `NIRI_SCREENSHARE_NO_PICKER=1` to skip the dialog and auto-select
the focused output instead.

**dynamic cast** — build with `--no-default-features` to remove the picker
entirely. without the picker the focused output is auto-selected. change
targets anytime via niri keybinds (`set-dynamic-cast-monitor`,
`set-dynamic-cast-window`). see [dynamic screencast
target](https://niri-wm.github.io/niri/Screencasting.html#dynamic-screencast-target)
in the niri wiki.

| build | behavior | size |
|-------|----------|------|
| `default` | GTK4 picker dialog | 1.6 MB |
| `--no-default-features` | dynamic cast, no dialog | 1.6 MB |

### env vars

| variable | effect |
|----------|--------|
| `NIRI_SCREENSHARE_NO_PICKER=1` | skip picker, auto-select focused output |
| `NIRI_BIN=/path/to/niri` | override niri binary path (for NixOS etc) |

### debug

open the picker dialog standalone without starting a portal session:

```
niri-screenshare --debug-picker
```

## configuration

the portal daemon reads `~/.config/xdg-desktop-portal/portals.conf` to
decide which backend to use. niri-screenshare writes this file on first
start. override by editing the file before starting the service.

## how it works

```
app → xdg-desktop-portal → niri-screenshare → Mutter.ScreenCast → PipeWire
```

1. an app calls `CreateSession` and `SelectSources` on the portal frontend
2. xdg-desktop-portal forwards to niri-screenshare
3. niri-screenshare shows the picker or auto-selects the focused output
4. app calls `Start` → niri-screenshare tells niri to start a PipeWire stream
5. the PipeWire node id is returned to the app through the portal
6. app connects to the PipeWire stream and captures frames

## dependencies

- **runtime:** `xdg-desktop-portal`, `pipewire`, `niri`, `gtk4`, `libadwaita`
- **build:** `cargo`, `gtk4`, `libadwaita`

## troubleshooting

**picker doesn't appear** — make sure you built with default features (`cargo build --release`)
and the service has `NIRI_SCREENSHARE_NO_PICKER` unset. check the service log:
`journalctl --user -u niri-screenshare -n 20`

**obs/discord shows "no capture sources"** — verify the portal backend is registered:
`busctl list | grep niri`. if nothing shows, restart the service:
`systemctl --user restart niri-screenshare`

**portal daemon crashes on screenshare** — some `xdg-desktop-portal` 1.22.1 builds
have a bug in session initialization. upgrading or reinstalling usually fixes it.

## credits

- [Ly-sec](https://github.com/Ly-sec) — GTK4 picker, cancel fix, NixOS packaging
