# niri-screenshare

portal backend for niri. replaces `xdg-desktop-portal-gnome` for screen sharing.

## why

niri's wiki tells you to install `xdg-desktop-portal-gnome` for screencasting. it works but pulls in half of gnome. this does the same thing without the bloat.
both call `org.gnome.Mutter.ScreenCast` on niri either way.

## install

```
paru -S niri-screenshare
```

or manually:

```
cargo build --release
sudo cp target/release/niri-screenshare /usr/lib/
sudo cp data/niri.portal /usr/share/xdg-desktop-portal/portals/
sudo cp data/org.freedesktop.impl.portal.desktop.niri.service /usr/share/dbus-1/services/
cp data/niri-screenshare.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now niri-screenshare.service
```

## config

the portal daemon (`xdg-desktop-portal`) chooses which backend to use for each interface based on `~/.config/xdg-desktop-portal/portals.conf`:

```ini
[preferred]
default=gtk
org.freedesktop.impl.portal.ScreenCast=niri
```

without this, the portal daemon would fall back to `UseIn=gnome` matching (which loads the gnome portal). this tells it to use our backend instead.

- `default=gtk` — routes stuff like file picker to the lightweight gtk portal instead of gnome's (avoids pulling nautilus)
- `ScreenCast=niri` — routes screen sharing to this backend

## behavior

**dynamic cast (default)** — `select_sources` returns immediately and auto-selects the focused output. the stream target can be changed at any time using niri's dynamic cast keybinds (`set-dynamic-cast-monitor`, `set-dynamic-cast-window`). no picker dialog, no friction.

**picker mode** — build with the `picker` feature and set `NIRI_SCREENSHARE_PICKER=1` to show a zenity dialog listing available monitors:

```bash
cargo build --release --features picker
NIRI_SCREENSHARE_PICKER=1 /usr/lib/niri-screenshare
```

this is useful when multiple monitors are connected and you want to choose which one to share at selection time.

| build | behavior | binary size |
|-------|----------|-------------|
| `default` | dynamic cast, no dialog | 1.5 MB |
| `--features picker` | dynamic cast + optional zenity picker | 1.5 MB |

## how

app → portal daemon → our backend → niri's Mutter.ScreenCast → PipeWire.
niri handles the gpu capture, we just relay the pipewire node id.
