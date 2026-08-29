# niri-screenshare

Portal backend for niri implementing `org.freedesktop.impl.portal.ScreenCast`.
It can replace `xdg-desktop-portal-gnome` for screen sharing while keeping other
portal interfaces on the user's existing backends.

## install

### arch

```sh
paru -S niri-screenshare
```

### other

The default picker build requires the `gtk4` and `libadwaita` system packages.
Build without the picker via `cargo build --release --no-default-features`.

```sh
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

No manual config is normally needed. On first service start the backend adds only
`org.freedesktop.impl.portal.ScreenCast=niri` to the user's portal preferences;
it does not replace the default backend for unrelated portal interfaces.

## behavior

**picker mode (default)** — a GTK4 dialog with Displays / Windows tabs appears
when an app requests screen sharing (OBS, Discord, Firefox, etc.). The requested
XDG source types are respected, and the user must explicitly approve sharing
even when only one capture target is available.

Set `NIRI_SCREENSHARE_NO_PICKER=1` to skip the dialog. Pickerless mode currently
auto-selects the focused output and therefore supports monitor capture only; it
does **not** claim to implement niri's synthetic Dynamic Cast Target.

| build | behavior |
|-------|----------|
| `default` | GTK4 picker dialog for requested displays/windows |
| `--no-default-features` | pickerless focused-output capture |

### env vars

| variable | effect |
|----------|--------|
| `NIRI_SCREENSHARE_NO_PICKER=1` | skip picker and auto-select the focused output |
| `NIRI_BIN=/path/to/niri` | override niri binary path (for NixOS etc.) |
| `NIRI_SOCKET=/path/to/socket` | explicitly select the niri IPC socket |

If more than one niri IPC socket exists and none can be matched to
`WAYLAND_DISPLAY`, the backend refuses to guess; set `NIRI_SOCKET` explicitly.

### debug

Open the picker dialog standalone without starting a portal session:

```sh
niri-screenshare --debug-picker
```

Run basic environment checks:

```sh
niri-screenshare check
```

## configuration

The portal daemon reads `$XDG_CONFIG_HOME/xdg-desktop-portal/portals.conf`
(or `~/.config/xdg-desktop-portal/portals.conf` when `XDG_CONFIG_HOME` is unset)
to decide which backend to use. niri-screenshare adds its ScreenCast preference
without changing unrelated defaults. Override it by editing that file before
starting the service.

## how it works

```text
app → xdg-desktop-portal → niri-screenshare → Mutter.ScreenCast → PipeWire
```

1. an app calls `CreateSession` and `SelectSources` on the portal frontend
2. xdg-desktop-portal forwards the request to niri-screenshare
3. niri-screenshare filters targets to the requested source types and asks for consent
4. the app calls `Start`; niri-screenshare asks niri to start the selected stream
5. the PipeWire node id is returned to xdg-desktop-portal
6. xdg-desktop-portal exposes the appropriate PipeWire remote to the app

Failed starts are cleaned up and may be retried; closing a portal session also
stops the corresponding compositor screencast session.

## dependencies

- **runtime:** `xdg-desktop-portal`, `pipewire`, `niri`, `gtk4`, `libadwaita`
- **build:** `cargo`, `gtk4`, `libadwaita`

GTK4/libadwaita are not needed by a `--no-default-features` build.

## troubleshooting

**picker doesn't appear** — make sure you built with default features
(`cargo build --release`) and the service has `NIRI_SCREENSHARE_NO_PICKER`
unset. Check the service log:

```sh
journalctl --user -u niri-screenshare -n 20
```

**obs/discord shows "no capture sources"** — verify the portal backend is
registered:

```sh
busctl list | grep niri
```

If nothing shows, restart the service:

```sh
systemctl --user restart niri-screenshare
```

**portal daemon crashes on screenshare** — some `xdg-desktop-portal` 1.22.1
builds have a bug in session initialization. Upgrading or reinstalling usually
fixes it.

## credits

- [Ly-sec](https://github.com/Ly-sec) — GTK4 picker, cancel fix, NixOS packaging
